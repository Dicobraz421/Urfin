use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use urfin_core::{Boot, Command, Location, Services, UiEvent, ValueKind, Waker};
use urfin_keymap::{FlagRegistry, Keymap, KeymapCfg, Os, WarnKind};

use super::*;

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn root() -> Location {
    Location::local(std::env::temp_dir().join("urfin-view-test"))
}

fn core() -> Core {
    let mut core = Core::new(
        Boot::new(root(), root()),
        Services::null(),
        Arc::new(NoWake),
    );
    register_with(&mut core, ViewerCfg::default(), EditorCfg::default());
    core
}

fn pump(core: &mut Core) {
    core.pump(Duration::from_millis(5));
}

/// Все id S3 §3.3/§3.7/§3.8 и S7 §7 — в реестре своего namespace, и команда с
/// обязательными аргументами проходит `CommandRegistry::validate`.
#[test]
fn every_command_validates() {
    let core = core();
    let reg = core.registry();
    for (list, ns) in [
        (&VIEWER_COMMANDS[..], "viewer"),
        (&EDITOR_COMMANDS[..], "editor"),
    ] {
        for m in list {
            assert!(m.id.starts_with(ns) && m.category == ns, "{}", m.id);
            assert_eq!(reg.get(m.id), Some(m), "{}", m.id);
            let mut cmd = Command::new(m.id);
            for a in m.args.iter().filter(|a| a.required) {
                cmd = match (a.name, a.kind) {
                    ("side", _) => cmd.arg(a.name, "left"),
                    (_, ValueKind::Loc) => cmd.arg(a.name, root()),
                    (_, ValueKind::Int) => cmd.arg(a.name, 1),
                    (_, ValueKind::Bool) => cmd.arg(a.name, true),
                    (_, _) => cmd.arg(a.name, "down"),
                };
            }
            reg.validate(&cmd)
                .unwrap_or_else(|e| panic!("{}: {e}", m.id));
        }
    }
    assert!(reg.get("editor.input").is_none(), "решение 2026-10-30 №1");
}

/// Пресеты far/dn привязывают команды вьювера и редактора без предупреждений:
/// id и аргументы совпадают с keymap (`crates/keymap/tests/known_commands.rs`).
#[test]
fn presets_bind_viewer_and_editor() {
    let core = core();
    let paths = urfin_core::Paths::in_dir(std::env::temp_dir().join("urfin-view-no-keymaps"));
    for preset in ["far", "dn"] {
        let cfg = KeymapCfg {
            preset: preset.into(),
            user: None,
            ..KeymapCfg::default()
        };
        let (km, warnings) = Keymap::load(
            &paths,
            &cfg,
            core.registry(),
            &FlagRegistry::with_builtin(),
            Os::Windows,
        );
        // Команды других крейтов в этом реестре не собраны.
        let ours: Vec<String> = warnings
            .iter()
            .filter(|w| match &w.kind {
                WarnKind::UnknownCommand(id) => {
                    id.starts_with("viewer.") || id.starts_with("editor.")
                }
                _ => true,
            })
            .map(ToString::to_string)
            .collect();
        assert_eq!(ours, Vec::<String>::new(), "{preset}");
        assert!(
            km.bindings()
                .iter()
                .any(|b| b.cmds.iter().any(|c| c.id.as_str() == "viewer.search_next")),
            "{preset}"
        );
    }
}

/// Заглушка — тост «пока не сделано», а не ошибка.
#[test]
fn stubs_reject_with_toast() {
    let mut core = core();
    pump(&mut core);
    core.dispatch(Command::new("viewer.follow"));
    core.dispatch(Command::new("editor.save"));
    pump(&mut core);
    let toasts = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(toasts, 2);
}

/// Тестовый handler: открывает документ редактора как экран.
struct AddDoc;

static ADD_DOC: [urfin_core::CommandMeta; 1] = [urfin_core::CommandMeta {
    id: "vtest.add",
    title: "add",
    category: "test",
    args: &[],
    repeatable: false,
}];

impl urfin_core::Handler for AddDoc {
    fn namespace(&self) -> &'static str {
        "vtest"
    }

    fn commands(&self) -> &'static [urfin_core::CommandMeta] {
        &ADD_DOC
    }

    fn handle(
        &mut self,
        _cmd: &Command,
        cx: &mut urfin_core::Cx<'_>,
    ) -> Result<(), urfin_core::CmdError> {
        let view = cx.state.ext.get_mut::<ViewState>().unwrap();
        let id = view.alloc_id();
        view.insert(Doc::Editor(EditorDoc::new(id, root(), Placement::Window)));
        view.screens.push(id);
        Ok(())
    }
}

/// `ViewState` встаёт в `AppState.ext` в первом pump; `Edited` из inbox меняет
/// документ и `rev` состояния, неизвестный документ — нет.
#[test]
fn inbox_edits_apply_in_pump() {
    let mut core = core();
    pump(&mut core);
    core.register(Box::new(AddDoc));
    core.dispatch(Command::new("vtest.add"));
    let view = core.state().ext.get::<ViewState>().unwrap();
    let (tx, doc) = (view.inbox.sender(), view.active().unwrap());
    let rev = core.state().rev;
    tx.send(InboxMsg::Edited {
        doc,
        dirty: true,
        cursor: (3, 7),
    });
    tx.send(InboxMsg::Edited {
        doc: DocId(999),
        dirty: true,
        cursor: (0, 0),
    });
    pump(&mut core);
    assert!(core.state().rev > rev);
    let view = core.state().ext.get::<ViewState>().unwrap();
    let Some(Doc::Editor(d)) = view.doc(doc) else {
        panic!("editor doc");
    };
    assert!(d.dirty);
    assert_eq!((d.cursor, d.rev), ((3, 7), 1));
    assert_eq!(view.active(), Some(doc));

    let rev = core.state().rev;
    pump(&mut core);
    assert_eq!(core.state().rev, rev, "пустой inbox не трогает rev");
}

#[test]
fn remove_clears_screens_and_quick() {
    let mut view = ViewState::new(ViewerCfg::default(), EditorCfg::default());
    let id = view.alloc_id();
    view.insert(Doc::Viewer(ViewerDoc::new(
        id,
        root(),
        Placement::Panel(Side::Right),
        true,
        &ViewerCfg::default(),
        urfin_core::CancelToken::new(),
    )));
    view.screens.push(id);
    view.quick[Side::Right.idx()] = Some(id);
    assert!(view.remove(id).is_some());
    assert!(view.is_empty() && view.screens.is_empty() && view.quick == [None, None]);
}

#[derive(Serialize)]
struct Sections {
    viewer: ViewerCfg,
    editor: EditorCfg,
}

/// Значения по умолчанию `[viewer]` и `[editor]` (S7 §8).
#[test]
fn default_config_snapshot() {
    let text = toml::to_string(&Sections {
        viewer: ViewerCfg::default(),
        editor: EditorCfg::default(),
    })
    .unwrap();
    insta::assert_snapshot!(text);
}

#[test]
fn config_parses_user_values() {
    let v: ViewerCfg = toml::from_str(
        r#"
placement = "panel"
hex_cols = 16
wrap = "words"
[[assoc]]
mask = "*.pdf"
cmd = "SumatraPDF.exe {path}"
"#,
    )
    .unwrap();
    assert_eq!(v.placement, PlacementCfg::Panel);
    assert_eq!(v.hex_cols, HexCols::Fixed(16));
    assert_eq!(v.wrap, WrapCfg::Words);
    assert_eq!(v.assoc.len(), 1);
    assert_eq!(v.tab_size, 8, "остальное — по умолчанию");
    assert!(toml::from_str::<ViewerCfg>("hex_cols = 12").is_err());
    assert!(toml::from_str::<ViewerCfg>("hex_cols = \"wide\"").is_err());

    let e: EditorCfg = toml::from_str("eol = \"crlf\"\nautosave = \"focus\"").unwrap();
    assert_eq!((e.eol, e.autosave), (EolCfg::Crlf, Autosave::Focus));
}
