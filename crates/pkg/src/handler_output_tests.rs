//! Вывод действий (F02-10) на подставном менеджере `fake-pkgmgr`: лента в сеансе
//! терминала, `pkg.output`, `pkg.info`, эвристика UAC, «нет вывода», `job.retry`, вето
//! выхода. Настоящие менеджеры не запускаются.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::time::Instant;

use compact_str::CompactString;
use urfin_core::{Boot, Core, JobId, JobState, Phase, QuitDecision, Row, Services, UiEvent, Value};
use urfin_term::TermState;
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

use super::*;
use crate::api::{ActionPlan, Hit, Mgr, MgrCaps, Package, PkgError, Update};
use crate::vfs::PkgVfs;

struct NoWake;

impl urfin_core::Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn fake(args: &[&str]) -> Vec<OsString> {
    std::iter::once(crate::testutil::fake().into_os_string())
        .chain(args.iter().map(OsString::from))
        .collect()
}

fn found() -> Detected {
    Detected {
        mgr: Mgr::Winget,
        exe: "winget.exe".into(),
        version: Some("1.0".into()),
        caps: MgrCaps::empty(),
    }
}

/// Менеджер по сценарию: у каждого действия — свои аргументы `fake-pkgmgr`.
struct Script {
    plans: Arc<Mutex<VecDeque<Vec<&'static str>>>>,
}

impl PackageManager for Script {
    fn mgr(&self) -> Mgr {
        Mgr::Winget
    }
    fn detect(&self) -> Option<Detected> {
        Some(found())
    }
    fn version(&self, _cx: &RunCx) -> Result<CompactString, PkgError> {
        Ok("1.0".into())
    }
    fn list_installed(&self, _cx: &RunCx) -> Result<Vec<Package>, PkgError> {
        Ok(vec![Package {
            id: "winget:A.A".parse().unwrap(),
            name: "A".into(),
            version: "1".into(),
            source: Some("winget".into()),
            publisher: None,
        }])
    }
    fn list_updates(&self, _cx: &RunCx) -> Result<Vec<Update>, PkgError> {
        Ok(Vec::new())
    }
    fn search(&self, _q: &str, _cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
        Ok(Vec::new())
    }
    fn info_argv(&self, _id: &PkgId) -> Result<Vec<OsString>, PkgError> {
        Ok(fake(&["--line", "Publisher: Igor Pavlov"]))
    }
    fn action(&self, _op: PkgOp, _id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
        let args = self.plans.lock().unwrap().pop_front().unwrap();
        Ok(ActionPlan {
            argv: fake(&args),
            elevate: false,
            success_codes: vec![0],
            reboot_codes: Vec::new(),
        })
    }
}

#[derive(Default)]
struct Source {
    plans: Arc<Mutex<VecDeque<Vec<&'static str>>>>,
    /// Окно UAC открыто.
    consent: AtomicBool,
}

impl PkgSource for Source {
    fn detect(&self, _cached: &Managers) -> Vec<Detected> {
        vec![found()]
    }
    fn arp(&self) -> Vec<ArpEntry> {
        Vec::new()
    }
    fn adapter(&self, _d: &Detected, _cfg: &PkgConfig) -> Arc<dyn PackageManager> {
        Arc::new(Script {
            plans: Arc::clone(&self.plans),
        })
    }
    fn uac_pending(&self, _since: SystemTime) -> bool {
        self.consent.load(Ordering::SeqCst)
    }
}

/// Core с терминалом и пакетами; менеджер найден и прочитан.
fn setup(source: Arc<Source>, config: PkgConfig) -> (Core, Arc<PkgBook>) {
    let book = PkgBook::new(&config);
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(PkgVfs::new(Arc::clone(&book))));
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::new(reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let start = Location::local(std::env::temp_dir());
    let mut core = Core::new(Boot::new(start.clone(), start), services, Arc::new(NoWake));
    urfin_term::register(&mut core, urfin_term::TermConfig::default());
    core.register(Box::new(PkgHandler::with(
        config,
        Arc::clone(&book),
        source,
        None,
    )));
    core.pump(Duration::from_millis(5));
    core.dispatch(Command::parse("pkg.open").unwrap());
    core.dispatch(Command::parse("pkg.refresh").unwrap());
    pump_until(&mut core, "checked", |_| {
        !book.view().rows(Filter::Installed).is_empty()
    });
    (core, book)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        core.pump(Duration::from_millis(5));
    }
}

fn install(core: &mut Core) {
    let targets = Value::List(vec![Value::Str("winget:A.A".into())].into());
    core.dispatch(Command::new("pkg.upgrade").arg("targets", targets));
}

fn job(core: &Core) -> JobId {
    core.state().jobs.iter().last().unwrap().id
}

fn feed_text(core: &Core) -> String {
    let st = core.state().ext.get::<TermState>().unwrap();
    st.sessions
        .slots()
        .iter()
        .find(|s| s.is_feed())
        .and_then(|s| s.shared())
        .map(|s| s.rows_text(0, 10_000))
        .unwrap_or_default()
}

fn feed_active(core: &Core) -> bool {
    let st = core.state().ext.get::<TermState>().unwrap();
    st.sessions
        .active()
        .is_some_and(urfin_term::manager::Slot::is_feed)
}

/// Просьбы к окну спрятать или показать панели.
fn panels(core: &mut Core) -> Vec<bool> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Ext { id, args } if id.as_str() == "term.panels" => args.bool("show"),
            _ => None,
        })
        .collect()
}

/// Заголовок, строки и код выхода — в ленте; Ctrl+O показывает её вместо shell и
/// возвращает панели.
#[test]
fn output_goes_to_feed() {
    let src = Arc::new(Source::default());
    src.plans
        .lock()
        .unwrap()
        .push_back(vec!["--line", "Found A [A.A]", "--line", "  41%"]);
    let (mut core, _book) = setup(Arc::clone(&src), PkgConfig::default());
    install(&mut core);
    pump_until(&mut core, "job", |c| {
        c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    let text = feed_text(&core);
    for want in [
        "▸ winget upgrade A.A",
        "Found A [A.A]",
        "  41%",
        "exit code 0",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    assert!(!feed_active(&core), "lane is not shown by itself");
    panels(&mut core);
    core.dispatch(Command::parse("pkg.output").unwrap());
    assert!(feed_active(&core));
    core.pump(Duration::from_millis(5));
    // Только «спрятать»: `refocus` этого же прохода вернул бы панели.
    let events: Vec<_> = core
        .take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Ext { id, args } if id.as_str() == "term.panels" => {
                Some((args.bool("show"), args.bool("refocus")))
            }
            _ => None,
        })
        .collect();
    assert_eq!(events, [(Some(false), None)]);
    core.dispatch(Command::parse("pkg.output").unwrap());
    assert_eq!(panels(&mut core), [true]);
}

/// F3: сведения о пакете строки под курсором — в ленте, она на экране.
#[test]
fn output_info_in_feed() {
    let src = Arc::new(Source::default());
    let (mut core, _book) = setup(src, PkgConfig::default());
    pump_until(&mut core, "row", |c| {
        c.state()
            .active_tab()
            .current()
            .is_some_and(|r| matches!(r, Row::Entry { .. }))
    });
    core.dispatch(Command::parse("pkg.info").unwrap());
    assert!(feed_active(&core));
    pump_until(&mut core, "info", |c| {
        feed_text(c).contains("Publisher: Igor Pavlov")
    });
    pump_until(&mut core, "code", |c| {
        let t = feed_text(c);
        t.contains("▸ winget info A.A") && t.contains("exit code 0")
    });
}

/// Менеджер молчит, окно UAC открыто: подпись с «возможно»; вывод её снимает.
#[test]
fn output_uac_heuristic() {
    let src = Arc::new(Source::default());
    src.consent.store(true, Ordering::SeqCst);
    src.plans.lock().unwrap().push_back(vec![
        "--delay-ms",
        "2500",
        "--line",
        "Installed",
        "--sleep-ms",
        "800",
    ]);
    let (mut core, book) = setup(Arc::clone(&src), PkgConfig::default());
    let id: PkgId = "winget:A.A".parse().unwrap();
    install(&mut core);
    pump_until(&mut core, "maybe", |_| {
        book.view().busy_of(&id).is_some_and(|b| b.maybe_uac)
    });
    let label = urfin_i18n::current().get("pkg-busy-uac-maybe");
    let current = |c: &Core| {
        c.state()
            .jobs
            .iter()
            .last()
            .unwrap()
            .progress
            .snapshot()
            .current
    };
    pump_until(&mut core, "job label", |c| {
        current(c).is_some_and(|t| *t == *label)
    });
    pump_until(&mut core, "output clears", |c| {
        book.view().busy_of(&id).is_some_and(|b| !b.maybe_uac)
            && current(c).is_some_and(|t| *t == *"A.A")
    });
    pump_until(&mut core, "done", |c| {
        c.state().jobs.iter().all(|j| !j.state.is_active())
    });
}

/// Менеджер молчит дольше порога: «нет вывода» в job-баре, «Прервать» — отмена job.
#[test]
fn output_silent_abort() {
    let src = Arc::new(Source::default());
    src.plans
        .lock()
        .unwrap()
        .push_back(vec!["--sleep-ms", "20000"]);
    let mut config = PkgConfig::default();
    config.timeouts.silent_warn_min = 0;
    let (mut core, _book) = setup(Arc::clone(&src), config);
    install(&mut core);
    pump_until(&mut core, "stalled", |c| {
        c.state().jobs.iter().any(|j| {
            let p = j.progress.snapshot();
            p.phase == Phase::Stalled
                && p.current.is_some_and(|t| t.contains("no output for 0 min"))
        })
    });
    let id = job(&core);
    core.dispatch(Command::new("job.cancel").arg("id", i64::try_from(id.0).unwrap()));
    pump_until(&mut core, "cancelled", |c| {
        c.state()
            .jobs
            .get(id)
            .is_some_and(|j| j.state == JobState::Cancelled)
    });
}

/// `job.retry` запускает упавшую job заново; второй раз повторять нечего.
#[test]
fn output_job_retry() {
    let src = Arc::new(Source::default());
    {
        let mut plans = src.plans.lock().unwrap();
        plans.push_back(vec!["--line", "boom", "--exit", "1"]);
        plans.push_back(vec!["--line", "fine"]);
    }
    let (mut core, _book) = setup(Arc::clone(&src), PkgConfig::default());
    install(&mut core);
    pump_until(&mut core, "failed", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| matches!(j.state, JobState::Failed(_)))
    });
    let id = i64::try_from(job(&core).0).unwrap();
    core.dispatch(Command::new("job.retry").arg("id", id));
    pump_until(&mut core, "second", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| matches!(j.state, JobState::Done(_)))
    });
    core.take_ui_events();
    core.dispatch(Command::new("job.retry").arg("id", id));
    let toasts = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(toasts, 1, "nothing to retry");
}

/// Выход при идущей установке — вопрос «идёт установка: N»; job кончилась — вопроса нет.
#[test]
fn output_quit_veto() {
    let src = Arc::new(Source::default());
    src.plans
        .lock()
        .unwrap()
        .push_back(vec!["--sleep-ms", "20000"]);
    let (mut core, _book) = setup(Arc::clone(&src), PkgConfig::default());
    install(&mut core);
    pump_until(&mut core, "running", |c| {
        c.state().jobs.iter().any(|j| j.state.is_active())
    });
    let QuitDecision::Veto(p) = core.quit_decision() else {
        panic!("no veto");
    };
    assert_eq!(p.title, "Installing: 1");
    assert_eq!(p.choices.len(), 2);
    let id = job(&core);
    core.dispatch(Command::new("job.cancel").arg("id", i64::try_from(id.0).unwrap()));
    pump_until(&mut core, "cancelled", |c| {
        c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    assert!(matches!(core.quit_decision(), QuitDecision::Allow));
}
