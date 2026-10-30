//! Задача скана `du` (F01-03): через core — события в UI-потоке, итог = эталон,
//! частичные результаты, отмена задачи ≤ 100 мс со снимком `PARTIAL`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_core::{
    ArgSpec, Attrs, Boot, CancelToken, CmdError, Command, CommandMeta, Core, Cx, Entry, EntryKind,
    Handler, JobState, Location, Services, ValueKind, Waker,
};
use urfin_vfs::du::{DuSnapshot, NodeFlags, ScanCfg, ScanEvent, start};
use urfin_vfs::{
    Caps, ListOpts, ListStream, LocalFs, Metadata, Space, Vfs, VfsCfg, VfsError, VfsListing,
    VfsPath, VfsRead, VfsRegistry, WatchGuard, WatchTx,
};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

#[derive(Default)]
struct Seen {
    ticks: usize,
    partial_tick: bool,
    done: Option<Arc<DuSnapshot>>,
}

/// Тестовый handler: `test.du {path}` запускает задачу скана.
struct TestDu {
    vfs: Arc<dyn Vfs>,
    seen: Arc<Mutex<Seen>>,
}

static COMMANDS: [CommandMeta; 1] = [CommandMeta {
    id: "test.du",
    title: "test",
    category: "test",
    args: &[ArgSpec {
        name: "path",
        kind: ValueKind::Str,
        required: true,
    }],
    repeatable: false,
}];

impl Handler for TestDu {
    fn namespace(&self) -> &'static str {
        "test"
    }
    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }
    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let path = cmd.args.str("path").unwrap_or_default().to_string();
        let seen = Arc::clone(&self.seen);
        start(
            cx,
            Arc::clone(&self.vfs),
            Location::local(path),
            ScanCfg {
                threads: 4,
                ..ScanCfg::default()
            },
            "scan".into(),
            Arc::new(move |_cx, ev| {
                let mut s = seen.lock().unwrap();
                match ev {
                    ScanEvent::Progress(t) => {
                        s.ticks += 1;
                        s.partial_tick |= !t.total().flags.has(NodeFlags::DONE);
                    }
                    ScanEvent::Done(snap) => s.done = Some(snap),
                }
            }),
        );
        Ok(())
    }
}

fn setup(start_dir: &Path, vfs: Arc<dyn Vfs>) -> (Core, Arc<Mutex<Seen>>) {
    let mut reg = VfsRegistry::default();
    reg.register(Arc::clone(&vfs));
    let reg = Arc::new(reg);
    let cfg = VfsCfg::default();
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg)),
        open_resolvers: Vec::new(),
    };
    let start_loc = Location::local(start_dir);
    let mut core = Core::new(
        Boot::new(start_loc.clone(), start_loc),
        services,
        Arc::new(NoWake),
    );
    let seen = Arc::new(Mutex::new(Seen::default()));
    core.register(Box::new(TestDu {
        vfs,
        seen: Arc::clone(&seen),
    }));
    (core, seen)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn tree(root: &Path, depth: u32, fan: u32) {
    fs::create_dir_all(root).unwrap();
    for i in 0..4u32 {
        fs::write(
            root.join(format!("f{i}.bin")),
            vec![1u8; (i * 101 + 1) as usize],
        )
        .unwrap();
    }
    if depth > 0 {
        for d in 0..fan {
            tree(&root.join(format!("d{d}")), depth - 1, fan);
        }
    }
}

fn reference(p: &Path) -> (u64, u32) {
    let mut sum = (0, 0);
    for e in fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        let md = fs::symlink_metadata(e.path()).unwrap();
        if md.is_dir() {
            let (b, n) = reference(&e.path());
            sum = (sum.0 + b, sum.1 + n);
        } else {
            sum = (sum.0 + md.len(), sum.1 + 1);
        }
    }
    sum
}

/// Задача `du` через core: итог = эталон, задача `Done`, дерево досчитано.
#[test]
fn scan_job_matches_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t");
    tree(&root, 3, 4);
    let (mut core, seen) = setup(tmp.path(), Arc::new(LocalFs::new()));
    core.dispatch(Command::new("test.du").arg("path", root.to_string_lossy().as_ref()));
    pump_until(&mut core, "scan done", |c| {
        seen.lock().unwrap().done.is_some() && c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    let snap = seen.lock().unwrap().done.clone().unwrap();
    let (bytes, files) = reference(&root);
    let r = snap.dirs[0];
    assert_eq!((r.logical, r.files), (bytes, files));
    assert!(r.flags.has(NodeFlags::DONE));
    assert_eq!(snap.flags & DuSnapshot::PARTIAL, 0);
    assert_eq!(snap.root, root);
    // Все файлы tree() — тип Other (.bin не в таблице): сумма типов = итог.
    assert_eq!(snap.types.files.iter().sum::<u64>(), u64::from(files));
    let job = core.state().jobs.iter().last().unwrap();
    assert!(matches!(job.state, JobState::Done(_)), "{:?}", job.state);
}

/// Бесконечное дерево с медленным листингом (5 мс на каталог).
struct Endless;

struct EndlessStream(u32);

impl ListStream for EndlessStream {
    fn size_hint(&self) -> Option<u32> {
        None
    }
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(5));
        self.0 += 1;
        let entry = |name: String, kind, size| Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: name.into(),
            size,
            mtime: 0,
            attrs: Attrs(0),
            kind,
        };
        for i in 0..3 {
            out.push(entry(format!("d{}-{i}", self.0), EntryKind::Dir, 0));
        }
        out.push(entry("f.bin".into(), EntryKind::File, 10));
        Ok(false)
    }
}

impl Vfs for Endless {
    fn scheme(&self) -> &'static str {
        "local"
    }
    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps::default()
    }
    fn list(&self, _dir: &VfsPath, _opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Ok(Box::new(EndlessStream(0)))
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
    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }
    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
}

/// Частичные результаты идут тиками; `job.cancel` — ≤ 100 мс до снимка `PARTIAL`,
/// задача `Cancelled`.
#[test]
fn job_cancel_is_quick_and_partial() {
    let missing = Path::new("Q:\\urfin-test-endless");
    let (mut core, seen) = setup(missing, Arc::new(Endless));
    core.dispatch(Command::new("test.du").arg("path", missing.to_string_lossy().as_ref()));
    pump_until(&mut core, "ticks", |_| seen.lock().unwrap().ticks >= 2);
    assert!(seen.lock().unwrap().partial_tick);
    let id = core.state().jobs.iter().last().unwrap().id;
    let t0 = Instant::now();
    core.dispatch(Command::new("job.cancel").arg("id", i64::try_from(id.0).unwrap()));
    pump_until(&mut core, "partial snapshot", |_| {
        seen.lock().unwrap().done.is_some()
    });
    let lag = t0.elapsed();
    assert!(lag <= Duration::from_millis(100), "snapshot after {lag:?}");
    let snap = seen.lock().unwrap().done.clone().unwrap();
    assert_ne!(snap.flags & DuSnapshot::PARTIAL, 0);
    assert!(snap.dirs[0].files > 0);
    assert!(!snap.dirs[0].flags.has(NodeFlags::DONE));
    pump_until(&mut core, "cancelled", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| j.id == id && matches!(j.state, JobState::Cancelled))
    });
}

/// Очередь устройства общая с копированием (S5-19, решение 2026-10-30): пока слоты
/// диска заняты, скан стоит в `Queued`; освободились — досчитывается.
#[test]
fn scan_waits_for_device_slot() {
    use urfin_core::{DeviceJobs, JobCtl, JobId};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t");
    tree(&root, 1, 2);
    let (mut core, seen) = setup(tmp.path(), Arc::new(LocalFs::new()));
    let vol = urfin_vfs::volume::of(&root).unwrap();
    let key = urfin_vfs::volume::device_key(&root).unwrap_or(vol.id);
    let lim = urfin_vfs::volume::jobs_for(&DeviceJobs::default(), vol.media);
    let gate = core.devices();
    let t = CancelToken::new();
    let busy: Vec<_> = (0..lim)
        .map(|i| {
            let ctl = Arc::new(JobCtl::default());
            gate.acquire(
                JobId(9000 + u64::from(i)),
                &[(key, lim)],
                &ctl,
                &t,
                &t,
                &mut || {},
            )
            .unwrap()
        })
        .collect();
    core.dispatch(Command::new("test.du").arg("path", root.to_string_lossy().as_ref()));
    pump_until(&mut core, "queued", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| matches!(j.state, JobState::Queued))
    });
    std::thread::sleep(Duration::from_millis(50));
    core.pump(Duration::from_millis(1));
    assert!(
        seen.lock().unwrap().done.is_none(),
        "scanned while the disk is busy"
    );
    drop(busy);
    pump_until(&mut core, "scan done", |c| {
        seen.lock().unwrap().done.is_some() && c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    let snap = seen.lock().unwrap().done.clone().unwrap();
    assert_eq!(snap.dirs[0].files, reference(&root).1);
}
