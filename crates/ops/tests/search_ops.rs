//! Операции из результатов поиска (S8-12, S8 §2.5; решение «s8-answers», В2 А): элемент
//! `search:` ops разрешает через `Vfs::resolve` в настоящий адрес; F5 и F6 сохраняют
//! относительную папку (при нескольких корнях — под меткой корня, решение
//! «s8-copy-roots»), F8 удаляет именно файл строки, цель в `search:` — отказ.
//!
//! `urfin-search` ops не зависит, поэтому схема здесь своя, с тем же видом имён: путь от
//! корня, при нескольких корнях — с корнем впереди. Листинг отдаёт только живые файлы —
//! как перепроверка после `panel.refresh`.

use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use urfin_core::search::SearchModel;
use urfin_core::{
    Attrs, Boot, CancelToken, CmdError, Command, CommandMeta, Core, Cx, Entry, EntryKind, Handler,
    JobState, Location, Services, UiEvent, Waker,
};
use urfin_vfs::{
    Caps, ListOpts, ListStream, Metadata, Space, Vfs, VfsCfg, VfsError, VfsListing, VfsPath,
    VfsRead, VfsRegistry, WatchGuard, WatchTx,
};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Строки панели: имя → настоящий путь.
type Rows = Arc<Mutex<Vec<(CompactString, PathBuf)>>>;

/// Панель `search:/1` над готовыми строками.
struct FakeSearch(Rows);

struct Ready(Option<Vec<Entry>>);
impl ListStream for Ready {
    fn next_batch(&mut self, out: &mut Vec<Entry>, _: &CancelToken) -> Result<bool, VfsError> {
        out.extend(self.0.take().unwrap_or_default());
        Ok(false)
    }
}

fn entry(name: &str, kind: EntryKind) -> Entry {
    Entry {
        name: name.into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind,
    }
}

fn dir_meta() -> Metadata {
    Metadata {
        kind: EntryKind::Dir,
        attrs: Attrs::default(),
        size: 0,
        alloc_size: None,
        mtime: 0,
        atime: 0,
        ctime: None,
        btime: None,
        mode: None,
        uid: None,
        gid: None,
        nlink: None,
        file_id: None,
        reparse_tag: None,
        link_target: None,
    }
}

impl Vfs for FakeSearch {
    fn scheme(&self) -> &'static str {
        "search"
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        Caps(Caps::VIRTUAL | Caps::READ_ONLY)
    }
    fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        let rows = self.0.lock().unwrap();
        let live = rows
            .iter()
            .filter(|(_, p)| p.exists())
            .map(|(n, _)| entry(n, EntryKind::File))
            .collect();
        Ok(Box::new(Ready(Some(live))))
    }
    fn stat(&self, _: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        Ok(dir_meta())
    }
    fn stat_entry(&self, d: &VfsPath, _: &str, _: Attrs) -> Result<Entry, VfsError> {
        Err(VfsError::NotFound(d.path.clone()))
    }
    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        Err(VfsError::NotFound(p.path.clone()))
    }
    fn space(&self, _: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("space"))
    }
    fn local_path(&self, _: &VfsPath) -> Option<PathBuf> {
        None
    }
    fn watch(&self, _: &VfsPath, _: WatchTx) -> Result<WatchGuard, VfsError> {
        Ok(WatchGuard::new(()))
    }
    fn resolve(&self, _: &VfsPath, item: &Entry) -> Result<VfsPath, VfsError> {
        let rows = self.0.lock().unwrap();
        rows.iter()
            .find(|(n, _)| *n == item.name)
            .map(|(_, p)| Location::local(p))
            .ok_or_else(|| VfsError::NotFound(item.name.as_str().into()))
    }
}

/// Кладёт `SearchModel` с корнями поиска №1 при первом pump (как handler `search`).
struct Seed(Option<Vec<Location>>);
impl Handler for Seed {
    fn namespace(&self) -> &'static str {
        "seed"
    }
    fn commands(&self) -> &'static [CommandMeta] {
        &[]
    }
    fn handle(&mut self, _: &Command, _: &mut Cx<'_>) -> Result<(), CmdError> {
        Ok(())
    }
    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some(roots) = self.0.take() {
            let mut m = SearchModel::default();
            m.open(roots);
            cx.state.ext.insert(m);
        }
    }
}

fn search_loc() -> Location {
    Location {
        backend: "search".into(),
        authority: None,
        path: "/1".into(),
        outer: None,
    }
}

/// Левая (активная) панель — `search:/1` по `rows`, правая — `right`.
fn setup(roots: &[&Path], rows: &[(String, PathBuf)], right: &Path) -> Core {
    let rows: Rows = Arc::new(Mutex::new(
        rows.iter()
            .map(|(n, p)| (CompactString::from(n.as_str()), p.clone()))
            .collect(),
    ));
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(FakeSearch(rows)));
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(search_loc(), Location::local(right)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
    core.register(Box::new(Seed(Some(
        roots.iter().copied().map(Location::local).collect(),
    ))));
    core
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn names(core: &Core) -> Vec<String> {
    let t = core.state().active_tab();
    t.listing
        .entries
        .iter()
        .map(|e| e.name.to_string())
        .collect()
}

fn listed(core: &mut Core, n: usize) {
    pump_until(core, "listed", |c| names(c).len() == n);
}

fn cursor(core: &mut Core, name: &str) {
    core.dispatch(Command::new("panel.cursor.to").arg("name", name));
    core.pump(Duration::from_millis(2));
}

fn job_done(core: &mut Core, kind: &str) {
    pump_until(core, "job ends", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| j.kind == kind && !j.state.is_active())
    });
    let j = core.state().jobs.iter().find(|j| j.kind == kind).unwrap();
    assert!(
        matches!(&j.state, JobState::Done(s) if s.errors == 0),
        "{kind}: {:?}",
        j.state
    );
}

/// Два одноимённых файла в разных папках одного корня.
fn two_cargo(root: &Path) -> Vec<(String, PathBuf)> {
    ["a", "b"]
        .iter()
        .map(|d| {
            let p = root.join(d).join("Cargo.toml");
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, d.as_bytes()).unwrap();
            (format!(r"{d}\Cargo.toml"), p)
        })
        .collect()
}

#[test]
fn search_f8_deletes_the_row_under_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let rows = two_cargo(&root);
    let mut core = setup(&[&root], &rows, tmp.path());
    listed(&mut core, 2);
    cursor(&mut core, r"b\Cargo.toml");
    core.dispatch(
        Command::new("ops.delete")
            .arg("current", true)
            .arg("permanent", true)
            .arg("confirmed", true),
    );
    // Строка ушла после перечитывания панели.
    pump_until(&mut core, "row removed", |c| names(c).len() == 1);
    assert!(root.join(r"a\Cargo.toml").exists());
    assert!(!root.join(r"b\Cargo.toml").exists());
    assert_eq!(names(&core), [r"a\Cargo.toml"]);
}

#[test]
fn search_f5_keeps_relative_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let out = tmp.path().join("out");
    let rows = two_cargo(&root);
    let mut core = setup(&[&root], &rows, tmp.path());
    listed(&mut core, 2);
    core.dispatch(Command::new("sel.all"));
    core.dispatch(Command::new("ops.copy").arg("dest", out.to_string_lossy().as_ref()));
    job_done(&mut core, "copy");
    assert_eq!(fs::read(out.join(r"a\Cargo.toml")).unwrap(), b"a");
    assert_eq!(fs::read(out.join(r"b\Cargo.toml")).unwrap(), b"b");
    assert!(root.join(r"a\Cargo.toml").exists());
}

/// Строки нескольких корней: файл `rel` с содержимым-номером в каждом корне; имя строки —
/// полный путь, как у поиска по нескольким корням.
fn rows_in(roots: &[&Path], rel: &str) -> Vec<(String, PathBuf)> {
    roots
        .iter()
        .enumerate()
        .map(|(i, root)| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, i.to_string()).unwrap();
            (p.to_string_lossy().into_owned(), p)
        })
        .collect()
}

/// Несколько корней (решение «s8-copy-roots», Б): одинаковые пути разных корней
/// ложатся под имя своего корня и не сталкиваются.
#[test]
fn search_f6_many_roots_under_root_name() {
    let tmp = tempfile::tempdir().unwrap();
    let (r1, r2) = (tmp.path().join("core"), tmp.path().join("ops"));
    let out = tmp.path().join("out");
    let rows = rows_in(&[&r1, &r2], r"src\lib.rs");
    let mut core = setup(&[&r1, &r2], &rows, tmp.path());
    listed(&mut core, 2);
    core.dispatch(Command::new("sel.all"));
    core.dispatch(Command::new("ops.move").arg("dest", out.to_string_lossy().as_ref()));
    job_done(&mut core, "move");
    assert_eq!(fs::read(out.join(r"core\src\lib.rs")).unwrap(), b"0");
    assert_eq!(fs::read(out.join(r"ops\src\lib.rs")).unwrap(), b"1");
    assert!(!out.join("src").exists());
    assert!(!r1.join(r"src\lib.rs").exists() && !r2.join(r"src\lib.rs").exists());
    // Перенесённое уходит из панели.
    pump_until(&mut core, "rows removed", |c| names(c).is_empty());
}

/// Одинаковые имена корней (`a\src`, `b\src`) — метка берёт родительскую папку.
#[test]
fn search_f5_same_root_names_take_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let (r1, r2) = (tmp.path().join(r"a\src"), tmp.path().join(r"b\src"));
    let out = tmp.path().join("out");
    let rows = rows_in(&[&r1, &r2], "lib.rs");
    let mut core = setup(&[&r1, &r2], &rows, tmp.path());
    listed(&mut core, 2);
    core.dispatch(Command::new("sel.all"));
    core.dispatch(Command::new("ops.copy").arg("dest", out.to_string_lossy().as_ref()));
    job_done(&mut core, "copy");
    assert_eq!(fs::read(out.join(r"a\src\lib.rs")).unwrap(), b"0");
    assert_eq!(fs::read(out.join(r"b\src\lib.rs")).unwrap(), b"1");
    assert!(r1.join("lib.rs").exists() && r2.join("lib.rs").exists());
}

/// Корень-диск — метка из буквы диска; файл под вложенным корнем берёт его метку.
#[test]
fn search_f5_drive_root_is_letter() {
    let tmp = tempfile::tempdir().unwrap();
    let mut comps = tmp.path().components();
    let (Some(Component::Prefix(pre)), Some(Component::RootDir)) = (comps.next(), comps.next())
    else {
        panic!("временная папка не на диске: {}", tmp.path().display());
    };
    let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = pre.kind() else {
        panic!("временная папка не на диске: {}", tmp.path().display());
    };
    let drive: PathBuf = tmp.path().components().take(2).collect();
    let under_drive = tmp.path().strip_prefix(&drive).unwrap();
    let inner = tmp.path().join("r2");
    let out = tmp.path().join("out");
    let mut rows = rows_in(&[&tmp.path().join("r1")], "f.txt");
    rows.extend(rows_in(&[&inner], r"y\g.txt"));
    let mut core = setup(&[&drive, &inner], &rows, tmp.path());
    listed(&mut core, 2);
    core.dispatch(Command::new("sel.all"));
    core.dispatch(Command::new("ops.copy").arg("dest", out.to_string_lossy().as_ref()));
    job_done(&mut core, "copy");
    let letter = char::from(letter).to_ascii_uppercase().to_string();
    let first = out.join(letter).join(under_drive).join(r"r1\f.txt");
    assert_eq!(fs::read(&first).unwrap(), b"0", "{}", first.display());
    assert_eq!(fs::read(out.join(r"r2\y\g.txt")).unwrap(), b"0");
}

fn toasts(core: &mut Core) -> Vec<String> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn search_as_destination_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let rows = two_cargo(&root);
    let mut core = setup(&[&root], &rows, tmp.path());
    listed(&mut core, 2);
    cursor(&mut core, r"a\Cargo.toml");
    core.take_ui_events();
    core.dispatch(Command::new("ops.copy").arg("dest", "search:/1"));
    core.pump(Duration::from_millis(5));
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("scheme", "search");
    let want = urfin_i18n::current().fmt("ops-no-write", &a);
    assert_eq!(toasts(&mut core), [want]);
    assert!(core.state().jobs.iter().all(|j| j.kind != "copy"));
}

#[test]
fn search_shift_f6_renames_in_real_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let rows = two_cargo(&root);
    let mut core = setup(&[&root], &rows, tmp.path());
    listed(&mut core, 2);
    cursor(&mut core, r"b\Cargo.toml");
    core.dispatch(Command::new("ops.rename").arg("to", r"b\New.toml"));
    pump_until(&mut core, "renamed", |_| root.join(r"b\New.toml").exists());
    assert!(!root.join(r"b\Cargo.toml").exists());
    assert!(root.join(r"a\Cargo.toml").exists());
}
