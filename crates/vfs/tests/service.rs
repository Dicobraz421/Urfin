//! `VfsListing` с настоящим core (S4-03): листинг, первый чанк до конца
//! перечисления, отмена, ошибки.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_core::{
    Attrs, Boot, CancelToken, Command, Core, Entry, EntryKind, ListingStatus, Location, Row,
    Services, Waker,
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

/// Левая панель (активная) — `start`, правая — `other` (своя загрузка не мешает).
fn core_with(reg: VfsRegistry, start: Location, other: Location) -> Core {
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::new(reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    Core::new(Boot::new(start, other), services, Arc::new(NoWake))
}

/// Качать pump, пока `done` не станет истинным (не дольше 10 с).
fn pump_until(core: &mut Core, done: impl Fn(&Core) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(core) {
        assert!(Instant::now() < deadline, "timeout");
        core.pump(Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn names(core: &Core) -> Vec<String> {
    let s = core.state();
    let t = s.panel(s.active).tab();
    (0..t.rows())
        .filter_map(|r| match t.row(r) {
            Some(Row::Entry { entry, .. }) => Some(entry.name.to_string()),
            _ => None,
        })
        .collect()
}

fn status(core: &Core) -> ListingStatus {
    let s = core.state();
    s.panel(s.active).tab().listing.status.clone()
}

#[test]
fn lists_real_directory_sorted() {
    let tmp = tempfile::tempdir().unwrap();
    for n in ["file10.txt", "file2.txt", "File1.txt"] {
        fs::write(tmp.path().join(n), b"x").unwrap();
    }
    fs::create_dir(tmp.path().join("zdir")).unwrap();
    let mut core = core_with(
        VfsRegistry::with_local(),
        Location::local(tmp.path()),
        Location::local(tmp.path()),
    );
    pump_until(&mut core, |c| matches!(status(c), ListingStatus::Complete));
    assert_eq!(
        names(&core),
        ["zdir", "File1.txt", "file2.txt", "file10.txt"]
    );
}

#[test]
fn missing_directory_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let mut core = core_with(
        VfsRegistry::with_local(),
        Location::local(tmp.path()),
        Location::local(tmp.path()),
    );
    pump_until(&mut core, |c| matches!(status(c), ListingStatus::Complete));
    let missing = tmp.path().join("missing");
    core.dispatch(Command::new("panel.go").arg("path", Location::local(&missing)));
    pump_until(&mut core, |c| matches!(status(c), ListingStatus::Failed(_)));
}

/// Медленная ФС: батч из 10 записей раз в 60 мс, 5 батчей; считает вызовы.
struct Slow {
    calls: Arc<AtomicU32>,
    cancelled_at: Arc<Mutex<Option<u32>>>,
}

struct SlowStream {
    left: u32,
    /// Записей в батче; имена по убыванию (`n99999`, `n99998`, …).
    per: u32,
    next_name: u32,
    calls: Arc<AtomicU32>,
    cancelled_at: Arc<Mutex<Option<u32>>>,
}

impl ListStream for SlowStream {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if cancel.is_cancelled() {
            self.cancelled_at.lock().unwrap().get_or_insert(n);
            return Err(VfsError::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(60));
        for i in 0..self.per {
            let name = if self.per == 10 {
                format!("e{}-{i}", self.left)
            } else {
                self.next_name -= 1;
                format!("n{:05}", self.next_name)
            };
            out.push(Entry {
                ctime: i64::MIN,
                atime: i64::MIN,
                name: name.into(),
                size: 0,
                mtime: 0,
                attrs: Attrs::default(),
                kind: EntryKind::File,
            });
        }
        self.left -= 1;
        Ok(self.left > 0)
    }
}

impl Vfs for Slow {
    fn scheme(&self) -> &'static str {
        "local"
    }
    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps::default()
    }
    fn list(&self, dir: &VfsPath, _opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        if dir.path.ends_with("desc") {
            return Ok(Box::new(SlowStream {
                left: 5,
                per: 1000,
                next_name: 100_000,
                calls: Arc::new(AtomicU32::new(0)),
                cancelled_at: Arc::default(),
            }));
        }
        if !dir.path.ends_with("slow") {
            return Ok(Box::new(SlowStream {
                left: 1,
                per: 10,
                next_name: 0,
                calls: Arc::new(AtomicU32::new(0)),
                cancelled_at: Arc::default(),
            }));
        }
        Ok(Box::new(SlowStream {
            left: 5,
            per: 10,
            next_name: 0,
            calls: Arc::clone(&self.calls),
            cancelled_at: Arc::clone(&self.cancelled_at),
        }))
    }
    fn stat(&self, _p: &VfsPath, _follow: bool) -> Result<Metadata, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn stat_entry(&self, _d: &VfsPath, _n: &str, _a: Attrs) -> Result<Entry, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn open_read(&self, _p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn space(&self, _at: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn local_path(&self, p: &VfsPath) -> Option<PathBuf> {
        Some(p.path.clone())
    }
    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
}

fn slow() -> (VfsRegistry, Arc<AtomicU32>, Arc<Mutex<Option<u32>>>) {
    let calls = Arc::new(AtomicU32::new(0));
    let cancelled_at = Arc::new(Mutex::new(None));
    let mut reg = VfsRegistry::default();
    reg.register(Arc::new(Slow {
        calls: Arc::clone(&calls),
        cancelled_at: Arc::clone(&cancelled_at),
    }));
    (reg, calls, cancelled_at)
}

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\m")
    } else {
        PathBuf::from("/m")
    }
}

#[test]
fn first_chunk_arrives_before_listing_ends() {
    let (reg, _, _) = slow();
    let mut core = core_with(
        reg,
        Location::local(root().join("slow")),
        Location::local(root().join("fast")),
    );
    // Первые записи — пока каталог ещё грузится.
    pump_until(&mut core, |c| !names(c).is_empty());
    assert!(matches!(status(&core), ListingStatus::Loading { .. }));
    assert_eq!(names(&core).len(), 10);
    pump_until(&mut core, |c| matches!(status(c), ListingStatus::Complete));
    assert_eq!(names(&core).len(), 50);
}

#[test]
fn navigation_cancels_listing_within_one_batch() {
    let (reg, calls, cancelled_at) = slow();
    let mut core = core_with(
        reg,
        Location::local(root().join("slow")),
        Location::local(root().join("fast")),
    );
    pump_until(&mut core, |c| !names(c).is_empty());
    let at_nav = calls.load(Ordering::SeqCst);
    core.dispatch(Command::new("panel.go").arg("path", Location::local(root().join("fast"))));
    pump_until(&mut core, |c| matches!(status(c), ListingStatus::Complete));
    std::thread::sleep(Duration::from_millis(400));
    core.pump(Duration::from_millis(20));
    // Не больше одного вызова после навигации — уже начатый батч; дальше поток
    // замечает отмену и выходит, не читая.
    let after = calls.load(Ordering::SeqCst);
    assert!(
        after <= at_nav + 1,
        "batches after cancel: {}",
        after - at_nav
    );
    assert!(after < 5, "listing did not run to the end");
    let _ = cancelled_at;
    // На экране — новый каталог, записей старого нет.
    assert!(names(&core).iter().all(|n| n.starts_with("e1-")));
}

/// ADR-0017: многочанковый каталог — `Complete` только с финальным порядком
/// (его считает core по своим записям); пока грузится, хвост — в порядке прихода.
#[test]
fn big_listing_is_complete_only_when_sorted() {
    let (reg, _, _) = slow();
    let mut core = core_with(
        reg,
        Location::local(root().join("desc")),
        Location::local(root().join("fast")),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "timeout");
        core.pump(Duration::from_millis(20));
        if matches!(status(&core), ListingStatus::Complete) {
            let v = names(&core);
            assert_eq!(v.len(), 5000);
            assert!(
                v.windows(2).all(|w| w[0] < w[1]),
                "Complete with an unsorted tail"
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
