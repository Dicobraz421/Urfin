//! Handlers `viewer` и `editor`: команды S3 §3.3/§3.7/§3.8 и S7 §7. S7-08 — открытие,
//! закрытие, режимы и кодировки; S7-09 — поиск; S7-07 — прокрутка и видимая область
//! (`InboxMsg::Viewport`); S7-10 — строки поиска и перехода (`Focus::Overlay(Find)`),
//! `viewer.goto`, `viewer.back`, закладки. Остальное — заглушки («пока не сделано»
//! тостом) до карточек S7-11…S7-19.

use urfin_core::{
    ArgSpec, Args, CmdError, Command, CommandId, CommandMeta, Cx, Focus, Handler, Level, Location,
    OverlayKind, Row, UiEvent, ValueKind,
};

use crate::detect::tld_for_lang;
use crate::layout::Mode;
use crate::search::{Pattern, SearchError, SearchReq};
use crate::source::spool_dir;
use crate::viewer::Scroll;
use crate::{Doc, DocId, InboxMsg, Placement, PlacementCfg, ViewEnv, ViewState};

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
    env: ViewEnv,
}

impl ViewerHandler {
    pub fn new(state: ViewState, env: ViewEnv) -> ViewerHandler {
        ViewerHandler {
            pending: Some(state),
            env,
        }
    }

    /// `viewer.open`: путь из аргумента или под курсором активной панели.
    fn open(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let a = &cmd.args;
        if a.bool("alt").unwrap_or(false) {
            // Внешний просмотрщик и маски `[[viewer.assoc]]` — S7-14.
            return Err(not_yet(cmd));
        }
        let loc = if let Some(l) = a.loc("path") {
            if l.path.is_relative() {
                // Относительный путь — от каталога активного таба.
                let tab = &cx.state.active_tab().loc;
                Location {
                    backend: tab.backend.clone(),
                    path: tab.path.join(&l.path),
                    outer: tab.outer.clone(),
                }
            } else {
                l
            }
        } else {
            let tab = cx.state.active_tab();
            match tab.current() {
                Some(Row::Entry { entry, .. }) if entry.kind.is_dir_like() => {
                    // F3 на каталоге — размер (S7 §3.1 п. 1).
                    cx.dispatch(Command::new("vfs.dir_size"));
                    return Ok(());
                }
                Some(Row::Entry { entry, .. }) => Location {
                    backend: tab.loc.backend.clone(),
                    path: tab.loc.path.join(entry.name.as_str()),
                    outer: tab.loc.outer.clone(),
                },
                _ => {
                    cx.keep_rev();
                    return Ok(());
                }
            }
        };
        let passive = cx.state.active.other();
        let rt = cx.rt;
        let view = view_mut(cx)?;
        let placement = match a.str("placement") {
            Some("panel") => Placement::Panel(passive),
            Some("window") => Placement::Window,
            Some(other) => {
                return Err(CmdError::Arg {
                    name: "placement".into(),
                    msg: format!("expected window or panel, got {other}").into(),
                });
            }
            None if view.viewer.placement == PlacementCfg::Panel => Placement::Panel(passive),
            None => Placement::Window,
        };
        let lang = urfin_i18n::current().lang().to_string();
        let id = crate::viewer::open(
            view,
            rt,
            &self.env.vfs,
            loc,
            placement,
            false,
            self.env.cache_dir.as_deref().map(spool_dir),
            tld_for_lang(&lang),
        );
        view.screens.push(id);
        set_focus(cx, Focus::Viewer);
        Ok(())
    }

    /// Команды над активным документом вьювера.
    #[allow(clippy::too_many_lines, reason = "таблица команд документа")]
    fn on_doc(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let rt = cx.rt;
        let view = view_mut(cx)?;
        let Some(id) = view.active_viewer() else {
            cx.keep_rev();
            return Ok(());
        };
        if cmd.id.as_str() == "viewer.close" {
            let remember = view.viewer.remember_pos;
            if let Some(Doc::Viewer(mut d)) = view.remove(id) {
                d.close();
                if remember && d.is_ready() {
                    view.history.record(d.memo());
                }
            }
            refocus(cx);
            return Ok(());
        }
        let a = &cmd.args;
        let ring = view.viewer.codepage_ring.clone();
        let wrap = view.viewer.search_wrap;
        let Some(d) = view.viewer_mut(id) else {
            return Ok(());
        };
        let mut ext = None;
        // Фокус после команды: строка поиска/перехода открыта (`Find`) или закрыта.
        let mut focus = None;
        let mut toast = None;
        let changed = match cmd.id.as_str() {
            "viewer.wrap" => {
                d.toggle_wrap();
                true
            }
            "viewer.wrap_mode" => {
                d.toggle_wrap_kind();
                true
            }
            "viewer.hex" => d.toggle_hex(rt),
            "viewer.mode" => match a.str("mode") {
                None => {
                    ext = Some("viewer.mode");
                    false
                }
                Some(m) => d.set_mode(parse_mode(m)?, rt),
            },
            "viewer.codepage" => match a.str("cp") {
                Some(cp) => d.set_codepage(cp, rt).ok_or_else(|| CmdError::Arg {
                    name: "cp".into(),
                    msg: format!("unknown encoding {cp}").into(),
                })?,
                None => d.next_codepage(&ring, rt),
            },
            "viewer.codepage_menu" => {
                ext = Some("viewer.codepage_menu");
                false
            }
            "viewer.search" => match a.str("text") {
                None => {
                    // Строка поиска — UI (S7-10); фокус — у неё.
                    ext = Some("viewer.search");
                    focus = Some(Focus::Overlay(OverlayKind::Find));
                    false
                }
                Some(text) => {
                    let back = a.bool("back").unwrap_or(false);
                    let pat = search_pattern(a, text)?;
                    match pat {
                        Ok(pat) => d.search(SearchReq { pat, back, wrap }, rt),
                        Err(e) => d.search_failed(
                            SearchReq {
                                pat: Pattern::Hex(Vec::new()),
                                back,
                                wrap,
                            },
                            e,
                        ),
                    }
                    true
                }
            },
            "viewer.search_next" | "viewer.search_prev" => {
                let back = cmd.id.as_str() == "viewer.search_prev";
                if !d.search_again(back, rt) {
                    ext = Some("viewer.search");
                    focus = Some(Focus::Overlay(OverlayKind::Find));
                }
                true
            }
            "viewer.search_cancel" => {
                // Esc в строке поиска или перехода: идущий поиск и ожидание индекса
                // отменяются, строка закрывается.
                d.search_cancel();
                d.goto_cancel();
                focus = Some(Focus::Viewer);
                true
            }
            "viewer.goto" => match a.str("to") {
                None => {
                    ext = Some("viewer.goto");
                    focus = Some(Focus::Overlay(OverlayKind::Find));
                    false
                }
                Some(to) => {
                    let g = crate::goto::parse(to).map_err(|e| CmdError::Arg {
                        name: "to".into(),
                        msg: e.to_string().into(),
                    })?;
                    d.goto(g, rt);
                    if d.goto_wait.is_some() {
                        // Ждём индекс: строка перехода показывает прогресс, Esc отменяет.
                        ext = Some("viewer.goto");
                        focus = Some(Focus::Overlay(OverlayKind::Find));
                    } else {
                        d.take_goto_done();
                        focus = Some(Focus::Viewer);
                    }
                    true
                }
            },
            "viewer.back" => d.back(rt),
            "viewer.bookmark.set" => {
                let n = bookmark_no(a)?;
                d.bookmark_set(n);
                toast = Some(bookmark_text("ui-view-bookmark-set", n));
                true
            }
            "viewer.bookmark.go" => {
                let n = bookmark_no(a)?;
                let went = d.bookmark_go(n, rt);
                if !went {
                    toast = Some(bookmark_text("ui-view-bookmark-none", n));
                }
                went
            }
            "viewer.scroll" => {
                let dir = a.str("dir").unwrap_or_default();
                let s = Scroll::parse(dir, a.int("n")).ok_or_else(|| CmdError::Arg {
                    name: "dir".into(),
                    msg: format!(
                        "expected up, down, page_up, page_down, home, end, left or right, got {dir}"
                    )
                    .into(),
                })?;
                d.scroll(s, rt)
            }
            _ => return Err(not_yet(cmd)),
        };
        if changed {
            // Режим, перенос, кодировка — другие видимые байты: страницы просит handler.
            d.settle(rt);
        }
        if let Some(e) = ext {
            cx.ui(UiEvent::Ext {
                id: CommandId::from_static(e),
                args: Args::default().with("doc", id.0),
            });
        }
        match focus {
            Some(Focus::Viewer) if cx.state.focus == Focus::Overlay(OverlayKind::Find) => {
                set_focus(cx, Focus::Viewer);
            }
            Some(f @ Focus::Overlay(_)) => set_focus(cx, f),
            _ => {}
        }
        if let Some(t) = toast {
            cx.toast(Level::Info, t);
        }
        if !changed && ext.is_none() && focus.is_none() {
            cx.keep_rev();
        }
        Ok(())
    }
}

/// Номер закладки `n`: 0…9.
fn bookmark_no(a: &Args) -> Result<usize, CmdError> {
    a.int("n")
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n < crate::viewer::BOOKMARKS)
        .ok_or_else(|| CmdError::Arg {
            name: "n".into(),
            msg: "expected a bookmark number 0..9".into(),
        })
}

fn bookmark_text(key: &str, n: usize) -> String {
    let mut fa = urfin_i18n::FluentArgs::new();
    fa.set("n", n as u64);
    urfin_i18n::current().fmt(key, &fa)
}

/// Переход, ждавший индекса или страниц, состоялся: строка перехода закрывается (фокус —
/// вьюверу). Зовут `IndexMsg` и `PagesMsg`.
pub(crate) fn after_goto(cx: &mut Cx<'_>, doc: DocId) {
    let done = cx
        .state
        .ext
        .get_mut::<ViewState>()
        .and_then(|v| v.viewer_mut(doc))
        .is_some_and(crate::ViewerDoc::take_goto_done);
    if done && cx.state.focus == Focus::Overlay(OverlayKind::Find) {
        set_focus(cx, Focus::Viewer);
    }
}

/// Образец `viewer.search` по `kind` (`text` по умолчанию); ошибка hex — во внутреннем
/// `Err` (её показывает строка поиска).
fn search_pattern(a: &Args, text: &str) -> Result<Result<Pattern, SearchError>, CmdError> {
    let case = a.bool("case").unwrap_or(false);
    Ok(match a.str("kind").unwrap_or("text") {
        "text" => Ok(Pattern::Text {
            s: text.to_owned(),
            case,
            word: a.bool("word").unwrap_or(false),
        }),
        "regex" => Ok(Pattern::Regex {
            s: text.to_owned(),
            case,
        }),
        "hex" => crate::search::hex::parse(text).map(Pattern::Hex),
        other => {
            return Err(CmdError::Arg {
                name: "kind".into(),
                msg: format!("expected text, regex or hex, got {other}").into(),
            });
        }
    })
}

fn view_mut<'a>(cx: &'a mut Cx<'_>) -> Result<&'a mut ViewState, CmdError> {
    cx.state
        .ext
        .get_mut::<ViewState>()
        .ok_or_else(|| CmdError::Rejected("viewer is not ready".into()))
}

fn parse_mode(m: &str) -> Result<Mode, CmdError> {
    match m {
        "text" => Ok(Mode::Text),
        "markup" => Ok(Mode::Markup),
        "hex" => Ok(Mode::Hex),
        other => Err(CmdError::Arg {
            name: "mode".into(),
            msg: format!("expected text, markup or hex, got {other}").into(),
        }),
    }
}

fn set_focus(cx: &mut Cx<'_>, f: Focus) {
    if cx.state.focus != f {
        cx.state.focus = f;
        cx.ui(UiEvent::FocusChanged(f));
    }
}

/// Фокус после закрытия документа: верхний экран (вьювер или редактор) или панели.
pub(crate) fn refocus(cx: &mut Cx<'_>) {
    let want = match cx
        .state
        .ext
        .get::<ViewState>()
        .and_then(|v| v.doc(v.active()?))
    {
        Some(Doc::Viewer(_)) => Focus::Viewer,
        Some(Doc::Editor(_)) => Focus::Editor,
        None => Focus::Panel,
    };
    let ours = matches!(
        cx.state.focus,
        Focus::Viewer | Focus::Editor | Focus::Overlay(OverlayKind::Find)
    );
    if ours {
        set_focus(cx, want);
    }
}

/// Id документа из `UiEvent::Ext` (`args.doc`): `viewer.mode` / `viewer.codepage_menu` —
/// открыть меню, `viewer.search` — строку поиска, `viewer.goto` — строку перехода.
pub fn ext_doc(args: &Args) -> Option<DocId> {
    args.int("doc")
        .and_then(|n| u32::try_from(n).ok())
        .map(DocId)
}

impl Handler for ViewerHandler {
    fn namespace(&self) -> &'static str {
        "viewer"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &VIEWER_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "viewer.open" => self.open(cmd, cx),
            "viewer.close"
            | "viewer.wrap"
            | "viewer.wrap_mode"
            | "viewer.hex"
            | "viewer.mode"
            | "viewer.codepage"
            | "viewer.codepage_menu"
            | "viewer.search"
            | "viewer.search_next"
            | "viewer.search_prev"
            | "viewer.search_cancel"
            | "viewer.goto"
            | "viewer.back"
            | "viewer.bookmark.set"
            | "viewer.bookmark.go"
            | "viewer.scroll" => Self::on_doc(cmd, cx),
            _ => Err(not_yet(cmd)),
        }
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some(state) = self.pending.take() {
            cx.state.ext.insert(state);
            // `on_pump` — в UI-потоке: чтение источника отсюда — ошибка (S7 §0).
            crate::source::mark_ui_thread();
        }
    }
}

/// Handler `editor`. В `on_pump` разбирает `ViewInbox` (и `Viewport` вьювера).
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
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let mut changed = false;
        while let Some(msg) = view.inbox.try_next() {
            changed |= apply(view, msg, rt);
        }
        if changed {
            cx.state.touch();
        }
    }
}

/// Применяет событие; `true` — состояние изменилось.
fn apply(view: &mut ViewState, msg: InboxMsg, rt: &urfin_core::Runtime) -> bool {
    match msg {
        InboxMsg::Viewport { doc, cols, rows } => {
            let Some(d) = view.viewer_mut(doc) else {
                tracing::debug!(?doc, "view inbox: viewport for a closed document");
                return false;
            };
            d.set_viewport(cols, rows, rt)
        }
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
