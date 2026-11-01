use std::sync::Arc;
use std::time::Duration;

use urfin_core::fake::FakeListing;
use urfin_core::{
    Boot, Command, Core, Entry, Focus, ListingStatus, Location, OverlayKind, Paths, Row, Services,
    Waker,
};

use crate::{FlagRegistry, KeymapCfg};

use super::{KeymapView, register};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

pub(crate) fn core_with(entries: Vec<Entry>) -> Core {
    let root = Location::local(std::env::temp_dir().join("urfin-keymap-qs"));
    let fake = FakeListing::new(1 << 16);
    fake.set_dir(root.clone(), entries);
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services {
            listing: Arc::new(fake),
            ..Services::null()
        },
        Arc::new(NoWake),
    );
    let _ = register(
        &mut core,
        Paths::in_dir(std::env::temp_dir().join("urfin-keymap-qs-nofiles")),
        KeymapCfg::default(),
        FlagRegistry::with_builtin(),
    );
    for _ in 0..200 {
        core.pump(Duration::from_millis(5));
        if core.state().active_tab().listing.status == ListingStatus::Complete {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    core
}

fn core() -> Core {
    core_with(
        [
            "alpha.txt",
            "beta.rs",
            "Bravo.toml",
            "beta2.txt",
            "gamma, delta.md",
        ]
        .iter()
        .map(|n| FakeListing::file(n, 1))
        .collect(),
    )
}

fn cmd(core: &mut Core, s: &str) {
    core.dispatch(Command::parse(s).unwrap());
}

fn cur(core: &Core) -> String {
    match core.state().active_tab().current() {
        Some(Row::Entry { entry, .. }) => entry.name.to_string(),
        Some(Row::Parent) => "..".into(),
        None => String::new(),
    }
}

fn view(core: &Core) -> KeymapView {
    core.state()
        .ext
        .get::<KeymapView>()
        .cloned()
        .unwrap_or_default()
}

fn mask(core: &Core) -> (String, u32) {
    let q = view(core).qs.expect("quick search is open");
    (q.mask.to_string(), q.matches)
}

#[test]
fn start_types_and_moves_cursor() {
    let mut c = core();
    cmd(&mut c, "keymap.qs.start text=b");
    assert_eq!(c.state().focus, Focus::Overlay(OverlayKind::QuickSearch));
    // Регистр не важен (Windows/macOS), курсор — на первом совпадении.
    let (m, n) = mask(&c);
    assert_eq!(m, "b");
    assert!(n >= 2, "{n}");
    assert!(cur(&c).to_lowercase().starts_with('b'), "{}", cur(&c));
    cmd(&mut c, "keymap.qs.input text=et");
    assert_eq!(mask(&c), ("bet".into(), 2));
    assert_eq!(cur(&c), "beta.rs");
}

/// Нет совпадения — символ не принят, курсор на месте, поле подсвечено.
#[test]
fn rejected_char_is_dropped() {
    let mut c = core();
    cmd(&mut c, "keymap.qs.start text=al");
    let before = cur(&c);
    cmd(&mut c, "keymap.qs.input text=z");
    assert_eq!(mask(&c).0, "al");
    assert_eq!(cur(&c), before);
    assert!(view(&c).qs.unwrap().miss_at.is_some());
    // Вставка: принимается, пока есть совпадения.
    cmd(&mut c, "keymap.qs.input text=phaX");
    assert_eq!(mask(&c).0, "alpha");
    // Нет совпадения и на старте — поиск открыт с пустой маской.
    cmd(&mut c, "keymap.qs.cancel");
    cmd(&mut c, "keymap.qs.start text=q");
    assert_eq!(mask(&c), (String::new(), 0));
    // Ни тоста, ни ошибки.
    assert!(
        !c.take_ui_events()
            .iter()
            .any(|e| matches!(e, urfin_core::UiEvent::Toast { .. }))
    );
}

#[test]
fn next_prev_backspace_cancel() {
    let mut c = core();
    cmd(&mut c, "keymap.qs.start text=b");
    let first = cur(&c);
    cmd(&mut c, "keymap.qs.next");
    let second = cur(&c);
    assert_ne!(first, second);
    cmd(&mut c, "keymap.qs.prev");
    assert_eq!(cur(&c), first);
    cmd(&mut c, "keymap.qs.backspace");
    assert_eq!(mask(&c), (String::new(), 0));
    cmd(&mut c, "keymap.qs.cancel");
    assert!(view(&c).qs.is_none());
    assert_eq!(c.state().focus, Focus::Panel);
}

/// Маски: `*`, `?`, `>ext` (DN), запятая в имени не делит маску.
#[test]
fn mask_syntax() {
    let mut c = core();
    cmd(&mut c, "keymap.qs.start text=*.t");
    assert_eq!(mask(&c).1, 3, "alpha.txt, Bravo.toml, beta2.txt");
    cmd(&mut c, "keymap.qs.cancel");
    cmd(&mut c, "keymap.qs.start text=b?ta");
    assert_eq!(mask(&c).1, 2);
    cmd(&mut c, "keymap.qs.cancel");
    cmd(&mut c, r#"keymap.qs.start text=">tx""#);
    assert_eq!(mask(&c).1, 2);
    cmd(&mut c, "keymap.qs.cancel");
    cmd(&mut c, r#"keymap.qs.start text="gamma,""#);
    assert_eq!(mask(&c), ("gamma,".into(), 1));
}

/// Фокус ушёл (меню, клик) — поиск закрывается.
#[test]
fn closes_when_focus_leaves() {
    let mut c = core();
    cmd(&mut c, "keymap.qs.start text=a");
    cmd(&mut c, "panel.focus");
    c.pump(Duration::from_millis(5));
    assert!(view(&c).qs.is_none());
}

#[test]
fn pending_chord() {
    let mut c = core();
    cmd(&mut c, "keymap.pending chord=Ctrl+K");
    assert_eq!(view(&c).pending.as_deref(), Some("Ctrl+K"));
    cmd(&mut c, "keymap.pending");
    assert_eq!(view(&c).pending, None);
}

// ---- S3-10: перезагрузка, пресет, show, dump, app.reload_config ----

use crate::ActiveKeymap;

/// Core с каталогами в `dir` (конфиг и `keymaps/` — там же) и пустым листингом.
fn core_in(dir: &std::path::Path) -> Core {
    let root = Location::local(dir.join("files"));
    let mut boot = Boot::new(root.clone(), root);
    boot.paths = Paths::in_dir(dir);
    let paths = boot.paths.clone();
    let mut core = Core::new(boot, Services::null(), Arc::new(NoWake));
    let _ = register(
        &mut core,
        paths,
        KeymapCfg::default(),
        FlagRegistry::with_builtin(),
    );
    core.pump(Duration::from_millis(5));
    core
}

/// Pump, пока не выполнится условие (загрузка идёт на `Lane::Bulk`).
fn pump_until(core: &mut Core, f: impl Fn(&Core) -> bool) {
    for _ in 0..400 {
        core.pump(Duration::from_millis(5));
        if f(core) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("condition not reached");
}

fn active(core: &Core) -> (u64, String, Vec<String>) {
    let a = core
        .state()
        .ext
        .get::<ActiveKeymap>()
        .expect("active keymap");
    let keys = a
        .keymap
        .bindings()
        .iter()
        .map(|b| format!("{} = {}", b.chord, b.cmds[0]))
        .collect();
    (a.generation, a.cfg.preset.to_string(), keys)
}

fn write_user(dir: &std::path::Path, text: &str) {
    std::fs::create_dir_all(dir.join("keymaps")).unwrap();
    std::fs::write(dir.join("keymaps/user.toml"), text).unwrap();
}

/// Правка `user.toml` + `keymap.reload` применяется в следующих кадрах.
#[test]
fn reload_applies_user_file() {
    let tmp = tempfile::tempdir().unwrap();
    write_user(
        tmp.path(),
        "format = 1\nname = \"user\"\n[context.panel]\n\"F11\" = \"panel.refresh\"",
    );
    let mut c = core_in(tmp.path());
    let (generation, _, keys) = active(&c);
    assert_eq!(generation, 0);
    assert!(keys.contains(&"F11 = panel.refresh".to_owned()), "{keys:?}");
    assert!(c.state().banner.is_none());
    write_user(
        tmp.path(),
        "format = 1\nname = \"user\"\n[context.panel]\n\"F11\" = \"panel.swap\"",
    );
    cmd(&mut c, "keymap.reload");
    pump_until(&mut c, |c| active(c).0 == 1);
    assert!(active(&c).2.contains(&"F11 = panel.swap".to_owned()));
}

/// Битый файл — баннер, прежняя раскладка остаётся; исправленный — баннер снят.
#[test]
fn broken_file_keeps_previous_keymap() {
    let tmp = tempfile::tempdir().unwrap();
    let good = "format = 1\nname = \"user\"\n[context.panel]\n\"F11\" = \"panel.refresh\"";
    write_user(tmp.path(), good);
    let mut c = core_in(tmp.path());
    write_user(tmp.path(), "format = 1\n[context.panel\n");
    cmd(&mut c, "keymap.reload");
    pump_until(&mut c, |c| c.state().banner.is_some());
    assert_eq!(active(&c).0, 0, "previous keymap kept");
    let acts: Vec<String> = c
        .state()
        .banner
        .as_ref()
        .unwrap()
        .actions
        .iter()
        .map(|(_, cmd)| cmd.to_string())
        .collect();
    assert_eq!(acts, ["keymap.reload", "app.dismiss_banner"]);
    assert!(active(&c).2.contains(&"F11 = panel.refresh".to_owned()));
    write_user(tmp.path(), good);
    cmd(&mut c, "keymap.reload");
    pump_until(&mut c, |c| active(c).0 == 1);
    assert!(c.state().banner.is_none());
}

/// `keymap.preset`: новый пресет; неизвестный — баннер, остаётся прежний.
#[test]
fn preset_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = core_in(tmp.path());
    assert_eq!(active(&c).1, "far");
    cmd(&mut c, "keymap.preset name=dn");
    pump_until(&mut c, |c| active(c).0 == 1);
    assert_eq!(active(&c).1, "dn");
    cmd(&mut c, "keymap.preset name=nope");
    pump_until(&mut c, |c| c.state().banner.is_some());
    assert_eq!(active(&c).1, "dn");
    // Принятый пресет — в `[keymap] preset` (S2-22), отвергнутый — нет.
    let _ = c.shutdown(Duration::from_millis(500));
    let text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(text.ends_with("[keymap]\npreset = \"dn\"\n"), "{text}");
}

/// `app.reload_config` с новой секцией `[keymap]` перечитывает раскладку.
#[test]
fn follows_config_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = core_in(tmp.path());
    std::fs::write(
        tmp.path().join("config.toml"),
        "[keymap]\npreset = \"dn\"\n",
    )
    .unwrap();
    cmd(&mut c, "app.reload_config");
    pump_until(&mut c, |c| active(c).1 == "dn");
}

/// `keymap.show` — таблица в `UiEvent::Ext`; `keymap.dump` — файл, который снова
/// читается как раскладка.
#[test]
fn show_and_dump() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = core_in(tmp.path());
    c.take_ui_events();
    cmd(&mut c, "keymap.show");
    let lines = c
        .take_ui_events()
        .into_iter()
        .find_map(|e| match e {
            urfin_core::UiEvent::Ext { id, args } if id.as_str() == "keymap.show" => {
                args.list("lines").map(<[_]>::len)
            }
            _ => None,
        })
        .expect("keymap.show event");
    assert!(lines > 10, "{lines}");
    let out = tmp.path().join("out/dump.toml");
    let arg = format!("keymap.dump path=\"{}\"", out.display());
    cmd(&mut c, &arg);
    // Файл появляется до конца записи: ждать содержимого, а не файла.
    pump_until(&mut c, |_| {
        std::fs::read_to_string(&out).is_ok_and(|t| t.contains("panel.cursor.up"))
    });
    let text = std::fs::read_to_string(&out).unwrap();
    // Выгрузка — валидный файл keymap.
    let mut reg = urfin_core::CommandRegistry::new();
    for m in c.registry().iter() {
        let _ = reg.register(std::slice::from_ref(m));
    }
    let (km, w) = crate::Keymap::from_str_layers(
        &[(&text, crate::Layer::User)],
        &reg,
        &FlagRegistry::with_builtin(),
        crate::Os::current(),
    );
    let w: Vec<String> = w.iter().map(ToString::to_string).collect();
    assert_eq!(w, Vec::<String>::new());
    assert!(!km.bindings().is_empty());
}
