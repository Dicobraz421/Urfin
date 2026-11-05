//! Баннер фоновой проверки и расписание через Core (F02-11): итог `check.json` при
//! старте, «Показать» / «Позже», `pkg.check_now`, звонок от `--pkg-check`,
//! `pkg.schedule` на подставном Планировщике.

use std::sync::Mutex;

use urfin_core::pkg::{Notice, PkgModel, Task};

use super::*;
use crate::SearchIn;
use crate::check::{CHECK_FILE, CheckReport, MgrStatus};
use crate::schedule::Tasks;

/// Подставной Планировщик: XML задачи в памяти.
#[derive(Default)]
struct FakeTasks(Mutex<Option<String>>);

impl Tasks for FakeTasks {
    fn create(&self, xml: &str) -> Result<(), PkgError> {
        *self.0.lock().unwrap() = Some(xml.to_owned());
        Ok(())
    }
    fn delete(&self) -> Result<(), PkgError> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
    fn query(&self) -> Result<Option<String>, PkgError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

fn setup_with(
    source: Arc<dyn PkgSource>,
    dir: &Path,
    tasks: Arc<dyn Tasks>,
    bell: Arc<crate::CheckBell>,
) -> Core {
    let book = PkgBook::new(&PkgConfig::default());
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(PkgVfs::new(Arc::clone(&book))));
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::new(reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let start = Location::local(std::env::temp_dir());
    let mut core = Core::new(Boot::new(start.clone(), start), services, Arc::new(NoWake));
    core.register(Box::new(PkgHandler::with_tasks(
        PkgConfig::default(),
        book,
        source,
        Some(dir.to_path_buf()),
        tasks,
        bell,
    )));
    core.pump(Duration::from_millis(5));
    core
}

/// Core с каталогом настроек: `app.reload_config` читает настоящий файл.
fn setup_paths(
    source: Arc<dyn PkgSource>,
    dir: &Path,
    paths: urfin_core::Paths,
) -> (Core, Arc<PkgBook>) {
    let book = PkgBook::new(&PkgConfig::default());
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(PkgVfs::new(Arc::clone(&book))));
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::new(reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let start = Location::local(std::env::temp_dir());
    let mut start_up = Boot::new(start.clone(), start);
    start_up.paths = paths;
    let mut core = Core::new(start_up, services, Arc::new(NoWake));
    core.register(Box::new(PkgHandler::with(
        PkgConfig::default(),
        Arc::clone(&book),
        source,
        Some(dir.to_path_buf()),
    )));
    core.pump(Duration::from_millis(5));
    (core, book)
}

fn notice(core: &Core) -> Option<Notice> {
    urfin_core::pkg::notice(core.state()).cloned()
}

fn task(core: &Core) -> Task {
    core.state().ext.get::<PkgModel>().unwrap().task.clone()
}

fn toasts(core: &mut Core) -> Vec<String> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            urfin_core::UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

fn report(at: &str, updates: usize) -> CheckReport {
    let ups: Vec<Update> = (0..updates)
        .map(|i| Update {
            pkg: pkg(&format!("winget:App.N{i}"), &format!("App {i}"), "1.0"),
            available: "2.0".into(),
            pinned: false,
        })
        .collect();
    CheckReport {
        checked_at: at.parse().unwrap(),
        managers: vec![MgrStatus {
            mgr: Mgr::Winget,
            ok: true,
            error: None,
        }],
        important: ups.iter().take(1).map(|u| u.pkg.id.clone()).collect(),
        updates: ups,
        dismissed_at: None,
        manual: false,
    }
}

fn no_managers() -> Arc<dyn PkgSource> {
    Arc::new(Source::default())
}

/// Итог прошлой проверки — баннер после старта (не в первом pump); «Позже» скрывает
/// его до новой проверки и при следующем запуске.
#[test]
fn banner_from_report_and_later() {
    let dir = tempfile::tempdir().unwrap();
    let r = report("2026-11-04T09:00:00Z", 7);
    r.save(&dir.path().join(CHECK_FILE)).unwrap();
    let mut core = setup_with(
        no_managers(),
        dir.path(),
        Arc::new(FakeTasks::default()),
        Arc::default(),
    );
    assert_eq!(notice(&core), None, "не в первом pump");
    pump_until(&mut core, "notice", |c| notice(c).is_some());
    assert_eq!(
        notice(&core),
        Some(Notice {
            updates: 7,
            important: 1,
            checked_at: ns(r.checked_at),
            scheduled: true,
        })
    );
    core.dispatch(Command::parse("pkg.banner.later").unwrap());
    assert_eq!(notice(&core), None);
    pump_until(&mut core, "dismissed", |_| {
        CheckReport::load(&dir.path().join(CHECK_FILE)).is_some_and(|r| r.dismissed_at.is_some())
    });
    // Следующий запуск: баннера нет.
    let mut core = setup_with(
        no_managers(),
        dir.path(),
        Arc::new(FakeTasks::default()),
        Arc::default(),
    );
    let t0 = Instant::now();
    while t0.elapsed() < crate::handler::check::STARTUP_DELAY * 2 {
        core.pump(Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(notice(&core), None);
}

/// «Показать» — вкладка `pkg:updates`, баннер скрыт.
#[test]
fn banner_show_opens_updates() {
    let dir = tempfile::tempdir().unwrap();
    report("2026-11-04T09:00:00Z", 2)
        .save(&dir.path().join(CHECK_FILE))
        .unwrap();
    let mut core = setup_with(
        no_managers(),
        dir.path(),
        Arc::new(FakeTasks::default()),
        Arc::default(),
    );
    pump_until(&mut core, "notice", |c| notice(c).is_some());
    core.dispatch(Command::parse("pkg.banner.show").unwrap());
    assert_eq!(notice(&core), None);
    pump_until(&mut core, "updates tab", |c| {
        c.state().active_tab().loc == location(Filter::Updates)
    });
}

/// «Проверить сейчас»: подставные менеджеры, итог — баннер «не по расписанию»; звонок
/// от `--pkg-check` — новый итог из файла.
#[test]
fn check_now_and_bell() {
    let dir = tempfile::tempdir().unwrap();
    let bell = Arc::new(crate::CheckBell::default());
    let src = Arc::new(Source {
        managers: winget_scoop(),
        ..Source::default()
    });
    let mut core = setup_with(
        src,
        dir.path(),
        Arc::new(FakeTasks::default()),
        Arc::clone(&bell),
    );
    core.dispatch(Command::parse("pkg.check_now").unwrap());
    pump_until(&mut core, "checked", |c| notice(c).is_some());
    let n = notice(&core).unwrap();
    assert_eq!((n.updates, n.important, n.scheduled), (1, 0, false));
    assert!(dir.path().join(CHECK_FILE).is_file());
    assert!(dir.path().join(CACHE_FILE).is_file());
    // Проверка по расписанию закончилась, пока Урфин открыт.
    let r = report("2030-01-01T09:00:00Z", 3);
    r.save(&dir.path().join(CHECK_FILE)).unwrap();
    bell.ring();
    pump_until(&mut core, "bell", |c| {
        notice(c).is_some_and(|n| n.checked_at == ns(r.checked_at))
    });
    assert_eq!(notice(&core).unwrap().updates, 3);
}

/// Тосты, пока не придёт `want` (или таймаут).
fn wait_toast(core: &mut Core, want: &str) -> Vec<String> {
    let t0 = Instant::now();
    let mut all = Vec::new();
    while !all.iter().any(|t: &String| t == want) {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "timeout: {want}; {all:?}"
        );
        core.pump(Duration::from_millis(2));
        all.extend(toasts(core));
        std::thread::sleep(Duration::from_millis(1));
    }
    all
}

/// Без обновлений — тост, баннера нет; занятый замок — «уже идёт».
#[test]
fn check_now_without_updates_or_busy() {
    let dir = tempfile::tempdir().unwrap();
    let src = Arc::new(Source {
        managers: vec![(
            Mgr::Scoop,
            Answer::Ok(vec![pkg("scoop:main/jq", "jq", "1.8.1")], Vec::new()),
            Duration::from_millis(5),
        )],
        ..Source::default()
    });
    let mut core = setup_with(
        src,
        dir.path(),
        Arc::new(FakeTasks::default()),
        Arc::default(),
    );
    core.dispatch(Command::parse("pkg.check_now").unwrap());
    wait_toast(&mut core, "No updates available");
    assert_eq!(notice(&core), None);
    let _held = crate::check::CheckLock::try_take(dir.path())
        .unwrap()
        .unwrap();
    core.dispatch(Command::parse("pkg.check_now").unwrap());
    wait_toast(&mut core, "An update check is already running");
}

/// `pkg.schedule`: задача с этим `urfin.exe` и временем, `[pkg.schedule]` в состоянии;
/// выключатель удаляет задачу; без `on` — сверка; плохое время — ошибка аргумента.
#[test]
fn schedule_creates_and_deletes_task() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = Arc::new(FakeTasks::default());
    let mut core = setup_with(no_managers(), dir.path(), tasks.clone(), Arc::default());
    assert_eq!(task(&core), Task::Unknown);
    core.dispatch(Command::parse("pkg.schedule on=true at=\"09:30\"").unwrap());
    wait_toast(&mut core, "Scheduled update check: daily at 09:30");
    assert_eq!(task(&core), Task::Ready);
    let xml = tasks.0.lock().unwrap().clone().unwrap();
    assert!(xml.contains("T09:30:00"), "{xml}");
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        crate::schedule::command_of(&xml).as_deref(),
        Some(exe.display().to_string().as_str())
    );
    let st = core.state().ext.get::<crate::PkgState>().unwrap();
    assert!(st.config.schedule.enabled);
    assert_eq!(st.config.schedule.at, "09:30");
    // Перенесённая копия: задача указывает на другой путь.
    *tasks.0.lock().unwrap() =
        crate::schedule::task_xml(Path::new(r"E:\old\urfin.exe"), "09:30", None);
    core.dispatch(Command::parse("pkg.schedule").unwrap());
    pump_until(&mut core, "verify", |c| {
        matches!(task(c), Task::OtherPath(_))
    });
    core.dispatch(Command::parse("pkg.schedule on=false").unwrap());
    wait_toast(&mut core, "Scheduled update check is off");
    assert_eq!(task(&core), Task::Absent);
    assert!(tasks.0.lock().unwrap().is_none());
    let st = core.state().ext.get::<crate::PkgState>().unwrap();
    assert!(!st.config.schedule.enabled);
    // Плохое время: задача не трогается.
    core.dispatch(Command::parse("pkg.schedule on=true at=\"25:00\"").unwrap());
    core.pump(Duration::from_millis(5));
    assert!(tasks.0.lock().unwrap().is_none());
}

/// Планировщик недоступен — состояние «ошибка» и тост.
#[test]
fn schedule_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = setup_with(
        no_managers(),
        dir.path(),
        Arc::new(crate::schedule::NoTasks),
        Arc::default(),
    );
    core.dispatch(Command::parse("pkg.schedule on=true").unwrap());
    let mut all = Vec::new();
    pump_until(&mut core, "failed", |c| matches!(task(c), Task::Failed(_)));
    all.extend(toasts(&mut core));
    assert!(
        all.iter().any(|t| t.starts_with("Task Scheduler: ")),
        "{all:?}"
    );
}

fn config(core: &Core) -> PkgConfig {
    core.state().ext.get::<PkgModel>().unwrap().config.clone()
}

/// Сверка при входе в раздел настроек: кэш и найденные менеджеры с версиями — в модель,
/// к менеджерам запросов нет; задача — на подставном Планировщике.
#[test]
fn survey_fills_settings() {
    let dir = tempfile::tempdir().unwrap();
    let src = Arc::new(Source {
        managers: winget_scoop(),
        ..Source::default()
    });
    let tasks = Arc::new(FakeTasks::default());
    let mut core = setup_with(src.clone(), dir.path(), tasks, Arc::default());
    let book = Arc::clone(&core.state().ext.get::<crate::PkgState>().unwrap().book);
    core.dispatch(Command::parse("pkg.schedule").unwrap());
    pump_until(&mut core, "task", |c| task(c) == Task::Absent);
    pump_until(&mut core, "managers", |_| book.info().managers.len() == 2);
    assert_eq!(book.info().managers[0].version.as_deref(), Some("1.0"));
    assert!(book.info().loaded);
    assert_eq!(
        src.calls.load(Ordering::SeqCst),
        0,
        "без запросов к менеджерам"
    );
}

/// `pkg.important.toggle`: по `id` и со строки под курсором; «Обновления» — важные
/// сверху, число в сводке, конфиг — в файле после отложенной записи.
#[test]
fn important_toggle_applies_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let paths = urfin_core::Paths::in_dir(dir.path());
    let src = Arc::new(Source {
        managers: winget_scoop(),
        ..Source::default()
    });
    let (mut core, book) = setup_paths(src, dir.path(), paths.clone());
    core.dispatch(Command::parse("pkg.open filter=updates").unwrap());
    pump_until(&mut core, "checked", |_| done(&book));
    pump_until(&mut core, "rows", |c| {
        shown(c).is_some_and(|n| n == ["Firefox"])
    });
    assert_eq!(book.info().important, 0);
    core.dispatch(Command::parse("panel.cursor.to name=Firefox").unwrap());
    core.pump(Duration::from_millis(5));
    core.dispatch(Command::parse("pkg.important.toggle").unwrap());
    core.pump(Duration::from_millis(5));
    assert_eq!(book.info().important, 1);
    let id: PkgId = "winget:Mozilla.Firefox".parse().unwrap();
    assert_eq!(config(&core).important, std::slice::from_ref(&id));
    // Чужой пакет — по `id`; конфиг после записи читается обратно.
    core.dispatch(Command::new("pkg.important.toggle").arg("id", "scoop:main/jq"));
    core.pump(Duration::from_millis(5));
    assert_eq!(config(&core).important.len(), 2);
    core.dispatch(Command::new("app.reload_config"));
    pump_until(&mut core, "file", |c| {
        c.state()
            .config
            .section::<PkgConfig>("pkg")
            .0
            .important
            .len()
            == 2
    });
    core.pump(Duration::from_millis(5));
    core.dispatch(Command::new("pkg.important.toggle").arg("id", id.to_string()));
    core.pump(Duration::from_millis(5));
    assert_eq!(book.info().important, 0);
    assert_eq!(config(&core).important.len(), 1);
    // Плохой id — ошибка аргумента, список цел.
    core.dispatch(Command::new("pkg.important.toggle").arg("id", "apt:vim"));
    core.pump(Duration::from_millis(5));
    assert_eq!(config(&core).important.len(), 1);
}

/// Порядок из настроек (`app.reload_config`): модель, книга и открытые результаты.
#[test]
fn reloaded_config_applies_live() {
    let dir = tempfile::tempdir().unwrap();
    let paths = urfin_core::Paths::in_dir(dir.path());
    let src = Arc::new(Source {
        managers: winget_scoop(),
        ..Source::default()
    });
    let (mut core, book) = setup_paths(src, dir.path(), paths.clone());
    core.pump(Duration::from_millis(5));
    assert_eq!(config(&core).order, Mgr::ALL);
    std::fs::write(
        paths.config_file(),
        "[pkg]\norder = [\"scoop\", \"winget\"]\nsearch_in = \"scoop\"\n",
    )
    .unwrap();
    core.dispatch(Command::new("app.reload_config"));
    pump_until(&mut core, "order", |c| config(c).order[0] == Mgr::Scoop);
    assert_eq!(book.view().search.info.scope, SearchIn::Scoop);
}
