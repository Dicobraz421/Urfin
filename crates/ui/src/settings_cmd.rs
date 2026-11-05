//! Команды вкладки настроек (D-08): `ui.settings`, `ui.setting`, `ui.about` (ADR-0022 §2).

use urfin_core::{
    Args, CmdError, Command, CommandMeta, Cx, Lane, Location, PolicySet, Value, ValueKind, msg,
    settings,
};

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
            // «Подключения» (F03-05): выбранная строка таблицы и фильтр поиска.
            arg("sel", ValueKind::Int, false),
            arg("filter", ValueKind::Str, false),
            // «SSH» (F03-14): поле нового ключа; открыть папку (нет — создаётся).
            arg("add_key", ValueKind::Bool, false),
            arg("open_dir", ValueKind::Str, false),
            // «Пакеты» (F02-12): поиск важной программы.
            arg("add_important", ValueKind::Bool, false),
        ],
    ),
    meta(
        "ui.setting",
        "Change a setting",
        &[
            arg("key", ValueKind::Str, true),
            arg("value", ValueKind::Str, false),
            // Список строк вместо `value` (`ssh.default_keys`).
            arg("list", ValueKind::List, false),
            // `value` — текст, даже если похож на число или `true`.
            arg("text", ValueKind::Bool, false),
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
        if let Some(dir) = a.str("open_dir") {
            open_dir(cx, dir);
            return Ok(());
        }
        let section = a.str("section").unwrap_or("view");
        // «Подключения»: профили перечитываются при открытии раздела (F03 §2.2); «SSH» —
        // так же: заодно смотрится агент для пояснения.
        let reload = matches!(section, "conn" | "ssh")
            && a.bool("close") != Some(true)
            && !matches!(ui_state(cx).settings.as_deref(), Some("conn" | "ssh"));
        // «Пакеты»: при входе — сверка задачи Планировщика, кэш и найденные менеджеры.
        let survey = section == "pkg"
            && a.bool("close") != Some(true)
            && ui_state(cx).settings.as_deref() != Some("pkg");
        let st = ui_state(cx);
        st.ssh_add_key = a.bool("add_key") == Some(true);
        st.pkg_add_important = a.bool("add_important") == Some(true);
        st.settings = if a.bool("close") == Some(true) {
            None
        } else {
            Some(section.into())
        };
        if let Some(n) = a.int("sel") {
            st.conn_sel = usize::try_from(n.max(0)).unwrap_or(0);
        }
        if let Some(f) = a.str("filter") {
            st.conn_filter = f.into();
            st.conn_sel = 0;
        }
        if reload {
            cx.dispatch(Command::new("ssh.reload"));
        }
        if survey {
            cx.dispatch(Command::new("pkg.schedule"));
        }
        return Ok(());
    }
    let Some(key) = a.str("key") else {
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
    let msg = if let Some(list) = a.list("list") {
        let items = list.iter().filter_map(|v| match v {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        });
        settings::set(&path, items.collect::<settings::Value>())
    } else {
        let Some(value) = a.str("value") else {
            cx.keep_rev();
            return Ok(());
        };
        match value {
            v if a.bool("text") == Some(true) => settings::set(&path, v),
            "true" => settings::set(&path, true),
            "false" => settings::set(&path, false),
            v => match v.parse::<i64>() {
                Ok(n) => settings::set(&path, n),
                Err(_) => settings::set(&path, v),
            },
        }
    };
    msg.apply(cx);
    cx.dispatch(Command::new("app.reload_config"));
    Ok(())
}

/// «Открыть» папку настройки (записи сеансов): создать, если её ещё нет, и перейти в
/// активной панели. Диск — задачей.
fn open_dir(cx: &mut Cx<'_>, dir: &str) {
    let path = std::path::PathBuf::from(dir);
    cx.rt
        .spawn(Lane::Interactive, "ui.open_dir", cx.rt.token(), move |t| {
            if let Err(e) = std::fs::create_dir_all(&path) {
                tracing::warn!(error = %e, "ui.open_dir");
            }
            let go = msg("ui.open_dir", move |cx| {
                cx.dispatch(Command::new("ui.settings").arg("close", true));
                cx.dispatch(Command::new("panel.go").arg("path", Location::local(&path)));
            });
            let _ = t.sink.send(go, &t.cancel);
        });
}
