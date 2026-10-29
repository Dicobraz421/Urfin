//! Handler `du` (F01 §2.4): команды анализа места. Работает в UI-потоке, без I/O;
//! скан и кэш — задачами Runtime. Каркас (F01-01): команды зарегистрированы, сами
//! пока отказывают.

use urfin_core::{ArgSpec, CmdError, Command, CommandMeta, Cx, Handler, ValueKind};

use super::DuCfg;

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "du",
        args,
        repeatable: false,
    }
}

static COMMANDS: [CommandMeta; 8] = [
    // Меню «Утилиты → Анализ места…» (Ctrl+F9, №83).
    meta(
        "du.open",
        "Disk usage",
        &[
            arg("loc", ValueKind::Loc, false),
            arg("drive", ValueKind::Bool, false),
        ],
    ),
    meta(
        "du.refresh",
        "Refresh disk usage",
        &[arg("full", ValueKind::Bool, false)],
    ),
    meta("du.stop", "Stop disk usage scan", &[]),
    // `kind`: dirs | large | types | old | dups | temp.
    meta(
        "du.filter",
        "Disk usage filter",
        &[arg("kind", ValueKind::Str, true)],
    ),
    meta("du.map", "Disk usage map", &[]),
    meta("du.verify_hash", "Verify duplicates by hash", &[]),
    // Выключен до принятия ADR-0021 (S0 v6 №10).
    meta("du.elevate", "Count as administrator", &[]),
    meta(
        "du.old_age",
        "Old files age",
        &[arg("age", ValueKind::Str, true)],
    ),
];

/// Handler `du`.
#[derive(Debug)]
pub struct DuHandler {
    cfg: DuCfg,
}

impl DuHandler {
    pub fn new(cfg: DuCfg) -> DuHandler {
        DuHandler { cfg }
    }
}

impl Handler for DuHandler {
    fn namespace(&self) -> &'static str {
        "du"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = cmd.id.as_str();
        if !COMMANDS.iter().any(|m| m.id == id) {
            return Err(CmdError::Unknown(id.into()));
        }
        tracing::debug!(target: "vfs.du", cmd = id, cfg = ?self.cfg, "not implemented");
        Err(CmdError::Rejected("not yet".into()))
    }
}
