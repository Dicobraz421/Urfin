//! Документ редактора (S7 §2 «Редактор», ADR-0023): текст, курсор, выделение и отмена —
//! в handler `editor` на собственном движке; UI рисует видимые строки из `TextView` и шлёт
//! ввод (`InboxMsg::Input`, `Caret`, `Viewport`, `Scroll`). S7-15 — кодировки и EOL
//! (`load`), S7-17 — сохранение (`save` — диск, `flow` — снимки, вопросы, задача, выход),
//! S7-19 — внешний редактор (`external`), новый файл, F6, автосохранение.
//!
//! Движок: `piece` — куски на персистентном дереве (S7-26); `lines`, `changes` — строки и
//! полоса изменений поверх правок (S7-27); `undo` — журнал отмены (S7-28); `open` —
//! открытие с защитой файла, `input` — курсор, движения и правка, `clip` — буфер обмена,
//! `write` — потоковый писатель снимка (S7-29); `replace` — поиск и замена по кускам
//! (S7-30). Документ на движке — [`Engine`] в `engine` (S7-34: прежний путь — Rope в
//! `EditorState` UI — снят). S7-35 — подсветка tree-sitter в фоне по правкам (`hl`) и
//! свёртка — слой отображения (`fold`).

pub mod changes;
pub(crate) mod clip;
pub(crate) mod cmd;
pub(crate) mod engine;
pub mod external;
pub(crate) mod flow;
pub mod fold;
pub mod hl;
pub mod hlwin;
pub mod input;
pub mod lines;
pub mod load;
pub mod open;
pub mod piece;
pub mod remote;
pub mod replace;
pub(crate) mod rflow;
pub mod save;
pub mod undo;
pub mod write;

use std::sync::Arc;
use std::time::Instant;

use urfin_core::{CancelToken, JobId, Location, Runtime};

use crate::detect::Eol;
use crate::enc::Enc;
use crate::history::{HistKind, ViewMemo};
use crate::layout::Mode;
use crate::{DocId, Placement};

pub use changes::{Change, LineChanges};
pub use flow::{AskChoice, EdAsk};
pub use input::{Act, Engine, Selections};
pub use load::{LoadError, LoadOpts, NewDoc};
pub use open::Guard;

/// Фаза документа.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdState {
    /// Открытие: прочитано из `total` байт (0 — размер неизвестен); ввод заблокирован.
    Loading {
        done: u64,
        total: u64,
    },
    Ready,
    /// Сохранение: правка разрешена (пишется снимок, ADR-0023 §8). Больше 8 мебибайт —
    /// задача в job-баре (`Some`), меньше — без неё.
    Saving(Option<JobId>),
    /// «Заменить все» в фоне (S7-30): документ только для чтения, прогресс и отмена —
    /// [`EditorDoc::replace`].
    Replacing,
    Failed,
}

/// Отметка файла на диске: внешнее изменение — сравнением перед сохранением (S7-17).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskStamp {
    pub size: u64,
    /// Наносекунды Unix; `i64::MIN` — неизвестно.
    pub mtime: i64,
    /// Том и индекс файла (только локальные).
    pub file_id: Option<(u64, u128)>,
}

/// Документ редактора (S7 §2).
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые признаки документа"
)]
#[derive(Debug)]
pub struct EditorDoc {
    pub id: DocId,
    pub loc: Location,
    pub placement: Placement,
    pub state: EdState,
    /// Кодировка текста и сохранения; `auto` — выбрана детектом.
    pub enc: Enc,
    /// В файле был BOM — при сохранении пишется снова.
    pub bom: bool,
    /// Концы строк файла (`Mixed` — разные, сохраняются как есть).
    pub eol: Eol,
    /// Конец строк для новых строк и вставки (`editor.eol` конфига и команды).
    pub eol_new: Eol,
    /// Язык подсветки.
    pub lang: Option<&'static str>,
    /// Атрибут «только чтение»: чип в шапке, правка не принимается.
    pub readonly: bool,
    /// В исходнике есть недекодируемые байты: строка `caution` внизу (§5.1).
    pub lossy: bool,
    pub stamp: DiskStamp,
    /// Есть несохранённые правки (позиция журнала ≠ сохранённой).
    pub dirty: bool,
    /// Счётчик в шапке: добавлено, изменено, швов удаления — фоном по кускам
    /// (`changes::count`); `lines` пуст (метки видимых строк — `changes::visible`).
    pub changes: LineChanges,
    /// Строка и колонка курсора, с 0.
    pub cursor: (u32, u32),
    /// Куда поставить курсор после открытия (`editor.open line= col=`, история), с 0.
    pub start: Option<(u32, u32)>,
    pub rev: u64,
    /// Inline-строка вопроса внизу экрана (S7 §5.4, ADR-0023 §3, §8): закрытие с
    /// правками, файл изменён на диске, непредставимые символы, только чтение, символа
    /// нет в кодировке, файл пишет другая программа, нет места. Ответ — `editor.answer
    /// choice=`.
    pub ask: Option<EdAsk>,
    /// Файла ещё нет (`editor.new`): создаётся первым сохранением.
    pub is_new: bool,
    /// Что дал детект при последнем открытии (кольцо F8: «авто»).
    detected: Enc,
    /// Снимок, ждущий ответа на вопрос (`ask`), и что с ним сделать.
    pub(crate) pending: Option<flow::Pending>,
    /// Следующее сохранение — без сверки `DiskStamp` («Сохранить поверх» до снимка).
    pub(crate) force: bool,
    /// После закрытия (вопрос «Сохранить?» от F6) — открыть вьювер на этой строке (с 0).
    pub(crate) then_view: Option<u32>,
    /// Последняя правка: автосохранение `delay`.
    pub(crate) edited_at: Option<Instant>,
    /// Поколение отметки диска: проверка по возврату на экран, начатая до сохранения или
    /// перечитывания, отбрасывается.
    pub(crate) stamp_gen: u32,
    /// Токен документа: открытие, индекс, задачи; отменяется при закрытии.
    pub(crate) cancel: CancelToken,
    load_cancel: CancelToken,
    load_gen: u32,
    /// Документ на движке (ADR-0023): текст, курсор, отмена — здесь, а не в UI. `None`
    /// — ещё открывается.
    pub engine: Option<Box<Engine>>,
    /// Окно текста из `Viewport` до готовности движка.
    pub(crate) view_size: (u32, u32),
    /// Поколение индекса файла: дельты прежнего (перечитывание) отбрасываются.
    pub(crate) index_epoch: u32,
    /// Идёт счёт переводов строк неизвестных кусков (`lf_patch`).
    pub(crate) lf_busy: bool,
    /// Идёт счёт изменений для шапки (`changes::count`); правок текста при его старте.
    pub(crate) changes_busy: bool,
    pub(crate) changes_edits: u64,
    /// Поиск на движке (S7-30): образец, итог, счёт и метки.
    pub search: Option<replace::EdSearch>,
    /// Номер образца поиска.
    pub(crate) search_gen: u64,
    /// Идёт «Заменить все» (`EdState::Replacing`): прогресс и отмена.
    pub replace: Option<replace::ReplaceJob>,
    /// Подсветка и места свёртки (S7-35); `None` — выключена или невозможна.
    pub hl: Option<hl::Hl>,
    /// Подсветка окном (ADR-0024): встроенные языки; тогда `hl` — `None`.
    pub win: Option<hlwin::EdWin>,
    /// `editor.highlight`.
    pub(crate) hl_on: bool,
    /// Копия файла схемы с записью (F03 §4.3): `loc` — временный файл, сохранение
    /// закачивает его обратно.
    pub remote: Option<Box<remote::Remote>>,
}

impl EditorDoc {
    pub fn new(id: DocId, loc: Location, placement: Placement) -> EditorDoc {
        EditorDoc::with_cancel(id, loc, placement, CancelToken::new())
    }

    /// С токеном документа (дочерний к токену Runtime: выход останавливает открытие).
    pub fn with_cancel(
        id: DocId,
        loc: Location,
        placement: Placement,
        cancel: CancelToken,
    ) -> EditorDoc {
        let load_cancel = cancel.child();
        EditorDoc {
            id,
            loc,
            placement,
            state: EdState::Loading { done: 0, total: 0 },
            enc: Enc::utf8(),
            bom: false,
            eol: Eol::default(),
            eol_new: Eol::default(),
            lang: None,
            readonly: false,
            lossy: false,
            stamp: DiskStamp::default(),
            dirty: false,
            changes: LineChanges::default(),
            cursor: (0, 0),
            start: None,
            rev: 0,
            ask: None,
            is_new: false,
            detected: Enc::utf8(),
            pending: None,
            force: false,
            then_view: None,
            edited_at: None,
            stamp_gen: 0,
            cancel,
            load_cancel,
            load_gen: 0,
            engine: None,
            view_size: (0, 0),
            index_epoch: 0,
            lf_busy: false,
            changes_busy: false,
            changes_edits: 0,
            search: None,
            search_gen: 0,
            replace: None,
            hl: None,
            win: None,
            hl_on: true,
            remote: None,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state == EdState::Ready
    }

    fn bump(&mut self) {
        self.rev += 1;
    }

    /// Адрес для шапки, табов и истории: у копии файла сервера — адрес на сервере.
    pub fn shown_loc(&self) -> &Location {
        self.remote.as_deref().map_or(&self.loc, |r| &r.loc)
    }

    /// Копия сохранена, но на сервер ещё не ушла (или уходит).
    pub fn unsynced(&self) -> bool {
        self.remote
            .as_deref()
            .is_some_and(|r| r.unsynced || r.uploading())
    }

    /// Запись истории правки: строка и колонка курсора, кодировка, если выбрана вручную.
    pub fn memo(&self) -> ViewMemo {
        ViewMemo {
            loc: self.shown_loc().clone(),
            top: u64::from(self.cursor.0),
            hscroll: self.cursor.1,
            enc: (!self.enc.auto).then(|| self.enc.enc.name().into()),
            mode: Mode::Text,
            wrap: None,
            frac: 0,
            kind: HistKind::Edit,
            ts: 0,
            // Строка курсора (с 0) — в списке «стр N».
            line: u64::from(self.cursor.0) + 1,
            end: false,
        }
    }

    /// Открытие (первое или перечитывание): `opts.enc` — вручную, `None` — детект.
    /// Прежнее открытие отменяется, документ пересобирается.
    pub(crate) fn load(&mut self, rt: &Runtime, reg: &Arc<urfin_vfs::VfsRegistry>, opts: LoadOpts) {
        self.load_cancel.cancel();
        self.load_cancel = self.cancel.child();
        self.load_gen = self.load_gen.wrapping_add(1);
        self.state = EdState::Loading {
            done: 0,
            total: self.stamp.size,
        };
        engine::spawn_open(self, rt, reg.clone(), opts);
        self.bump();
    }

    /// Имя файла для строк вопросов и тостов.
    pub fn name(&self) -> String {
        let loc = self.shown_loc();
        loc.file_name().map_or_else(
            || loc.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Закрытие: открытие, индекс и задачи останавливаются.
    pub(crate) fn close(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests;
