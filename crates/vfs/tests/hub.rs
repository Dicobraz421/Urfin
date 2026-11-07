//! `WatchHub` с настоящим core (S4-14): изменение каталога → `panel.refresh`,
//! жизненный цикл наблюдения по табам. На Windows — `ReadDirectoryChangesExW`,
//! на unix пока опрос (S4-13: `notify` — позже), интервал в тесте короткий.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, ListingStatus, Location, Row, Services, Side, TabId, Waker};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn setup(left: &Path, right: &Path) -> (Core, Arc<VfsListing>) {
    setup_with(left, right, VfsCfg::default().patch_threshold)
}

fn setup_with(left: &Path, right: &Path, patch_threshold: u32) -> (Core, Arc<VfsListing>) {
    let cfg = VfsCfg {
        poll_interval_ms: 100,
        patch_threshold,
        ..VfsCfg::default()
    };
    let listing = Arc::new(VfsListing::new(Arc::new(VfsRegistry::with_local()), cfg));
    let services = Services {
        listing: listing.clone(),
        open_resolvers: Vec::new(),
    };
    let core = Core::new(
        Boot::new(Location::local(left), Location::local(right)),
        services,
        Arc::new(NoWake),
    );
    (core, listing)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) -> Duration {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(10), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
    t0.elapsed()
}

fn names(core: &Core, side: Side) -> Vec<String> {
    let t = core.state().panel(side).tab();
    (0..t.rows())
        .filter_map(|r| match t.row(r) {
            Some(Row::Entry { entry, .. }) => Some(entry.name.to_string()),
            _ => None,
        })
        .collect()
}

fn complete(core: &Core, side: Side) -> bool {
    matches!(
        core.state().panel(side).tab().listing.status,
        ListingStatus::Complete
    )
}

fn tab_id(core: &Core, side: Side) -> TabId {
    core.state().panel(side).tab().id
}

fn has(core: &Core, side: Side, name: &str) -> bool {
    names(core, side).iter().any(|n| n == name)
}

/// Приёмка: p50 «событие → обновлённый листинг» < 100 мс.
#[test]
fn created_file_appears_quickly() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (mut core, listing) = setup(a.path(), b.path());
    pump_until(&mut core, "initial", |c| {
        complete(c, Side::Left) && complete(c, Side::Right)
    });
    pump_until(&mut core, "watched", |_| listing.watched_tabs().len() == 2);
    let mut times = Vec::new();
    for i in 0..9 {
        let name = format!("new{i}.txt");
        let t0 = Instant::now();
        fs::write(a.path().join(&name), b"x").unwrap();
        pump_until(&mut core, &name, |c| has(c, Side::Left, &name));
        times.push(t0.elapsed());
    }
    times.sort();
    let p50 = times[times.len() / 2];
    eprintln!("event → listing: p50 {p50:?}, all {times:?}");
    // На стенде p50 ≈ debounce 50 мс + листинг; на CI — с запасом.
    assert!(
        p50 < Duration::from_millis(if cfg!(windows) { 100 } else { 400 }),
        "{times:?}"
    );
    // Чужой каталог не трогается.
    assert_eq!(names(&core, Side::Right), Vec::<String>::new());
}

/// Изменения во время чтения каталога не теряются: наблюдение ставится до листинга.
#[test]
fn rename_and_delete_are_reflected() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("old.txt"), b"x").unwrap();
    fs::write(a.path().join("doomed.txt"), b"x").unwrap();
    let (mut core, listing) = setup(a.path(), b.path());
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    let left = tab_id(&core, Side::Left);
    pump_until(&mut core, "watched", |_| {
        listing.watched_tabs().contains(&left)
    });
    fs::rename(a.path().join("old.txt"), a.path().join("new.txt")).unwrap();
    fs::remove_file(a.path().join("doomed.txt")).unwrap();
    pump_until(&mut core, "rename+delete", |c| {
        names(c, Side::Left) == ["new.txt"]
    });
}

/// Скрытый таб не наблюдается; при показе — перечитывается.
#[test]
fn hidden_tab_refreshes_when_shown() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (mut core, listing) = setup(a.path(), b.path());
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    let first = tab_id(&core, Side::Left);
    pump_until(&mut core, "watched", |_| {
        listing.watched_tabs().contains(&first)
    });

    // Новый таб слева уходит в b — первый скрыт.
    core.dispatch(Command::new("tab.new"));
    core.dispatch(Command::new("panel.go").arg("path", Location::local(b.path())));
    pump_until(&mut core, "second tab", |c| {
        tab_id(c, Side::Left) != first && complete(c, Side::Left)
    });
    pump_until(&mut core, "first unwatched", |_| {
        !listing.watched_tabs().contains(&first)
    });

    fs::write(a.path().join("while-hidden.txt"), b"x").unwrap();
    std::thread::sleep(Duration::from_millis(150));
    core.pump(Duration::from_millis(10));

    core.dispatch(Command::new("tab.prev"));
    assert_eq!(tab_id(&core, Side::Left), first);
    pump_until(&mut core, "refreshed on show", |c| {
        has(c, Side::Left, "while-hidden.txt")
    });
    pump_until(&mut core, "watched again", |_| {
        listing.watched_tabs().contains(&first)
    });
}

#[test]
fn closing_tab_drops_watch() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (mut core, listing) = setup(a.path(), b.path());
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    core.dispatch(Command::new("tab.new"));
    let second = tab_id(&core, Side::Left);
    pump_until(&mut core, "second watched", |_| {
        listing.watched_tabs().contains(&second)
    });
    core.dispatch(Command::new("tab.close"));
    pump_until(&mut core, "second unwatched", |_| {
        !listing.watched_tabs().contains(&second)
    });
}

/// Каталог таба удалён — таб уходит в ближайший существующий родитель.
#[test]
fn deleted_directory_goes_up() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let deep = a.path().join("x").join("y");
    fs::create_dir_all(&deep).unwrap();
    let (mut core, listing) = setup(&deep, b.path());
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    let left = tab_id(&core, Side::Left);
    pump_until(&mut core, "watched", |_| {
        listing.watched_tabs().contains(&left)
    });
    fs::remove_dir_all(a.path().join("x")).unwrap();
    let here = Location::local(a.path());
    pump_until(&mut core, "went up", |c| {
        let t = c.state().panel(Side::Left).tab();
        t.loc == here && complete(c, Side::Left)
    });
}

/// Шторм в видимом каталоге: не больше ~1 перечитывания в секунду (S4 §7).
/// Только Windows: на Linux/macOS наблюдение пока опросом (inotify/FSEvents —
/// S4-08/S4-09), а приёмка шторма — про поток событий watcher'а.
#[cfg(windows)]
#[test]
fn storm_is_rate_limited() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (mut core, listing) = setup(a.path(), b.path());
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    let left = tab_id(&core, Side::Left);
    pump_until(&mut core, "watched", |_| {
        listing.watched_tabs().contains(&left)
    });
    let generation = |c: &Core| c.state().panel(Side::Left).tab().listing.generation;
    let g0 = generation(&core);
    // 3 с непрерывных созданий из другого потока (как чужой процесс); core
    // качается здесь же.
    let dir = a.path().to_path_buf();
    let writer = std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut i = 0u32;
        while t0.elapsed() < Duration::from_secs(3) {
            fs::write(dir.join(format!("s{i}")), b"").unwrap();
            i += 1;
        }
        i
    });
    while !writer.is_finished() {
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
    let i = writer.join().unwrap();
    let refreshes = generation(&core).0 - g0.0;
    eprintln!("storm: {i} files in 3 s, {refreshes} refreshes");
    // Первые ~0,75 с — по max_latency (до 3), дальше — не чаще раза в секунду.
    assert!(refreshes <= 3 + 3, "{refreshes} refreshes");
    // После шторма список догоняет.
    let n = i as usize;
    pump_until(&mut core, "caught up", |c| names(c, Side::Left).len() == n);
}

fn cursor_name(core: &Core, side: Side) -> Option<String> {
    match core.state().panel(side).tab().current() {
        Some(Row::Entry { entry, .. }) => Some(entry.name.to_string()),
        _ => None,
    }
}

fn selected(core: &Core, side: Side) -> Vec<String> {
    let t = core.state().panel(side).tab();
    let mut v: Vec<String> = t
        .selection
        .names(&t.listing)
        .into_iter()
        .map(|n| n.to_string())
        .collect();
    v.sort();
    v
}

/// Большой каталог (≥ `patch_threshold`): изменения приходят патчем — без
/// перечитывания (поколение то же), курсор и выделение на тех же именах,
/// порядок как у полного листинга (S4-15).
#[test]
fn big_directory_is_patched_in_place() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for i in 0..40 {
        fs::write(a.path().join(format!("file{i:02}.txt")), b"x").unwrap();
    }
    for n in ["keep-1.md", "keep-2.md"] {
        fs::write(a.path().join(n), b"x").unwrap();
    }
    let (mut core, listing) = setup_with(a.path(), b.path(), 10);
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    let left = tab_id(&core, Side::Left);
    pump_until(&mut core, "watched", |_| {
        listing.watched_tabs().contains(&left)
    });
    core.dispatch(Command::new("sel.add").arg("mask", "keep-*"));
    core.dispatch(Command::new("panel.cursor.down").arg("n", 20));
    let at = cursor_name(&core, Side::Left).expect("cursor on an entry");
    let generation = core.state().panel(Side::Left).tab().listing.generation;
    assert_eq!(selected(&core, Side::Left), ["keep-1.md", "keep-2.md"]);

    // Удалить файлы до и после курсора, добавить новые, изменить размер.
    fs::remove_file(a.path().join("file00.txt")).unwrap();
    fs::remove_file(a.path().join("file39.txt")).unwrap();
    fs::write(a.path().join("aaa-new.txt"), b"x").unwrap();
    fs::write(a.path().join("zzz-new.txt"), b"x").unwrap();
    fs::write(a.path().join("file05.txt"), b"bigger").unwrap();
    fs::rename(a.path().join("keep-2.md"), a.path().join("keep-3.md")).unwrap();

    pump_until(&mut core, "patched", |c| {
        let n = names(c, Side::Left);
        n.contains(&"aaa-new.txt".to_string())
            && n.contains(&"zzz-new.txt".to_string())
            && !n.contains(&"file00.txt".to_string())
            && !n.contains(&"file39.txt".to_string())
            && n.contains(&"keep-3.md".to_string())
    });
    let t = core.state().panel(Side::Left).tab();
    assert_eq!(t.listing.generation, generation, "patched, not re-read");
    assert_eq!(cursor_name(&core, Side::Left).as_deref(), Some(at.as_str()));
    // Переименованный выбранный файл — уже другое имя: выделение остаётся на keep-1.
    assert_eq!(selected(&core, Side::Left), ["keep-1.md"]);
    let size = |c: &Core, n: &str| {
        let t = c.state().panel(Side::Left).tab();
        t.listing
            .entries
            .iter()
            .find(|e| e.name == n)
            .map(|e| e.size)
    };
    pump_until(&mut core, "size updated", |c| {
        size(c, "file05.txt") == Some(6)
    });

    // Порядок — как у свежего листинга того же каталога.
    let patched = names(&core, Side::Left);
    core.dispatch(Command::new("panel.refresh"));
    pump_until(&mut core, "refreshed", |c| {
        c.state().panel(Side::Left).tab().listing.generation != generation
            && complete(c, Side::Left)
    });
    assert_eq!(names(&core, Side::Left), patched);
}

/// `LocalFs` без уведомлений: путь опроса (как на Linux/macOS до S4-08/09).
struct NoWatch(urfin_vfs::LocalFs);

impl urfin_vfs::Vfs for NoWatch {
    fn scheme(&self) -> &'static str {
        "local"
    }
    fn caps(&self, at: &urfin_vfs::VfsPath) -> urfin_vfs::Caps {
        self.0.caps(at)
    }
    fn list(
        &self,
        dir: &urfin_vfs::VfsPath,
        opts: &urfin_vfs::ListOpts,
    ) -> Result<Box<dyn urfin_vfs::ListStream>, urfin_vfs::VfsError> {
        self.0.list(dir, opts)
    }
    fn stat(
        &self,
        p: &urfin_vfs::VfsPath,
        follow: bool,
    ) -> Result<urfin_vfs::Metadata, urfin_vfs::VfsError> {
        self.0.stat(p, follow)
    }
    fn stat_entry(
        &self,
        d: &urfin_vfs::VfsPath,
        n: &str,
        a: urfin_core::Attrs,
    ) -> Result<urfin_core::Entry, urfin_vfs::VfsError> {
        self.0.stat_entry(d, n, a)
    }
    fn open_read(
        &self,
        p: &urfin_vfs::VfsPath,
    ) -> Result<Box<dyn urfin_vfs::VfsRead>, urfin_vfs::VfsError> {
        self.0.open_read(p)
    }
    fn space(&self, at: &urfin_vfs::VfsPath) -> Result<urfin_vfs::Space, urfin_vfs::VfsError> {
        self.0.space(at)
    }
    fn local_path(&self, p: &urfin_vfs::VfsPath) -> Option<std::path::PathBuf> {
        self.0.local_path(p)
    }
    fn watch(
        &self,
        _dir: &urfin_vfs::VfsPath,
        _tx: urfin_vfs::WatchTx,
    ) -> Result<urfin_vfs::WatchGuard, urfin_vfs::VfsError> {
        Err(urfin_vfs::VfsError::Unsupported("test: poll"))
    }
}

/// Опрос: файл, созданный сразу после листинга, не теряется (снимок — записи
/// core, а не первый опрос).
#[test]
fn polled_directory_sees_changes_right_after_listing() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let mut reg = VfsRegistry::default();
    reg.register(Arc::new(NoWatch(urfin_vfs::LocalFs::new())));
    let cfg = VfsCfg {
        poll_interval_ms: 100,
        ..VfsCfg::default()
    };
    let listing = Arc::new(VfsListing::new(Arc::new(reg), cfg));
    let services = Services {
        listing: listing.clone(),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(a.path()), Location::local(b.path())),
        services,
        Arc::new(NoWake),
    );
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    let left = tab_id(&core, Side::Left);
    pump_until(&mut core, "watched", |_| {
        listing.watched_tabs().contains(&left)
    });
    fs::write(a.path().join("early.txt"), b"x").unwrap();
    pump_until(&mut core, "early file", |c| has(c, Side::Left, "early.txt"));
    fs::write(a.path().join("later.txt"), b"x").unwrap();
    pump_until(&mut core, "later file", |c| has(c, Side::Left, "later.txt"));
}

/// Локальная ФС, у которой наблюдение каталога `slow` открывается долго.
struct SlowWatch {
    fs: urfin_vfs::LocalFs,
    slow: std::path::PathBuf,
    /// Сколько раз открывалось медленное наблюдение.
    slow_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl urfin_vfs::Vfs for SlowWatch {
    fn scheme(&self) -> &'static str {
        "local"
    }
    fn caps(&self, at: &urfin_vfs::VfsPath) -> urfin_vfs::Caps {
        self.fs.caps(at)
    }
    fn list(
        &self,
        dir: &urfin_vfs::VfsPath,
        opts: &urfin_vfs::ListOpts,
    ) -> Result<Box<dyn urfin_vfs::ListStream>, urfin_vfs::VfsError> {
        self.fs.list(dir, opts)
    }
    fn stat(
        &self,
        p: &urfin_vfs::VfsPath,
        follow: bool,
    ) -> Result<urfin_vfs::Metadata, urfin_vfs::VfsError> {
        self.fs.stat(p, follow)
    }
    fn stat_entry(
        &self,
        d: &urfin_vfs::VfsPath,
        n: &str,
        a: urfin_core::Attrs,
    ) -> Result<urfin_core::Entry, urfin_vfs::VfsError> {
        self.fs.stat_entry(d, n, a)
    }
    fn open_read(
        &self,
        p: &urfin_vfs::VfsPath,
    ) -> Result<Box<dyn urfin_vfs::VfsRead>, urfin_vfs::VfsError> {
        self.fs.open_read(p)
    }
    fn space(&self, at: &urfin_vfs::VfsPath) -> Result<urfin_vfs::Space, urfin_vfs::VfsError> {
        self.fs.space(at)
    }
    fn local_path(&self, p: &urfin_vfs::VfsPath) -> Option<std::path::PathBuf> {
        self.fs.local_path(p)
    }
    fn watch(
        &self,
        dir: &urfin_vfs::VfsPath,
        tx: urfin_vfs::WatchTx,
    ) -> Result<urfin_vfs::WatchGuard, urfin_vfs::VfsError> {
        if dir.path == self.slow {
            self.slow_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(300));
        }
        self.fs.watch(dir, tx)
    }
}

/// Новая вкладка сразу уходит в другой каталог: подписка первого листинга (её наблюдение
/// открывалось дольше) приходит последней и не снимает наблюдение нового каталога.
#[test]
fn late_subscribe_of_an_old_navigation_keeps_the_new_watch() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let here = Location::local(a.path());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut reg = VfsRegistry::default();
    reg.register(Arc::new(SlowWatch {
        fs: urfin_vfs::LocalFs::new(),
        slow: here.path.clone(),
        slow_calls: Arc::clone(&calls),
    }));
    let listing = Arc::new(VfsListing::new(Arc::new(reg), VfsCfg::default()));
    let services = Services {
        listing: listing.clone(),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(here, Location::local(b.path())),
        services,
        Arc::new(NoWake),
    );
    pump_until(&mut core, "initial", |c| complete(c, Side::Left));
    let first = tab_id(&core, Side::Left);

    let n = calls.load(std::sync::atomic::Ordering::Relaxed);
    core.dispatch(Command::new("tab.new"));
    // Листинг новой вкладки уже открывает наблюдение `a` — тогда уходим в `b`.
    pump_until(&mut core, "slow watch of the new tab", |_| {
        calls.load(std::sync::atomic::Ordering::Relaxed) > n
    });
    core.dispatch(Command::new("panel.go").arg("path", Location::local(b.path())));
    let there = Location::local(b.path());
    pump_until(&mut core, "second tab in b", |c| {
        let t = c.state().panel(Side::Left).tab();
        t.id != first && t.loc == there && complete(c, Side::Left)
    });
    // Опоздавшая подписка каталога `a` успевает прийти.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(600) {
        core.pump(Duration::from_millis(5));
    }
    fs::write(b.path().join("new.txt"), b"x").unwrap();
    pump_until(&mut core, "watched b", |c| has(c, Side::Left, "new.txt"));
}
