//! Handlers `viewer` и `editor`: команды S3 §3.3/§3.7/§3.8 и S7 §7. S7-08 — открытие,
//! закрытие, режимы и кодировки; S7-09 — поиск; S7-07 — прокрутка и видимая область
//! (`InboxMsg::Viewport`); S7-10 — строки поиска и перехода (`Focus::Overlay(Find)`),
//! `viewer.goto`, `viewer.back`, закладки; S7-14 — соседние файлы, `viewer.reveal`,
//! история (файл, Alt+F11), экраны (`viewer.screen`), внешний просмотр и
//! `[[viewer.assoc]]`, ссылки `.md` в том же экране, перечитывание `[viewer]`. Остальное —
//! заглушки («пока не сделано» тостом) до карточек S7-11…S7-19.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use urfin_core::{
    AppState, ArgSpec, Args, CancelToken, CmdError, Command, CommandId, CommandMeta, Config, Cx,
    Focus, Handler, Lane, Level, Location, Mask, OverlayKind, QuitDecision, Row, Side, Tab,
    UiEvent, ValueKind, settings,
};

use crate::detect::tld_for_lang;
use crate::history::{HISTORY_DEBOUNCE, ViewHistory, ViewMemo};
use crate::inbox::After;
use crate::layout::Mode;
use crate::search::{Pattern, SearchError, SearchReq};
use crate::source::spool_dir;
use crate::viewer::Scroll;
use crate::{
    Doc, DocId, EditorCfg, InboxMsg, Placement, PlacementCfg, ViewEnv, ViewState, ViewerCfg,
};

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

/// `viewer.open`: путь (иначе — под курсором), внешний, всегда встроенный, размещение
/// (`window` | `panel` | `here` — вместо активного документа, ссылка `.md`), якорь
/// заголовка Markdown, строка с 1 (F6 из редактора).
const VIEWER_OPEN: [ArgSpec; 6] = [
    arg("path", ValueKind::Loc, false),
    arg("alt", ValueKind::Bool, false),
    arg("internal", ValueKind::Bool, false),
    arg("placement", ValueKind::Str, false),
    arg("anchor", ValueKind::Str, false),
    arg("line", ValueKind::Int, false),
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
pub static VIEWER_COMMANDS: [CommandMeta; 28] = [
    v("viewer.open", "View", &VIEWER_OPEN),
    v(
        "viewer.quick",
        "Quick view",
        &[
            arg("side", ValueKind::Str, true),
            arg("path", ValueKind::Loc, true),
            arg("focus", ValueKind::Bool, false),
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
    v(
        "viewer.reveal",
        "Show file in panel",
        &[arg("path", ValueKind::Loc, false)],
    ),
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
    v(
        "viewer.screen",
        "Switch to screen",
        &[arg("doc", ValueKind::Int, true)],
    ),
];

/// `editor.md.heading`: уровень 0–3 (0 — обычный текст); без него — меню.
const LEVEL: [ArgSpec; 1] = [arg("level", ValueKind::Int, false)];

/// Команды редактора (S3 §3.3, §3.8; S7 §7). Нажатия, undo и IME — `EditorState` в UI;
/// `editor.input` нет (решение 2026-10-30 №1). Панель инструментов и Markdown — S7 §5.8,
/// §5.9 (решение 2026-10-30).
pub static EDITOR_COMMANDS: [CommandMeta; 36] = [
    e("editor.open", "Edit", &EDITOR_OPEN),
    e("editor.new", "New file", &EDITOR_OPEN),
    e("editor.save", "Save", &[]),
    e(
        "editor.save_as",
        "Save as…",
        &[arg("path", ValueKind::Loc, false)],
    ),
    e("editor.save_all", "Save all", &[]),
    e("editor.save_close", "Save and close", &[]),
    e("editor.close", "Close editor", &[]),
    // Ответ на inline-вопрос редактора (S7 §5.4): кнопки строки, Esc — `cancel`.
    e(
        "editor.answer",
        "Answer editor question",
        &[
            arg("choice", ValueKind::Str, true),
            arg("doc", ValueKind::Int, false),
        ],
    ),
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
    e("editor.eol_menu", "Line endings…", &[]),
    e("editor.wrap", "Wrap lines", &[]),
    e("editor.toolbar", "Toolbar", &[]),
    e("editor.preview", "Preview", &[]),
    er("editor.md.bold", "Bold"),
    er("editor.md.italic", "Italic"),
    e("editor.md.heading", "Heading", &LEVEL),
    er("editor.md.list", "Bulleted list"),
    er("editor.md.numbered", "Numbered list"),
    er("editor.md.task", "Task list"),
    er("editor.md.link", "Link"),
    er("editor.md.code", "Code"),
    er("editor.md.table", "Table"),
];

pub(crate) fn not_yet(cmd: &Command) -> CmdError {
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
    /// Конфиг, из которого прочитан `[viewer]`: другой `Arc` — `app.reload_config`.
    seen: Option<Arc<Config>>,
    /// `state_dir/view-history.json`; `None` — история только в памяти.
    hist_file: Option<PathBuf>,
    hist: HistSync,
}

/// Запись истории в файл: debounce от последнего изменения.
#[derive(Debug, Default)]
struct HistSync {
    /// `ViewHistory::rev`, записанное в файл (или ушедшее в задачу записи).
    saved: u64,
    /// Последнее увиденное `rev` и когда оно сменилось.
    seen: u64,
    changed: Option<Instant>,
}

/// Файлов в «назад» по ссылкам `.md` — не больше.
const FILES_BACK: usize = 32;

impl ViewerHandler {
    pub fn new(state: ViewState, env: ViewEnv) -> ViewerHandler {
        let hist_file = env.history_file();
        ViewerHandler {
            pending: Some(state),
            env,
            seen: None,
            hist_file,
            hist: HistSync::default(),
        }
    }

    /// `viewer.open`: путь из аргумента или под курсором активной панели.
    fn open(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let a = &cmd.args;
        let here = a.str("placement") == Some("here");
        // Ссылка `.md` из документа: вместо него, относительный путь — от его папки.
        let replace = here
            .then(|| cx.state.ext.get::<ViewState>()?.active_viewer())
            .flatten();
        let from_cursor = a.loc("path").is_none();
        let loc = if let Some(l) = a.loc("path") {
            if l.path.is_relative() {
                let base = replace
                    .and_then(|id| cx.state.ext.get::<ViewState>()?.viewer(id))
                    .map_or_else(|| cx.state.active_tab().loc.clone(), |d| parent_loc(&d.loc));
                Location {
                    path: base.path.join(&l.path),
                    ..base
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
        if a.bool("alt").unwrap_or(false) {
            return external(cx, &loc, from_cursor);
        }
        if !a.bool("internal").unwrap_or(false) && assoc(cx, &loc) {
            return Ok(());
        }
        let anchor = a.str("anchor").map(str::to_owned);
        if let Some(old) = replace {
            return self.follow_link(cx, old, loc, anchor);
        }
        let source = cx.state.active_tab().id;
        let passive = cx.state.active.other();
        let rt = cx.rt;
        let view = view_mut(cx)?;
        let placement = match a.str("placement") {
            Some("panel") => Placement::Panel(passive),
            Some("window" | "here") => Placement::Window,
            Some(other) => {
                return Err(CmdError::Arg {
                    name: "placement".into(),
                    msg: format!("expected window, panel or here, got {other}").into(),
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
        let line = a.int("line").and_then(|n| u64::try_from(n.max(1) - 1).ok());
        if let Some(d) = view.viewer_mut(id) {
            d.source = Some(source);
            d.set_anchor(anchor);
            d.start_line = line;
        }
        view.screens.push(id);
        set_focus(cx, Focus::Viewer);
        Ok(())
    }

    /// Ссылка `.md` из документа `old`: файл открывается в этом же экране, прежний — в
    /// «назад» (`viewer.back`). Тот же файл — только переход к якорю.
    fn follow_link(
        &self,
        cx: &mut Cx<'_>,
        old: DocId,
        loc: Location,
        anchor: Option<String>,
    ) -> Result<(), CmdError> {
        let rt = cx.rt;
        let view = view_mut(cx)?;
        let Some(d) = view.viewer_mut(old) else {
            cx.keep_rev();
            return Ok(());
        };
        if d.loc == loc {
            if let Some(a) = anchor {
                d.md_jump_anchor(a);
                d.settle(rt);
            } else {
                cx.keep_rev();
            }
            return Ok(());
        }
        let mut back = d.files_back.clone();
        if d.is_ready() {
            back.push(d.memo());
        }
        if back.len() > FILES_BACK {
            back.remove(0);
        }
        self.reopen(cx, old, loc, None, anchor, back)?;
        Ok(())
    }

    /// Документ `old` переоткрывается на месте файлом `loc` (соседний файл, ссылка `.md`,
    /// «назад» к файлу): экран тот же, `old` уходит в историю. `memo` — с какой позиции и
    /// в каком режиме (`None` — из истории).
    fn reopen(
        &self,
        cx: &mut Cx<'_>,
        old: DocId,
        loc: Location,
        memo: Option<ViewMemo>,
        anchor: Option<String>,
        files_back: Vec<ViewMemo>,
    ) -> Result<DocId, CmdError> {
        let rt = cx.rt;
        let lang = urfin_i18n::current().lang().to_string();
        let view = view_mut(cx)?;
        let Some((placement, quick, source, wrap_kind)) = view
            .viewer(old)
            .map(|d| (d.placement, d.quick, d.source, d.wrap_kind))
        else {
            return Err(CmdError::Rejected("viewer is closed".into()));
        };
        let id = crate::viewer::open(
            view,
            rt,
            &self.env.vfs,
            loc,
            placement,
            quick,
            self.env.cache_dir.as_deref().map(spool_dir),
            tld_for_lang(&lang),
        );
        view.replace_screen(old, id);
        let remember = view.viewer.remember_pos;
        if let Some(Doc::Viewer(mut d)) = view.remove(old) {
            d.close();
            if remember && d.is_ready() {
                view.history.record(d.memo());
            }
        }
        if let Some(d) = view.viewer_mut(id) {
            d.source = source;
            d.wrap_kind = wrap_kind;
            d.files_back = files_back;
            if memo.is_some() {
                d.set_memo(memo);
            }
            d.set_anchor(anchor);
        }
        refocus(cx);
        Ok(id)
    }

    /// `viewer.next_file` / `prev_file` (Gray+ / Gray−): соседний файл в порядке таба,
    /// откуда открыт документ; каталоги пропускаются. Режим и перенос — как были,
    /// кодировка — `auto`, позиция — из истории.
    fn neighbor(&self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let back = cmd.id.as_str() == "viewer.prev_file";
        let view = view_mut(cx)?;
        let Some(id) = view.active_viewer() else {
            cx.keep_rev();
            return Ok(());
        };
        let remember = view.viewer.remember_pos;
        let Some(d) = view.viewer(id) else {
            cx.keep_rev();
            return Ok(());
        };
        let (mode, wrap, cur, source) = (d.mode, d.wrap, d.loc.clone(), d.source);
        let next = {
            let st: &AppState = cx.state;
            let tab = source
                .and_then(|t| st.tab(t))
                .map_or_else(|| st.active_tab(), |(_, t)| t);
            neighbor_loc(tab, &cur, back)
        };
        let Some(next) = next else {
            cx.keep_rev();
            return Ok(());
        };
        let view = view_mut(cx)?;
        let mut m = remember
            .then(|| view.history.get(&next).cloned())
            .flatten()
            .unwrap_or_else(|| ViewMemo::start(next.clone()));
        m.enc = None;
        m.mode = mode;
        m.wrap = wrap;
        self.reopen(cx, id, next, Some(m), None, Vec::new())?;
        Ok(())
    }

    /// `viewer.back` (Alt+Backspace): прошлая позиция в файле, а если их нет — файл до
    /// перехода по ссылке `.md`.
    fn back(&self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let rt = cx.rt;
        let view = view_mut(cx)?;
        let Some(id) = view.active_viewer() else {
            cx.keep_rev();
            return Ok(());
        };
        let Some(d) = view.viewer_mut(id) else {
            cx.keep_rev();
            return Ok(());
        };
        if d.back(rt) {
            return Ok(());
        }
        let Some(m) = d.files_back.pop() else {
            cx.keep_rev();
            return Ok(());
        };
        let rest = std::mem::take(&mut d.files_back);
        let loc = m.loc.clone();
        self.reopen(cx, id, loc, Some(m), None, rest)?;
        Ok(())
    }

    /// `[viewer]` и `[editor]` после `app.reload_config` (`ui.setting`): новые значения —
    /// в `ViewState`, открытым документам — то, что меняет их вид (Markdown, картинки, таб,
    /// Hex). Первый проход только запоминает конфиг: секции прочитаны при регистрации.
    fn reload_cfg(&mut self, cx: &mut Cx<'_>) {
        match &self.seen {
            Some(c) if Arc::ptr_eq(c, &cx.state.config) => return,
            None => {
                self.seen = Some(Arc::clone(&cx.state.config));
                return;
            }
            Some(_) => {}
        }
        self.seen = Some(Arc::clone(&cx.state.config));
        let (viewer, warning) = cx.state.config.section::<ViewerCfg>("viewer");
        if let Some(w) = warning {
            tracing::warn!(warning = ?w, "[viewer] config section ignored");
        }
        let (editor, warning) = cx.state.config.section::<EditorCfg>("editor");
        if let Some(w) = warning {
            tracing::warn!(warning = ?w, "[editor] config section ignored");
        }
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if view.viewer == viewer && view.editor == editor {
            return;
        }
        view.history.set_max(viewer.history_max as usize);
        for d in view.viewers_mut() {
            d.apply_cfg(&viewer, rt);
        }
        view.viewer = viewer;
        view.editor = editor;
        cx.state.touch();
    }

    /// Файл истории: чтение в первом pump (Bulk), запись через 2 с после последнего
    /// изменения (Bulk). До чтения файл не пишется.
    fn sync_history(&mut self, cx: &mut Cx<'_>, load: bool) {
        let Some(file) = self.hist_file.clone() else {
            return;
        };
        if load {
            cx.rt
                .spawn(Lane::Bulk, "view.history.load", cx.rt.token(), move |t| {
                    let items = match std::fs::read_to_string(&file) {
                        Ok(text) => ViewHistory::from_json(&text),
                        Err(e) => {
                            if e.kind() != std::io::ErrorKind::NotFound {
                                tracing::warn!(error = %e, "view history is not readable");
                            }
                            Vec::new()
                        }
                    };
                    let apply = urfin_core::msg("view.history", move |cx| {
                        if let Some(v) = cx.state.ext.get_mut::<ViewState>() {
                            v.history.merge(items);
                        }
                        cx.keep_rev();
                    });
                    let _ = t.sink.send(apply, &t.cancel);
                });
            return;
        }
        let Some(view) = cx.state.ext.get::<ViewState>() else {
            return;
        };
        let rev = view.history.rev();
        if rev == self.hist.saved || !view.history.loaded() {
            return;
        }
        let now = Instant::now();
        if rev != self.hist.seen || self.hist.changed.is_none() {
            self.hist.seen = rev;
            self.hist.changed = Some(now);
            cx.rt.waker().wake_after(HISTORY_DEBOUNCE);
            return;
        }
        if self
            .hist
            .changed
            .is_some_and(|t| now.duration_since(t) < HISTORY_DEBOUNCE)
        {
            return;
        }
        let text = view.history.to_json();
        self.hist.saved = rev;
        self.hist.changed = None;
        cx.rt
            .spawn(Lane::Bulk, "view.history.save", cx.rt.token(), move |_| {
                save_history(&file, &text);
            });
    }

    /// Выход: позиции открытых документов — в историю, файл — сразу (выделенный поток:
    /// очереди полос при остановке Runtime не выполняются, выделенные потоки он ждёт).
    fn flush_history(&mut self, cx: &mut Cx<'_>) {
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if view.viewer.remember_pos {
            let memos: Vec<ViewMemo> = view
                .viewers_mut()
                .filter(|d| d.is_ready() && !d.quick)
                .map(|d| d.memo())
                .collect();
            for m in memos.into_iter().chain(view.edit_memos()) {
                view.history.record(m);
            }
        }
        let rev = view.history.rev();
        let Some(file) = self.hist_file.clone() else {
            return;
        };
        if rev == self.hist.saved || !view.history.loaded() {
            return;
        }
        let text = view.history.to_json();
        self.hist.saved = rev;
        self.hist.changed = None;
        let _ = cx.rt.spawn_dedicated(
            "view.history.save".to_owned(),
            CancelToken::new(),
            move |_| save_history(&file, &text),
        );
    }
}

/// Пишет файл истории: временный рядом и замена (на Windows `rename` заменяет файл).
fn save_history(file: &Path, text: &str) {
    let res = (|| {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, file)
    })();
    if let Err(e) = res {
        tracing::warn!(error = %e, "view history is not saved");
    }
}

/// Папка файла как `Location` (тот же backend и контейнер).
fn parent_loc(loc: &Location) -> Location {
    Location {
        backend: loc.backend.clone(),
        path: loc
            .path
            .parent()
            .map_or_else(|| loc.path.clone(), Path::to_path_buf),
        outer: loc.outer.clone(),
    }
}

/// Соседний файл `cur` в видимом порядке таба (каталоги пропускаются); `None` — файл не
/// из этого каталога или дальше файлов нет.
fn neighbor_loc(tab: &Tab, cur: &Location, back: bool) -> Option<Location> {
    if tab.loc.backend != cur.backend
        || tab.loc.outer != cur.outer
        || cur.path.parent() != Some(tab.loc.path.as_path())
    {
        return None;
    }
    let name = cur.path.file_name()?.to_str()?;
    let l = &tab.listing;
    let entry = |ix: u32| l.entries.get(ix as usize);
    let i = l
        .order
        .iter()
        .position(|&ix| entry(ix).is_some_and(|e| e.name.as_str() == name))
        .or_else(|| {
            // Путь набран вручную: регистр имени может отличаться (Windows).
            let low = name.to_lowercase();
            l.order
                .iter()
                .position(|&ix| entry(ix).is_some_and(|e| e.name.to_lowercase() == low))
        })?;
    let mut rest: Box<dyn Iterator<Item = &u32>> = if back {
        Box::new(l.order[..i].iter().rev())
    } else {
        Box::new(l.order[i + 1..].iter())
    };
    let e = rest.find_map(|&ix| entry(ix).filter(|e| !e.kind.is_dir_like()))?;
    Some(Location {
        backend: tab.loc.backend.clone(),
        path: tab.loc.path.join(e.name.as_str()),
        outer: tab.loc.outer.clone(),
    })
}

/// Alt+F3 (S7 §3.1 п. 1): `[viewer] external`. Пусто — программа Windows для типа файла
/// (`file.open` под курсором, `term.open` по пути); шаблон с `{path}` — командой в сеансе
/// терминала (`term.exec`). Только локальные файлы.
fn external(cx: &mut Cx<'_>, loc: &Location, from_cursor: bool) -> Result<(), CmdError> {
    if !loc.is_local() {
        return Err(CmdError::Rejected(
            urfin_i18n::current().get("ui-view-external-local").into(),
        ));
    }
    let tpl = view_mut(cx)?.viewer.external.trim().to_owned();
    if !tpl.is_empty() {
        run_template(cx, &tpl, &loc.path);
    } else if from_cursor {
        cx.dispatch(Command::new("file.open"));
    } else {
        cx.dispatch(Command::new("term.open").arg("path", loc.path.to_string_lossy().as_ref()));
    }
    Ok(())
}

/// Маски `[[viewer.assoc]]` (по умолчанию пусто): первая подошедшая запускает свою
/// программу. `true` — файл ушёл внешней программе.
fn assoc(cx: &mut Cx<'_>, loc: &Location) -> bool {
    let Some(view) = cx.state.ext.get::<ViewState>() else {
        return false;
    };
    if view.viewer.assoc.is_empty() || !loc.is_local() {
        return false;
    }
    let Some(name) = loc.path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let hit = view
        .viewer
        .assoc
        .iter()
        .find(|a| match Mask::parse(&a.mask) {
            Ok(m) => m.matches(name),
            Err(e) => {
                tracing::warn!(mask = %a.mask, error = %e, "[[viewer.assoc]] mask ignored");
                false
            }
        });
    let Some(cmd) = hit.map(|a| a.cmd.clone()) else {
        return false;
    };
    run_template(cx, &cmd, &loc.path);
    true
}

/// Шаблон команды: `{path}` → путь в кавычках (`"{path}"` в шаблоне — тоже), каталог —
/// папка файла.
fn run_template(cx: &mut Cx<'_>, tpl: &str, path: &Path) {
    let line = expand_template(tpl, path);
    let mut c = Command::new("term.exec").arg("cmd", line);
    if let Some(dir) = path.parent() {
        c = c.arg("cwd", dir.to_string_lossy().as_ref());
    }
    cx.dispatch(c);
}

/// `{path}` → `"путь"`; кавычки вокруг `{path}` в шаблоне не удваиваются.
pub(crate) fn expand_template(tpl: &str, path: &Path) -> String {
    let quoted = format!("\"{}\"", path.display());
    tpl.replace("\"{path}\"", "{path}")
        .replace("{path}", &quoted)
}

/// `viewer.reveal` (Ctrl+F10; ссылка на файл или папку в документе): панель-источник
/// документа идёт в папку файла и ставит курсор на него, фокус — в панель; вьювер
/// остаётся экраном (S7 §3.9). Без `path` — файл активного документа.
fn reveal(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let view = view_mut(cx)?;
    let doc = view.active_viewer().and_then(|id| view.viewer(id));
    let source = doc.and_then(|d| d.source);
    let Some(loc) = cmd.args.loc("path").or_else(|| doc.map(|d| d.loc.clone())) else {
        cx.keep_rev();
        return Ok(());
    };
    let Some(name) = loc
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        cx.keep_rev();
        return Ok(());
    };
    let dir = parent_loc(&loc);
    let (side, tab) = source
        .and_then(|t| cx.state.tab(t))
        .map_or_else(|| (cx.state.active, cx.state.active_tab()), |(s, t)| (s, t));
    let side = if side == Side::Left { "left" } else { "right" };
    let go = tab.loc != dir;
    cx.dispatch(Command::new("panel.focus").arg("side", side));
    if go {
        cx.dispatch(Command::new("panel.go").arg("path", dir).arg("side", side));
    }
    cx.dispatch(Command::new("panel.cursor.to").arg("name", name));
    Ok(())
}

/// `viewer.history` (Alt+F11): список просмотров и правок рисует UI (`UiEvent::Ext` с
/// `kind` — `view` | `edit` | все); пусто — тост.
fn history(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let kind = cmd.args.str("kind");
    if let Some(k) = kind
        && !matches!(k, "view" | "edit")
    {
        return Err(CmdError::Arg {
            name: "kind".into(),
            msg: format!("expected view or edit, got {k}").into(),
        });
    }
    if view_mut(cx)?.history.is_empty() {
        cx.toast(
            Level::Info,
            urfin_i18n::current().get("ui-view-history-empty"),
        );
        return Ok(());
    }
    let mut args = Args::default();
    if let Some(k) = kind {
        args = args.with("kind", k);
    }
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("viewer.history"),
        args,
    });
    Ok(())
}

/// `viewer.screen doc=` (F12, вкладка экрана): документ наверх, фокус — ему.
fn screen(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let doc = ext_doc(&cmd.args).ok_or_else(|| CmdError::Arg {
        name: "doc".into(),
        msg: "expected a document id".into(),
    })?;
    let view = view_mut(cx)?;
    if !view.activate(doc) {
        return Err(CmdError::Arg {
            name: "doc".into(),
            msg: format!("no screen {}", doc.0).into(),
        });
    }
    let f = if view.viewer(doc).is_some() {
        Focus::Viewer
    } else {
        Focus::Editor
    };
    set_focus(cx, f);
    Ok(())
}

impl ViewerHandler {
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
        let follow =
            crate::FollowCfg::new(view.viewer.follow_interval_ms, view.viewer.follow_reopen);
        let reg = view.vfs.clone();
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
                            rt,
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
                    // В документе Markdown строка перехода — в Тексте (S7 §3.10).
                    let left = d.leave_doc(rt);
                    ext = Some("viewer.goto");
                    focus = Some(Focus::Overlay(OverlayKind::Find));
                    left
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
            // Не локальный файл (S8) — follow пока не умеет.
            "viewer.follow" => d
                .toggle_follow(follow, &reg, rt)
                .map_err(|_| not_yet(cmd))?,
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

pub(crate) fn view_mut<'a>(cx: &'a mut Cx<'_>) -> Result<&'a mut ViewState, CmdError> {
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

pub(crate) fn set_focus(cx: &mut Cx<'_>, f: Focus) {
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
            "viewer.back" => self.back(cx),
            "viewer.next_file" | "viewer.prev_file" => self.neighbor(cmd, cx),
            "viewer.reveal" => reveal(cmd, cx),
            "viewer.history" => history(cmd, cx),
            "viewer.screen" => screen(cmd, cx),
            "viewer.to_editor" => to_editor(cx),
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
            | "viewer.bookmark.set"
            | "viewer.bookmark.go"
            | "viewer.scroll"
            | "viewer.follow" => Self::on_doc(cmd, cx),
            // S7-11, S7-13: `viewer/select.rs`, `viewer/quick.rs`.
            "viewer.copy" => {
                crate::viewer::copy(cx);
                Ok(())
            }
            "viewer.select_all" => {
                crate::viewer::select_all(cx);
                Ok(())
            }
            "viewer.quick" => crate::viewer::quick(cmd, cx, &self.env),
            "viewer.dock" => crate::viewer::dock(cx),
            _ => Err(not_yet(cmd)),
        }
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        let first = self.pending.is_some();
        if let Some(state) = self.pending.take() {
            cx.state.ext.insert(state);
            // `on_pump` — в UI-потоке: чтение источника отсюда — ошибка (S7 §0).
            crate::source::mark_ui_thread();
        }
        self.reload_cfg(cx);
        self.sync_history(cx, first);
        crate::viewer::quick_pump(cx);
    }

    fn on_quit(&mut self, cx: &mut Cx<'_>) -> QuitDecision {
        self.flush_history(cx);
        QuitDecision::Allow
    }
}

/// Handler `editor`. В `on_pump` разбирает `ViewInbox` (и `Viewport` вьювера), запускает
/// сохранение по снимкам, автосохранение и проверку диска по возврату на экран.
#[derive(Debug, Default)]
pub struct EditorHandler {
    /// Редактор с фокусом в прошлом pump: уход с экрана — автосохранение `focus`,
    /// возврат — сверка `DiskStamp`.
    seen: Option<DocId>,
}

/// `viewer.to_editor` (F6 в Far): встроенный редактор на строке верха экрана, вьювер
/// закрывается (позиция — в историю просмотра).
fn to_editor(cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let view = view_mut(cx)?;
    let Some(id) = view.active_viewer() else {
        cx.keep_rev();
        return Ok(());
    };
    let Some(d) = view.viewer(id) else {
        return Ok(());
    };
    let line = crate::viewer::visible_lines(d).map_or(0, |(first, _)| first);
    let loc = d.loc.clone();
    let remember = view.viewer.remember_pos;
    if let Some(Doc::Viewer(mut d)) = view.remove(id) {
        d.close();
        if remember && d.is_ready() {
            view.history.record(d.memo());
        }
    }
    cx.dispatch(
        Command::new("editor.open")
            .arg("path", loc)
            .arg("internal", true)
            .arg("line", i64::try_from(line + 1).unwrap_or(i64::MAX)),
    );
    Ok(())
}

impl Handler for EditorHandler {
    fn namespace(&self) -> &'static str {
        "editor"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &EDITOR_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "editor.wrap" | "editor.toolbar" | "editor.preview" | "editor.eol_menu" => {
                on_ui(cmd, cx)
            }
            id if id.starts_with("editor.md.") => on_ui(cmd, cx),
            "editor.open"
            | "editor.new"
            | "editor.close"
            | "editor.save"
            | "editor.save_as"
            | "editor.save_close"
            | "editor.save_all"
            | "editor.answer"
            | "editor.to_viewer"
            | "editor.codepage"
            | "editor.codepage_menu"
            | "editor.reload"
            | "editor.eol" => crate::edit::cmd::handle(cmd, cx),
            _ => Err(not_yet(cmd)),
        }
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let mut changed = false;
        let mut saves = Vec::new();
        let mut blur = false;
        while let Some(msg) = view.inbox.try_next() {
            match msg {
                InboxMsg::Snapshot { doc, text, then } if then != After::Diff => {
                    saves.push((doc, text, then));
                }
                InboxMsg::Blur => blur = true,
                m => changed |= apply(view, m, rt),
            }
        }
        for (doc, text, then) in saves {
            changed |= crate::edit::flow::start_save(
                cx,
                doc,
                text,
                then,
                crate::edit::flow::Flags::default(),
            );
        }
        // Редактор с фокусом: уход — автосохранение `focus`, возврат — отметка диска.
        let now = (cx.state.focus == Focus::Editor)
            .then(|| cx.state.ext.get::<ViewState>()?.active_editor())
            .flatten();
        if now != self.seen {
            if let Some(old) = self.seen {
                crate::edit::flow::autosave_leave(cx, old);
            }
            if let Some(new) = now {
                crate::edit::flow::check_stamp(cx, new);
            }
            self.seen = now;
        } else if blur && let Some(doc) = now {
            crate::edit::flow::autosave_leave(cx, doc);
        }
        crate::edit::flow::autosave_tick(cx);
        if changed {
            cx.state.touch();
        }
    }

    fn on_quit(&mut self, cx: &mut Cx<'_>) -> QuitDecision {
        crate::edit::flow::quit_decision(cx)
    }

    fn on_quit_reply(&mut self, choice: &str, cx: &mut Cx<'_>) -> urfin_core::QuitReply {
        crate::edit::flow::quit_reply(choice, cx)
    }
}

/// Команды панели и Markdown (S7-23a, §5.8, §5.9): текст живёт в `EditorState`, поэтому
/// выполняет их фича `Editor` в UI — `UiEvent::Ext` с id команды и `doc` (как меню
/// кодировки вьювера). Переключатели `editor.wrap` / `editor.toolbar` / `editor.preview`
/// меняют `[editor]` и пишут свой ключ в конфиг; в Ext — новое значение `on`.
/// `editor.md.*` и `editor.preview` — только у Markdown, иначе `Rejected`.
fn on_ui(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let id = cmd.id.as_str();
    let view = view_mut(cx)?;
    let doc = view
        .active()
        .filter(|d| matches!(view.doc(*d), Some(Doc::Editor(_))));
    let md_only = id == "editor.preview" || id.starts_with("editor.md.");
    if md_only || id == "editor.eol_menu" {
        let Some(d) = doc else {
            // Нет редактора на экране (палитра поверх панелей) — нечего делать.
            cx.keep_rev();
            return Ok(());
        };
        if md_only && !view.doc(d).is_some_and(|d| is_markdown(d.loc())) {
            return Err(CmdError::Rejected(
                urfin_i18n::current().get("ui-edit-md-only").into(),
            ));
        }
    }
    let mut args = Args::default();
    if let Some(d) = doc {
        args = args.with("doc", d.0);
    }
    let toggle = match id {
        "editor.wrap" => Some(("wrap", &mut view.editor.wrap)),
        "editor.toolbar" => Some(("toolbar", &mut view.editor.toolbar)),
        "editor.preview" => Some(("preview", &mut view.editor.preview)),
        _ => None,
    };
    let write = toggle.map(|(key, v)| {
        *v = !*v;
        (key, *v)
    });
    if let Some((key, on)) = write {
        args = args.with("on", on);
        settings::set(&["editor", key], on).apply(cx);
    }
    if id == "editor.md.heading"
        && let Some(level) = cmd.args.int("level")
    {
        if !(0..=3).contains(&level) {
            return Err(CmdError::Arg {
                name: "level".into(),
                msg: format!("expected 0..3, got {level}").into(),
            });
        }
        args = args.with("level", level);
    }
    cx.ui(UiEvent::Ext {
        id: cmd.id.clone(),
        args,
    });
    Ok(())
}

/// Markdown по имени файла (S7 §3.10: `.md`, `.markdown`, `.mdown`, `.mkd` —
/// `lang::detect`).
fn is_markdown(loc: &Location) -> bool {
    loc.path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| crate::lang::detect(n, &[]))
        == Some("markdown")
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
        InboxMsg::MdTop { doc, block, frac } => view
            .viewer_mut(doc)
            .is_some_and(|d| d.md_set_top(block, frac)),
        InboxMsg::MdToggle { doc, block } => {
            view.viewer_mut(doc).is_some_and(|d| d.md_toggle(block))
        }
        InboxMsg::MdSelect { doc, sel } => view.viewer_mut(doc).is_some_and(|d| d.md_select(sel)),
        InboxMsg::Select { doc, sel } => view.viewer_mut(doc).is_some_and(|d| d.select(sel)),
        InboxMsg::ScrollTo { doc, off } => {
            view.viewer_mut(doc).is_some_and(|d| d.scroll_to(off, rt))
        }
        InboxMsg::Scroll { doc, scroll } => {
            view.viewer_mut(doc).is_some_and(|d| d.scroll(scroll, rt))
        }
        InboxMsg::Edited { doc, dirty, cursor } => {
            let Some(d) = view.editor_mut(doc) else {
                tracing::debug!(?doc, "view inbox: edit for a closed document");
                return false;
            };
            if dirty {
                // Автосохранение `delay` — от последнего события правки.
                d.edited_at = Some(Instant::now());
            }
            if d.dirty == dirty && d.cursor == cursor {
                return false;
            }
            d.dirty = dirty;
            d.cursor = cursor;
            d.rev += 1;
            true
        }
        InboxMsg::Adopted(doc) => crate::edit::cmd::adopted(view, doc),
        // Diff — полоса изменений (S7-15); сохранение разбирает `on_pump` (S7-17).
        InboxMsg::Snapshot { doc, text, .. } => {
            crate::edit::cmd::diff(view, doc, text, rt);
            false
        }
        InboxMsg::Blur => false,
    }
}
