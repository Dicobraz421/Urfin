//! `ViewInbox`: события документа UI → handlers `viewer`/`editor` (S7 §2, решение
//! 2026-10-30 №1). Нажатия, undo и IME живут в `EditorState` gpui-kit в UI; сюда идут
//! только «текст принят», «изменён» и «снимок для сохранения». Видимая область вьювера
//! (колонки и строки окна) — `Viewport` (решение 2026-10-30 «viewer-viewport»). Разбор —
//! в `on_pump`.

use crossbeam_channel::{Receiver, Sender};
use urfin_core::{Args, Location};

use crate::DocId;

/// Что сделать со снимком текста. Handler просит снимок `UiEvent::Ext { id:
/// "editor.snapshot", args: { doc, then, path? } }`; фича UI отвечает
/// `InboxMsg::Snapshot { then: After::from_args(&args) }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum After {
    Save,
    SaveAs(Location),
    SaveClose,
    Diff,
    /// Автосохранение (`editor.autosave`): как `Save`, но вопросы не задаются.
    Autosave,
}

impl After {
    /// Аргументы `editor.snapshot`: `then` (`save` | `save_as` + `path` | `save_close` |
    /// `diff` | `autosave`).
    pub fn to_args(&self) -> Args {
        let a = Args::default();
        match self {
            After::Save => a.with("then", "save"),
            After::SaveAs(loc) => a.with("then", "save_as").with("path", loc.clone()),
            After::SaveClose => a.with("then", "save_close"),
            After::Diff => a.with("then", "diff"),
            After::Autosave => a.with("then", "autosave"),
        }
    }

    /// Обратно из аргументов `editor.snapshot`.
    pub fn from_args(a: &Args) -> Option<After> {
        Some(match a.str("then")? {
            "save" => After::Save,
            "save_as" => After::SaveAs(a.loc("path")?),
            "save_close" => After::SaveClose,
            "diff" => After::Diff,
            "autosave" => After::Autosave,
            _ => return None,
        })
    }
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
    /// Выделение мышью в Тексте и Hex (S7 §3.7): байты источника (`None` — снято).
    Select {
        doc: DocId,
        sel: Option<std::ops::Range<u64>>,
    },
    /// Бегунок полосы прокрутки перетащен: верх экрана — на это смещение (в Тексте —
    /// к началу строки).
    ScrollTo { doc: DocId, off: u64 },
    /// Колесо над документом без фокуса (быстрый просмотр): прокрутка этого документа.
    Scroll {
        doc: DocId,
        scroll: crate::viewer::Scroll,
    },
    /// UI создал `EditorState` из загруженного текста.
    Adopted(DocId),
    /// После правки и движения курсора (строка, колонка — с 0).
    Edited {
        doc: DocId,
        dirty: bool,
        cursor: (u32, u32),
    },
    /// Текст для сохранения (ответ на `UiEvent::Ext { id: "editor.snapshot" }`) или для
    /// полосы изменений (`Diff`, по паузе после правок).
    Snapshot {
        doc: DocId,
        text: ropey::Rope,
        then: After,
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
