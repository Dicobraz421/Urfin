//! F01-14: изменённый снимок анализа места пишется в кэш не только раз в 30 с, но и
//! при закрытии вкладки `du:` и при выходе (`Core::shutdown`).

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, ListingStatus, Location, Services, Waker};
use urfin_vfs::du::{DuCache, DuCfg, DuRegistry, DuVfs, SizeMode, cache};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn pump_for(core: &mut Core, d: Duration) {
    let t0 = Instant::now();
    while t0.elapsed() < d {
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Логический размер корня в снимке кэша (`None` — снимка нет).
fn cached_bytes(dir: &Path) -> Option<u64> {
    let mut best = None;
    for e in fs::read_dir(dir).ok()? {
        let p = e.ok()?.path();
        let Ok(bytes) = fs::read(&p) else { continue };
        let Ok(snap) = cache::decode(&bytes) else {
            continue;
        };
        best = snap
            .dirs
            .iter()
            .find(|n| n.parent == u32::MAX)
            .map(|n| n.logical);
    }
    best
}

struct Rig {
    core: Core,
    du: Arc<DuRegistry>,
    cache_dir: std::path::PathBuf,
    root: std::path::PathBuf,
    _tmp: tempfile::TempDir,
}

/// Анализ `w/` (три файла: 1000 + 200 + 30 байт) посчитан и записан в кэш.
fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("w");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.bin"), vec![0u8; 1000]).unwrap();
    fs::write(root.join("b.bin"), vec![0u8; 200]).unwrap();
    fs::write(root.join("c.bin"), vec![0u8; 30]).unwrap();
    let cache_dir = tmp.path().join("cache");

    let du = DuRegistry::new();
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(DuVfs::new(Arc::clone(&du))));
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(&root), Location::local(&root)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    let cfg = DuCfg {
        size: SizeMode::Logical,
        ..DuCfg::default()
    };
    let cache = DuCache::in_dir(cache_dir.clone(), &cfg);
    urfin_vfs::du::register_with(&mut core, cfg, Arc::clone(&du), Some(cache));

    core.dispatch(Command::new("du.open"));
    pump_until(&mut core, "scan", |c| {
        let t = c.state().active_tab();
        t.loc.backend == "du"
            && matches!(t.listing.status, ListingStatus::Complete)
            && c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    let dir = cache_dir.clone();
    pump_until(&mut core, "scan stored", |_| {
        cached_bytes(&dir) == Some(1230)
    });
    Rig {
        core,
        du,
        cache_dir,
        root,
        _tmp: tmp,
    }
}

impl Rig {
    /// Удалить файл и перечитать вкладку `du:` — снимок изменён (вычтено удалённое).
    fn remove(&mut self, name: &str, total: u64) {
        fs::remove_file(self.root.join(name)).unwrap();
        self.core.dispatch(Command::new("panel.refresh"));
        let (du, top) = (
            Arc::clone(&self.du),
            self.core.state().active_tab().loc.clone(),
        );
        pump_until(&mut self.core, "subtracted", |_| {
            du.info(&top).is_some_and(|i| i.bytes == total)
        });
    }

    /// Первое изменение — в кэш сразу (прошлой записи нет), второе в пределах 30 с —
    /// ждёт: на диске остаётся снимок после первого.
    fn two_changes(&mut self) {
        self.remove("c.bin", 1200);
        let dir = self.cache_dir.clone();
        pump_until(&mut self.core, "first change stored", |_| {
            cached_bytes(&dir) == Some(1200)
        });
        self.remove("b.bin", 1000);
        pump_for(&mut self.core, Duration::from_millis(300));
        assert_eq!(cached_bytes(&self.cache_dir), Some(1200), "throttled");
    }
}

#[test]
fn du_store_on_tab_close() {
    let mut r = rig();
    r.two_changes();
    r.core.dispatch(Command::new("tab.close"));
    let dir = r.cache_dir.clone();
    pump_until(&mut r.core, "stored on close", |c| {
        c.state().active_tab().loc.backend != "du" && cached_bytes(&dir) == Some(1000)
    });
}

#[test]
fn du_store_on_exit() {
    let mut r = rig();
    r.two_changes();
    let _ = r.core.shutdown(Duration::from_secs(5));
    assert_eq!(cached_bytes(&r.cache_dir), Some(1000), "stored on exit");
}
