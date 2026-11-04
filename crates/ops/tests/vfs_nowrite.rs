//! Схема без записи (решение 2026-11-04 «vfs-write»): методы записи `Vfs` по умолчанию
//! «не поддерживается» — операция кончается понятной ошибкой в отчёте, а схема с
//! `Caps::READ_ONLY` получает отказ ещё до задачи.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{
    Attrs, Boot, CancelToken, Command, Core, Entry, EntryKind, JobState, Location, Services,
    UiEvent, Waker,
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

/// Схема с пустой папкой `/dst` и без методов записи.
struct Bare(&'static str, Caps);

struct Empty;
impl ListStream for Empty {
    fn next_batch(&mut self, _: &mut Vec<Entry>, _: &CancelToken) -> Result<bool, VfsError> {
        Ok(false)
    }
}

impl Vfs for Bare {
    fn scheme(&self) -> &'static str {
        self.0
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        self.1
    }
    fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Ok(Box::new(Empty))
    }
    fn stat(&self, p: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        if p.path.to_string_lossy().trim_end_matches('/') != "/dst" {
            return Err(VfsError::NotFound(p.path.clone()));
        }
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
}

fn setup(left: &std::path::Path) -> Core {
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(Bare("bare", Caps::default())));
    reg.register(Arc::new(Bare("ro", Caps(Caps::READ_ONLY))));
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(left), Location::local(left)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
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

fn no_write(scheme: &str) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("scheme", scheme.to_owned());
    urfin_i18n::current().fmt("ops-no-write", &a)
}

/// Курсор на `name` в левой (активной) панели.
fn cursor(core: &mut Core, name: &str) {
    pump_until(core, "listed", |c| {
        c.state()
            .active_tab()
            .listing
            .entries
            .iter()
            .any(|e| e.name == name)
    });
    core.dispatch(Command::new("panel.cursor.to").arg("name", name));
    core.pump(Duration::from_millis(2));
}

#[test]
fn copy_into_scheme_without_write_fails_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("d")).unwrap();
    fs::write(tmp.path().join("d").join("a.txt"), b"abc").unwrap();
    let mut core = setup(tmp.path());
    cursor(&mut core, "d");
    core.dispatch(Command::new("ops.copy").arg("dest", "bare://h/dst/"));
    pump_until(&mut core, "job ends", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| j.kind == "copy" && !j.state.is_active())
    });
    let j = core.state().jobs.iter().find(|j| j.kind == "copy").unwrap();
    let JobState::Done(sum) = &j.state else {
        panic!("copy ends with errors: {:?}", j.state);
    };
    assert_eq!((sum.items, sum.errors), (0, 1), "{sum:?}");
    let want = no_write("bare");
    assert!(!want.contains("ops-no-write"), "fluent key exists");
    let toasts = toasts(&mut core);
    assert!(toasts.iter().any(|t| t.contains(&want)), "{toasts:?}");
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
fn read_only_scheme_is_rejected_before_job() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.txt"), b"abc").unwrap();
    let mut core = setup(tmp.path());
    cursor(&mut core, "a.txt");
    core.take_ui_events();
    core.dispatch(Command::new("ops.copy").arg("dest", "ro://h/dst/"));
    core.pump(Duration::from_millis(5));
    assert_eq!(toasts(&mut core), [no_write("ro")]);
    assert!(core.state().jobs.iter().all(|j| j.kind != "copy"));
}
