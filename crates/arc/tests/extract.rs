//! `arc.extract` (S8-28, S8 §3.5, §3.7) в core с настоящими zip и tar: черновик
//! `arc.draft`, подтверждение → `ops.copy` на архив, Esc, пароль с тремя попытками.
//! `ops` здесь — заглушка, которая запоминает команды: копирование проверяют тесты ops.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_arc::{ArcCache, ArcCfg, root_of};
use urfin_core::{
    ArgSpec, Args, Boot, CmdError, Command, CommandMeta, Core, Cx, Handler, ListingStatus,
    Location, Services, UiEvent, ValueKind, Waker,
};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, ZipWriter};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

const fn arg(name: &'static str, kind: ValueKind) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required: false,
    }
}

static OPS_ARGS: [ArgSpec; 5] = [
    arg("dest", ValueKind::Str),
    arg("src", ValueKind::Str),
    arg("paths", ValueKind::Bool),
    arg("conflict", ValueKind::Str),
    arg("queue", ValueKind::Str),
];

static OPS: [CommandMeta; 1] = [CommandMeta {
    id: "ops.copy",
    title: "Copy",
    category: "ops",
    args: &OPS_ARGS,
    repeatable: false,
}];

/// `ops` — запоминает `ops.copy`.
struct FakeOps(Arc<Mutex<Vec<Args>>>);

impl Handler for FakeOps {
    fn namespace(&self) -> &'static str {
        "ops"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &OPS
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        self.0.lock().unwrap().push(cmd.args.clone());
        Ok(())
    }
}

struct Rig {
    core: Core,
    cache: Arc<ArcCache>,
    copies: Arc<Mutex<Vec<Args>>>,
    events: Vec<UiEvent>,
}

fn rig(left: &Path, right: &Path) -> Rig {
    let cfg = ArcCfg::default();
    let cache = urfin_arc::cache(&cfg);
    let mut reg = VfsRegistry::with_local();
    for b in urfin_arc::backends(&cache, &cfg) {
        reg.register(b);
    }
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(left), Location::local(right)),
        services,
        Arc::new(NoWake),
    );
    urfin_arc::register(&mut core, reg, Arc::clone(&cache), &cfg);
    let copies = Arc::new(Mutex::new(Vec::new()));
    core.register(Box::new(FakeOps(Arc::clone(&copies))));
    Rig {
        core,
        cache,
        copies,
        events: Vec::new(),
    }
}

impl Rig {
    fn until(&mut self, what: &str, mut done: impl FnMut(&Rig) -> bool) {
        let t = Instant::now();
        loop {
            self.core.pump(Duration::from_millis(5));
            let ev = self.core.take_ui_events();
            self.events.extend(ev);
            if done(self) {
                return;
            }
            assert!(
                t.elapsed() < Duration::from_secs(20),
                "не дождались: {what}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn loaded(&mut self, names: usize) {
        self.until("каталог", |r| {
            let t = r.core.state().active_tab();
            !matches!(
                t.listing.status,
                ListingStatus::Loading { .. } | ListingStatus::Idle
            ) && t.listing.entries.len() == names
        });
    }

    /// Последнее `arc.draft` нужной стадии, вынутое из событий.
    fn draft(&mut self, stage: &str) -> Args {
        let seen = |r: &Rig| {
            r.events.iter().any(|e| {
                matches!(e, UiEvent::Ext { id, args }
                    if id.as_str() == "arc.draft" && args.str("stage") == Some(stage))
            })
        };
        self.until(stage, seen);
        let i = self
            .events
            .iter()
            .rposition(|e| {
                matches!(e, UiEvent::Ext { id, args }
                if id.as_str() == "arc.draft" && args.str("stage") == Some(stage))
            })
            .unwrap();
        match self.events.remove(i) {
            UiEvent::Ext { args, .. } => args,
            _ => unreachable!(),
        }
    }

    fn toasts(&mut self) -> Vec<String> {
        self.core.pump(Duration::from_millis(5));
        let ev = self.core.take_ui_events();
        self.events.extend(ev);
        self.events
            .iter()
            .filter_map(|e| match e {
                UiEvent::Toast { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn copies(&self) -> Vec<Args> {
        self.copies.lock().unwrap().clone()
    }

    fn confirm(&mut self, dest: &str, pw: Option<&str>) {
        let mut c = Command::new("arc.extract")
            .arg("confirmed", true)
            .arg("dest", dest)
            .arg("conflict", "skip")
            .arg("queue", "bg");
        if let Some(p) = pw {
            c = c.arg("password", p);
        }
        self.core.dispatch(c);
    }
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("urfin-arc-extract-")
        .tempdir()
        .unwrap()
}

fn make_zip(path: &Path, files: &[(&str, &[u8])], aes: Option<&str>) {
    let mut z = ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, body) in files {
        let mut o = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        if let Some(pw) = aes {
            o = o.with_aes_encryption(AesMode::Aes256, pw);
        }
        z.start_file(*name, o).unwrap();
        z.write_all(body).unwrap();
    }
    z.finish().unwrap();
}

fn make_tar(path: &Path, files: &[(&str, &[u8])]) {
    let mut b = tar::Builder::new(Vec::new());
    for (name, body) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        b.append_data(&mut h, name, *body).unwrap();
    }
    std::fs::write(path, b.into_inner().unwrap()).unwrap();
}

fn dest_of(out: &Path) -> String {
    format!("{}\\", out.display())
}

#[test]
fn three_archives_three_subfolders_in_the_passive_panel() {
    let (src, out) = (tmp(), tmp());
    make_zip(&src.path().join("a.zip"), &[("x/1.txt", b"one")], None);
    make_zip(&src.path().join("b.zip"), &[("2.txt", b"twotwo")], None);
    make_tar(&src.path().join("c.tar"), &[("3.txt", b"333")]);
    std::fs::write(src.path().join("notes.txt"), b"not an archive").unwrap();
    let mut r = rig(src.path(), out.path());
    r.loaded(4);
    r.core.dispatch(Command::new("sel.all"));
    r.core.dispatch(Command::new("arc.extract"));
    let d = r.draft("open");
    assert_eq!(d.int("count"), Some(3), "{d:?}");
    assert_eq!(d.int("size"), Some(3 + 6 + 3));
    assert_eq!(d.str("name"), Some("a.zip"));
    assert_eq!(d.str("subdir"), Some("a"));
    assert_eq!(d.bool("encrypted"), Some(false));
    assert_eq!(d.str("dest"), Some(dest_of(out.path()).as_str()));
    assert!(r.copies().is_empty(), "до подтверждения — ничего");

    r.confirm(&dest_of(out.path()), None);
    r.draft("close");
    let copies = r.copies();
    assert_eq!(copies.len(), 3, "job на архив");
    for (c, (file, scheme, sub)) in copies.iter().zip([
        ("a.zip", "zip", "a"),
        ("b.zip", "zip", "b"),
        ("c.tar", "tar", "c"),
    ]) {
        let want = root_of(scheme, Location::local(src.path().join(file))).to_uri();
        assert_eq!(c.str("src"), Some(want.as_str()));
        assert_eq!(
            c.str("dest"),
            Some(format!("{}\\{sub}\\", out.path().display()).as_str())
        );
        assert_eq!(c.str("conflict"), Some("skip"));
        assert_eq!(c.str("queue"), Some("bg"));
    }
}

#[test]
fn esc_starts_nothing() {
    let (src, out) = (tmp(), tmp());
    make_zip(&src.path().join("a.zip"), &[("1.txt", b"one")], None);
    let mut r = rig(src.path(), out.path());
    r.loaded(1);
    r.core.dispatch(Command::new("sel.all"));
    r.core.dispatch(Command::new("arc.extract"));
    r.draft("open");
    r.core
        .dispatch(Command::new("arc.extract").arg("cancel", true));
    r.confirm(&dest_of(out.path()), None);
    let toasts = r.toasts();
    assert!(r.copies().is_empty(), "после Esc — ничего");
    assert_eq!(
        toasts.len(),
        1,
        "подтверждение без черновика — отказ: {toasts:?}"
    );
}

#[test]
fn wrong_password_keeps_the_strip_with_tries_left() {
    let (src, out) = (tmp(), tmp());
    let file = src.path().join("s.zip");
    make_zip(
        &file,
        &[("big.bin", &[7u8; 5000]), ("t.txt", b"tiny")],
        Some("pw"),
    );
    let mut r = rig(src.path(), out.path());
    r.loaded(1);
    r.core.dispatch(Command::new("sel.all"));
    r.core.dispatch(Command::new("arc.extract"));
    let d = r.draft("open");
    assert_eq!(d.bool("encrypted"), Some(true));
    assert_eq!(d.str("cipher"), Some("AES-256"));
    assert_eq!(d.int("left"), Some(3));

    r.confirm(&dest_of(out.path()), Some("bad"));
    assert_eq!(r.draft("wrong").int("left"), Some(2));
    r.confirm(&dest_of(out.path()), Some(""));
    assert_eq!(r.draft("wrong").int("left"), Some(2), "пустой — не попытка");
    r.confirm(&dest_of(out.path()), Some("worse"));
    assert_eq!(r.draft("wrong").int("left"), Some(1));
    assert_eq!(r.copies(), Vec::<Args>::new());
    let idx = r.cache.latest(&Location::local(&file)).unwrap();
    assert!(!idx.ctx().has_password(), "неверный пароль не остаётся");

    r.confirm(&dest_of(out.path()), Some("pw"));
    r.draft("close");
    assert_eq!(r.copies().len(), 1);
    assert!(idx.ctx().has_password(), "пароль — в индексе до ops.copy");
}

#[test]
fn three_wrong_passwords_close_the_strip() {
    let (src, out) = (tmp(), tmp());
    make_zip(&src.path().join("s.zip"), &[("t.txt", b"tiny")], Some("pw"));
    let mut r = rig(src.path(), out.path());
    r.loaded(1);
    r.core.dispatch(Command::new("sel.all"));
    r.core.dispatch(Command::new("arc.extract"));
    r.draft("open");
    for left in [2, 1] {
        r.confirm(&dest_of(out.path()), Some("bad"));
        assert_eq!(r.draft("wrong").int("left"), Some(left));
    }
    r.confirm(&dest_of(out.path()), Some("bad"));
    r.draft("close");
    assert_eq!(r.copies(), Vec::<Args>::new());
    assert_eq!(r.toasts().len(), 1, "попыток не осталось");
}

#[test]
fn selection_inside_the_archive_goes_to_ops_without_src() {
    let (src, out) = (tmp(), tmp());
    let file = src.path().join("urfin-0.4.2.zip");
    make_zip(
        &file,
        &[
            ("docs/a.md", b"aaaa"),
            ("docs/b.md", b"bb"),
            ("x.txt", b"x"),
        ],
        None,
    );
    let mut r = rig(src.path(), out.path());
    r.loaded(1);
    let root = root_of("zip", Location::local(&file));
    r.core.dispatch(Command::new("panel.go").arg("path", root));
    r.loaded(2);
    r.core
        .dispatch(Command::new("panel.cursor.to").arg("name", "docs"));
    r.core.dispatch(Command::new("arc.extract"));
    let d = r.draft("open");
    assert_eq!(d.int("items"), Some(1));
    assert_eq!(d.int("size"), Some(6), "размер поддерева");
    assert_eq!(d.str("subdir"), Some("urfin-0.4.2"));
    r.core.dispatch(
        Command::new("arc.extract")
            .arg("confirmed", true)
            .arg("dest", dest_of(out.path()))
            .arg("subdir", false)
            .arg("paths", false),
    );
    r.draft("close");
    let c = r.copies();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].str("src"), None, "выделенное берёт ops");
    assert_eq!(c[0].str("dest"), Some(dest_of(out.path()).as_str()));
    assert_eq!(c[0].bool("paths"), Some(false));
}
