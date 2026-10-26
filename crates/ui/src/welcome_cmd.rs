//! Команды приветствия (D-02): `ui.welcome`, `ui.welcome.done` (ADR-0022 §2).

use urfin_core::{Args, CmdError, Command, CommandMeta, Cx, ValueKind};

use crate::handler::{Part, arg, meta, ui_state};

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

const COMMANDS: &[CommandMeta] = &[
    // Приветствие (D-02): показать (`--screens`, справка) и принять выбор.
    meta(
        "ui.welcome",
        "Welcome",
        &[
            arg("kind", ValueKind::Str, false),
            arg("from", ValueKind::Str, false),
        ],
    ),
    meta(
        "ui.welcome.done",
        "Close welcome",
        &[
            arg("preset", ValueKind::Str, false),
            arg("follow_os", ValueKind::Bool, false),
        ],
    ),
];

fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    match cmd.id.as_str() {
        id @ ("ui.welcome" | "ui.welcome.done") => run(cx, id, &cmd.args),
        _ => return None,
    }
    Some(Ok(()))
}

/// Приветствие (D-02): `ui.welcome` — показать, `ui.welcome.done` — принять выбор
/// (пресет клавиш, тема как в Windows) и закрыть.
fn run(cx: &mut Cx<'_>, id: &str, a: &Args) {
    if id == "ui.welcome" {
        ui_state(cx).welcome = Some(match a.str("kind") {
            Some("updated") => crate::state::Welcome::Updated {
                from: a.str("from").unwrap_or("0.0.0").to_owned(),
                to: crate::version().to_owned(),
            },
            _ => crate::state::Welcome::First,
        });
        return;
    }
    if let Some(p) = a.str("preset") {
        cx.dispatch(Command::new("keymap.preset").arg("name", p));
    }
    if a.bool("follow_os") == Some(true) {
        cx.dispatch(Command::new("ui.theme.follow_os"));
    }
    ui_state(cx).welcome = None;
}
