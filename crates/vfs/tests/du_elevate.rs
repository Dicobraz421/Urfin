//! Досчёт с правами через core (F01-12): `du.elevate` → задача → запуск (подставка) →
//! файл результата → итоги в снимке и тост. Подставка `InProc` выполняет тот же
//! повышенный код ([`elevated::serve`]) в потоке, без `runas`; `Declined` — отказ в UAC.

// Корни с буквой диска и повышенный процесс — только Windows.
#![cfg(windows)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, Level, ListingStatus, Location, Services, UiEvent, Waker};
use urfin_vfs::du::elevated::{self, Elevated, Elevation, Elevator, LaunchError, Request};
use urfin_vfs::du::{DirId, DuCfg, DuRegistry, DuTree, DuVfs, SizeMode, SnapMeta, VolumeKey, dvfs};
use urfin_vfs::volume::FsKind;
use urfin_vfs::walk::RawName;
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Тот же повышенный код в потоке этого процесса (без `runas` и укрепления).
struct InProc;

struct Thread(Option<JoinHandle<u32>>);

impl Elevated for Thread {
    fn wait(&mut self, t: Duration) -> Option<u32> {
        if self.0.as_ref().is_some_and(JoinHandle::is_finished) {
            return self.0.take().and_then(|h| h.join().ok());
        }
        std::thread::sleep(t);
        None
    }
    fn kill(&mut self) {}
}

impl Elevator for InProc {
    fn launch(&self, args: &[OsString]) -> Result<Box<dyn Elevated>, LaunchError> {
        let args = args.to_vec();
        let h = std::thread::spawn(move || match Request::parse(&args) {
            Ok(r) => u32::from(elevated::serve(&r)),
            Err(_) => u32::from(elevated::exit::BAD_ARGS),
        });
        Ok(Box::new(Thread(Some(h))))
    }
}

/// Отказ в окне UAC.
struct Declined;

impl Elevator for Declined {
    fn launch(&self, _: &[OsString]) -> Result<Box<dyn Elevated>, LaunchError> {
        Err(LaunchError::Declined)
    }
}

fn raw(name: &str) -> RawName<'_> {
    RawName::Str {
        name,
        escaped: false,
    }
}

/// Итоговый путь (`C:\…`, длинные имена): так корень видит повышенный процесс.
fn real(p: &Path) -> PathBuf {
    let s = fs::canonicalize(p).unwrap().to_string_lossy().into_owned();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

/// Корень: `open\` (посчитан, 100 Б) и `locked\` (в снимке — «нет доступа»; на диске —
/// два файла, 3000 Б, их и досчитывает подставка).
fn tree(root: &Path) -> (urfin_vfs::du::DuSnapshot, DirId) {
    fs::create_dir_all(root.join("open")).unwrap();
    fs::create_dir_all(root.join("locked").join("in")).unwrap();
    fs::write(root.join("open").join("a.bin"), vec![0u8; 100]).unwrap();
    fs::write(root.join("locked").join("x.bin"), vec![0u8; 1000]).unwrap();
    fs::write(
        root.join("locked").join("in").join("y.bin"),
        vec![0u8; 2000],
    )
    .unwrap();
    let t = DuTree::new();
    let open = t.add_dir(0, &raw("open"), 0, 0).unwrap();
    let locked = t.add_dir(0, &raw("locked"), 0, 0).unwrap();
    t.add_sizes(open, 100, 100, 1);
    t.deny(locked);
    for d in [open, locked, 0] {
        t.complete(d);
    }
    let snap = t.freeze(SnapMeta {
        root: root.to_path_buf(),
        volume: VolumeKey {
            serial: 0,
            fs: FsKind::Ntfs,
        },
        scanned_at: 1,
        took_ms: 1,
        size_mode: SizeMode::Logical,
        track_min: 1 << 20,
        flags: 0,
    });
    (snap, locked)
}

fn setup(
    root: &Path,
    runner: Arc<dyn Elevator>,
    state: &Path,
) -> (Core, Arc<DuRegistry>, Location) {
    let du = DuRegistry::new();
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(DuVfs::new(Arc::clone(&du))));
    let reg = Arc::new(reg);
    let cfg = VfsCfg::default();
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg.clone())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(root), Location::local(root)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, cfg);
    urfin_vfs::du::register_full(
        &mut core,
        DuCfg {
            size: SizeMode::Logical,
            ..DuCfg::default()
        },
        Arc::clone(&du),
        None,
        Some(Elevation::new(runner, state)),
    );
    let (snap, _) = tree(root);
    let id = du.open_snapshot(snap, false);
    du.fix(id);
    let loc = dvfs::location(id, Path::new(""));
    core.dispatch(Command::new("panel.go").arg("path", loc.clone()));
    pump_until(&mut core, "du tab", |c| {
        let t = c.state().active_tab();
        t.loc == loc && t.listing.status == ListingStatus::Complete
    });
    (core, du, loc)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn idle(core: &Core) -> bool {
    core.state().jobs.iter().all(|j| !j.state.is_active())
}

fn toasts(core: &mut Core) -> Vec<(Level, String)> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { level, text } => Some((level, text)),
            _ => None,
        })
        .collect()
}

#[test]
fn elevate_merges_sums_into_snapshot() {
    let _ = urfin_i18n::set_language("en");
    let tmp = tempfile::tempdir().unwrap();
    let base = real(tmp.path());
    let root = base.join("work");
    let state = base.join("state");
    let (mut core, du, loc) = setup(&root, Arc::new(InProc), &state);
    let before = du.info(&loc).unwrap();
    assert_eq!((before.denied, before.bytes), (1, 100));
    assert_eq!(du.denied_under(&loc), 1);
    let _ = toasts(&mut core);
    core.dispatch(Command::new("du.elevate"));
    pump_until(&mut core, "elevated sums", |c| {
        idle(c)
            && du
                .info(&loc)
                .is_some_and(|i| i.denied == 0 && !i.elev.running)
    });
    let after = du.info(&loc).unwrap();
    assert_eq!(after.bytes, 3100);
    assert_eq!(after.files, 3);
    assert_eq!(du.denied_under(&loc), 0);
    let t = toasts(&mut core);
    assert!(
        t.iter().any(|(l, _)| *l == Level::Info),
        "toast «досчитано»: {t:?}"
    );
    // Файл ответа прочитан и удалён; знаков отмены нет.
    let left: Vec<_> = fs::read_dir(state.join("du").join(elevated::SUBDIR))
        .unwrap()
        .flatten()
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // Больше нечего досчитывать.
    core.dispatch(Command::new("du.elevate"));
    core.pump(Duration::from_millis(1));
    assert!(
        toasts(&mut core).iter().any(|(_, s)| !s.is_empty()),
        "отказ «нет папок без доступа»"
    );
}

#[test]
fn declined_uac_keeps_snapshot() {
    let _ = urfin_i18n::set_language("en");
    let tmp = tempfile::tempdir().unwrap();
    let base = real(tmp.path());
    let root = base.join("work");
    let (mut core, du, loc) = setup(&root, Arc::new(Declined), &base.join("state"));
    let before = du.info(&loc).unwrap();
    let _ = toasts(&mut core);
    core.dispatch(Command::new("du.elevate"));
    pump_until(&mut core, "declined", |c| {
        idle(c) && du.info(&loc).is_some_and(|i| !i.elev.running)
    });
    let t = toasts(&mut core);
    assert!(t.iter().any(|(l, _)| *l == Level::Warn), "{t:?}");
    let after = du.info(&loc).unwrap();
    assert_eq!((after.denied, after.bytes), (before.denied, before.bytes));
    // Esc без задач — закрыть баннер.
    core.dispatch(Command::new("du.stop"));
    core.pump(Duration::from_millis(1));
    assert!(du.info(&loc).unwrap().elev.banner_closed);
}
