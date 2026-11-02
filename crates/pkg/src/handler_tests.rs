//! Вкладка `pkg:` через Core: открытие из кэша без процессов, Ctrl+R по менеджерам,
//! ошибка одного менеджера, фильтры. Менеджеры — подстановка ([`Fake`]) и, на Windows,
//! подставной процесс `fake-pkgmgr` с фикстурой choco.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use compact_str::CompactString;
use urfin_core::{Boot, Core, ListingStatus, Services, SortKey, UiEvent};
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

/// Подставной менеджер: ответ после паузы, счётчик запросов.
struct Fake {
    mgr: Mgr,
    answer: Answer,
    delay: Duration,
    calls: Arc<AtomicUsize>,
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
    fn search(&self, _q: &str, _cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
        Ok(Vec::new())
    }
    fn action(&self, _op: PkgOp, _id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
        Err(PkgError::NotFound)
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
        Arc::new(Fake {
            mgr: d.mgr,
            answer,
            delay,
            calls: Arc::clone(&self.calls),
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
    assert_eq!(book.info().checked_at, cache.checked_at);
    // Ctrl+R — менеджеры спрашиваются, строки — их ответы.
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    core.pump(Duration::from_millis(1));
    assert!(book.info().busy());
    pump_until(&mut core, "refreshed", |c| {
        shown(c).is_some_and(|n| n == ["Firefox", "jq", "VLC"])
    });
    pump_until(&mut core, "checked", |_| done(&book));
    assert!(book.info().checked_at > cache.checked_at);
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
    let _ = core.take_ui_events();
    core.dispatch(Command::parse("pkg.filter name=search").unwrap());
    let toasts: Vec<String> = core
        .take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(toasts, ["Not available yet: pkg.filter"]);
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
