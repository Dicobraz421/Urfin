//! Urfin — бинарь `urfin`.
//!
//! Старт (S1 §2.9, S1-13): каталоги → конфиг → логи → обработчик паник → один
//! экземпляр (S1-15) → сессия → Core → окно GPUI (ADR-0018).
//!
//! Аргументы без `--` — пути: каждый открывается новым табом; если Urfin уже
//! запущен, пути уходят ему, а этот процесс выходит. Флаги — таблица `flags::FLAGS`.

// Release — оконное приложение без консоли; debug — с консолью для логов.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bench_sftp;
mod boot;
mod demo_tree;
mod flags;
mod gpui_app;
mod ipc;
mod sandbox;
mod sys;

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use urfin_core::fake::{FakeListing, FakeTree};
use urfin_core::{
    Boot, Command, Config, ConfigWarning, Core, Level, Location, Paths, Services, Session, UiEvent,
    Waker,
};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

/// Версия приложения из `Cargo.toml`.
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn main() -> ExitCode {
    // Досчёт с правами через UAC (F01-12) и помощник `ssh.exe` (F03 §2.7) — раньше
    // всего остального.
    if let Some(code) = boot::du_elevated() {
        return code;
    }
    if let Some(code) = boot::askpass() {
        return code;
    }
    // Проверка обновлений по расписанию (F02 §4): без окна и единственного экземпляра.
    if let Some(code) = boot::pkg_check() {
        return code;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let text = format!("Urfin {} failed to start: {e:#}", version());
            eprintln!("{text}");
            tracing::error!(target: "urfin::startup", "{e:#}");
            sys::event_log(sys::EventId::StartupFailed, &text);
            ExitCode::FAILURE
        }
    }
}

/// Длительности этапов старта (имена — контракт S0 v5 №10). Отсчёт — от начала
/// `main`; от создания процесса (`GetProcessTimes`) — S9.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Startup {
    pub(crate) process: Instant,
    paths: Duration,
    config: Duration,
    tracing: Duration,
    session: Duration,
    pub(crate) core: Duration,
    pub(crate) window: Duration,
    pub(crate) first_frame: Duration,
    pub(crate) bench: bool,
}

impl Startup {
    pub(crate) fn json(&self) -> String {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        format!(
            "{{\"startup.paths\":{:.3},\"startup.config\":{:.3},\"startup.tracing\":{:.3},\
             \"startup.session\":{:.3},\"startup.core\":{:.3},\"startup.window\":{:.3},\
             \"startup.first_frame\":{:.3}}}",
            ms(self.paths),
            ms(self.config),
            ms(self.tracing),
            ms(self.session),
            ms(self.core),
            ms(self.window),
            ms(self.first_frame),
        )
    }
}

fn run() -> anyhow::Result<()> {
    let process = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = flags::parse(&args, flags::FLAGS);
    let flag = |f: &str| cli.has(f);

    let screens = cli.value("--screens").map(PathBuf::from);

    // До подписчика tracing спаны не пишутся: длительность меряем сами.
    let t = Instant::now();
    // Песочница объявлена до логов: удаляется после того, как лог закрыт.
    let (_sandbox, paths) = app_paths(screens.is_some())?;
    paths.ensure().context("cannot create app directories")?;
    let t_paths = t.elapsed();
    let t = Instant::now();
    if let Err(e) = Config::write_default_if_missing(&paths) {
        eprintln!("cannot write default config: {e}");
    }
    let (config, warnings) = Config::load(&paths);
    let t_config = t.elapsed();
    // `[ui] renderer = "warp"` — программный Direct3D (WARP): переменная для патча gpui,
    // до логов и Runtime — пока других потоков нет (S0-decisions №50, ADR-0018).
    let renderer = config
        .section::<urfin_ui::config::UiCfg>("ui")
        .0
        .renderer
        .trim()
        .to_ascii_lowercase();
    if renderer == "warp" {
        sys::force_warp();
    }
    let t = Instant::now();
    let _log = boot::init_tracing(&paths, &config.log)?;
    boot::install_panic_hook(&paths);
    let t_tracing = t.elapsed();
    tracing::info!(target: "startup.paths", elapsed = ?t_paths, portable = paths.portable);
    tracing::info!(target: "startup.config", elapsed = ?t_config, warnings = warnings.len());
    {
        let _s = tracing::info_span!("startup.i18n").entered();
        // Предупреждения (битый .ftl, неизвестный язык) i18n пишет в лог сам.
        urfin_i18n::init(&config.general.language, Some(&paths.config_dir));
    }
    tracing::info!(version = version(), ?args, "starting");

    // Только debug: проверка записи ошибки в журнал Windows.
    if flag("--error-test") {
        tracing::error!(test = "error-test", "error test");
        return Ok(());
    }
    // Только debug: паника не в UI-потоке — ошибка в логе, не сбой (S1 §1).
    if flag("--thread-panic-test") {
        let joined = std::thread::Builder::new()
            .name("panic-test".into())
            .spawn(|| panic!("thread panic test"))?
            .join();
        anyhow::ensure!(joined.is_err(), "thread did not panic");
        return Ok(());
    }
    // Только debug: проверка crash-файла и баннера (S1-12).
    assert!(!flag("--crash-test"), "crash test");

    // Один экземпляр (S1-15, решение владельца №3): второй запуск отдаёт пути
    // первому и выходит. Smoke, замер старта и самопроверка — всегда отдельно.
    let request = ipc::Request::new(cli.paths.clone());
    let bench = bench_dir(&cli);
    let separate = separate_run(&cli) || bench.is_some() || screens.is_some();
    let ControlFlow::Continue(inbox) = single_instance(&paths, &request, separate) else {
        return Ok(());
    };
    if flag("--ipc-test-server") {
        return ipc_test_server(inbox);
    }

    let crashed = boot::take_crash_marker(&paths);
    if flag("--smoke") {
        println!("urfin {}", version());
        return smoke(paths, config, warnings, crashed);
    }

    let (session, t_session) = load_session(&paths, screens.is_some());
    // Без сессии обе панели — в домашнем каталоге.
    let home = home_dir();
    let mut boot = Boot::new(home.clone(), home);
    boot.paths = paths;
    boot.config = config;
    boot.warnings = warnings;
    boot.session = session;
    boot.crashed_last_time = crashed;
    let startup = Startup {
        process,
        paths: t_paths,
        config: t_config,
        tracing: t_tracing,
        session: t_session,
        core: Duration::ZERO,
        window: Duration::ZERO,
        first_frame: Duration::ZERO,
        bench: flag("--bench-startup"),
    };
    let bench_editor = cli
        .value("--bench-editor")
        .map(|file| bench_editor_open(&mut boot.config, PathBuf::from(file)))
        .or_else(|| cli.value("--bench-viewer").map(bench_viewer_open))
        .or_else(|| flag("--bench-search").then(|| bench_search_open(&cli)));
    gpui_app::run(gpui_app::Launch {
        boot,
        startup,
        ipc: inbox,
        open: request.commands(),
        bench_editor,
        bench,
        screens,
    });
    Ok(())
}

/// Каталоги приложения. `--screens` — песочница во временном каталоге (конфиг,
/// состояние, логи, кэш; удаляется в конце): профиль владельца снимки не читают и не
/// пишут, сцены идут с умолчаниями конфига.
fn app_paths(screens: bool) -> anyhow::Result<(Option<sandbox::Sandbox>, Paths)> {
    if !screens {
        return Ok((None, Paths::detect()));
    }
    let sb = sandbox::Sandbox::create().context("cannot create screens sandbox")?;
    let paths = sb.paths();
    Ok((Some(sb), paths))
}

/// Режимы отдельного экземпляра: пути не уходят запущенному.
fn separate_run(cli: &flags::Cli) -> bool {
    [
        "--new-instance",
        "--smoke",
        "--bench-startup",
        "--bench-editor",
        "--bench-viewer",
        "--bench-search",
    ]
    .into_iter()
    .any(|f| cli.has(f))
}

/// `--bench [каталог]` (G-07): по умолчанию — датасет `gen-100000-s1` в
/// `%LOCALAPPDATA%\Pets\testdata\urfin` (`cargo run -p urfin-testdata --example gen_tree --release`).
fn bench_dir(cli: &flags::Cli) -> Option<PathBuf> {
    if cli.has("--bench-sftp") {
        bench_sftp::enable();
        return Some(PathBuf::new());
    }
    if !cli.has("--bench") {
        return None;
    }
    cli.value("--bench").map(PathBuf::from).or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join(r"Pets\testdata\urfin\gen-100000-s1"))
    })
}

/// `--bench-editor <файл>` (S7-16, спайк размера): лимит редактора снят в памяти, фича
/// редактора меряет и выходит; команда открытия файла.
fn bench_editor_open(config: &mut Config, file: PathBuf) -> Command {
    urfin_ui::bench::editor_mode(config);
    let file = std::path::absolute(&file).unwrap_or(file);
    Command::new("editor.open").arg("path", Location::local(file))
}

/// `--bench-search [строк]` (S8-07): фича экрана меряет кадр на фикстуре и выходит.
fn bench_search_open(cli: &flags::Cli) -> Command {
    urfin_ui::bench::SEARCH.store(true, std::sync::atomic::Ordering::Relaxed);
    let rows = cli
        .value("--bench-search")
        .and_then(|r| r.parse::<i64>().ok());
    Command::new("screens.search").arg("rows", rows.unwrap_or(1_000_000))
}

/// `--bench-viewer <файл>` (S7-20): фича вьювера меряет и выходит; команда открытия файла.
fn bench_viewer_open(file: &str) -> Command {
    urfin_ui::bench::VIEWER.store(true, std::sync::atomic::Ordering::Relaxed);
    let file = std::path::absolute(file).unwrap_or_else(|_| file.into());
    Command::new("viewer.open").arg("path", Location::local(file))
}

/// Сессия и время её чтения. Снимки — с чистого листа: табы и пути из прошлых
/// прогонов не нужны.
fn load_session(paths: &Paths, screens: bool) -> (Option<Session>, Duration) {
    let t = Instant::now();
    let _s = tracing::info_span!("startup.session").entered();
    let session = (!screens).then(|| Session::load(paths)).flatten();
    (session, t.elapsed())
}

fn single_instance(
    paths: &Paths,
    request: &ipc::Request,
    separate: bool,
) -> ControlFlow<(), Option<ipc::Inbox>> {
    if separate {
        return ControlFlow::Continue(None);
    }
    match ipc::start(paths, request) {
        Ok(ipc::Start::Primary(inbox)) => ControlFlow::Continue(Some(inbox)),
        Ok(ipc::Start::Forwarded(took)) => {
            tracing::info!(?took, argv = ?request.argv, "handed over to the running instance");
            ControlFlow::Break(())
        }
        Ok(ipc::Start::Standalone) => ControlFlow::Continue(None),
        Err(e) => {
            tracing::warn!(error = %e, "single-instance unavailable, starting standalone");
            ControlFlow::Continue(None)
        }
    }
}

/// Только debug: первый экземпляр без окна печатает `ready` и один принятый
/// запрос (тест S1-15).
fn ipc_test_server(inbox: Option<ipc::Inbox>) -> anyhow::Result<()> {
    let inbox = inbox.context("--ipc-test-server: another instance is running")?;
    println!("ready");
    std::io::Write::flush(&mut std::io::stdout())?;
    if let Some(r) = inbox.recv() {
        println!("{}", serde_json::to_string(&r)?);
    }
    Ok(())
}

/// Домашний каталог (или корень системного диска, если ОС его не знает).
fn home_dir() -> Location {
    Location::local(
        std::env::home_dir()
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" })),
    )
}

/// Листинг настоящей ФС (S4): `VfsListing` с секцией `[vfs]` конфига.
pub(crate) fn vfs_cfg(config: &Config) -> VfsCfg {
    let (cfg, warning) = config.section::<VfsCfg>("vfs");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[vfs] config section ignored");
    }
    cfg
}

/// Открытые анализы места (F-01): один реестр на процесс — у листинга вкладок `du:`,
/// handler `du`, вьювера и операций.
fn du_registry() -> Arc<urfin_vfs::du::DuRegistry> {
    static REG: std::sync::OnceLock<Arc<urfin_vfs::du::DuRegistry>> = std::sync::OnceLock::new();
    Arc::clone(REG.get_or_init(urfin_vfs::du::DuRegistry::new))
}

/// Backend `sftp` (F03-04): один на процесс — у листинга вкладок, handler'ов и
/// подключений (`NetState`); каналы ему отдаёт подключение (`sftp.open`).
fn sftp_fs() -> Arc<urfin_net::SftpFs> {
    static FS: std::sync::OnceLock<Arc<urfin_net::SftpFs>> = std::sync::OnceLock::new();
    Arc::clone(FS.get_or_init(|| Arc::new(urfin_net::SftpFs::new())))
}

/// `--screens`: backend `sftp` — папка сцен из памяти (F03-06), без сети.
static DEMO_SFTP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Модель вкладки пакетов `pkg:` (F02-08): одна на процесс — у листинга вкладок и
/// handler `pkg`.
fn pkg_book() -> Arc<urfin_pkg::PkgBook> {
    static BOOK: std::sync::OnceLock<Arc<urfin_pkg::PkgBook>> = std::sync::OnceLock::new();
    Arc::clone(BOOK.get_or_init(|| urfin_pkg::PkgBook::new(&urfin_pkg::PkgConfig::default())))
}

/// Звонок «проверка по расписанию закончилась» (F02-11): его звонит приём IPC, слушает
/// handler `pkg`.
pub(crate) fn pkg_bell() -> Arc<urfin_pkg::CheckBell> {
    static BELL: std::sync::OnceLock<Arc<urfin_pkg::CheckBell>> = std::sync::OnceLock::new();
    Arc::clone(BELL.get_or_init(Arc::default))
}

/// Backend'ы VFS: локальная ФС, вкладка анализа места `du:`, `sftp://` и пакеты `pkg:`.
fn vfs_registry() -> VfsRegistry {
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(urfin_vfs::du::DuVfs::new(du_registry())));
    if bench_sftp::on() {
        reg.register(Arc::new(bench_sftp::BenchFs));
    } else if DEMO_SFTP.load(std::sync::atomic::Ordering::Relaxed) {
        reg.register(Arc::new(urfin_net::sftp::demo::DemoFs::new()));
    } else {
        reg.register(sftp_fs());
    }
    reg.register(Arc::new(urfin_pkg::PkgVfs::new(pkg_book())));
    reg
}

/// Кэш паролей SSH (F03 §2.5): Диспетчер учётных данных с политикой `password_cache`;
/// без каталога состояния (`--smoke`, `--screens`) — без кэша.
fn ssh_vault(
    policies: &urfin_core::PolicySet,
    ssh: &urfin_net::SshCfg,
    paths: Option<&Paths>,
) -> Option<Arc<urfin_net::vault::Vault>> {
    paths?;
    #[cfg(windows)]
    {
        Some(Arc::new(urfin_net::vault::Vault::new(
            policies,
            ssh,
            Box::new(urfin_net::vault::WinCredStore),
        )))
    }
    #[cfg(not(windows))]
    {
        let _ = (policies, ssh);
        None
    }
}

/// Сервисы VFS; `screens` — сцены `--screens`: `sftp://srv1/…` из памяти (и у реестра
/// handler'ов, он строится позже).
pub(crate) fn vfs_services(config: &Config, screens: bool) -> Services {
    if screens {
        DEMO_SFTP.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Services {
        listing: Arc::new(VfsListing::new(Arc::new(vfs_registry()), vfs_cfg(config))),
        // Enter по файлу (S6-19): последнее звено — `term.open`. Строки без файла во
        // вкладке анализа места (группа «Типов») — первыми (F01-08).
        open_resolvers: vec![urfin_vfs::du::open_resolver(), urfin_term::open_resolver()],
    }
}

/// Handlers сессий поверх core: ui, vfs, du, ops, term, ssh/sftp, pkg (keymap регистрируется
/// последним — ему нужен полный реестр команд).
pub(crate) fn register_handlers(
    core: &mut Core,
    config: &Config,
    paths: Option<&Paths>,
    ui_env: urfin_ui::ThemeEnv,
) {
    let policies = ui_env.policies.clone();
    urfin_ui::register_with(core, ui_env);
    let vfs = Arc::new(vfs_registry());
    urfin_vfs::register(core, vfs.clone(), vfs_cfg(config));
    // Поиск (S8): история запросов — в каталоге состояния; без него — только в памяти.
    urfin_search::register(core, vfs.clone(), paths.map(|p| p.state_dir.clone()));
    let (du, warning) = config.section::<urfin_vfs::du::DuCfg>("du");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[du] config section ignored");
    }
    // Кэш снимков — в `state\du\` (без каталога состояния — без кэша); досчёт с правами
    // (F01-12) — ответы в `state\du\elevated\`, без каталога состояния недоступен.
    let cache = paths.map(|p| urfin_vfs::du::DuCache::new(p, &du));
    let elevation = paths.and_then(|p| urfin_ui::sys::du_elevation(&p.state_dir));
    urfin_vfs::du::register_full(core, du, du_registry(), cache, elevation);
    let (ops, warning) = config.section::<urfin_ops::OpsConfig>("ops");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[ops] config section ignored");
    }
    // Журналы job (S5-12): прерванные — баннер «Продолжить / Очистить» при старте.
    urfin_ops::register_with(core, ops, paths.map(|p| p.state_dir.join("jobs")));
    // Терминал и строка ввода (S6). С каталогом состояния — поиск shells и прогрев
    // shell по умолчанию после первого кадра; `--smoke`/`--screens` — без них.
    let (term, warning) = config.section::<urfin_term::TermConfig>("term");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[term] config section ignored");
    }
    urfin_term::register_with(core, term, paths.map(|p| p.state_dir.join("term")));
    // SSH и SFTP (F-03): без `paths` — без `connections.toml` и истории хостов.
    let (ssh, warning) = config.section::<urfin_net::SshCfg>("ssh");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[ssh] config section ignored");
    }
    let vault = ssh_vault(&policies, &ssh, paths);
    urfin_net::register_shared(core, ssh, paths, sftp_fs(), vault);
    // Менеджеры пакетов (F02): без фоновых запросов при старте; кэш вкладки `pkg:` и
    // найденные менеджеры — в `state\pkg\`.
    let (pkg, warning) = config.section::<urfin_pkg::PkgConfig>("pkg");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[pkg] config section ignored");
    }
    urfin_pkg::register_with(
        core,
        pkg,
        pkg_book(),
        paths.map(|p| p.state_dir.join("pkg")),
        pkg_bell(),
    );
    // Вьювер и редактор (S7): тот же реестр VFS; Spool — в `<cache_dir>/view-spool`.
    urfin_view::register_from(
        core,
        config,
        urfin_view::ViewEnv {
            vfs,
            cache_dir: paths.map(|p| p.cache_dir.clone()),
            // `--screens` (без каталогов): копии файлов сервера `screen-sftpedit` — в
            // каталоге прогона, он удаляется в конце.
            temp: DEMO_SFTP
                .load(std::sync::atomic::Ordering::Relaxed)
                .then(sandbox::run_temp),
        },
    );
}

/// Корень демо-дерева `FakeListing` для `--smoke`: детерминированный прогон без ФС.
fn demo_root() -> Location {
    Location::local(if cfg!(windows) {
        r"C:\FakeListing"
    } else {
        "/FakeListing"
    })
}

/// Генерируемое дерево для `--smoke`, чанки как у S4.
fn demo_services() -> Services {
    Services {
        listing: Arc::new(FakeListing::new(2048).with_tree(FakeTree {
            dirs: 12,
            files: 300,
            max_depth: 6,
        })),
        open_resolvers: Vec::new(),
    }
}

/// Ничего не будит: в `--smoke` pump качается вручную.
struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Команды `--smoke`: навигация, табы, сортировка, выделение, refresh. Enter —
/// только на каталог (строка 2 — первый каталог: `..`, каталоги первыми);
/// открытие файлов — S5.
const SMOKE_SCRIPT: [&str; 26] = [
    "panel.cursor.home",
    "panel.cursor.down n=2",
    "panel.enter",
    "panel.cursor.end",
    "sel.toggle",
    "panel.sort key=size",
    "panel.up",
    "tab.new",
    "panel.switch",
    "panel.cursor.page_down",
    "sel.all",
    "panel.cursor.home",
    "panel.enter",
    "panel.back",
    "tab.close side=left",
    "panel.switch",
    "panel.sort key=name",
    "sel.none",
    "panel.hidden.toggle",
    "panel.refresh",
    "ui.qsearch text=a",
    "ui.cancel",
    "ui.jobs.toggle",
    "ui.lang lang=ru",
    "ui.split delta=1",
    "ui.lang lang=en",
];

/// Headless-прогон без окна: Core с `FakeListing`, 1000 команд, shutdown.
/// Сессию пользователя не читает и не перезаписывает (`state_dir` — временный).
/// Ошибка любой команды — провал прогона.
fn smoke(
    paths: Paths,
    config: Config,
    warnings: Vec<ConfigWarning>,
    crashed: Option<PathBuf>,
) -> anyhow::Result<()> {
    let root = demo_root();
    let mut boot = Boot::new(root.clone(), root);
    // Сессия, keymap и запись настроек (`ui.lang`, `ui.split`) прогона — в своём
    // временном каталоге; удаляется в конце. Конфиг пользователя только читается.
    let scratch = std::env::temp_dir().join(format!("urfin-smoke-{}", std::process::id()));
    boot.paths = Paths {
        state_dir: scratch.join("state"),
        config_dir: scratch.join("config"),
        ..paths
    };
    boot.config = config;
    boot.warnings = warnings;
    boot.crashed_last_time = crashed;
    let smoke_config = boot.config.clone();
    let mut core = Core::new(boot, demo_services(), Arc::new(NoWake));
    register_handlers(
        &mut core,
        &smoke_config,
        None,
        urfin_ui::ThemeEnv::default(),
    );
    let _ = urfin_keymap::register(
        &mut core,
        Paths::in_dir(scratch.join("keymap")),
        urfin_keymap::KeymapCfg::default(),
        urfin_keymap::FlagRegistry::with_builtin(),
    );
    let pump = |core: &mut Core| {
        for _ in 0..3 {
            core.pump(Duration::from_millis(5));
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    pump(&mut core);
    if let Some(b) = &core.state().banner {
        println!("banner: {}", b.text);
    }
    let mut n = 0;
    let mut errors = Vec::new();
    while n < 1000 {
        for text in SMOKE_SCRIPT {
            core.dispatch(Command::parse(text).expect("smoke script"));
            n += 1;
        }
        pump(&mut core);
        let events = core.take_ui_events();
        errors.extend(events.into_iter().filter_map(|e| match e {
            UiEvent::Toast {
                level: Level::Error | Level::Warn,
                text,
            } => Some(text),
            _ => None,
        }));
    }
    tracing::debug!(commands = n, errors = errors.len(), "smoke done");
    let _ = core.shutdown(Duration::from_millis(500));
    let _ = std::fs::remove_dir_all(&scratch);
    anyhow::ensure!(
        errors.is_empty(),
        "smoke: {} errors, first: {}",
        errors.len(),
        errors[0]
    );
    println!("smoke ok: {n} commands");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use urfin_core::{Boot, Command, Config, Core, Paths};

    #[test]
    fn version_smoke() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert_ne!(super::version(), "");
    }

    fn stamp(p: &Path) -> Option<SystemTime> {
        std::fs::metadata(p).and_then(|m| m.modified()).ok()
    }

    /// `--screens`: команды сцен, пишущие настройки (`ui.icons.set`, `ui.setting`,
    /// `ui.theme.set`), и сессия при выходе — только в песочнице; профиль ОС не
    /// меняется, песочница удаляется.
    #[test]
    fn screens_write_only_into_sandbox() {
        let real = Paths::detect();
        let watched = [real.config_file(), real.session_file()];
        let before = watched.each_ref().map(|f| stamp(f));
        let (sb, paths) = super::app_paths(true).unwrap();
        let sb = sb.expect("sandbox");
        for d in [
            &paths.config_dir,
            &paths.state_dir,
            &paths.log_dir,
            &paths.cache_dir,
        ] {
            assert!(!d.starts_with(&real.state_dir), "{}", d.display());
            assert_ne!(*d, real.config_dir);
        }
        paths.ensure().unwrap();
        Config::write_default_if_missing(&paths).unwrap();
        let (config, _) = Config::load(&paths);
        let root = super::demo_root();
        let mut boot = Boot::new(root.clone(), root);
        boot.paths = paths.clone();
        boot.config = config.clone();
        let mut core = Core::new(boot, super::demo_services(), Arc::new(super::NoWake));
        super::register_handlers(&mut core, &config, None, urfin_ui::ThemeEnv::default());
        let _ = urfin_keymap::register(
            &mut core,
            paths.clone(),
            urfin_keymap::KeymapCfg::default(),
            urfin_keymap::FlagRegistry::with_builtin(),
        );
        for c in [
            "ui.icons.set mode=mono",
            "ui.theme.set id=blue",
            "ui.setting key=ui.keybar value=false",
        ] {
            core.dispatch(Command::parse(c).unwrap());
        }
        let file = paths.config_file();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            core.pump(Duration::from_millis(5));
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            if text.contains("\"mono\"") && text.contains("\"blue\"") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = core.shutdown(Duration::from_millis(500));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("\"mono\"") && text.contains("\"blue\""),
            "{text}"
        );
        assert!(paths.session_file().is_file());
        assert_eq!(watched.each_ref().map(|f| stamp(f)), before);
        let dir = paths.config_dir.parent().unwrap().to_path_buf();
        drop(sb);
        assert!(!dir.exists());
    }
}
