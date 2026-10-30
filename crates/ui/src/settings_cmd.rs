//! Команды вкладки настроек (D-08): `ui.settings`, `ui.setting`, `ui.about` (ADR-0022 §2).

use urfin_core::{Args, CmdError, Command, CommandMeta, Cx, PolicySet, ValueKind, settings};

use crate::handler::{Part, arg, meta, ui_state};

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

const COMMANDS: &[CommandMeta] = &[
    // Настройки (D-08): вкладка с разделом; `close` — закрыть. `ui.setting` — записать
    // ключ конфига и перечитать (применяется сразу).
    meta(
        "ui.settings",
        "Settings",
        &[
            arg("section", ValueKind::Str, false),
            arg("close", ValueKind::Bool, false),
        ],
    ),
    meta(
        "ui.setting",
        "Change a setting",
        &[
            arg("key", ValueKind::Str, true),
            arg("value", ValueKind::Str, true),
        ],
    ),
    // «О программе» (S0-decisions №49): имя Úrfionn и пасхалка — тостом, пока нет окна.
    meta("ui.about", "About", &[]),
];

fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    match cmd.id.as_str() {
        id @ ("ui.settings" | "ui.setting") => return Some(run(cx, id, &cmd.args)),
        // «О программе» — раздел настроек (D-08).
        "ui.about" => ui_state(cx).settings = Some("about".into()),
        _ => return None,
    }
    Some(Ok(()))
}

/// Настройки (D-08): `ui.settings` — открыть раздел или закрыть; `ui.setting` — записать
/// ключ (`panels.sort.key`) в конфиг и перечитать его. Ключ под политикой организации
/// (ADR-0020) не меняется.
fn run(cx: &mut Cx<'_>, id: &str, a: &Args) -> Result<(), CmdError> {
    if id == "ui.settings" {
        let st = ui_state(cx);
        st.settings = if a.bool("close") == Some(true) {
            None
        } else {
            Some(a.str("section").unwrap_or("view").into())
        };
        return Ok(());
    }
    let (Some(key), Some(value)) = (a.str("key"), a.str("value")) else {
        cx.keep_rev();
        return Ok(());
    };
    let path: Vec<&str> = key.split('.').collect();
    let policy = cx.state.ext.get::<PolicySet>();
    if policy.and_then(|p| p.setting(&path)).is_some() {
        cx.keep_rev();
        let text = urfin_i18n::current().get("ui-set-by-policy");
        return Err(CmdError::Rejected(format!("{key}: {text}").into()));
    }
    let msg = match value {
        "true" => settings::set(&path, true),
        "false" => settings::set(&path, false),
        v => match v.parse::<i64>() {
            Ok(n) => settings::set(&path, n),
            Err(_) => settings::set(&path, v),
        },
    };
    msg.apply(cx);
    cx.dispatch(Command::new("app.reload_config"));
    Ok(())
}
