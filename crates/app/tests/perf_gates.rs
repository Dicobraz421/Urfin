//! Гейты бюджетов S9 §2 без окна (S9-08): F.2 — аллокации стационарного кадра, F.4 — ввод
//! в том же `update`, S.2 — простой без пробуждений, S.6 — soak навигаций и табов, L.6 —
//! навигация в свежий кэш без I/O, O.18 — пауза и отмена job на 100k элементов.
//!
//! F.1 (время кадра) абсолютом в CI не гейтится: раннер с WARP не похож на стенд — только
//! A/B (S9-07). Тесты меряют процесс целиком (S.6 — живую кучу): гонять по одному процессу
//! на тест (nextest), не `cargo test` с потоками.

#![allow(unsafe_code, reason = "тестовый GlobalAlloc — обёртка над System")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as StdCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_core::{
    ArrangeReq, Attrs, Boot, CancelToken, Command, Core, Entry, EntryKind, JobState, Lane,
    ListingService, ListingStatus, ListingUpdate, LoadReq, Location, Paths, Row, Services, Side,
    Waker, listing_msg,
};
use urfin_vfs::{
    Caps, ListOpts, ListStream, LocalFs, Metadata, Space, Vfs, VfsCfg, VfsError, VfsListing,
    VfsPath, VfsRead, VfsRegistry, WatchGuard, WatchTx,
};

// ---------------------------------------------------------------------------------------
// Считающий аллокатор: число аллокаций на потоке теста (F.2) и живая куча процесса (S.6).

struct Counting;

thread_local! {
    /// Считать аллокации этого потока; `const` — без ленивой инициализации.
    static ON: StdCell<bool> = const { StdCell::new(false) };
    static COUNT: StdCell<u64> = const { StdCell::new(0) };
}

/// Живые байты кучи процесса.
static LIVE: AtomicI64 = AtomicI64::new(0);

#[allow(clippy::cast_possible_wrap, reason = "размер аллокации < i64::MAX")]
fn bytes(layout: Layout) -> i64 {
    layout.size() as i64
}

fn count() {
    if ON.with(StdCell::get) {
        COUNT.with(|c| c.set(c.get() + 1));
    }
}

// SAFETY: делегирует System; счётчики — потоковый `Cell` и атомик, без аллокаций.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        LIVE.fetch_add(bytes(layout), Ordering::Relaxed);
        // SAFETY: тот же контракт, что у вызывающего.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        LIVE.fetch_add(bytes(layout), Ordering::Relaxed);
        // SAFETY: тот же контракт, что у вызывающего.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(bytes(layout), Ordering::Relaxed);
        // SAFETY: `ptr` выделен `System` с тем же `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        count();
        #[allow(clippy::cast_possible_wrap, reason = "размер аллокации < i64::MAX")]
        LIVE.fetch_add(new as i64 - bytes(layout), Ordering::Relaxed);
        // SAFETY: тот же контракт, что у вызывающего.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Аллокаций на этом потоке за `f`.
fn allocations(f: impl FnOnce()) -> u64 {
    COUNT.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    f();
    ON.with(|o| o.set(false));
    COUNT.with(StdCell::get)
}

// ---------------------------------------------------------------------------------------
// Общее: будильник, листинг без записи вызовов, ожидание.

/// Будильник со счётчиком: `wake` и `wake_after` — пробуждения UI-потока.
#[derive(Default)]
struct Wakes {
    n: AtomicU32,
    woke: AtomicBool,
}

impl Waker for Wakes {
    fn wake(&self) {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.woke.store(true, Ordering::SeqCst);
    }
    fn wake_after(&self, _d: Duration) {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.woke.store(true, Ordering::SeqCst);
    }
}

/// Листинг как `FakeListing`, но без журнала вызовов (у `FakeListing` он растёт с каждой
/// загрузкой и мешал бы soak): каталог `<корень>\big` — `big` файлов, прочие — `small`
/// каталогов `d00…` и файлов.
struct GenListing {
    big: u32,
    small: u32,
    cache: Mutex<Vec<(Location, Arc<Vec<Entry>>)>>,
}

impl GenListing {
    fn new(big: u32, small: u32) -> Arc<GenListing> {
        Arc::new(GenListing {
            big,
            small,
            cache: Mutex::new(Vec::new()),
        })
    }

    fn entries(&self, loc: &Location) -> Arc<Vec<Entry>> {
        let mut c = self.cache.lock().unwrap();
        if let Some((_, e)) = c.iter().find(|(l, _)| l == loc) {
            return Arc::clone(e);
        }
        let big = loc.path.file_name().is_some_and(|n| n == "big");
        let v: Vec<Entry> =
            if big {
                (0..self.big)
                    .map(|i| urfin_core::fake::FakeListing::file(&format!("f{i:06}"), u64::from(i)))
                    .collect()
            } else {
                (0..self.small)
                    .map(|i| urfin_core::fake::FakeListing::dir(&format!("d{i:02}")))
                    .chain((0..self.small).map(|i| {
                        urfin_core::fake::FakeListing::file(&format!("file-{i:03}.txt"), 7)
                    }))
                    .collect()
            };
        let v = Arc::new(v);
        // Кэш ограничен: soak ходит по кругу из нескольких каталогов.
        if c.len() < 64 {
            c.push((loc.clone(), Arc::clone(&v)));
        }
        v
    }
}

impl ListingService for GenListing {
    fn load(&self, req: LoadReq, rt: &urfin_core::Runtime) {
        let all = self.entries(&req.loc);
        rt.spawn(
            Lane::Interactive,
            "gen.list",
            req.cancel.clone(),
            move |cx| {
                #[allow(clippy::cast_possible_truncation, reason = "≤ 100k")]
                let n = all.len() as u32;
                let send = |u| {
                    cx.sink
                        .send(listing_msg(req.tab, req.generation, u), &cx.cancel)
                        .is_ok()
                };
                let _ = send(ListingUpdate::Chunk {
                    entries: all.to_vec(),
                    visible: (0..n).collect(),
                }) && send(ListingUpdate::Order {
                    order: Arc::new((0..n).collect()),
                    for_len: n,
                    seq: req.seq,
                }) && send(ListingUpdate::Done);
            },
        );
    }

    fn arrange(&self, req: ArrangeReq, rt: &urfin_core::Runtime) {
        rt.spawn_cpu("gen.arrange", req.cancel.clone(), move |cx| {
            #[allow(clippy::cast_possible_truncation, reason = "≤ 100k")]
            let n = req.entries.len() as u32;
            let u = ListingUpdate::Order {
                order: Arc::new((0..n).collect()),
                for_len: n,
                seq: req.seq,
            };
            let _ = cx
                .sink
                .send(listing_msg(req.tab, req.generation, u), &cx.cancel);
        });
    }
}

fn root() -> PathBuf {
    PathBuf::from(if cfg!(windows) { r"C:\Gen" } else { "/Gen" })
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        core.pump(Duration::from_millis(2));
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn listed(core: &Core, loc: &Location) -> bool {
    let t = core.state().active_tab();
    t.loc == *loc && matches!(t.listing.status, ListingStatus::Complete)
}

fn go(core: &mut Core, loc: &Location) {
    core.dispatch(Command::new("panel.go").arg("path", loc.clone()));
    pump_until(core, "listed", |c| listed(c, loc));
}

/// Core на `GenListing` с handler `ui` (его `on_pump` — часть кадра) и keymap.
fn gen_core(listing: Arc<GenListing>, start: &Location, scratch: &std::path::Path) -> Core {
    let mut boot = Boot::new(start.clone(), start.clone());
    boot.paths = Paths::in_dir(scratch);
    let mut core = Core::new(
        boot,
        Services {
            listing,
            open_resolvers: Vec::new(),
        },
        Arc::new(Wakes::default()),
    );
    urfin_ui::register(&mut core);
    let _ = urfin_keymap::register(
        &mut core,
        Paths::in_dir(scratch.join("keymap")),
        urfin_keymap::KeymapCfg::default(),
        urfin_keymap::FlagRegistry::with_builtin(),
    );
    core
}

// ---------------------------------------------------------------------------------------
// F.2: аллокации стационарного кадра одинаковы на 1k и 100k; форматирование строк — 0.

/// Строк на странице панели.
const PAGE: u32 = 40;
/// Стационарный кадр: курсор ходит по стольким строкам вниз и обратно (как `--bench`).
const SPAN: usize = 10;

/// Аллокации кадров стационарной фазы: (весь кадр, строки страницы).
fn steady_frames(n: u32) -> (Vec<u64>, u64) {
    let tmp = tempfile::tempdir().unwrap();
    let big = Location::local(root().join("big"));
    let mut core = gen_core(GenListing::new(n, 8), &big, tmp.path());
    pump_until(&mut core, "big listed", |c| listed(c, &big));
    assert!(core.state().active_tab().rows() >= n);
    core.dispatch(Command::new("panel.viewport").arg("rows", i64::from(PAGE)));
    let i18n = urfin_i18n::Catalog::embedded()
        .build(&"en".parse().unwrap())
        .0;
    let cfg = urfin_ui::fmt_cell::FmtCfg::new(&i18n, false, urfin_ui::sys::Zone::system());
    let mut cell = urfin_ui::fmt_cell::Cell::new();
    let cols = [
        urfin_ui::fmt_cell::ColKind::Name,
        urfin_ui::fmt_cell::ColKind::Ext,
        urfin_ui::fmt_cell::ColKind::Size,
        urfin_ui::fmt_cell::ColKind::Mtime,
        urfin_ui::fmt_cell::ColKind::Attrs,
    ];
    let rows = |core: &Core, cell: &mut urfin_ui::fmt_cell::Cell| {
        let t = core.state().active_tab();
        for r in t.cursor.top..t.cursor.top + PAGE {
            if let Some(Row::Entry { entry, .. }) = t.row(r) {
                for col in cols {
                    cell.clear();
                    urfin_ui::fmt_cell::cell_text(col, entry, &cfg, cell);
                    std::hint::black_box(cell.as_str());
                }
            }
        }
    };
    let frame = |core: &mut Core, k: usize, cell: &mut urfin_ui::fmt_cell::Cell| {
        let cmd = if k % (2 * SPAN) < SPAN {
            "panel.cursor.down"
        } else {
            "panel.cursor.up"
        };
        core.dispatch(Command::new(cmd));
        core.pump(Duration::from_millis(1));
        rows(core, cell);
    };
    // Прогрев: ленивые таблицы, ёмкости буферов.
    for k in 0..4 * SPAN {
        frame(&mut core, k, &mut cell);
    }
    let per_frame: Vec<u64> = (0..4 * SPAN)
        .map(|k| allocations(|| frame(&mut core, k, &mut cell)))
        .collect();
    let fmt = allocations(|| rows(&core, &mut cell));
    let _ = core.shutdown(Duration::from_millis(500));
    (per_frame, fmt)
}

#[test]
fn f2_steady_frame_allocations_do_not_grow() {
    let (k1, fmt1) = steady_frames(1_000);
    let (k100, fmt100) = steady_frames(100_000);
    eprintln!("F.2 allocs/frame 1k={k1:?} 100k={k100:?} fmt={fmt1}/{fmt100}");
    assert_eq!(k1, k100, "F.2: аллокации кадра зависят от размера каталога");
    // Наш код в стационарном кадре не аллоцирует: ни Core (команда, pump), ни строки.
    assert!(k1.iter().all(|&a| a == 0), "F.2: кадр аллоцирует: {k1:?}");
    assert_eq!((fmt1, fmt100), (0, 0), "F.2: строки страницы аллоцируют");
    // Счётчик работает.
    assert!(allocations(|| drop(std::hint::black_box(vec![1u8; 10]))) > 0);
}

// ---------------------------------------------------------------------------------------
// F.4: команда применена в том же `update` (без следующего pump).

#[test]
fn f4_command_applies_in_the_same_update() {
    let tmp = tempfile::tempdir().unwrap();
    let start = Location::local(root().join("d"));
    let mut core = gen_core(GenListing::new(0, 50), &start, tmp.path());
    pump_until(&mut core, "listed", |c| listed(c, &start));
    core.dispatch(Command::new("panel.viewport").arg("rows", 20));
    let probe = |c: &Core| {
        let s = c.state();
        let t = s.active_tab();
        (
            t.cursor.row,
            s.active,
            s.panel(s.active).tabs.len(),
            t.cursor.top,
        )
    };
    for text in [
        "panel.cursor.down",
        "panel.cursor.page_down",
        "panel.cursor.end",
        "panel.cursor.home",
        "panel.switch",
        "tab.new",
    ] {
        let before = (probe(&core), core.state().rev);
        core.dispatch(Command::parse(text).unwrap());
        let after = (probe(&core), core.state().rev);
        assert_ne!(
            before.0, after.0,
            "F.4: `{text}` не применена в том же update"
        );
        assert!(after.1 > before.1, "F.4: `{text}` не сменила rev");
        core.pump(Duration::from_millis(1));
    }
    let _ = core.shutdown(Duration::from_millis(500));
}

// ---------------------------------------------------------------------------------------
// S.2: простой — 0 пробуждений за 5 с (настоящие handlers, локальная ФС, слежение).

#[test]
fn s2_idle_has_no_wakeups() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dir");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for i in 0..20 {
        std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
    }
    let wakes = Arc::new(Wakes::default());
    let reg = Arc::new(VfsRegistry::with_local());
    let mut boot = Boot::new(Location::local(&dir), Location::local(tmp.path()));
    boot.paths = Paths::in_dir(tmp.path().join("app"));
    let mut core = Core::new(
        boot,
        Services {
            listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
            open_resolvers: vec![urfin_term::open_resolver()],
        },
        Arc::clone(&wakes) as Arc<dyn Waker>,
    );
    register_all(&mut core, &reg, tmp.path());
    // Старт: листинги, слежение, отложенные задачи — пока не затихнет на 1 с.
    let t0 = Instant::now();
    let mut quiet = Instant::now();
    while quiet.elapsed() < Duration::from_secs(1) {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "S.2: старт не затих"
        );
        if wakes.woke.swap(false, Ordering::SeqCst) {
            quiet = Instant::now();
        }
        core.pump(Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(listed(&core, &Location::local(&dir)));
    // Простой: pump — только по пробуждению, как в окне.
    let before = wakes.n.load(Ordering::SeqCst);
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        if wakes.woke.swap(false, Ordering::SeqCst) {
            core.pump(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let n = wakes.n.load(Ordering::SeqCst) - before;
    let _ = core.shutdown(Duration::from_millis(500));
    assert_eq!(n, 0, "S.2: пробуждений за 5 с простоя");
}

/// Handlers приложения без окна (как `register_handlers` в `main.rs`, без обновления
/// программы): ui, vfs, du, ops, term, ssh/sftp, pkg, поиск, вьювер, keymap.
fn register_all(core: &mut Core, reg: &Arc<VfsRegistry>, scratch: &std::path::Path) {
    urfin_ui::register(core);
    urfin_vfs::register(core, Arc::clone(reg), VfsCfg::default());
    urfin_search::register(core, Arc::clone(reg), None);
    urfin_vfs::du::register(core, urfin_vfs::du::DuCfg::default());
    urfin_ops::register(core, urfin_ops::OpsConfig::default());
    urfin_term::register(core, urfin_term::TermConfig::default());
    urfin_net::register(core, urfin_net::SshCfg::default());
    urfin_pkg::register(core, urfin_pkg::PkgConfig::default());
    urfin_view::register(core, &urfin_core::Config::default());
    let _ = urfin_keymap::register(
        core,
        Paths::in_dir(scratch.join("keymap")),
        urfin_keymap::KeymapCfg::default(),
        urfin_keymap::FlagRegistry::with_builtin(),
    );
}

// ---------------------------------------------------------------------------------------
// S.6: 10k навигаций и 1k табов — рост живой кучи < 2 МБ.

#[test]
fn s6_soak_navigation_and_tabs_do_not_grow() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs: Vec<Location> = (0..8)
        .map(|i| Location::local(root().join(format!("d{i:02}"))))
        .collect();
    let mut core = gen_core(GenListing::new(0, 40), &dirs[0], tmp.path());
    pump_until(&mut core, "listed", |c| listed(c, &dirs[0]));
    let navigate = |core: &mut Core, n: usize| {
        for i in 0..n {
            go(core, &dirs[i % dirs.len()]);
        }
    };
    let tabs = |core: &mut Core, n: usize| {
        for _ in 0..n {
            core.dispatch(Command::new("tab.new"));
            core.pump(Duration::from_millis(1));
            core.dispatch(Command::new("tab.close"));
            core.pump(Duration::from_millis(1));
        }
    };
    // Прогрев: история, кэши и ёмкости доходят до своих пределов.
    navigate(&mut core, 1_000);
    tabs(&mut core, 200);
    pump_until(&mut core, "warm", |c| {
        listed(c, &c.state().active_tab().loc)
    });
    let base = LIVE.load(Ordering::SeqCst);
    navigate(&mut core, 10_000);
    tabs(&mut core, 1_000);
    navigate(&mut core, dirs.len());
    let grown = LIVE.load(Ordering::SeqCst) - base;
    eprintln!("S.6: рост кучи {grown} Б");
    let _ = core.shutdown(Duration::from_millis(500));
    assert!(grown < 2 << 20, "S.6: куча выросла на {grown} Б (≥ 2 МБ)");
}

// ---------------------------------------------------------------------------------------
// L.6: навигация в свежий кэш — 0 I/O.

/// `LocalFs`, считающий обращения к ФС.
struct CountingFs {
    inner: LocalFs,
    io: Arc<AtomicU32>,
}

impl CountingFs {
    fn hit(&self) {
        self.io.fetch_add(1, Ordering::SeqCst);
    }
}

impl Vfs for CountingFs {
    fn scheme(&self) -> &'static str {
        "local"
    }
    fn caps(&self, at: &VfsPath) -> Caps {
        self.inner.caps(at)
    }
    fn list(&self, dir: &VfsPath, opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        self.hit();
        self.inner.list(dir, opts)
    }
    fn stat(&self, p: &VfsPath, follow: bool) -> Result<Metadata, VfsError> {
        self.hit();
        self.inner.stat(p, follow)
    }
    fn stat_entry(&self, d: &VfsPath, n: &str, a: Attrs) -> Result<Entry, VfsError> {
        self.hit();
        self.inner.stat_entry(d, n, a)
    }
    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        self.hit();
        self.inner.open_read(p)
    }
    fn space(&self, at: &VfsPath) -> Result<Space, VfsError> {
        self.hit();
        self.inner.space(at)
    }
    fn local_path(&self, p: &VfsPath) -> Option<PathBuf> {
        self.inner.local_path(p)
    }
    fn watch(&self, dir: &VfsPath, tx: WatchTx) -> Result<WatchGuard, VfsError> {
        self.inner.watch(dir, tx)
    }
}

#[cfg(windows)]
#[test]
fn l6_fresh_cache_navigation_does_no_io() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    for i in 0..50 {
        std::fs::write(a.join(format!("a{i:02}.txt")), b"x").unwrap();
    }
    std::fs::write(b.join("b.txt"), b"x").unwrap();
    let io = Arc::new(AtomicU32::new(0));
    let mut reg = VfsRegistry::default();
    reg.register(Arc::new(CountingFs {
        inner: LocalFs::new(),
        io: Arc::clone(&io),
    }));
    let listing = Arc::new(VfsListing::new(Arc::new(reg), VfsCfg::default()));
    let mut core = Core::new(
        Boot::new(Location::local(&a), Location::local(tmp.path())),
        Services {
            listing: listing.clone(),
            open_resolvers: Vec::new(),
        },
        Arc::new(Wakes::default()),
    );
    let (la, lb) = (Location::local(&a), Location::local(&b));
    pump_until(&mut core, "initial", |c| listed(c, &la));
    let left = core.state().panel(Side::Left).tab().id;
    pump_until(&mut core, "watched", |_| {
        listing.watched_tabs().contains(&left)
    });
    go(&mut core, &lb);
    pump_until(&mut core, "cached", |_| listing.cache_stats().0 >= 1);
    let before = io.load(Ordering::SeqCst);
    let t = Instant::now();
    go(&mut core, &la);
    let took = t.elapsed();
    let n = io.load(Ordering::SeqCst) - before;
    eprintln!("L.6: {n} I/O, {took:?}");
    let _ = core.shutdown(Duration::from_millis(500));
    assert_eq!(n, 0, "L.6: навигация в свежий кэш читала ФС");
}

// ---------------------------------------------------------------------------------------
// O.18: пауза job на 100k — ≤ 50 мс, отмена — ≤ 100 мс.

const JOB_ITEMS: u32 = 100_000;

/// Схема `mem://h/big` из `JOB_ITEMS` файлов в памяти: `remove` — около 20 мкс работы,
/// время последнего вызова — в `last_ns` (от `base`).
struct MemFs {
    base: Instant,
    removed: AtomicU64,
    last_ns: AtomicU64,
}

fn dir_meta(kind: EntryKind) -> Metadata {
    Metadata {
        kind,
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

struct Batch(Option<Vec<Entry>>);

impl ListStream for Batch {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        match self.0.take() {
            Some(b) => {
                out.extend(b);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

impl MemFs {
    fn since(&self) -> Duration {
        #[allow(clippy::cast_possible_truncation, reason = "нс теста < 584 лет")]
        Duration::from_nanos(self.last_ns.load(Ordering::SeqCst))
    }
}

impl Vfs for MemFs {
    fn scheme(&self) -> &'static str {
        "mem"
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        Caps::default()
    }
    fn list(&self, dir: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        let p = dir.path.to_string_lossy().replace('\\', "/");
        let v = match p.trim_end_matches('/') {
            "" => vec![urfin_core::fake::FakeListing::dir("big")],
            "/big" => (0..JOB_ITEMS)
                .map(|i| urfin_core::fake::FakeListing::file(&format!("f{i:06}"), 1))
                .collect(),
            _ => return Err(VfsError::NotFound(dir.path.clone())),
        };
        Ok(Box::new(Batch(Some(v))))
    }
    fn stat(&self, p: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        let s = p.path.to_string_lossy().replace('\\', "/");
        Ok(dir_meta(match s.trim_end_matches('/') {
            "" | "/big" => EntryKind::Dir,
            _ => EntryKind::File,
        }))
    }
    fn stat_entry(&self, d: &VfsPath, name: &str, _: Attrs) -> Result<Entry, VfsError> {
        let _ = d;
        Ok(if name == "big" {
            urfin_core::fake::FakeListing::dir(name)
        } else {
            urfin_core::fake::FakeListing::file(name, 1)
        })
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
    fn remove(&self, _p: &VfsPath) -> Result<(), VfsError> {
        let t = Instant::now();
        while t.elapsed() < Duration::from_micros(20) {
            std::hint::spin_loop();
        }
        self.removed.fetch_add(1, Ordering::SeqCst);
        #[allow(clippy::cast_possible_truncation, reason = "нс теста < 584 лет")]
        self.last_ns
            .store(self.base.elapsed().as_nanos() as u64, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn o18_pause_and_cancel_of_100k_job_are_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let mem = Arc::new(MemFs {
        base: Instant::now(),
        removed: AtomicU64::new(0),
        last_ns: AtomicU64::new(0),
    });
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::clone(&mem) as Arc<dyn Vfs>);
    let reg = Arc::new(reg);
    let at = Location::parse_uri("mem://h/").unwrap();
    let mut core = Core::new(
        Boot::new(at.clone(), Location::local(tmp.path())),
        Services {
            listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
            open_resolvers: Vec::new(),
        },
        Arc::new(Wakes::default()),
    );
    urfin_vfs::register(&mut core, Arc::clone(&reg), VfsCfg::default());
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
    pump_until(&mut core, "listed", |c| {
        c.state()
            .active_tab()
            .listing
            .entries
            .iter()
            .any(|e| e.name == "big")
    });
    core.dispatch(Command::new("panel.cursor.to").arg("name", "big"));
    core.dispatch(
        Command::new("ops.delete")
            .arg("permanent", true)
            .arg("confirmed", true),
    );
    let job = |c: &Core| {
        c.state()
            .jobs
            .iter()
            .find(|j| j.kind == "delete")
            .map(|j| j.id)
    };
    pump_until(&mut core, "job started", |c| job(c).is_some());
    let id = job(&core).unwrap();
    let id_arg = i64::try_from(id.0).unwrap();
    let state = |c: &Core| {
        c.state()
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.state.clone())
    };
    pump_until(&mut core, "job running", |_| {
        mem.removed.load(Ordering::SeqCst) >= 2_000
    });
    // Пауза: после `job.pause` ни одна работа не стартует позже 50 мс.
    let t_pause = mem.base.elapsed();
    core.dispatch(Command::new("job.pause").arg("id", id_arg));
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(300) {
        core.pump(Duration::from_millis(2));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(state(&core), Some(JobState::Paused));
    let lag = mem.since().saturating_sub(t_pause);
    let frozen = mem.removed.load(Ordering::SeqCst);
    assert!(frozen < u64::from(JOB_ITEMS), "job кончилась до паузы");
    eprintln!("O.18: пауза {lag:?}");
    assert!(
        lag <= Duration::from_millis(50),
        "O.18: пауза {lag:?} > 50 мс"
    );
    // Отмена (после продолжения): job кончается и работа встаёт за ≤ 100 мс.
    core.dispatch(Command::new("job.resume").arg("id", id_arg));
    pump_until(&mut core, "resumed", |_| {
        mem.removed.load(Ordering::SeqCst) >= frozen + 1_000
    });
    let t_cancel = mem.base.elapsed();
    let t = Instant::now();
    core.dispatch(Command::new("job.cancel").arg("id", id_arg));
    pump_until(&mut core, "cancelled", |c| {
        state(c).is_none_or(|s| !s.is_active())
    });
    let ended = t.elapsed();
    std::thread::sleep(Duration::from_millis(150));
    let tail = mem.since().saturating_sub(t_cancel);
    eprintln!("O.18: отмена {ended:?}, последняя работа через {tail:?}");
    let _ = core.shutdown(Duration::from_millis(500));
    assert!(
        mem.removed.load(Ordering::SeqCst) < u64::from(JOB_ITEMS),
        "job кончилась до отмены"
    );
    assert!(
        ended <= Duration::from_millis(100),
        "O.18: отмена {ended:?} > 100 мс"
    );
    assert!(
        tail <= Duration::from_millis(100),
        "O.18: работа шла {tail:?} после отмены"
    );
}
