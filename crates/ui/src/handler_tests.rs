//! Настройки `ui.*` в `config.toml` и обратно в [`UiState`] (S2-22).

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use urfin_core::{
    Boot, Command, Config, Core, Location, Paths, Services, Session, UiEvent, ViewMode, Waker,
};

use crate::state::UiState;

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Core с handler `ui`; конфиг и сессия — из `paths`, как при старте приложения.
fn core(paths: &Paths) -> Core {
    let (config, warnings) = Config::load(paths);
    assert_eq!(warnings, vec![]);
    let root = Location::local(paths.config_dir.join("files"));
    let mut boot = Boot::new(root.clone(), root);
    boot.paths = paths.clone();
    boot.config = config;
    boot.session = Session::load(paths);
    let mut core = Core::new(boot, Services::null(), Arc::new(NoWake));
    crate::register(&mut core);
    core.pump(Duration::from_millis(5));
    core
}

fn ui(core: &Core) -> &UiState {
    core.state()
        .ext
        .get::<UiState>()
        .expect("UiState after first pump")
}

const USER: &str = "# мой конфиг\n[ui]\ntheme = \"dark\"   # тёмная\nzoom = 120\n\
                    [ui.view_modes.dev]\ncolumns = [\"name\", \"type\"]\n";

#[test]
fn config_seeds_state_and_commands_write_back() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    fs::write(paths.config_file(), USER).unwrap();
    let mut c = core(&paths);
    let st = ui(&c);
    assert_eq!(
        (st.theme.as_str(), st.zoom, st.split),
        ("system-dark", 120, 0.5)
    );

    for cmd in [
        "ui.theme.set id=system-light",
        "ui.zoom delta=1",
        "ui.split delta=1",
        "ui.columns set=dev",
        "ui.lang lang=ru",
        "ui.lang lang=xx-unknown",
    ] {
        c.dispatch(Command::parse(cmd).unwrap());
        c.pump(Duration::from_millis(5));
    }
    urfin_i18n::set_language("en");
    let st = ui(&c);
    assert_eq!((st.theme.as_str(), st.zoom), ("system-light", 130));
    // Выход пишет отложенное сразу, не дожидаясь debounce.
    let _ = c.shutdown(Duration::from_millis(500));
    let text = fs::read_to_string(paths.config_file()).unwrap();
    assert!(
        text.starts_with("# мой конфиг\n[ui]\ntheme = \"system-light\"   # тёмная\nzoom = 130\n"),
        "{text}"
    );
    assert!(text.contains("split = 0.55\n"), "{text}");
    assert!(text.contains("columns = \"dev\"\n"), "{text}");
    assert!(
        text.contains("[general]\nlanguage = \"ru\"\n"),
        "unknown language is not saved: {text}"
    );

    // Следующий запуск — с сохранёнными значениями.
    let c = core(&paths);
    let st = ui(&c);
    assert_eq!(st.theme, "system-light");
    assert_eq!(st.zoom, 130);
    assert!((st.split - 0.55).abs() < 1e-6);
    assert_eq!(st.columns.as_deref(), Some("dev"));
    assert_eq!(c.state().config.general.language, "ru");
}

#[test]
fn reload_config_applies_edited_ui_section() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    let mut c = core(&paths);
    assert_eq!(ui(&c).theme, "auto");
    fs::write(
        paths.config_file(),
        "[ui]\ntheme = \"dark\"\nzoom = 999\nsplit = 0.05\n",
    )
    .unwrap();
    c.dispatch(Command::new("app.reload_config"));
    for _ in 0..400 {
        c.pump(Duration::from_millis(5));
        if ui(&c).theme == "system-dark" {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let st = ui(&c);
    assert_eq!(st.theme, "system-dark");
    assert_eq!(st.zoom, 300, "clamped to the zoom range");
    assert!((st.split - 0.1).abs() < 1e-6, "clamped to the split range");
}

fn toasts(c: &mut Core) -> Vec<String> {
    c.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

const SETS: &str = "[ui.view_modes.dev]\ntitle = \"Разработка\"\n\
                    columns = [\"name\", \"type\", \"size\", \"mtime\", \"ctime\", \"attrs\"]\n\
                    [ui.view_modes.audit]\ncolumns = [\"name\", \"atime\", \"owner\"]\n";

/// Наборы колонок (S2-19): неизвестная колонка — предупреждение; `ui.columns` — вид
/// `Custom(n)` активной панели и имя набора в конфиге; после перезапуска панель — в том
/// же наборе (сессия), набор — в состоянии (конфиг).
#[test]
fn column_set_warns_and_survives_restart() {
    urfin_i18n::set_language("en");
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    fs::write(paths.config_file(), SETS).unwrap();
    let mut c = core(&paths);
    let st = ui(&c);
    assert_eq!(
        st.view_modes
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>(),
        ["audit", "Разработка"]
    );
    let warned = toasts(&mut c);
    assert!(
        warned
            .iter()
            .any(|t| t.contains("ui.view_modes.audit") && t.contains("owner")),
        "{warned:?}"
    );

    c.dispatch(Command::parse("ui.columns set=nope").unwrap());
    c.pump(Duration::from_millis(5));
    assert_eq!(
        c.state().active_tab().view_mode,
        ViewMode::Full,
        "нет набора"
    );
    c.dispatch(Command::parse("ui.columns set=dev").unwrap());
    c.pump(Duration::from_millis(5));
    assert_eq!(c.state().active_tab().view_mode, ViewMode::Custom(1));
    assert_eq!(ui(&c).columns.as_deref(), Some("dev"));
    let _ = c.shutdown(Duration::from_millis(500));
    let text = fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("columns = \"dev\"\n"), "{text}");

    let c = core(&paths);
    assert_eq!(c.state().active_tab().view_mode, ViewMode::Custom(1));
    assert_eq!(ui(&c).columns.as_deref(), Some("dev"));
    assert_eq!(ui(&c).view_modes[1].cols.len(), 5);
}

/// Перечитанный конфиг с новым набором впереди: панель остаётся в своём наборе.
#[test]
fn reload_keeps_the_set_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    fs::write(paths.config_file(), SETS).unwrap();
    let mut c = core(&paths);
    c.dispatch(Command::parse("ui.columns set=dev").unwrap());
    c.pump(Duration::from_millis(5));
    assert_eq!(c.state().active_tab().view_mode, ViewMode::Custom(1));
    fs::write(
        paths.config_file(),
        format!("{SETS}[ui.view_modes.aaa]\ncolumns = [\"ext\"]\n"),
    )
    .unwrap();
    c.dispatch(Command::new("app.reload_config"));
    for _ in 0..400 {
        c.pump(Duration::from_millis(5));
        if ui(&c).view_modes.len() == 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(ui(&c).view_modes.len(), 3);
    assert_eq!(c.state().active_tab().view_mode, ViewMode::Custom(2));
}
