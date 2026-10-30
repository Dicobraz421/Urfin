//! `ViewInbox`: события документа UI → handlers `viewer`/`editor` (S7 §2, решение
//! 2026-10-30 №1). Нажатия, undo и IME живут в `EditorState` gpui-kit в UI; сюда идут
//! только «текст принят», «изменён» и «снимок для сохранения». Видимая область вьювера
//! (колонки и строки окна) — `Viewport` (решение 2026-10-30 «viewer-viewport»). Разбор —
//! в `on_pump`.

use crossbeam_channel::{Receiver, Sender};
use urfin_core::Location;

use crate::DocId;

/// Что сделать со снимком текста.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum After {
    Save,
    SaveAs(Location),
    SaveClose,
    Diff,
}

/// Событие документа.
#[derive(Debug)]
pub enum InboxMsg {
    /// Окно текста вьювера: колонки и строки. UI шлёт при открытии, ресайзе и смене
    /// шрифта; handler по нему прокручивает, выводит совпадение и просит страницы.
    Viewport { doc: DocId, cols: u32, rows: u32 },
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
    /// UI создал `EditorState` из загруженного текста.
    Adopted(DocId),
    /// После правки и движения курсора (строка, колонка — с 0).
    Edited {
        doc: DocId,
        dirty: bool,
        cursor: (u32, u32),
    },
    /// Текст для сохранения.
    Snapshot {
        doc: DocId,
        text: ropey::Rope,
        then: After,
    },
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
