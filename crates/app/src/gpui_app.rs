//! Окно (ADR-0018): Core с handlers приложения, вторые запуски (S1-15) — в его `pump`;
//! окно и цикл — [`urfin_ui::gpui_shell`].

use std::sync::Arc;
use std::time::Instant;

use urfin_core::Core;
use urfin_keymap::{FlagRegistry, KeymapCfg};
use urfin_ui::gpui_shell::{self, Signal};

use urfin_core::{Boot, Command};

use crate::Startup;

/// Всё, что окну нужно от `main`.
pub struct Launch {
    pub boot: Boot,
    pub startup: Startup,
    /// Запросы вторых запусков (S1-15); `None` — отдельный экземпляр.
    pub ipc: Option<crate::ipc::Inbox>,
    /// Пути из своей командной строки: новые табы после восстановления сессии.
    pub open: Vec<Command>,
    /// `--bench <каталог>`: сценарий замера на этом каталоге (G-07).
    pub bench: Option<std::path::PathBuf>,
    /// `--screens <каталог>`: снимки сцен (проверка дизайна).
    pub screens: Option<std::path::PathBuf>,
    /// `--bench-editor <файл>`, `--bench-viewer <файл>`: команда открытия файла замера.
    pub bench_editor: Option<Command>,
}

pub fn run(launch: Launch) {
    let Launch {
        mut boot,
        mut startup,
        ipc,
        open,
        bench,
        screens,
        bench_editor,
    } = launch;
    // Тема, акцент, шрифты, политика и контекст ОС — до окна (S2 §1.5, T-01).
    let probe = urfin_ui::sys::probe_system();
    let policies = org_policy(&probe, screens.is_some(), &mut boot.config);
    let geom = boot.session.as_ref().and_then(|s| s.window);
    let services = crate::vfs_services(&boot.config, screens.is_some());
    let (config, paths) = (boot.config.clone(), boot.paths.clone());
    let (signal, wake) = Signal::new();
    let ui_env = theme_env(policies, &signal, &paths, screens.is_some());
    let mut core = {
        let _s = tracing::info_span!("startup.core").entered();
        let t = Instant::now();
        let mut core = Core::new(boot, services, signal.clone());
        // `--screens`: без журналов job — сцены не должны зависеть от прошлых прогонов.
        let jpaths = screens.is_none().then_some(&paths);
        crate::register_handlers(&mut core, &config, jpaths, ui_env);
        startup.core = t.elapsed();
        core
    };
    // Конвейер клавиш keymap (S3) — в окно GPUI.
    let (cfg, warning) = config.section::<KeymapCfg>("keymap");
    if let Some(w) = warning {
        tracing::warn!(key = %w.key, msg = %w.msg, "[keymap] section ignored");
    }
    let mut flags = FlagRegistry::with_builtin();
    flags.register("cmdline_empty", urfin_term::cmdline_empty);
    flags.register("cmdline_menu", urfin_term::cmdline_menu);
    flags.register("cmdline_ghost", urfin_term::cmdline_ghost);
    flags.register("cast_view", urfin_view::cast_view);
    let keys = urfin_keymap::register(&mut core, paths, cfg, flags);
    let (ui_cfg, _) = config.section::<urfin_ui::config::UiCfg>("ui");
    let theme = gpui_shell::Theme::new(&probe, &ui_cfg);
    let context = if screens.is_none() {
        probe.context
    } else {
        None
    };
    for c in open {
        core.dispatch(c);
    }
    open_benches(&mut core, bench.as_deref(), bench_editor);
    if screens.is_some() {
        open_demo_tree(&mut core);
    }
    if let Some(inbox) = &ipc {
        let s = signal.clone();
        inbox.set_waker(move || urfin_core::Waker::wake(&*s));
    }
    let poll = second_runs(ipc);
    // Первый кадр: длительности старта в лог; `--bench-startup` — JSON и выход (S0 v5 №10).
    let window_at = std::time::Instant::now();
    // `URFIN_PROF=<файл>`: профиль UI-потока от создания окна до первого кадра (G-07),
    // только верхушки стека — во время загрузки DLL раскрутка заблокировалась бы.
    #[cfg(windows)]
    let mut prof = urfin_ui::sys::prof::start_leaf();
    let first_frame = Box::new(move || {
        #[cfg(windows)]
        if let Some(p) = prof.take() {
            p.finish();
        }
        startup.window = window_at.elapsed();
        startup.first_frame = startup.process.elapsed();
        tracing::info!(target: "startup", json = %startup.json(), "first frame");
        if startup.bench {
            println!("{}", startup.json());
        }
        startup.bench
    });
    gpui_shell::run(
        gpui_shell::Launch {
            core,
            wake,
            signal,
            poll,
            keys,
            theme,
            first_frame: Some(first_frame),
            bench: bench.map(|_| urfin_ui::bench::Bench::new((1200.0, 760.0))),
            screens: screens.inspect(|d| {
                let _ = std::fs::create_dir_all(d);
            }),
            context,
        },
        geom,
    );
}

/// Приветствие (D-02): файла версии нет — первый запуск, версия другая — обновление;
/// файл переписывается текущей версией (без сети, S0 «офлайн»).
fn welcome(paths: &urfin_core::Paths) -> Option<urfin_ui::state::Welcome> {
    let file = paths.state_dir.join("version");
    let now = urfin_ui::version();
    let was = std::fs::read_to_string(&file).ok();
    if was.as_deref().map(str::trim) != Some(now) {
        let _ = std::fs::create_dir_all(&paths.state_dir);
        if let Err(e) = std::fs::write(&file, now) {
            tracing::warn!(error = %e, "version file not written");
        }
    }
    match was.as_deref().map(str::trim) {
        None => Some(urfin_ui::state::Welcome::First),
        Some(v) if v != now => Some(urfin_ui::state::Welcome::Updated {
            from: v.to_owned(),
            to: now.to_owned(),
        }),
        Some(_) => None,
    }
}

/// `--bench`: обе панели — на каталог замера; демо-экран терминала (`screens.term`) —
/// для фазы `term` (S6-06).
fn open_bench(core: &mut Core, dir: &std::path::Path) {
    urfin_ui::register_demo(core);
    let sftp = crate::bench_sftp::on();
    if sftp {
        core.dispatch(Command::new("screens.ssh").arg("kind", "sftp"));
    }
    for side in ["left", "right"] {
        let path = if sftp {
            crate::bench_sftp::location()
        } else {
            urfin_core::Location::local(dir.to_path_buf())
        };
        core.dispatch(Command::new("panel.go").arg("side", side).arg("path", path));
    }
}

/// Замеры: `--bench` — каталог в панели; `--bench-editor <файл>` (S7-16) и
/// `--bench-viewer <файл>` — файл откроет фича с рабочего окна (состояние вьювера и
/// редактора handler кладёт в первом проходе Core).
fn open_benches(core: &mut Core, dir: Option<&std::path::Path>, editor: Option<Command>) {
    if let Some(dir) = dir {
        open_bench(core, dir);
    }
    if let Some(open) = editor {
        // `--bench-search`: ряд — на фикстуре сцен.
        if urfin_ui::bench::SEARCH.load(std::sync::atomic::Ordering::Relaxed) {
            urfin_ui::register_demo(core);
        }
        core.pump(std::time::Duration::from_millis(50));
        urfin_ui::bench::open_later(open);
    }
}

/// `--screens`: обе панели — на демо-дерево (одно и то же на любой машине).
fn open_demo_tree(core: &mut Core) {
    // Задачи для job-бара (D-06): только у сцен.
    urfin_ui::register_demo(core);
    // Панель результатов поиска на фикстуре (S8-11): стор сцены — в реестр `search:`.
    urfin_ui::set_search_adopt(Box::new(|store, text| {
        crate::search_stores().insert(store, &Arc::default(), text);
    }));
    // Панель сравнения на фикстуре (S8-39): стор сцены — в реестр `cmp:`.
    urfin_ui::set_cmp_adopt(Box::new(|store| {
        crate::search_stores().insert_cmp(store);
    }));
    match crate::demo_tree::create() {
        Ok(root) => {
            for (side, dir) in [("left", root.clone()), ("right", root.join("Projects"))] {
                core.dispatch(
                    Command::new("panel.go")
                        .arg("side", side)
                        .arg("path", urfin_core::Location::local(dir)),
                );
            }
        }
        Err(e) => {
            eprintln!("screens: demo tree: {e}");
            std::process::exit(1);
        }
    }
}

/// Политика организации (ADR-0020) — поверх конфига, до всех, кто его читает;
/// `--screens` — набор макетов, одинаковый на любой машине.
fn org_policy(
    probe: &urfin_ui::sys::SysProbe,
    screens: bool,
    config: &mut urfin_core::Config,
) -> urfin_core::PolicySet {
    let policies = if screens {
        urfin_ui::sys::policy::demo()
    } else {
        probe.policy.clone()
    };
    policies.apply(config);
    policies
}

/// Свои темы (`<config_dir>/themes`) и политика организации (T-01, ADR-0020) со
/// слежением за её изменениями. `--screens` — без своих тем и слежения: сцены одинаковы
/// на любой машине.
fn theme_env(
    policies: urfin_core::PolicySet,
    signal: &Arc<Signal>,
    paths: &urfin_core::Paths,
    screens: bool,
) -> urfin_ui::ThemeEnv {
    if screens {
        return urfin_ui::ThemeEnv {
            policies,
            ..urfin_ui::ThemeEnv::default()
        };
    }
    let inbox = urfin_ui::sys::policy::PolicyInbox::default();
    let s = Arc::clone(signal);
    urfin_ui::sys::policy::watch(policies.clone(), Arc::clone(&inbox), move || {
        urfin_core::Waker::wake(&*s);
    });
    urfin_ui::ThemeEnv {
        dir: Some(paths.config_dir.join("themes")),
        policies,
        policy_inbox: Some(inbox),
        fields: Some(paths.state_dir.join("fields.toml")),
        welcome: welcome(paths),
        config_dir: Some(paths.config_dir.clone()),
        log_dir: Some(paths.log_dir.clone()),
    }
}

/// Вторые запуски (S1-15): пути — новыми табами, окно — вперёд; звонок `--pkg-check`
/// (F02-11) — баннер обновлений, окно не выходит вперёд.
fn second_runs(ipc: Option<crate::ipc::Inbox>) -> gpui_shell::Poll {
    let bell = crate::pkg_bell();
    Box::new(move |core: &mut Core| {
        let Some(inbox) = &ipc else {
            return false;
        };
        let mut activate = false;
        for req in inbox.try_iter() {
            if req.pkg_checked {
                bell.ring();
                continue;
            }
            for c in req.commands() {
                core.dispatch(c);
            }
            activate = true;
        }
        activate
    })
}
