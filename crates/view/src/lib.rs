//! Urfin — вьювер (F3, быстрый просмотр) и редактор (F4): модель документов, команды
//! `viewer.*` / `editor.*`, конфиг `[viewer]` / `[editor]` (S7). Без gpui: экраны — фичи
//! окна в `urfin-ui` (`gpui_shell/view`).
//!
//! S7-01 — каркас: `ViewState` в `AppState.ext`, handlers с командами-заглушками,
//! `ViewInbox`. S7-02…S7-06 — источники, кэш страниц, детект, индекс строк, раскладка;
//! S7-08 — открытие, режимы, кодировки (`viewer`); S7-09 — поиск (`search`); S7-07 —
//! видимая область и прокрутка (`viewer::viewport`, экран — в UI); S7-10 — переход,
//! «назад», закладки (`goto`, `viewer::jump`); S7-22 — Markdown-документ (`md`, режим
//! документа у `ViewerDoc`); S7-14 — экраны, соседние файлы, история в файле, внешний
//! просмотр, переходы по ссылкам `.md`, перечитывание `[viewer]`; остальные модули §1
//! наполняют S7-11…S7-20 (`docs/specs/S7.md` §10).

pub mod cache;
pub mod config;
pub mod detect;
pub mod edit;
pub mod enc;
pub mod goto;
pub mod handler;
pub mod history;
pub mod inbox;
pub mod index;
pub mod lang;
pub mod layout;
pub mod md;
pub mod search;
pub mod source;
mod sys;
pub mod viewer;

pub use cache::{ByteView, Bytes, Missing, Page, PageCache, Prio};
pub use config::{
    Assoc, Autosave, EditorCfg, EditorDefault, EolCfg, HexCols, MdView, PlacementCfg, ViewerCfg,
    WrapCfg,
};
pub use detect::{Detected, Eol, detect};
pub use edit::{AskChoice, Change, DiskStamp, EdAsk, EdState, EditorDoc, LineChanges};
pub use enc::Enc;
pub use goto::{Goto, GotoError};
pub use handler::{EDITOR_COMMANDS, EditorHandler, VIEWER_COMMANDS, ViewerHandler};
pub use history::{HistKind, ViewHistory, ViewMemo};
pub use inbox::{After, InboxMsg, InboxSender, ViewInbox};
pub use index::{LineGuess, LineIndex};
pub use layout::{LayoutSpec, Mode, RowText, VRow, WrapKind};
pub use md::{Block, BlockKind, MdDoc, Run, RunStyle};
pub use search::{Matcher, Pattern, SearchError, SearchReq, SearchState, compile};
pub use source::{ByteSource, SourceKind, SourceOpts, open_source};
pub use viewer::{
    CpGroup, FollowCfg, FollowError, FollowEvent, GotoWait, MdState, OpenState, QUICK_DOC_MAX,
    QUICK_READS, QuickNote, QuickNoteKind, QuickTarget, Scroll, ViewerDoc, codepage_menu,
    decode_range, line_count, panel_shows, quick_target, visible_lines,
};

use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

use urfin_core::{Config, Core, Location, Side};
use urfin_vfs::VfsRegistry;

/// Документ вьювера или редактора: ключ в `ViewState`, вкладках экранов и в UI.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct DocId(pub u32);

/// Где показан документ: во весь экран поверх панелей или в области панели
/// (быстрый просмотр, `viewer.dock`).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Placement {
    Window,
    Panel(Side),
}

/// Документ в `ViewState`.
#[allow(
    clippy::large_enum_variant,
    reason = "документов единицы, лежат в карте по id"
)]
#[derive(Debug)]
pub enum Doc {
    Viewer(ViewerDoc),
    Editor(EditorDoc),
}

impl Doc {
    pub fn id(&self) -> DocId {
        match self {
            Doc::Viewer(d) => d.id,
            Doc::Editor(d) => d.id,
        }
    }

    pub fn loc(&self) -> &Location {
        match self {
            Doc::Viewer(d) => &d.loc,
            Doc::Editor(d) => &d.loc,
        }
    }
}

/// Состояние вьювера и редактора в `AppState.ext` (S0 v2 №10). Мутации — только
/// handlers `viewer`/`editor` и их `Msg`; UI читает и шлёт события в `inbox`.
#[derive(Debug)]
pub struct ViewState {
    docs: FxHashMap<DocId, Doc>,
    next: u32,
    /// Порядок экранов для `ui.screens` (F12) и вкладок; последний — активный.
    pub screens: Vec<DocId>,
    /// Документ в области панели по стороне (`Side::idx`): быстрый просмотр
    /// (`ViewerDoc::quick`) или вьювер, перенесённый туда `viewer.dock` (S7 §4).
    pub quick: [Option<DocId>; 2],
    /// Файл области панели без документа: в облаке (`Attrs::OFFLINE`, не читается) или
    /// не открылся (S7 §4).
    pub qnote: [Option<QuickNote>; 2],
    /// Документ области, чьё чтение идёт (не больше одного на сторону, S7 §4).
    pub qflight: [Option<DocId>; 2],
    /// Фокус у документа в области панели (Tab из панели, S7 §4): сторона и число экранов
    /// в тот момент — новый экран (F3, F4) фокус у панели забирает.
    pub qfocus: Option<(Side, usize)>,
    /// События редактора UI → handler, разбор в `on_pump` (CONTRACTS «Ввод и клавиатура»).
    pub inbox: ViewInbox,
    /// Позиция, кодировка и режим по файлам (№36).
    pub history: ViewHistory,
    pub viewer: ViewerCfg,
    pub editor: EditorCfg,
    /// Реестр VFS приложения: открытие источников и загрузка редактора.
    pub(crate) vfs: Arc<VfsRegistry>,
    /// «Сохранить все» в вопросе выхода: документы, чьё сохранение ждём; все
    /// сохранены — снова `app.quit` (решение 2026-10-31 «quit-reply»).
    pub(crate) quit_save: Option<Vec<DocId>>,
}

impl ViewState {
    pub fn new(viewer: ViewerCfg, editor: EditorCfg) -> ViewState {
        ViewState {
            docs: FxHashMap::default(),
            next: 1,
            screens: Vec::new(),
            quick: [None, None],
            qnote: [None, None],
            qflight: [None, None],
            qfocus: None,
            inbox: ViewInbox::new(),
            history: ViewHistory::new(viewer.history_max as usize),
            viewer,
            editor,
            vfs: Arc::new(VfsRegistry::with_local()),
            quit_save: None,
        }
    }

    /// Новый id документа; документ кладёт `insert`.
    pub fn alloc_id(&mut self) -> DocId {
        let id = DocId(self.next);
        self.next += 1;
        id
    }

    /// Кладёт документ под его id; прежний с тем же id возвращается.
    pub fn insert(&mut self, doc: Doc) -> Option<Doc> {
        self.docs.insert(doc.id(), doc)
    }

    /// Убирает документ отовсюду: экраны, быстрый просмотр.
    pub fn remove(&mut self, id: DocId) -> Option<Doc> {
        self.screens.retain(|d| *d != id);
        for q in &mut self.quick {
            if *q == Some(id) {
                *q = None;
            }
        }
        self.docs.remove(&id)
    }

    pub fn doc(&self, id: DocId) -> Option<&Doc> {
        self.docs.get(&id)
    }

    pub fn doc_mut(&mut self, id: DocId) -> Option<&mut Doc> {
        self.docs.get_mut(&id)
    }

    pub fn viewer(&self, id: DocId) -> Option<&ViewerDoc> {
        match self.docs.get(&id)? {
            Doc::Viewer(d) => Some(d),
            Doc::Editor(_) => None,
        }
    }

    pub fn viewer_mut(&mut self, id: DocId) -> Option<&mut ViewerDoc> {
        match self.docs.get_mut(&id)? {
            Doc::Viewer(d) => Some(d),
            Doc::Editor(_) => None,
        }
    }

    /// Активный документ вьювера: документ области панели с фокусом (быстрый просмотр,
    /// `viewer.dock`), иначе верхний экран, если это вьювер.
    pub fn active_viewer(&self) -> Option<DocId> {
        if let Some(side) = self.panel_focus() {
            return self.quick[side.idx()].filter(|id| self.viewer(*id).is_some());
        }
        self.active().filter(|id| self.viewer(*id).is_some())
    }

    /// Все документы вьювера (перечитывание конфига, выход).
    pub(crate) fn viewers_mut(&mut self) -> impl Iterator<Item = &mut ViewerDoc> {
        self.docs.values_mut().filter_map(|d| match d {
            Doc::Viewer(v) => Some(v),
            Doc::Editor(_) => None,
        })
    }

    /// Сторона, чей документ в области панели держит фокус (S7 §4); `None` — фокус у
    /// экрана или у панелей.
    pub fn panel_focus(&self) -> Option<Side> {
        let (side, screens) = self.qfocus?;
        (screens == self.screens.len() && self.quick[side.idx()].is_some()).then_some(side)
    }

    /// Документ в области панели `side`: быстрый просмотр или перенесённый вьювер.
    pub fn in_panel(&self, side: Side) -> Option<&ViewerDoc> {
        self.viewer(self.quick[side.idx()]?)
    }

    /// Позиции открытых редакторов для истории правок (выход).
    pub(crate) fn edit_memos(&self) -> Vec<ViewMemo> {
        self.docs
            .values()
            .filter_map(|d| match d {
                Doc::Editor(e) if e.is_ready() => Some(e.memo()),
                _ => None,
            })
            .collect()
    }

    pub fn editor(&self, id: DocId) -> Option<&EditorDoc> {
        match self.docs.get(&id)? {
            Doc::Editor(d) => Some(d),
            Doc::Viewer(_) => None,
        }
    }

    /// Все документы редактора (выход, автосохранение, «Сохранить все»).
    pub fn editors(&self) -> impl Iterator<Item = &EditorDoc> {
        self.docs.values().filter_map(|d| match d {
            Doc::Editor(e) => Some(e),
            Doc::Viewer(_) => None,
        })
    }

    pub(crate) fn editors_mut(&mut self) -> impl Iterator<Item = &mut EditorDoc> {
        self.docs.values_mut().filter_map(|d| match d {
            Doc::Editor(e) => Some(e),
            Doc::Viewer(_) => None,
        })
    }

    /// Активный документ редактора: верхний экран, если это редактор.
    pub fn active_editor(&self) -> Option<DocId> {
        self.active().filter(|id| self.editor(*id).is_some())
    }

    pub fn editor_mut(&mut self, id: DocId) -> Option<&mut EditorDoc> {
        match self.docs.get_mut(&id)? {
            Doc::Editor(d) => Some(d),
            Doc::Viewer(_) => None,
        }
    }

    /// Активный экран: последний в `screens`.
    pub fn active(&self) -> Option<DocId> {
        self.screens.last().copied()
    }

    /// Экран `id` — наверх, он активный (`viewer.screen`, F12, вкладка). `false` — такого
    /// экрана нет.
    pub fn activate(&mut self, id: DocId) -> bool {
        let Some(i) = self.screens.iter().position(|d| *d == id) else {
            return false;
        };
        let d = self.screens.remove(i);
        self.screens.push(d);
        true
    }

    /// Экраны для вкладок и списка F12: id, файл, вьювер ли; верхний — последний.
    pub fn screen_list(&self) -> impl Iterator<Item = (DocId, &Location, bool)> + '_ {
        self.screens.iter().filter_map(|id| {
            let d = self.docs.get(id)?;
            Some((*id, d.loc(), matches!(d, Doc::Viewer(_))))
        })
    }

    /// Документ `new` встаёт на место экрана `old` (соседний файл, ссылка `.md`); `old` из
    /// `screens` уходит. Нет `old` на экране — `new` наверх.
    pub(crate) fn replace_screen(&mut self, old: DocId, new: DocId) {
        match self.screens.iter().position(|d| *d == old) {
            Some(i) => self.screens[i] = new,
            None => self.screens.push(new),
        }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

/// Регистрирует handlers `viewer` и `editor`; конфиг — секции `[viewer]` и `[editor]`
/// (ошибка секции — значения по умолчанию и предупреждение в лог). Только локальная ФС,
/// без Spool — приложение зовёт [`register_from`] со своим окружением.
pub fn register(core: &mut Core, config: &Config) {
    register_from(core, config, ViewEnv::default());
}

/// Как [`register`], с окружением приложения (реестр VFS, каталог кэша для Spool).
pub fn register_from(core: &mut Core, config: &Config, env: ViewEnv) {
    let (viewer, warning) = config.section::<ViewerCfg>("viewer");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[viewer] config section ignored");
    }
    let (editor, warning) = config.section::<EditorCfg>("editor");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[editor] config section ignored");
    }
    register_env(core, viewer, editor, env);
}

/// Как `register`, с готовым конфигом (тесты).
pub fn register_with(core: &mut Core, viewer: ViewerCfg, editor: EditorCfg) {
    register_env(core, viewer, editor, ViewEnv::default());
}

/// Окружение вьювера от приложения: реестр VFS (не-локальные источники) и каталог кэша
/// (Spool — `cache_dir/view-spool`). По умолчанию — только локальная ФС, без Spool.
#[derive(Clone)]
pub struct ViewEnv {
    pub vfs: Arc<VfsRegistry>,
    pub cache_dir: Option<PathBuf>,
}

impl std::fmt::Debug for ViewEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewEnv")
            .field("cache_dir", &self.cache_dir)
            .finish_non_exhaustive()
    }
}

impl ViewEnv {
    /// Файл истории просмотра (`state_dir/view-history.json`, S7 §3.9). `state_dir` — сосед
    /// `cache_dir` во всех раскладках Windows (`Paths`: `<LocalAppData>\Urfin\{cache,state}`,
    /// portable — `data\{cache,state}`). Без `cache_dir` (тесты) — `None`, история только
    /// в памяти.
    pub fn history_file(&self) -> Option<PathBuf> {
        let state = self.cache_dir.as_deref()?.parent()?.join("state");
        Some(state.join(crate::history::HISTORY_FILE))
    }
}

impl Default for ViewEnv {
    fn default() -> Self {
        ViewEnv {
            vfs: Arc::new(VfsRegistry::with_local()),
            cache_dir: None,
        }
    }
}

/// Как `register_with`, со своим окружением.
pub fn register_env(core: &mut Core, viewer: ViewerCfg, editor: EditorCfg, env: ViewEnv) {
    // Порядок важен: `ViewState` встаёт в первом pump у `viewer`, `editor` разбирает
    // inbox после него.
    let mut state = ViewState::new(viewer, editor);
    state.vfs = env.vfs.clone();
    core.register(Box::new(ViewerHandler::new(state, env)));
    core.register(Box::new(EditorHandler::default()));
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod viewer_tests;

#[cfg(test)]
mod screens_tests;

#[cfg(test)]
mod quick_tests;

#[cfg(test)]
mod follow_tests;
