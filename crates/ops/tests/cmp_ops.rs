//! F5 и F8 из панели сравнения `cmp:` (S8-38, D-12): строка — файл стороны, где он есть
//! (у пары — более новой); F5 копирует его на другую сторону по тому же пути от корня
//! сравнения, F8 удаляет именно его. Адрес строки ops берёт у схемы (`Vfs::resolve`),
//! корни — у стора в `SearchModel`.
//!
//! `urfin-search` ops не зависит, поэтому схема `cmp:` здесь своя: строки — имя от корня и
//! сторона.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Панель `cmp:/1`: строки «путь от корня → сторона» над корнями `roots`; листинг — только
/// то, что ещё отличается (файл есть с одной стороны).
struct FakeCmp {
    roots: [PathBuf; 2],
    rows: Vec<(&'static str, usize)>,
}

struct Ready(Option<Vec<Entry>>);
impl ListStream for Ready {
    fn next_batch(&mut self, out: &mut Vec<Entry>, _: &CancelToken) -> Result<bool, VfsError> {
        out.extend(self.0.take().unwrap_or_default());
        Ok(false)
    }
}

fn entry(name: &str) -> Entry {
    Entry {
        name: name.into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind: EntryKind::File,
    }
}

impl Vfs for FakeCmp {
    fn scheme(&self) -> &'static str {
        "cmp"
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        Caps(Caps::VIRTUAL | Caps::READ_ONLY)
    }
    fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        let live = self
            .rows
            .iter()
            .filter(|(n, side)| {
                self.roots[*side].join(n).exists() && !self.roots[1 - side].join(n).exists()
            })
            .map(|(n, _)| entry(n))
            .collect();
        Ok(Box::new(Ready(Some(live))))
    }
    fn stat(&self, _: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        Ok(Metadata {
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
        })
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
        self.rows
            .iter()
            .find(|(n, _)| *n == item.name)
            .map(|(n, side)| Location::local(self.roots[*side].join(n)))
            .ok_or_else(|| VfsError::NotFound(item.name.as_str().into()))
    }
}

/// Кладёт стор сравнения №1 с корнями сторон (как `ops.compare { recursive }`).
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

fn setup(l: &Path, r: &Path, rows: Vec<(&'static str, usize)>) -> Core {
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(FakeCmp {
        roots: [l.to_path_buf(), r.to_path_buf()],
        rows,
    }));
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let cmp = Location {
        backend: "cmp".into(),
        authority: None,
        path: "/1".into(),
        outer: None,
    };
    let mut core = Core::new(
        Boot::new(cmp, Location::local(r)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
    core.register(Box::new(Seed(Some(vec![
        Location::local(l),
        Location::local(r),
    ]))));
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

fn put(root: &Path, rel: &str, data: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

#[test]
fn cmp_f5_copies_to_the_other_side_by_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (tmp.path().join("l"), tmp.path().join("r"));
    put(&l, r"src\a.rs", b"left");
    put(&r, r"docs\b.md", b"right");
    fs::create_dir_all(&r).unwrap();
    let mut core = setup(&l, &r, vec![(r"src\a.rs", 0), (r"docs\b.md", 1)]);
    pump_until(&mut core, "listed", |c| names(c).len() == 2);
    core.take_ui_events();

    // Без подтверждения — черновик «на другую сторону», ничего не копируется.
    core.dispatch(Command::new("sel.all"));
    core.dispatch(Command::new("ops.copy"));
    core.pump(Duration::from_millis(2));
    let draft = core.take_ui_events().into_iter().find_map(|e| match e {
        UiEvent::Ext { id, args } if id.as_str() == "ops.draft" => Some(args),
        _ => None,
    });
    let draft = draft.expect("ops.draft");
    assert_eq!(draft.bool("cmp"), Some(true));
    assert!(!r.join(r"src\a.rs").exists());

    core.dispatch(Command::new("ops.copy").arg("confirmed", true));
    job_done(&mut core, "copy");
    assert_eq!(fs::read(r.join(r"src\a.rs")).unwrap(), b"left");
    assert_eq!(fs::read(l.join(r"docs\b.md")).unwrap(), b"right");
    // Обе строки сравнялись — после перечитывания панели их нет.
    pump_until(&mut core, "rows gone", |c| names(c).is_empty());
}

#[test]
fn cmp_f8_deletes_the_row_side() {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (tmp.path().join("l"), tmp.path().join("r"));
    put(&l, "only.txt", b"x");
    put(&r, "keep.txt", b"y");
    let mut core = setup(&l, &r, vec![("only.txt", 0), ("keep.txt", 1)]);
    pump_until(&mut core, "listed", |c| names(c).len() == 2);
    core.dispatch(Command::new("panel.cursor.to").arg("name", "only.txt"));
    core.pump(Duration::from_millis(2));
    core.dispatch(
        Command::new("ops.delete")
            .arg("current", true)
            .arg("permanent", true)
            .arg("confirmed", true),
    );
    pump_until(&mut core, "row removed", |c| names(c).len() == 1);
    assert!(!l.join("only.txt").exists());
    assert!(r.join("keep.txt").exists());
}
