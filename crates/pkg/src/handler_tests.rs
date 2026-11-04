//! Вкладка `pkg:` через Core: открытие из кэша без процессов, Ctrl+R по менеджерам,
//! ошибка одного менеджера, фильтры. Менеджеры — подстановка ([`Fake`]) и, на Windows,
//! подставной процесс `fake-pkgmgr` с фикстурой choco.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use compact_str::CompactString;
use urfin_core::{Boot, Core, ListingStatus, Services, SortKey};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

use super::*;
use crate::api::{Hit, Mgr, MgrCaps, Package, PkgError, PkgId, PkgOp, Update};
use crate::book::{Run, View};
use crate::vfs::PkgVfs;
use crate::{ActionPlan, PkgCache};

struct NoWake;

impl urfin_core::Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn pkg(id: &str, name: &str, version: &str) -> Package {
    let id: PkgId = id.parse().unwrap();
    Package {
        source: (id.mgr != Mgr::Choco).then(|| CompactString::from(id.mgr.as_str())),
        id,
        name: name.into(),
        version: version.into(),
        publisher: None,
    }
}

/// Ответ подставного менеджера.
#[derive(Clone)]
enum Answer {
    Ok(Vec<Package>, Vec<Update>),
    Fail(PkgError),
}

/// Запуск действия подставного менеджера: менеджер, id, начало и конец.
type RunLog = Arc<std::sync::Mutex<Vec<(Mgr, String, Instant, Instant)>>>;

/// Подставной менеджер: ответ после паузы, счётчик запросов; поиск — свои результаты
/// и пауза; действие — пауза и запись в журнал.
struct Fake {
    mgr: Mgr,
    answer: Answer,
    delay: Duration,
    calls: Arc<AtomicUsize>,
    hits: Vec<Hit>,
    search_delay: Duration,
    cancels: Arc<AtomicUsize>,
    /// Поисков, дошедших до ответа.
    searched: Arc<AtomicUsize>,
    runs: RunLog,
}

impl Fake {
    fn wait(&self, cx: &RunCx) -> Result<(), PkgError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let t0 = Instant::now();
        while t0.elapsed() < self.delay {
            if cx.cancel.is_cancelled() {
                return Err(PkgError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}

impl PackageManager for Fake {
    fn mgr(&self) -> Mgr {
        self.mgr
    }
    fn detect(&self) -> Option<Detected> {
        Some(found(self.mgr))
    }
    fn version(&self, _cx: &RunCx) -> Result<CompactString, PkgError> {
        Ok("1.0".into())
    }
    fn list_installed(&self, cx: &RunCx) -> Result<Vec<Package>, PkgError> {
        self.wait(cx)?;
        match &self.answer {
            Answer::Ok(l, _) => Ok(l.clone()),
            Answer::Fail(e) => Err(e.clone()),
        }
    }
    fn list_updates(&self, cx: &RunCx) -> Result<Vec<Update>, PkgError> {
        self.wait(cx)?;
        match &self.answer {
            Answer::Ok(_, u) => Ok(u.clone()),
            Answer::Fail(e) => Err(e.clone()),
        }
    }
    fn search(&self, _q: &str, cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
        let t0 = Instant::now();
        while t0.elapsed() < self.search_delay {
            if cx.cancel.is_cancelled() {
                self.cancels.fetch_add(1, Ordering::SeqCst);
                return Err(PkgError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.searched.fetch_add(1, Ordering::SeqCst);
        Ok(self.hits.clone())
    }
    fn action(&self, op: PkgOp, id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
        Ok(ActionPlan {
            argv: vec![format!("{op:?}").into(), id.id.as_str().into()],
            elevate: false,
            success_codes: vec![0],
            reboot_codes: vec![3010],
        })
    }
    fn run(&self, plan: &ActionPlan, cx: &RunCx) -> Result<crate::ActionDone, PkgError> {
        let t0 = Instant::now();
        if let Some(l) = &cx.lines {
            l("  ██▒▒  41%");
        }
        std::thread::sleep(Duration::from_millis(120));
        let id = plan.argv[1].to_string_lossy().into_owned();
        self.runs
            .lock()
            .unwrap()
            .push((self.mgr, id, t0, Instant::now()));
        Ok(crate::ActionDone::Ok)
    }
}

fn found(mgr: Mgr) -> Detected {
    Detected {
        mgr,
        exe: format!("{mgr}.exe").into(),
        version: Some("1.0".into()),
        caps: MgrCaps::empty(),
    }
}

/// Менеджеры теста; `detects` и `calls` — сколько раз спрашивали.
#[derive(Default)]
struct Source {
    managers: Vec<(Mgr, Answer, Duration)>,
    detects: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    /// Поиск: результаты и пауза менеджера.
    search: Vec<(Mgr, Vec<Hit>, Duration)>,
    cancels: Arc<AtomicUsize>,
    /// Поисков, дошедших до ответа.
    searched: Arc<AtomicUsize>,
    runs: RunLog,
}

impl PkgSource for Source {
    fn detect(&self, _cached: &Managers) -> Vec<Detected> {
        self.detects.fetch_add(1, Ordering::SeqCst);
        self.managers.iter().map(|(m, _, _)| found(*m)).collect()
    }
    fn arp(&self) -> Vec<ArpEntry> {
        Vec::new()
    }
    fn adapter(&self, d: &Detected, _cfg: &PkgConfig) -> Arc<dyn PackageManager> {
        let (_, answer, delay) = self
            .managers
            .iter()
            .find(|(m, _, _)| *m == d.mgr)
            .unwrap()
            .clone();
        let (hits, search_delay) = self
            .search
            .iter()
            .find(|(m, _, _)| *m == d.mgr)
            .map(|(_, h, t)| (h.clone(), *t))
            .unwrap_or_default();
        Arc::new(Fake {
            mgr: d.mgr,
            answer,
            delay,
            calls: Arc::clone(&self.calls),
            hits,
            search_delay,
            cancels: Arc::clone(&self.cancels),
            searched: Arc::clone(&self.searched),
            runs: Arc::clone(&self.runs),
        })
    }
}

fn setup(source: Arc<dyn PkgSource>, dir: Option<PathBuf>) -> (Core, Arc<PkgBook>) {
    let book = PkgBook::new(&PkgConfig::default());
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(PkgVfs::new(Arc::clone(&book))));
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::new(reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let start = Location::local(std::env::temp_dir());
    let mut core = Core::new(Boot::new(start.clone(), start), services, Arc::new(NoWake));
    core.register(Box::new(PkgHandler::with(
        PkgConfig::default(),
        Arc::clone(&book),
        source,
        dir,
    )));
    core.pump(Duration::from_millis(5));
    (core, book)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(2));
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Имена строк активной вкладки, когда её листинг загружен.
fn shown(core: &Core) -> Option<Vec<String>> {
    let t = core.state().active_tab();
    (filter_of(&t.loc).is_some() && t.listing.status == ListingStatus::Complete).then(|| {
        (0..t.rows())
            .filter_map(|i| match t.row(i) {
                Some(urfin_core::Row::Entry { entry, .. }) => Some(entry.name.to_string()),
                _ => None,
            })
            .collect()
    })
}

fn done(book: &PkgBook) -> bool {
    let i = book.info();
    i.loaded && !i.busy() && i.checked_at.is_some()
}

fn winget_scoop() -> Vec<(Mgr, Answer, Duration)> {
    let ff = pkg("winget:Mozilla.Firefox", "Firefox", "143.0.4");
    vec![
        (
            Mgr::Winget,
            Answer::Ok(
                vec![ff.clone(), pkg("winget:VideoLAN.VLC", "VLC", "3.0.21")],
                vec![Update {
                    pkg: ff,
                    available: "144.0".into(),
                    pinned: false,
                }],
            ),
            Duration::from_millis(30),
        ),
        (
            Mgr::Scoop,
            Answer::Ok(vec![pkg("scoop:main/jq", "jq", "1.8.1")], Vec::new()),
            Duration::from_millis(60),
        ),
    ]
}

/// Первое открытие без кэша: проверка всех менеджеров, вкладка `pkg:installed` в
/// порядке модели, кэш и `managers.toml` записаны.
#[test]
fn open_without_cache_checks() {
    let dir = tempfile::tempdir().unwrap();
    let src = Arc::new(Source {
        managers: winget_scoop(),
        ..Source::default()
    });
    let (mut core, book) = setup(src.clone(), Some(dir.path().to_path_buf()));
    core.dispatch(Command::parse("pkg.open").unwrap());
    pump_until(&mut core, "checked", |_| done(&book));
    pump_until(&mut core, "rows", |c| {
        shown(c).is_some_and(|n| n == ["Firefox", "jq", "VLC"])
    });
    let t = core.state().active_tab();
    assert_eq!(t.loc, location(Filter::Installed));
    assert_eq!(t.sort.key, SortKey::Unsorted);
    assert_eq!(src.detects.load(Ordering::SeqCst), 1);
    // Установленные и обновления у каждого менеджера.
    assert_eq!(src.calls.load(Ordering::SeqCst), 4);
    pump_until(&mut core, "cache", |_| {
        dir.path().join(CACHE_FILE).is_file() && dir.path().join(MANAGERS_FILE).is_file()
    });
}

/// Открытие с кэшем: строки и дата из кэша, ни обнаружения, ни запросов.
#[test]
fn open_with_cache_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut cache = PkgCache {
        checked_at: Some("2025-01-02T09:00:00Z".parse().unwrap()),
        ..PkgCache::default()
    };
    cache
        .installed
        .insert(Mgr::Scoop, vec![pkg("scoop:main/fd", "fd", "10.3.0")]);
    cache.save(&dir.path().join(CACHE_FILE)).unwrap();
    let src = Arc::new(Source {
        managers: winget_scoop(),
        ..Source::default()
    });
    let (mut core, book) = setup(src.clone(), Some(dir.path().to_path_buf()));
    core.dispatch(Command::parse("pkg.open").unwrap());
    pump_until(&mut core, "rows", |c| shown(c).is_some_and(|n| n == ["fd"]));
    // Ещё немного: никаких фоновых запросов.
    for _ in 0..20 {
        core.pump(Duration::from_millis(2));
    }
    assert_eq!(src.detects.load(Ordering::SeqCst), 0);
    assert_eq!(src.calls.load(Ordering::SeqCst), 0);
    assert_eq!(book.info().checked_at, cache.checked_at.map(ns));
    // Ctrl+R — менеджеры спрашиваются, строки — их ответы.
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    core.pump(Duration::from_millis(1));
    assert!(book.info().busy());
    pump_until(&mut core, "refreshed", |c| {
        shown(c).is_some_and(|n| n == ["Firefox", "jq", "VLC"])
    });
    pump_until(&mut core, "checked", |_| done(&book));
    assert!(book.info().checked_at > cache.checked_at.map(ns));
}

/// Ошибка одного менеджера: остальные отвечают, ход — «не удалось» у него одного.
#[test]
fn one_failure_does_not_block() {
    let mut managers = winget_scoop();
    managers[1].1 = Answer::Fail(PkgError::Timeout);
    let src = Arc::new(Source {
        managers,
        ..Source::default()
    });
    let (mut core, book) = setup(src, None);
    core.dispatch(Command::parse("pkg.open").unwrap());
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    pump_until(&mut core, "checked", |_| done(&book));
    pump_until(&mut core, "rows", |c| {
        shown(c).is_some_and(|n| n == ["Firefox", "VLC"])
    });
    let runs: Vec<(Mgr, Run)> = book
        .info()
        .managers
        .into_iter()
        .map(|m| (m.mgr, m.run))
        .collect();
    assert_eq!(
        runs,
        [
            (Mgr::Winget, Run::Idle),
            (Mgr::Scoop, Run::Failed("timed out".into()))
        ]
    );
}

/// Ctrl+R во время проверки ничего не запускает повторно.
#[test]
fn refresh_while_busy() {
    let mut managers = winget_scoop();
    managers[0].2 = Duration::from_millis(300);
    let src = Arc::new(Source {
        managers,
        ..Source::default()
    });
    let (mut core, book) = setup(src.clone(), None);
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    core.pump(Duration::from_millis(5));
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    pump_until(&mut core, "checked", |_| done(&book));
    assert_eq!(src.detects.load(Ordering::SeqCst), 1);
    assert_eq!(src.calls.load(Ordering::SeqCst), 4);
}

/// Фильтры: Ctrl+2 — «Обновления» в той же вкладке, повторное «Пакеты…» — та же
/// вкладка; поиск — F02-09.
#[test]
fn filters() {
    let src = Arc::new(Source {
        managers: winget_scoop(),
        ..Source::default()
    });
    let (mut core, book) = setup(src, None);
    core.dispatch(Command::parse("pkg.open").unwrap());
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    pump_until(&mut core, "checked", |_| done(&book));
    let tabs = core.state().panel(core.state().active).tabs.len();
    core.dispatch(Command::parse("pkg.filter name=updates").unwrap());
    pump_until(&mut core, "updates", |c| {
        shown(c).is_some_and(|n| n == ["Firefox"])
    });
    assert_eq!(core.state().active_tab().loc, location(Filter::Updates));
    core.dispatch(Command::parse("pkg.open").unwrap());
    pump_until(&mut core, "installed", |c| {
        shown(c).is_some_and(|n| n.len() == 3)
    });
    assert_eq!(core.state().panel(core.state().active).tabs.len(), tabs);
    core.dispatch(Command::parse("pkg.filter name=search").unwrap());
    pump_until(&mut core, "search", |c| {
        c.state().active_tab().loc == location(Filter::Search)
    });
    assert_eq!(book.view().search.info.back, Some(Filter::Installed));
    let v: Arc<View> = book.view();
    assert_eq!(
        v.rows(Filter::Updates)[0].available.as_deref(),
        Some("144.0")
    );
}

/// Настоящие процессы: choco — подставной `fake-pkgmgr` с фикстурой `choco list`,
/// второй менеджер падает с кодом 1 — первый всё равно в списке.
#[cfg(windows)]
#[test]
fn fake_pkgmgr_processes() {
    struct Proc {
        mgr: Mgr,
        args: Vec<String>,
    }
    impl PackageManager for Proc {
        fn mgr(&self) -> Mgr {
            self.mgr
        }
        fn detect(&self) -> Option<Detected> {
            None
        }
        fn version(&self, _cx: &RunCx) -> Result<CompactString, PkgError> {
            Ok("2.3.0".into())
        }
        fn list_installed(&self, cx: &RunCx) -> Result<Vec<Package>, PkgError> {
            let mut argv = vec![crate::testutil::fake().into_os_string()];
            argv.extend(self.args.iter().map(Into::into));
            let out = crate::proc::run_query(&argv, cx)?.ok()?;
            Ok(crate::choco::parse_list(&out.lines))
        }
        fn list_updates(&self, _cx: &RunCx) -> Result<Vec<Update>, PkgError> {
            Ok(Vec::new())
        }
        fn search(&self, _q: &str, _cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
            Ok(Vec::new())
        }
        fn action(&self, _op: PkgOp, _id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
            Err(PkgError::NotFound)
        }
    }
    struct ProcSource;
    impl PkgSource for ProcSource {
        fn detect(&self, _cached: &Managers) -> Vec<Detected> {
            vec![found(Mgr::Winget), found(Mgr::Choco)]
        }
        fn arp(&self) -> Vec<ArpEntry> {
            Vec::new()
        }
        fn adapter(&self, d: &Detected, _cfg: &PkgConfig) -> Arc<dyn PackageManager> {
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/choco/list.v2.txt");
            let args = match d.mgr {
                Mgr::Choco => vec![
                    "--fixture".to_owned(),
                    fixture.display().to_string(),
                    "--delay-ms".to_owned(),
                    "1".to_owned(),
                ],
                _ => vec![
                    "--line".to_owned(),
                    "boom".to_owned(),
                    "--exit".to_owned(),
                    "1".to_owned(),
                ],
            };
            Arc::new(Proc { mgr: d.mgr, args })
        }
    }
    let (mut core, book) = setup(Arc::new(ProcSource), None);
    core.dispatch(Command::parse("pkg.open").unwrap());
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    pump_until(&mut core, "checked", |_| done(&book));
    let info = book.info();
    assert_eq!(
        info.installed,
        8,
        "{:?}",
        book.view().rows(Filter::Installed)
    );
    let winget = info.managers.iter().find(|m| m.mgr == Mgr::Winget).unwrap();
    assert!(
        matches!(&winget.run, Run::Failed(e) if e.contains("exit code 1")),
        "{winget:?}"
    );
    pump_until(&mut core, "rows", |c| {
        shown(c).is_some_and(|n| n.len() == 8)
    });
}

fn hit(id: &str, name: &str, version: &str) -> Hit {
    let p = pkg(id, name, version);
    Hit {
        matched: crate::api::MatchKind::of("7zip", &p.id.id, &p.name),
        pkg: p,
    }
}

fn answers(book: &PkgBook) -> Vec<(Mgr, crate::Answer)> {
    book.view().search.info.answers.clone()
}

/// Поиск (приёмка F02-09): ответы 50/300/1000 мс — первые результаты видны, как только
/// ответил самый быстрый; пустой ответ и «не ответил» различимы; новый запрос отменяет
/// процессы прежнего; Esc-возврат — к прежнему фильтру.
#[test]
fn search_streams() {
    use crate::Answer as Got;
    let ok = Answer::Ok(Vec::new(), Vec::new());
    let src = Arc::new(Source {
        managers: vec![
            (Mgr::Winget, ok.clone(), Duration::ZERO),
            (Mgr::Scoop, ok.clone(), Duration::ZERO),
            (Mgr::Choco, ok, Duration::ZERO),
        ],
        search: vec![
            (
                Mgr::Winget,
                vec![hit("winget:7zip.7zip", "7-Zip", "24.08")],
                Duration::from_millis(1000),
            ),
            (
                Mgr::Scoop,
                vec![hit("scoop:main/7zip", "7zip", "24.08")],
                Duration::from_millis(50),
            ),
            (Mgr::Choco, Vec::new(), Duration::from_millis(300)),
        ],
        ..Source::default()
    });
    let (mut core, book) = setup(src.clone(), None);
    core.dispatch(Command::parse("pkg.open filter=updates").unwrap());
    pump_until(&mut core, "updates", |c| {
        c.state().active_tab().loc == location(Filter::Updates)
    });
    let t0 = Instant::now();
    core.dispatch(Command::parse("pkg.search q=7zip").unwrap());
    pump_until(&mut core, "first", |_| {
        answers(&book)
            .iter()
            .any(|(_, a)| matches!(a, Got::Found(_)))
    });
    let first = t0.elapsed();
    assert!(first < Duration::from_millis(700), "{first:?}");
    let a = answers(&book);
    assert_eq!(a[0], (Mgr::Winget, Got::Busy));
    assert_eq!(a[1], (Mgr::Scoop, Got::Found(1)));
    pump_until(&mut core, "rows", |c| {
        shown(c).is_some_and(|n| n == ["7zip", "7zip (scoop)"])
    });
    pump_until(&mut core, "all", |_| !book.view().search.info.busy());
    assert_eq!(answers(&book)[2], (Mgr::Choco, Got::Found(0)));
    pump_until(&mut core, "merged", |c| {
        shown(c).is_some_and(|n| n == ["7-Zip", "7zip.7zip", "7zip"])
    });
    let info = book.view().search.info.clone();
    assert_eq!((info.groups, info.variants), (1, 2));
    assert_eq!(info.back, Some(Filter::Updates));
    assert!(!info.empty());

    // Пробел на группе сворачивает её.
    core.dispatch(Command::parse("panel.cursor.to name=7-Zip").unwrap());
    core.dispatch(Command::parse("pkg.pick").unwrap());
    pump_until(&mut core, "closed", |c| {
        shown(c).is_some_and(|n| n == ["7-Zip"])
    });

    // Новый запрос отменяет процессы прежнего: медленные winget и choco не дождались.
    let before = src.searched.load(Ordering::SeqCst);
    core.dispatch(Command::parse("pkg.search q=first").unwrap());
    pump_until(&mut core, "started", |_| {
        book.view().search.info.q == "first"
    });
    core.dispatch(Command::parse("pkg.search q=second").unwrap());
    pump_until(&mut core, "second", |_| {
        let i = book.view().search.info.clone();
        i.q == "second" && !i.busy()
    });
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_millis(1200) {
        core.pump(Duration::from_millis(2));
    }
    assert!(src.searched.load(Ordering::SeqCst) - before <= 4);
    assert!(src.cancels.load(Ordering::SeqCst) <= 2);
    // Только winget.
    core.dispatch(Command::parse("pkg.search in=winget").unwrap());
    pump_until(&mut core, "winget only", |_| {
        let i = book.view().search.info.clone();
        i.answers.len() == 1 && !i.busy()
    });
}

/// Действия (приёмка F02-10): два пакета одного менеджера — последовательно, разных —
/// параллельно; после — менеджер перечитан; «Обновить все» без закреплённых; F8 —
/// подтверждение.
#[test]
fn actions_queue() {
    let list = vec![pkg("winget:A.A", "A", "1"), pkg("winget:B.B", "B", "1")];
    let ups = vec![
        Update {
            pkg: pkg("winget:A.A", "A", "1"),
            available: "2".into(),
            pinned: false,
        },
        Update {
            pkg: pkg("winget:B.B", "B", "1"),
            available: "2".into(),
            pinned: true,
        },
    ];
    let src = Arc::new(Source {
        managers: vec![
            (Mgr::Winget, Answer::Ok(list, ups), Duration::ZERO),
            (
                Mgr::Scoop,
                Answer::Ok(vec![pkg("scoop:main/c", "c", "1")], Vec::new()),
                Duration::ZERO,
            ),
        ],
        ..Source::default()
    });
    let (mut core, book) = setup(src.clone(), None);
    core.dispatch(Command::parse("pkg.open").unwrap());
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    pump_until(&mut core, "checked", |_| done(&book));
    let calls = src.calls.load(Ordering::SeqCst);
    let list = |ids: &[&str]| {
        urfin_core::Value::List(
            ids.iter()
                .map(|s| urfin_core::Value::Str((*s).into()))
                .collect(),
        )
    };
    core.dispatch(Command::new("pkg.upgrade").arg(
        "targets",
        list(&["winget:A.A", "winget:B.B", "scoop:main/c"]),
    ));
    pump_until(&mut core, "jobs", |c| {
        c.state().jobs.iter().count() == 2 && c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    assert!(book.view().busy.is_empty());
    let runs = src.runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 3, "{runs:?}");
    let w: Vec<_> = runs.iter().filter(|r| r.0 == Mgr::Winget).collect();
    let s = runs.iter().find(|r| r.0 == Mgr::Scoop).unwrap();
    assert!(w[0].3 <= w[1].2 || w[1].3 <= w[0].2, "winget — по очереди");
    assert!(s.2 < w[0].3.max(w[1].3), "scoop — параллельно");
    pump_until(&mut core, "requery", |_| {
        src.calls.load(Ordering::SeqCst) >= calls + 4
    });
    // «Обновить все» пропускает закреплённый B.
    src.runs.lock().unwrap().clear();
    core.dispatch(Command::parse("pkg.upgrade_all").unwrap());
    pump_until(&mut core, "all", |_| src.runs.lock().unwrap().len() == 1);
    assert_eq!(src.runs.lock().unwrap()[0].1, "A.A");
    // F8: подтверждение одной строкой; «удалить» запускает действие.
    src.runs.lock().unwrap().clear();
    core.dispatch(Command::new("pkg.uninstall").arg("targets", list(&["scoop:main/c"])));
    core.pump(Duration::from_millis(5));
    let id = core.state().prompts.active().map(|p| p.id).unwrap();
    assert!(src.runs.lock().unwrap().is_empty());
    core.dispatch(
        Command::new("prompt.reply")
            .arg("id", i64::try_from(id.0).unwrap())
            .arg("choice", "uninstall"),
    );
    pump_until(&mut core, "uninstall", |_| {
        src.runs.lock().unwrap().len() == 1
    });
}

/// `Timestamp` кэша — как `PkgInfo::checked_at` (Unix, нс).
fn ns(t: jiff::Timestamp) -> i64 {
    i64::try_from(t.as_nanosecond()).unwrap()
}

#[path = "handler_check_tests.rs"]
mod check;
