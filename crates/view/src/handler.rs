//! Handlers `viewer` и `editor`: команды S3 §3.3/§3.7/§3.8 и S7 §7. В S7-01 — заглушки
//! («пока не сделано» тостом); поведение — карточки S7-08…S7-19.

use urfin_core::{ArgSpec, CmdError, Command, CommandMeta, Cx, Handler, ValueKind};

use crate::{InboxMsg, ViewState};

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
    category: &'static str,
    args: &'static [ArgSpec],
    repeatable: bool,
) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category,
        args,
        repeatable,
    }
}

const fn v(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    meta(id, title, "viewer", args, false)
}

const fn vr(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    meta(id, title, "viewer", args, true)
}

const fn e(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    meta(id, title, "editor", args, false)
}

const fn er(id: &'static str, title: &'static str) -> CommandMeta {
    meta(id, title, "editor", &[], true)
}

/// `viewer.open`: путь (иначе — под курсором), внешний, всегда встроенный, размещение.
const VIEWER_OPEN: [ArgSpec; 4] = [
    arg("path", ValueKind::Loc, false),
    arg("alt", ValueKind::Bool, false),
    arg("internal", ValueKind::Bool, false),
    arg("placement", ValueKind::Str, false),
];

/// `editor.open` / `editor.new`: строка и колонка — с 1.
const EDITOR_OPEN: [ArgSpec; 5] = [
    arg("path", ValueKind::Loc, false),
    arg("alt", ValueKind::Bool, false),
    arg("internal", ValueKind::Bool, false),
    arg("line", ValueKind::Int, false),
    arg("col", ValueKind::Int, false),
];

const TO: [ArgSpec; 1] = [arg("to", ValueKind::Str, false)];
const CP: [ArgSpec; 1] = [arg("cp", ValueKind::Str, false)];
const N: [ArgSpec; 1] = [arg("n", ValueKind::Int, true)];

/// Команды вьювера (S3 §3.3, §3.7; S7 §7).
pub static VIEWER_COMMANDS: [CommandMeta; 27] = [
    v("viewer.open", "View", &VIEWER_OPEN),
    v(
        "viewer.quick",
        "Quick view",
        &[
            arg("side", ValueKind::Str, true),
            arg("path", ValueKind::Loc, true),
        ],
    ),
    v("viewer.close", "Close viewer", &[]),
    v("viewer.wrap", "Wrap lines", &[]),
    v("viewer.wrap_mode", "Wrap by words or characters", &[]),
    v("viewer.hex", "Text / hex", &[]),
    v(
        "viewer.mode",
        "View mode",
        &[arg("mode", ValueKind::Str, false)],
    ),
    v("viewer.to_editor", "Edit this file", &[]),
    v(
        "viewer.search",
        "Find",
        &[
            arg("text", ValueKind::Str, false),
            arg("kind", ValueKind::Str, false),
            arg("case", ValueKind::Bool, false),
            arg("word", ValueKind::Bool, false),
            arg("back", ValueKind::Bool, false),
        ],
    ),
    vr("viewer.search_next", "Find next", &[]),
    vr("viewer.search_prev", "Find previous", &[]),
    v("viewer.search_cancel", "Stop search", &[]),
    v("viewer.codepage", "Next encoding", &CP),
    v("viewer.codepage_menu", "Encoding…", &[]),
    v("viewer.goto", "Go to…", &TO),
    v("viewer.next_file", "Next file", &[]),
    v("viewer.prev_file", "Previous file", &[]),
    v("viewer.reveal", "Show file in panel", &[]),
    v("viewer.bookmark.set", "Set bookmark", &N),
    v("viewer.bookmark.go", "Go to bookmark", &N),
    v("viewer.back", "Back", &[]),
    vr(
        "viewer.scroll",
        "Scroll",
        &[
            arg("dir", ValueKind::Str, true),
            arg("n", ValueKind::Int, false),
        ],
    ),
    v(
        "viewer.history",
        "View and edit history",
        &[arg("kind", ValueKind::Str, false)],
    ),
    v("viewer.follow", "Follow file end", &[]),
    v("viewer.copy", "Copy selection", &[]),
    v("viewer.select_all", "Select all", &[]),
    v("viewer.dock", "Window / panel", &[]),
];

/// Команды редактора (S3 §3.3, §3.8; S7 §7). Нажатия, undo и IME — `EditorState` в UI;
/// `editor.input` нет (решение 2026-10-30 №1).
pub static EDITOR_COMMANDS: [CommandMeta; 22] = [
    e("editor.open", "Edit", &EDITOR_OPEN),
    e("editor.new", "New file", &EDITOR_OPEN),
    e("editor.save", "Save", &[]),
    e("editor.save_as", "Save as…", &[]),
    e("editor.save_all", "Save all", &[]),
    e("editor.save_close", "Save and close", &[]),
    e("editor.close", "Close editor", &[]),
    e("editor.search", "Find", &[]),
    e("editor.replace", "Replace", &[]),
    er("editor.search_next", "Find next"),
    er("editor.search_prev", "Find previous"),
    e("editor.goto", "Go to line…", &TO),
    e("editor.codepage", "Next encoding", &CP),
    e("editor.codepage_menu", "Encoding…", &[]),
    e("editor.to_viewer", "View this file", &[]),
    er("editor.undo", "Undo"),
    er("editor.redo", "Redo"),
    er("editor.delete_line", "Delete line"),
    er("editor.block.shift_left", "Shift block left"),
    er("editor.block.shift_right", "Shift block right"),
    e(
        "editor.eol",
        "Line endings",
        &[arg("to", ValueKind::Str, true)],
    ),
    e("editor.reload", "Reload from disk", &[]),
];

fn not_yet(cmd: &Command) -> CmdError {
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("what", cmd.id.as_str().to_owned());
    // Общий текст «Пока не сделано: …»; свои строки M4 — S7-01a.
    CmdError::Rejected(urfin_i18n::current().fmt("ops-not-yet", &args).into())
}

/// Handler `viewer`. Кладёт `ViewState` в `AppState.ext` в первом pump.
#[derive(Debug)]
pub struct ViewerHandler {
    pending: Option<ViewState>,
}

impl ViewerHandler {
    pub fn new(state: ViewState) -> ViewerHandler {
        ViewerHandler {
            pending: Some(state),
        }
    }
}

impl Handler for ViewerHandler {
    fn namespace(&self) -> &'static str {
        "viewer"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &VIEWER_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        Err(not_yet(cmd))
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some(state) = self.pending.take() {
            cx.state.ext.insert(state);
        }
    }
}

/// Handler `editor`. В `on_pump` разбирает `ViewInbox`.
#[derive(Debug, Default)]
pub struct EditorHandler;

impl Handler for EditorHandler {
    fn namespace(&self) -> &'static str {
        "editor"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &EDITOR_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        Err(not_yet(cmd))
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let mut changed = false;
        while let Some(msg) = view.inbox.try_next() {
            changed |= apply(view, msg);
        }
        if changed {
            cx.state.touch();
        }
    }
}

/// Применяет событие; `true` — состояние изменилось.
fn apply(view: &mut ViewState, msg: InboxMsg) -> bool {
    match msg {
        InboxMsg::Edited { doc, dirty, cursor } => {
            let Some(d) = view.editor_mut(doc) else {
                tracing::debug!(?doc, "view inbox: edit for a closed document");
                return false;
            };
            if d.dirty == dirty && d.cursor == cursor {
                return false;
            }
            d.dirty = dirty;
            d.cursor = cursor;
            d.rev += 1;
            true
        }
        // Загрузка и сохранение — S7-15, S7-17.
        InboxMsg::Adopted(doc) => {
            tracing::debug!(?doc, "view inbox: adopted");
            false
        }
        InboxMsg::Snapshot { doc, then, .. } => {
            tracing::debug!(?doc, ?then, "view inbox: snapshot is not handled yet");
            false
        }
    }
}
