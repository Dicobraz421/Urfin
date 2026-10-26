//! Обходчик и размер каталогов (S4-17).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use urfin_core::{
    Attrs, Boot, CancelToken, Command, Core, Entry, EntryKind, JobState, ListingStatus, Location,
    Row, Services, Side, Waker,
};
use urfin_vfs::{
    Caps, ListOpts, ListStream, LocalFs, Metadata, Space, Vfs, VfsCfg, VfsError, VfsListing,
    VfsPath, VfsRead, VfsRegistry, WatchGuard, WatchTx, walk,
};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Эталон: рекурсивный `std::fs` без захода в ссылки; сумма размеров файлов.
fn reference(p: &Path) -> u64 {
    let mut sum = 0;
    for e in fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        let ft = e.file_type().unwrap();
        if ft.is_symlink() {
            continue;
        }
        let md = fs::symlink_metadata(e.path()).unwrap();
        // Junction на Windows: каталог-точка повторной обработки — не обходим.
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if md.file_attributes() & 0x400 != 0 {
                continue;
            }
        }
        if ft.is_dir() {
            sum += reference(&e.path());
        } else {
            sum += md.len();
        }
    }
    sum
}

/// Дерево: `depth` уровней по `fan` каталогов, в каждом — файлы разных размеров.
fn tree(root: &Path, depth: u32, fan: u32) {
    fs::create_dir_all(root).unwrap();
    for i in 0..5u32 {
        fs::write(
            root.join(format!("f{i}.bin")),
            vec![0u8; ((i + 1) * 37 + depth) as usize],
        )
        .unwrap();
    }
    if depth > 0 {
        for d in 0..fan {
            tree(&root.join(format!("d{d}")), depth - 1, fan);
        }
    }
}

fn walk_sizes(roots: &[PathBuf], threads: usize) -> Vec<u64> {
    let locs: Vec<Location> = roots.iter().map(Location::local).collect();
    let sizes: Vec<AtomicU64> = locs.iter().map(|_| AtomicU64::new(0)).collect();
    let fs = LocalFs::new();
    let stats = walk::walk(
        &fs,
        &locs,
        threads,
        &CancelToken::new(),
        &|| false,
        &|r, _, e| {
            if e.kind == EntryKind::File {
                sizes[r].fetch_add(e.size, Ordering::Relaxed);
            }
        },
        &|_| {},
        || {},
        Duration::from_millis(100),
    );
    assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
    sizes.iter().map(|s| s.load(Ordering::Relaxed)).collect()
}

/// Приёмка: размер совпадает с эталонным рекурсивным обходом (1 и 4 потока).
#[test]
fn sizes_match_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    tree(&a, 3, 3);
    tree(&b, 2, 4);
    fs::create_dir(tmp.path().join("empty")).unwrap();
    let roots = [a.clone(), b.clone(), tmp.path().join("empty")];
    let want = vec![reference(&a), reference(&b), 0];
    assert_eq!(walk_sizes(&roots, 1), want);
    assert_eq!(walk_sizes(&roots, 4), want);
}

/// Приёмка: junction (Windows) и symlink на каталог не обходятся — цикл не
/// зацикливает и не удваивает размер.
#[test]
fn directory_links_are_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    tree(&a, 1, 2);
    let lp = a.join("d0").join("loop");
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&lp)
        .arg(&a)
        .output()
        .is_ok_and(|o| o.status.success());
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&a, &lp).is_ok();
    assert!(made, "cannot create a directory link");
    assert_eq!(walk_sizes(std::slice::from_ref(&a), 4), vec![reference(&a)]);
}

/// Бесконечное дерево с медленным листингом: отмена.
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
        for i in 0..3 {
            out.push(Entry {
                ctime: i64::MIN,
                atime: i64::MIN,
                name: format!("d{}-{i}", self.0).into(),
                size: 0,
                mtime: 0,
                attrs: Attrs(0),
                kind: EntryKind::Dir,
            });
        }
        out.push(Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: "f".into(),
            size: 1,
            mtime: 0,
            attrs: Attrs(0),
            kind: EntryKind::File,
        });
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

/// Приёмка: отмена ≤ 100 мс.
#[test]
fn cancel_is_quick() {
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let cancelled_at = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c2.cancel();
        Instant::now()
    });
    let roots = [Location::local("/endless")];
    walk::walk(
        &Endless,
        &roots,
        4,
        &cancel,
        &|| false,
        &|_, _, _| {},
        &|_| {},
        || {},
        Duration::from_millis(100),
    );
    let lag = cancelled_at.join().unwrap().elapsed();
    assert!(
        lag <= Duration::from_millis(100),
        "walk ended {lag:?} after cancel"
    );
}

fn setup(start: &Path) -> Core {
    let reg = Arc::new(VfsRegistry::with_local());
    let cfg = VfsCfg::default();
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg.clone())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(start), Location::local(start)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, cfg);
    core
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(10), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn dir_size(core: &Core, name: &str) -> Option<u64> {
    let t = core.state().panel(Side::Left).tab();
    t.listing
        .entries
        .iter()
        .find(|e| e.name == name && e.attrs.has(Attrs::SIZE_KNOWN))
        .map(|e| e.size)
}

fn names(core: &Core) -> Vec<String> {
    let t = core.state().panel(Side::Left).tab();
    (0..t.rows())
        .filter_map(|r| match t.row(r) {
            Some(Row::Entry { entry, .. }) => Some(entry.name.to_string()),
            _ => None,
        })
        .collect()
}

/// Через core: `vfs.dir_size all=true` → размеры с `SIZE_KNOWN`, задача `Done`,
/// при сортировке по размеру — порядок каталогов по размеру.
#[test]
fn dir_size_command_patches_listing() {
    let tmp = tempfile::tempdir().unwrap();
    let small = tmp.path().join("small");
    let big = tmp.path().join("big");
    let mid = tmp.path().join("mid");
    tree(&small, 0, 0);
    tree(&big, 2, 3);
    tree(&mid, 1, 2);
    fs::write(tmp.path().join("file.txt"), b"x").unwrap();
    let mut core = setup(tmp.path());
    pump_until(&mut core, "listing", |c| {
        matches!(
            c.state().panel(Side::Left).tab().listing.status,
            ListingStatus::Complete
        )
    });
    core.dispatch(Command::new("panel.sort").arg("key", "size"));
    core.dispatch(Command::new("vfs.dir_size").arg("all", true));
    pump_until(&mut core, "sizes", |c| {
        ["small", "big", "mid"]
            .iter()
            .all(|n| dir_size(c, n).is_some())
            && c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    assert_eq!(dir_size(&core, "small"), Some(reference(&small)));
    assert_eq!(dir_size(&core, "big"), Some(reference(&big)));
    assert_eq!(dir_size(&core, "mid"), Some(reference(&mid)));
    let job = core.state().jobs.iter().last().unwrap();
    assert!(matches!(job.state, JobState::Done(_)), "{:?}", job.state);
    // Каталоги сверху, по размеру по возрастанию.
    pump_until(&mut core, "ordered by size", |c| {
        names(c)
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>()
            == ["small", "mid", "big"]
    });
}

/// Без выделения и без `all` — каталог под курсором; на файле — отказ без задачи.
#[test]
fn dir_size_targets_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    tree(&tmp.path().join("only"), 1, 1);
    fs::write(tmp.path().join("z.txt"), b"x").unwrap();
    let mut core = setup(tmp.path());
    pump_until(&mut core, "listing", |c| names(c).len() == 2);
    // Курсор на «..»? — в корне временного каталога есть родитель: на первую запись.
    core.dispatch(Command::new("panel.cursor.home"));
    core.dispatch(Command::new("panel.cursor.down"));
    let on_dir = matches!(
        core.state().panel(Side::Left).tab().current(),
        Some(Row::Entry { entry, .. }) if entry.name == "only"
    );
    assert!(on_dir, "cursor on the directory");
    core.dispatch(Command::new("vfs.dir_size"));
    pump_until(&mut core, "size", |c| dir_size(c, "only").is_some());
    core.dispatch(Command::new("panel.cursor.down"));
    let jobs = core.state().jobs.iter().count();
    core.dispatch(Command::new("vfs.dir_size"));
    assert_eq!(core.state().jobs.iter().count(), jobs, "no job for a file");
}

/// Приёмка: `job.cancel` останавливает подсчёт за ≤ 100 мс (бесконечное дерево).
#[test]
fn job_cancel_is_quick() {
    let mut reg = VfsRegistry::default();
    reg.register(Arc::new(Endless));
    let reg = Arc::new(reg);
    let cfg = VfsCfg::default();
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg.clone())),
        open_resolvers: Vec::new(),
    };
    let start = Location::local("/endless");
    let mut core = Core::new(Boot::new(start.clone(), start), services, Arc::new(NoWake));
    urfin_vfs::register(&mut core, reg, cfg);
    pump_until(&mut core, "listing", |c| names(c).len() == 4);
    core.dispatch(Command::new("vfs.dir_size").arg("all", true));
    std::thread::sleep(Duration::from_millis(200));
    core.pump(Duration::from_millis(5));
    let id = core.state().jobs.iter().last().unwrap().id;
    assert!(core.state().jobs.iter().last().unwrap().state.is_active());
    let t0 = Instant::now();
    core.dispatch(Command::new("job.cancel").arg("id", i64::try_from(id.0).unwrap()));
    pump_until(&mut core, "cancelled", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| j.id == id && matches!(j.state, JobState::Cancelled))
    });
    let lag = t0.elapsed();
    assert!(
        lag <= Duration::from_millis(100),
        "job cancelled after {lag:?}"
    );
}
