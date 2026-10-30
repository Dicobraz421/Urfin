//! Urfin — вьювер (F3, быстрый просмотр) и редактор (F4): модель документов, команды
//! `viewer.*` / `editor.*`, конфиг `[viewer]` / `[editor]` (S7). Без gpui: экраны — фичи
//! окна в `urfin-ui` (`gpui_shell/view`).
//!
//! S7-01 — каркас: `ViewState` в `AppState.ext`, handlers с командами-заглушками,
//! `ViewInbox`. S7-02…S7-06 — источники, кэш страниц, детект, индекс строк, раскладка;
//! остальные модули §1 наполняют S7-08…S7-20 (`docs/specs/S7.md` §10).

pub mod cache;
pub mod config;
pub mod detect;
pub mod edit;
pub mod enc;
pub mod follow;
pub mod handler;
pub mod history;
pub mod inbox;
pub mod index;
pub mod lang;
pub mod layout;
pub mod search;
pub mod source;
mod sys;

pub use cache::{ByteView, Bytes, Missing, Page, PageCache, Prio};
pub use config::{
    Assoc, Autosave, EditorCfg, EditorDefault, EolCfg, HexCols, PlacementCfg, ViewerCfg, WrapCfg,
};
pub use detect::{Detected, Eol, detect};
pub use edit::{EdState, EditorDoc};
pub use enc::Enc;
pub use handler::{EDITOR_COMMANDS, EditorHandler, VIEWER_COMMANDS, ViewerHandler};
pub use inbox::{After, InboxMsg, InboxSender, ViewInbox};
pub use index::{LineGuess, LineIndex};
pub use layout::{LayoutSpec, Mode, RowText, VRow, WrapKind};
pub use source::{ByteSource, SourceKind, SourceOpts, open_source};

use rustc_hash::FxHashMap;
use urfin_core::{CancelToken, Config, Core, Location, Side};

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

/// Документ вьювера. Открытие (источник, детект, режимы) — S7-08, поиск — S7-09.
#[derive(Debug)]
pub struct ViewerDoc {
    pub id: DocId,
    pub loc: Location,
    pub placement: Placement,
    /// Быстрый просмотр (Ctrl+Q): без индекса и поиска, пока нет фокуса.
    pub quick: bool,
    /// Страницы файла: UI рисует только отсюда (S7-03).
    pub pages: PageCache,
    /// Разреженный индекс строк (S7-05).
    pub index: LineIndex,
    /// Токен документа: loader, индексатор, поиск; отменяется при закрытии.
    pub cancel: CancelToken,
    pub rev: u64,
}

/// Документ в `ViewState`.
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
    /// Быстрый просмотр по стороне (`Side::idx`).
    pub quick: [Option<DocId>; 2],
    /// События редактора UI → handler, разбор в `on_pump` (CONTRACTS «Ввод и клавиатура»).
    pub inbox: ViewInbox,
    pub viewer: ViewerCfg,
    pub editor: EditorCfg,
}

impl ViewState {
    pub fn new(viewer: ViewerCfg, editor: EditorCfg) -> ViewState {
        ViewState {
            docs: FxHashMap::default(),
            next: 1,
            screens: Vec::new(),
            quick: [None, None],
            inbox: ViewInbox::new(),
            viewer,
            editor,
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

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

/// Регистрирует handlers `viewer` и `editor`; конфиг — секции `[viewer]` и `[editor]`
/// (ошибка секции — значения по умолчанию и предупреждение в лог).
pub fn register(core: &mut Core, config: &Config) {
    let (viewer, warning) = config.section::<ViewerCfg>("viewer");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[viewer] config section ignored");
    }
    let (editor, warning) = config.section::<EditorCfg>("editor");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[editor] config section ignored");
    }
    register_with(core, viewer, editor);
}

/// Как `register`, с готовым конфигом (тесты).
pub fn register_with(core: &mut Core, viewer: ViewerCfg, editor: EditorCfg) {
    // Порядок важен: `ViewState` встаёт в первом pump у `viewer`, `editor` разбирает
    // inbox после него.
    core.register(Box::new(ViewerHandler::new(ViewState::new(viewer, editor))));
    core.register(Box::new(EditorHandler));
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests;
