//! Обработчики namespaces core (S1 §2.3): `panel`, `sel`, `tab`, `app`, `job`, `prompt`.

mod app;
mod job;
mod panel;
mod prompt;
mod sel;
mod tabs;

pub(crate) use app::AppHandler;
pub(crate) use job::{JobHandler, job_id};
pub(crate) use panel::PanelHandler;
pub(crate) use prompt::{PromptHandler, QUIT_CHOICE};
pub(crate) use sel::SelHandler;
pub(crate) use tabs::TabHandler;

use crate::cmd::{ArgSpec, Args, CmdError, CommandMeta, SideRef, ValueKind};
use crate::ids::{Side, TabId};
use crate::msg::Cx;
use crate::tab::Tab;
use crate::text::tr;

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(
    id: &'static str,
    title: &'static str,
    args: &'static [ArgSpec],
    repeatable: bool,
) -> CommandMeta {
    let b = id.as_bytes();
    let category = match (b[0], b[1]) {
        (b'p', b'a') => "panel",
        (b'p', _) => "prompt",
        (b's', _) => "selection",
        (b't', _) => "tab",
        (b'j', _) => "job",
        _ => "app",
    };
    CommandMeta {
        id,
        title,
        category,
        args,
        repeatable,
    }
}

const SIDE: ArgSpec = arg("side", ValueKind::Str, false);
const N: ArgSpec = arg("n", ValueKind::Int, false);

/// Аргумент `side` относительно активной панели; по умолчанию — `default`.
fn side_arg(cx: &Cx<'_>, args: &Args, default: SideRef) -> Side {
    let active = cx.state.active;
    match args.side("side").unwrap_or(default) {
        SideRef::Left => Side::Left,
        SideRef::Right => Side::Right,
        SideRef::Active => active,
        SideRef::Passive => active.other(),
    }
}

fn tab_id(cx: &Cx<'_>, side: Side) -> TabId {
    cx.state.panel(side).tab().id
}

fn active_tab_mut<'c>(cx: &'c mut Cx<'_>) -> &'c mut Tab {
    let side = cx.state.active;
    cx.state.panel_mut(side).tab_mut()
}

fn rejected(key: &'static str, args: &[(&str, &str)]) -> CmdError {
    CmdError::Rejected(tr(key, args).into())
}

/// Значение строкового аргумента из набора.
fn choice<T: Copy>(args: &Args, name: &str, options: &[(&str, T)]) -> Result<T, CmdError> {
    let v = args.str(name).unwrap_or_default();
    options
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(v))
        .map(|&(_, t)| t)
        .ok_or_else(|| {
            let all: Vec<&str> = options.iter().map(|(k, _)| *k).collect();
            CmdError::arg(name, format!("expected {}", all.join(" | ")))
        })
}

/// Сравнение имён с регистром по ОС (как у `Mask`).
fn same_text(a: &str, b: &str) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        a.chars()
            .flat_map(char::to_lowercase)
            .eq(b.chars().flat_map(char::to_lowercase))
    } else {
        a == b
    }
}
