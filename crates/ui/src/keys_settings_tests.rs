//! Сквозной путь раздела: команды `ui.keys` → `keymap.bind` → файл → перечитанная
//! раскладка. Настоящие Core, handler `ui` и `keymap`, файл — во временном каталоге.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Config, Core, Location, Paths, Services, Session, Waker};
use urfin_keymap::{ActiveKeymap, FlagRegistry, KeymapCfg};

use super::*;

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Состояние «идёт захват» для тестов моста ввода.
pub(crate) fn start_capture(st: &mut AppState) {
    let mut u = UiState::new();
    u.settings = Some("keys".into());
    st.ext.insert(u);
    st.ext.insert(KeysUi {
        capture: Some(Capture {
            row: RowId {
                key: "ui.help".into(),
                place: "keys-where-panel",
            },
            old: None,
        }),
        ..KeysUi::default()
    });
}

fn core(paths: &Paths) -> Core {
    let (config, _) = Config::load(paths);
    let root = Location::local(paths.config_dir.join("files"));
    let mut boot = Boot::new(root.clone(), root);
    boot.paths = paths.clone();
    boot.config = config;
    boot.session = Session::load(paths);
    let mut core = Core::new(boot, Services::null(), Arc::new(NoWake));
    crate::register(&mut core);
    let _ = urfin_keymap::register(
        &mut core,
        paths.clone(),
        KeymapCfg::default(),
        FlagRegistry::with_builtin(),
    );
    core.pump(Duration::from_millis(5));
    core
}

fn gen_of(c: &Core) -> u64 {
    c.state().ext.get::<ActiveKeymap>().map_or(0, |a| a.generation)
}

/// Команда и ожидание, пока задачи и перечитывание дойдут до условия.
fn until(c: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(c) {
        assert!(t0.elapsed() < Duration::from_secs(10), "не дождались: {what}");
        c.pump(Duration::from_millis(5));
    }
}

fn run(c: &mut Core, text: &str) {
    c.dispatch(Command::parse(text).unwrap());
    c.pump(Duration::from_millis(5));
}

/// Открыть раздел с поиском строки.
fn open_at(c: &mut Core, filter: &str) {
    let was = gen_of(c);
    run(c, "ui.settings section=keys");
    until(c, "перечитывание", |c| gen_of(c) > was);
    c.dispatch(Command::new("ui.keys").arg("filter", filter));
    c.pump(Duration::from_millis(5));
}

fn k(c: &Core) -> &KeysUi {
    c.state().ext.get::<KeysUi>().unwrap()
}

fn user(paths: &Paths) -> String {
    fs::read_to_string(paths.config_dir.join("keymaps").join("user.toml")).unwrap_or_default()
}

fn row(c: &Core, key: &str) -> KeyRow {
    let v = view(c.state()).unwrap();
    v.catalog
        .rows
        .iter()
        .find(|r| r.target.key() == key)
        .cloned()
        .unwrap_or_else(|| panic!("нет строки {key}"))
}

fn labels(r: &KeyRow) -> Vec<String> {
    r.chips.iter().map(|c| c.label.to_string()).collect()
}

fn chord(c: &mut Core, text: &str) {
    c.dispatch(Command::new("ui.keys").arg("chord", text));
    c.pump(Duration::from_millis(5));
}

#[test]
fn esc_cancels_capture_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    let mut c = core(&paths);
    open_at(&mut c, "ui.keybar.toggle");
    run(&mut c, "ui.keys capture=replace");
    assert!(capturing(c.state()));
    chord(&mut c, "Esc");
    assert!(!capturing(c.state()));
    assert_eq!(user(&paths), "");
    // Смена раздела закрывает захват: клавиатура не остаётся заблокированной.
    run(&mut c, "ui.keys capture=add");
    assert!(capturing(c.state()));
    run(&mut c, "ui.settings section=view");
    assert!(!capturing(c.state()));
}

/// Решение 2026-11-05: AltGr-сочетание не запрещено — записывается, читается обратно,
/// пояснение показано, баннера нет.
#[test]
fn altgr_is_written_with_a_note_and_no_banner() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    let mut c = core(&paths);
    open_at(&mut c, "ui.keybar.toggle");
    run(&mut c, "ui.keys capture=add");
    let was = gen_of(&c);
    chord(&mut c, "Ctrl+Alt+K");
    until(&mut c, "запись", |c| gen_of(c) > was);
    assert!(user(&paths).contains("\"Ctrl+Alt+K\" = \"ui.keybar.toggle\""));
    assert!(labels(&row(&c, "ui.keybar.toggle")).contains(&"Ctrl+Alt+K".to_owned()));
    assert_eq!(k(&c).note.as_ref().unwrap().key, "ui-keys-risk-altgr");
    assert!(c.state().banner.is_none());
    assert!(k(&c).ask.is_none(), "отдельного подтверждения нет");
}

#[test]
fn conflict_asks_then_steals_and_reset_returns_preset() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    let mut c = core(&paths);
    let before = labels(&row(&c, "ui.keybar.toggle"));
    let help = labels(&row(&c, "ui.help"));
    assert_eq!(help, ["F1"]);
    open_at(&mut c, "ui.keybar.toggle");
    run(&mut c, "ui.keys capture=add");
    chord(&mut c, "F1");
    let ask = k(&c).ask.clone().expect("конфликт спрашивает");
    assert_eq!(ask.chord, "F1");
    assert_eq!(ask.others.len(), 1);
    assert_eq!(user(&paths), "", "до подтверждения файл не трогаем");
    // Отмена: всё как было.
    run(&mut c, "ui.keys cancel=true");
    assert!(k(&c).ask.is_none());
    assert_eq!(user(&paths), "");
    // Подтверждение: клавиша ушла к новой команде.
    run(&mut c, "ui.keys capture=add");
    chord(&mut c, "F1");
    let was = gen_of(&c);
    run(&mut c, "ui.keys confirm=true");
    until(&mut c, "запись", |c| gen_of(c) > was);
    assert!(labels(&row(&c, "ui.keybar.toggle")).contains(&"F1".to_owned()));
    assert!(row(&c, "ui.help").chips.is_empty());
    // Сброс строки возвращает её клавиши; файл без лишних строк.
    let was = gen_of(&c);
    run(&mut c, "ui.keys reset=true");
    until(&mut c, "сброс", |c| gen_of(c) > was);
    assert_eq!(labels(&row(&c, "ui.keybar.toggle")), before);
    assert!(!row(&c, "ui.keybar.toggle").changed);
    // Сброс всего возвращает и строку без клавиши.
    let was = gen_of(&c);
    c.dispatch(Command::new("keymap.reset"));
    until(&mut c, "сброс всего", |c| gen_of(c) > was);
    assert_eq!(labels(&row(&c, "ui.help")), ["F1"]);
    assert!(!user(&paths).contains("F1"));
}

#[test]
fn write_error_is_a_toast_not_a_lost_file() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    let dir = paths.config_dir.join("keymaps");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("user.toml"), "[context.panel\n").unwrap();
    let mut c = core(&paths);
    open_at(&mut c, "ui.keybar.toggle");
    run(&mut c, "ui.keys capture=add");
    chord(&mut c, "Ctrl+Shift+F12");
    for _ in 0..40 {
        c.pump(Duration::from_millis(5));
    }
    assert_eq!(user(&paths), "[context.panel\n", "битый файл не тронут");
}

#[test]
fn status_shows_file_and_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    let dir = paths.config_dir.join("keymaps");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("user.toml"),
        "[context.panel]\n\"F20\" = \"ui.help\"\n[context.\"panel || cmdline\"]\n\"F20\" = \"ui.menu\"\n",
    )
    .unwrap();
    let mut c = core(&paths);
    open_at(&mut c, "");
    let z = status(c.state()).unwrap();
    assert!(z[0].ends_with("user.toml"), "{z:?}");
    assert!(z[1].contains('1'), "{z:?}");
    assert!(keys(c.state()).unwrap().len() > 3);
}
