//! `ViewInbox`: события документа UI → handlers `viewer`/`editor` (S7 §2, решение
//! 2026-10-30 №1, ADR-0023 §9). Видимая область вьювера и редактора (колонки и строки
//! окна) — `Viewport` (решение 2026-10-30 «viewer-viewport»). Документ редактора на
//! движке (ADR-0023 §9): набранный текст и commit IME — `Input`, мышь — `Caret`,
//! прокрутка — `Scroll`/`ScrollTo`; клавиши правки — команды `editor.*`. Разбор — в
//! `on_pump`.

use compact_str::CompactString;
use crossbeam_channel::{Receiver, Sender};
use urfin_core::Location;

use crate::DocId;
pub use crate::edit::input::Unit;

/// Что сделать со снимком документа при сохранении (`flow::request_snapshot`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum After {
    Save,
    SaveAs(Location),
    SaveClose,
    /// Автосохранение (`editor.autosave`): как `Save`, но вопросы не задаются.
    Autosave,
}

/// Событие документа.
#[derive(Debug)]
pub enum InboxMsg {
    /// Окно текста вьювера или редактора: колонки и строки. UI шлёт при открытии,
    /// ресайзе и смене шрифта; handler по нему прокручивает, выводит совпадение или курсор
    /// и просит страницы.
    Viewport { doc: DocId, cols: u32, rows: u32 },
    /// Набранный текст, commit IME (UTF-8): правка документа на движке.
    Input { doc: DocId, text: CompactString },
    /// Мышь в редакторе: клик (`Char`), двойной (`Word`), тройной (`Line`); `extend` —
    /// drag и Shift+клик; `block` — Alt+мышь, блок до экранной колонки ряда `off` (с
    /// `hscroll`; за концом строки — тоже, S7-40).
    Caret {
        doc: DocId,
        off: u64,
        extend: bool,
        unit: Unit,
        block: Option<u32>,
    },
    /// Документ Markdown (S7 §3.10): UI прокрутил список блоков — верхний блок и доля его
    /// высоты (0…1).
    MdTop { doc: DocId, block: u32, frac: f32 },
    /// Клик по `<details>`: раскрыть / свернуть.
    MdToggle { doc: DocId, block: u32 },
    /// Выделение мышью по проекции документа (`None` — снято).
    MdSelect {
        doc: DocId,
        sel: Option<std::ops::Range<u32>>,
    },
    /// Колесо над предпросмотром редактора (S7 §5.9): ведёт документ, текст идёт за ним.
    MdLead { doc: DocId },
    /// Клик по флажку задачи в предпросмотре: пункт на этом байте текста.
    MdCheck { doc: DocId, off: u64 },
    /// Выделение мышью в Тексте и Hex (S7 §3.7): байты источника (`None` — снято).
    Select {
        doc: DocId,
        sel: Option<std::ops::Range<u64>>,
    },
    /// Бегунок полосы прокрутки перетащен: верх экрана — на это смещение (в Тексте и
    /// редакторе — к началу строки).
    ScrollTo { doc: DocId, off: u64 },
    /// Колесо над документом без фокуса (быстрый просмотр) или над редактором: прокрутка
    /// этого документа (курсор редактора остаётся).
    Scroll {
        doc: DocId,
        scroll: crate::viewer::Scroll,
    },
    /// Ползунок записи (F03 §3.6): момент, мс.
    CastSeek { doc: DocId, ms: u64 },
    /// Цвета терминала темы для кадров записи.
    CastColors {
        doc: DocId,
        colors: urfin_term::vt::grid::Defaults,
    },
    /// Окно Урфина потеряло фокус: автосохранение `focus` активного редактора.
    Blur,
}

/// Отправитель для UI: дешёвый клон, не блокирует.
#[derive(Clone, Debug)]
pub struct InboxSender(Sender<InboxMsg>);

impl InboxSender {
    /// Кладёт событие; handler разберёт его в начале следующего pump. UI будит core
    /// сам (как после команды).
    pub fn send(&self, msg: InboxMsg) {
        // Приёмник живёт в `ViewState` до выхода: ошибка — только при завершении.
        let _ = self.0.send(msg);
    }
}

/// Очередь событий в `ViewState`.
#[derive(Debug)]
pub struct ViewInbox {
    tx: Sender<InboxMsg>,
    rx: Receiver<InboxMsg>,
}

impl Default for ViewInbox {
    fn default() -> Self {
        ViewInbox::new()
    }
}

impl ViewInbox {
    pub fn new() -> ViewInbox {
        let (tx, rx) = crossbeam_channel::unbounded();
        ViewInbox { tx, rx }
    }

    pub fn sender(&self) -> InboxSender {
        InboxSender(self.tx.clone())
    }

    /// Следующее событие без ожидания.
    pub fn try_next(&self) -> Option<InboxMsg> {
        self.rx.try_recv().ok()
    }
}
