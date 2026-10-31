//! Документ редактора без текста UI (S7 §2 «Редактор»). S7-15 — загрузка, кодировки,
//! EOL, полоса изменений (`load`, `diff`, кодирование — `save`); сохранение, поиск и
//! внешний редактор — S7-17…S7-19. Текст правки живёт в `Entity<EditorState>` gpui-kit в
//! UI: фича забирает `loaded`, отвечает `InboxMsg::Adopted`, шлёт `Edited` и снимки
//! `Snapshot` (решение 2026-10-30 №1, B1).

pub(crate) mod cmd;
pub mod diff;
pub mod load;
pub mod save;

use std::sync::Arc;

use ropey::Rope;
use urfin_core::{CancelToken, JobId, Lane, Location, Msg, Runtime};

use crate::detect::Eol;
use crate::enc::Enc;
use crate::history::{HistKind, ViewMemo};
use crate::layout::Mode;
use crate::{DocId, Placement, ViewState};

pub use diff::{Change, LineChanges};
pub use load::{Decoded, LoadError, LoadOpts};
pub use save::{Encoded, encode};

/// Фаза документа.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdState {
    /// Загрузка: прочитано из `total` байт (0 — размер неизвестен); ввод заблокирован.
    Loading {
        done: u64,
        total: u64,
    },
    Ready,
    Saving(JobId),
    Failed,
    /// Больше `editor.max_size_mb`: строка в шапке «Файл слишком большой для редактора:
    /// [Открыть в просмотре] [Внешний редактор]» (§5.1), текста нет.
    TooLarge {
        size: u64,
    },
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
    /// Текст загрузки: UI создаёт из него `EditorState` (клон Rope дёшев) и отвечает
    /// `InboxMsg::Adopted` — тогда поле пустеет. Снова `Some` — после перечитывания
    /// (F8 без правок, `editor.reload`): UI заменяет текст.
    pub loaded: Option<Rope>,
    /// Кодировка текста и сохранения; `auto` — выбрана детектом.
    pub enc: Enc,
    /// В файле был BOM — при сохранении пишется снова.
    pub bom: bool,
    /// Концы строк файла (`Mixed` — разные, сохраняются как есть).
    pub eol: Eol,
    /// Конец строк для новых строк и вставки (`editor.eol` конфига и команды).
    pub eol_new: Eol,
    /// Язык подсветки (имя gpui-kit).
    pub lang: Option<&'static str>,
    /// Атрибут «только чтение»: чип в шапке, `EditorState::set_readonly`.
    pub readonly: bool,
    /// При загрузке были недекодируемые байты: строка `caution` внизу (§5.1).
    pub lossy: bool,
    pub stamp: DiskStamp,
    /// Есть несохранённые правки (из `InboxMsg::Edited`).
    pub dirty: bool,
    /// Полоса изменений и счётчик в шапке (diff по `Snapshot { then: Diff }`).
    pub changes: LineChanges,
    /// Строка и колонка курсора, с 0.
    pub cursor: (u32, u32),
    /// Куда поставить курсор после загрузки (`editor.open line= col=`, история), с 0;
    /// UI ставит его, когда забирает `loaded`.
    pub start: Option<(u32, u32)>,
    pub rev: u64,
    /// Что дал детект при последней загрузке (кольцо F8: «авто»).
    detected: Enc,
    /// Хэши строк сохранённого текста.
    base: Arc<[u64]>,
    /// Токен документа: загрузка, diff; отменяется при закрытии.
    pub(crate) cancel: CancelToken,
    load_cancel: CancelToken,
    load_gen: u32,
    diff_seq: u64,
}

impl EditorDoc {
    pub fn new(id: DocId, loc: Location, placement: Placement) -> EditorDoc {
        EditorDoc::with_cancel(id, loc, placement, CancelToken::new())
    }

    /// С токеном документа (дочерний к токену Runtime: выход останавливает загрузку).
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
            loaded: None,
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
            detected: Enc::utf8(),
            base: Arc::from(Vec::new()),
            cancel,
            load_cancel,
            load_gen: 0,
            diff_seq: 0,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state == EdState::Ready
    }

    fn bump(&mut self) {
        self.rev += 1;
    }

    /// Запись истории правки: строка и колонка курсора, кодировка, если выбрана вручную.
    pub fn memo(&self) -> ViewMemo {
        ViewMemo {
            loc: self.loc.clone(),
            top: u64::from(self.cursor.0),
            hscroll: self.cursor.1,
            enc: (!self.enc.auto).then(|| self.enc.enc.name().into()),
            mode: Mode::Text,
            wrap: None,
            frac: 0,
            kind: HistKind::Edit,
            ts: 0,
        }
    }

    /// Загрузка (первая или перечитывание): `opts.enc` — вручную, `None` — детект.
    /// Прежняя загрузка и текст, ещё не забранный UI, отменяются.
    pub(crate) fn load(&mut self, rt: &Runtime, reg: &Arc<urfin_vfs::VfsRegistry>, opts: LoadOpts) {
        self.load_cancel.cancel();
        self.load_cancel = self.cancel.child();
        self.load_gen = self.load_gen.wrapping_add(1);
        self.loaded = None;
        self.state = EdState::Loading {
            done: 0,
            total: self.stamp.size,
        };
        let job = load::LoadJob {
            doc: self.id,
            gen_: self.load_gen,
            loc: self.loc.clone(),
            reg: reg.clone(),
            opts,
        };
        load::spawn_load(rt, job, self.load_cancel.clone());
        self.bump();
    }

    fn on_progress(&mut self, gen_: u32, done: u64) {
        if gen_ != self.load_gen {
            return;
        }
        if let EdState::Loading { total, .. } = self.state {
            self.state = EdState::Loading { done, total };
            self.bump();
        }
    }

    /// Итог загрузки. `Some(ошибка)` — документ не открылся (handler закрывает его).
    fn on_loaded(&mut self, gen_: u32, res: Result<load::Loaded, LoadError>) -> Option<String> {
        if gen_ != self.load_gen {
            return None;
        }
        self.bump();
        match res {
            Ok(l) => {
                let d = l.decoded;
                self.loaded = Some(d.text);
                self.enc = d.enc;
                self.detected = d.detected;
                self.bom = d.bom;
                self.eol = d.eol;
                self.eol_new = d.eol_new;
                self.lang = d.lang;
                self.lossy = d.lossy;
                self.base = d.hashes;
                self.changes = LineChanges::default();
                self.dirty = false;
                self.stamp = l.stamp;
                self.readonly = l.readonly;
                self.state = EdState::Ready;
                None
            }
            Err(LoadError::TooLarge(size)) => {
                self.state = EdState::TooLarge { size };
                None
            }
            Err(LoadError::Cancelled) => None,
            Err(LoadError::Io(e)) => {
                self.state = EdState::Failed;
                Some(e)
            }
        }
    }

    /// `InboxMsg::Adopted`: UI забрал текст.
    pub(crate) fn adopted(&mut self) -> bool {
        if self.loaded.take().is_none() {
            return false;
        }
        self.bump();
        true
    }

    /// `InboxMsg::Snapshot { then: Diff }`: построчный diff с сохранённым текстом в фоне
    /// (Bulk lane); итог — `changes`. Ответ прежнего снимка отбрасывается.
    pub(crate) fn diff_snapshot(&mut self, text: Rope, rt: &Runtime) {
        if !self.is_ready() {
            return;
        }
        self.diff_seq += 1;
        let (doc, seq, gen_) = (self.id, self.diff_seq, self.load_gen);
        let base = self.base.clone();
        rt.spawn(Lane::Bulk, "edit.diff", self.cancel.clone(), move |cx| {
            let _span = tracing::debug_span!("edit.diff").entered();
            let now = diff::line_hashes(&text);
            let changes = diff::diff(&base, &now);
            let msg = DiffMsg {
                doc,
                seq,
                gen_,
                changes,
            };
            let _ = cx.sink.send(Box::new(msg), &cx.cancel);
        });
    }

    fn on_diff(&mut self, seq: u64, gen_: u32, changes: LineChanges) -> bool {
        if seq != self.diff_seq || gen_ != self.load_gen || changes == self.changes {
            return false;
        }
        self.changes = changes;
        self.bump();
        true
    }

    /// Закрытие: загрузка и diff останавливаются.
    pub(crate) fn close(&mut self) {
        self.cancel.cancel();
    }
}

/// Итог diff → документ.
#[derive(Debug)]
struct DiffMsg {
    doc: DocId,
    seq: u64,
    gen_: u32,
    changes: LineChanges,
}

impl Msg for DiffMsg {
    fn name(&self) -> &'static str {
        "edit.diff"
    }

    fn apply(self: Box<Self>, cx: &mut urfin_core::Cx<'_>) {
        let changed = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(self.doc))
            .is_some_and(|d| d.on_diff(self.seq, self.gen_, self.changes));
        if !changed {
            cx.keep_rev();
        }
    }
}

#[cfg(test)]
mod tests;
