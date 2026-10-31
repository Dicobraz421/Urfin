//! `ui.screens` (F12, S7 §3.9): список экранов и панелей (ADR-0022 §2). Экраны — в
//! `ViewState` (`urfin-view`); список рисует фича окна `Viewer` по `UiEvent::Ext`.

use urfin_core::{Args, CmdError, Command, CommandId, CommandMeta, Cx, UiEvent};

use crate::handler::{Part, meta};

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

const COMMANDS: &[CommandMeta] = &[meta("ui.screens", "Screens", &[])];

fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    if cmd.id.as_str() != "ui.screens" {
        return None;
    }
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("ui.screens"),
        args: Args::default(),
    });
    Some(Ok(()))
}
