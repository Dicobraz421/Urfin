//! Быстрый просмотр (Ctrl+Q) и `viewer.dock` (S7 §4, карточка S7-13).
//!
//! Область пассивной панели в режиме `qview` показывает документ `ViewState.quick[side]`:
//! фича окна следит за курсором активной панели и через 50 мс без движения шлёт
//! `viewer.quick {side, path}`. Handler переоткрывает документ в том же слоте области:
//! прошлое открытие отменяется токеном документа, в очереди Interactive — не больше
//! одного чтения. `DocId` — новый: ответы прошлого файла (страницы, индекс, разбор) без
//! поколения и в новый документ не попадают. Кэш —
//! 1 МБ, поиск не стартует; индекс — только у файлов, прочитанных целиком (`Owned`,
//! ≤ 4 МБ: «стр 1–26 из 41» макета), остальным — при фокусе (Tab). Облачные файлы
//! (`Attrs::OFFLINE`: `RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN`, `OFFLINE`) не читаются —
//! скачивание `OneDrive` не будится.
//!
//! `viewer.dock` переносит вьювер F3 из окна в область пассивной панели (как быстрый
//! просмотр, без следования за курсором) и обратно; F3 в фокусе быстрого просмотра —
//! тот же перенос в окно.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use compact_str::CompactString;
use urfin_core::{
    AppState, Attrs, CancelToken, CmdError, Command, Cx, Focus, Lane, Location, Msg, OverlayKind,
    PanelMode, Row, Runtime, Side, SideRef, UiEvent,
};
use urfin_vfs::{VfsError, VfsRegistry};

use super::{OpenJob, OpenedMsg, ViewerDoc, probe};
use crate::cache::PageCache;
use crate::detect::tld_for_lang;
use crate::layout::Mode;
use crate::source::{SourceKind, SourceOpts, open_source};
use crate::{Doc, DocId, Placement, ViewEnv, ViewState};

/// Документ Markdown в области панели — до этого размера (источник `Owned`); больше —
/// Текст (S7 §4).
pub const QUICK_DOC_MAX: u64 = 4 << 20;

/// Чтений быстрого просмотра с начала процесса (тест «не больше одного в очереди»).
pub static QUICK_READS: AtomicUsize = AtomicUsize::new(0);

/// Атрибуты Windows облачной заглушки: чтение будит скачивание.
const CLOUD_ATTRS: u32 = 0x1000 /* OFFLINE */ | 0x4_0000 /* RECALL_ON_OPEN */ | 0x40_0000 /* RECALL_ON_DATA_ACCESS */;

/// Что показать вместо документа в области панели.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuickNoteKind {
    /// Файл в облаке: «Файл в облаке — F3 для загрузки».
    Cloud,
    /// Открыть не удалось.
    Error(CompactString),
}

/// Файл быстрого просмотра без документа.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickNote {
    pub loc: Location,
    pub kind: QuickNoteKind,
}

/// Что под курсором для быстрого просмотра стороны `side` (курсор — у другой панели).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuickTarget {
    /// `..`, пустой каталог.
    Nothing,
    /// Каталог: сводка рисует фича окна.
    Dir {
        name: CompactString,
    },
    File {
        loc: Location,
        offline: bool,
    },
}

/// Цель быстрого просмотра области `side` (без I/O).
pub fn quick_target(s: &AppState, side: Side) -> QuickTarget {
    let t = s.panel(side.other()).tab();
    match t.current() {
        Some(Row::Entry { entry, .. }) if entry.kind.is_dir_like() => QuickTarget::Dir {
            name: entry.name.clone(),
        },
        Some(Row::Entry { entry, .. }) => QuickTarget::File {
            loc: Location {
                path: t.loc.path.join(entry.name.as_str()),
                ..t.loc.clone()
            },
            offline: entry.attrs.has(Attrs::OFFLINE),
        },
        _ => QuickTarget::Nothing,
    }
}

/// Область панели `side` показывает документ: быстрый просмотр (`qview`) или
/// перенесённый туда вьювер.
pub fn panel_shows(s: &AppState, side: Side) -> bool {
    let docked = s
        .ext
        .get::<ViewState>()
        .and_then(|v| v.in_panel(side))
        .is_some_and(|d| !d.quick);
    docked || s.panel(side).tab().mode == PanelMode::QView
}

/// Строк в документе: точно — по индексу (последняя строка без разделителя — тоже);
/// `None` — индекс не дошёл до конца.
pub fn line_count(d: &ViewerDoc) -> Option<u64> {
    if !d.index.done {
        return None;
    }
    if d.len == 0 {
        return Some(0);
    }
    let sep = crate::index::Sep::of(&d.enc, d.eol);
    let u = sep.unit();
    let want: smallvec::SmallVec<[u8; 2]> = match sep {
        crate::index::Sep::Byte(b) => smallvec::smallvec![b],
        crate::index::Sep::Le(b) => smallvec::smallvec![b, 0],
        crate::index::Sep::Be(b) => smallvec::smallvec![0, b],
    };
    let ends = d
        .pages
        .get(d.len.saturating_sub(u)..d.len)
        .is_ok_and(|t| t.to_vec().as_slice() == want.as_slice());
    Some(d.index.lines + u64::from(!ends))
}

/// Строки на экране документа (с 0): первая и последняя — по кэшу страниц и индексу,
/// без I/O; документ Markdown — от строки верхнего блока. `None` — Hex или не открыт.
pub fn visible_lines(d: &ViewerDoc) -> Option<(u64, u64)> {
    if !d.is_ready() || d.mode == Mode::Hex {
        return None;
    }
    let sep = crate::index::Sep::of(&d.enc, d.eol);
    let line = |off: u64| {
        d.index
            .line_of_in(&d.pages, off, sep)
            .unwrap_or_else(|_| d.index.line_of(off))
            .get()
    };
    let (_, rows) = d.viewport();
    if let Some(doc) = d.md_doc() {
        let top = d.md.as_ref().map_or(0, |m| m.top as usize);
        let first = doc.blocks.get(top).map_or(0, |b| line(b.src.start));
        return Some((first, first + u64::from(rows.max(1)) - 1));
    }
    let spec = d.view_layout();
    let rows = crate::layout::rows_down(&d.pages, d.pos(&spec), rows as usize, &spec).ok()?;
    let first = rows.first()?;
    let start = line(first.seg);
    let last = start + rows.iter().skip(1).filter(|r| !r.cont).count() as u64;
    Some((start, last))
}

impl ViewerDoc {
    /// Индекс быстрого просмотра: файл целиком в памяти или документ получил фокус.
    fn start_index_quick(&mut self, rt: &Runtime) {
        if !self.quick {
            self.start_index(rt);
            return;
        }
        self.quick = false;
        self.start_index(rt);
        self.quick = true;
    }

    /// Окно ↔ область панели: другой бюджет кэша, индекс в окне.
    fn relocate(&mut self, p: Placement, budget: usize, rt: &Runtime) {
        self.placement = p;
        if p == Placement::Window {
            self.quick = false;
        }
        if budget != self.budget {
            self.budget = budget;
            if let Some(src) = self.src.clone() {
                self.pages = PageCache::with_source(budget, src);
                self.pages.set_len(self.len, self.len_final);
            }
        }
        if self.is_ready() && self.mode != Mode::Hex {
            self.start_index(rt);
        }
        // Документ Markdown мог стать доступным (окно) или недоступным (> 4 МБ в панели).
        self.md_sync(rt);
        self.settle(rt);
        self.bump();
    }
}

fn view_mut<'a>(cx: &'a mut Cx<'_>) -> Result<&'a mut ViewState, CmdError> {
    cx.state
        .ext
        .get_mut::<ViewState>()
        .ok_or_else(|| CmdError::Rejected("viewer is not ready".into()))
}

fn set_focus(cx: &mut Cx<'_>, f: Focus) {
    if cx.state.focus != f {
        cx.state.focus = f;
        cx.ui(UiEvent::FocusChanged(f));
    }
}

/// Убирает документ области `side` (без записи в историю: быстрый просмотр).
fn drop_slot(view: &mut ViewState, side: Side) {
    if let Some(id) = view.quick[side.idx()]
        && let Some(Doc::Viewer(mut d)) = view.remove(id)
    {
        d.close();
    }
    view.quick[side.idx()] = None;
}

/// `viewer.quick {side, path, focus?}`: документ быстрого просмотра на `path`; `focus` —
/// Tab из панели: фокус — ему (прокрутка, F7, F8, режимы; индекс включается).
pub(crate) fn quick(cmd: &Command, cx: &mut Cx<'_>, env: &ViewEnv) -> Result<(), CmdError> {
    let a = &cmd.args;
    let side = match a.side("side") {
        Some(SideRef::Left) => Side::Left,
        Some(SideRef::Right) => Side::Right,
        Some(SideRef::Active) => cx.state.active,
        Some(SideRef::Passive) => cx.state.active.other(),
        None => {
            return Err(CmdError::Arg {
                name: "side".into(),
                msg: "expected left, right, active or passive".into(),
            });
        }
    };
    let Some(loc) = a.loc("path") else {
        return Err(CmdError::Arg {
            name: "path".into(),
            msg: "path is required".into(),
        });
    };
    let focus = a.bool("focus").unwrap_or(false);
    // Облако — по атрибутам элемента под курсором (листинг, без I/O).
    let offline = matches!(
        quick_target(cx.state, side),
        QuickTarget::File { loc: ref l, offline: true } if *l == loc
    );
    let rt = cx.rt;
    let view = view_mut(cx)?;
    let i = side.idx();
    let docked = view.in_panel(side).is_some_and(|d| !d.quick);
    let same = view.in_panel(side).is_some_and(|d| d.loc == loc)
        || view.qnote[i].as_ref().is_some_and(|n| n.loc == loc);
    let mut changed = false;
    if !docked && !same {
        // Прошлое открытие отменяется токеном прошлого документа; id — новый (см. выше).
        drop_slot(view, side);
        let id = view.alloc_id();
        view.qnote[i] = None;
        if offline {
            view.qnote[i] = Some(QuickNote {
                loc,
                kind: QuickNoteKind::Cloud,
            });
        } else {
            open_quick(view, rt, env, id, loc, side);
        }
        changed = true;
    }
    if focus && let Some(id) = view.quick[i] {
        view.qfocus = Some((side, view.screens.len()));
        if let Some(d) = view.viewer_mut(id) {
            // При фокусе включается индекс (номера строк, переход, «из N»).
            d.start_index_quick(rt);
        }
        set_focus(cx, Focus::Viewer);
        changed = true;
    }
    if !changed {
        cx.keep_rev();
    }
    Ok(())
}

/// Документ быстрого просмотра: `Opening` в области; чтение — задача на Interactive, но не
/// больше одной на сторону: пока прошлая идёт (её отменил новый курсор), новый документ
/// ждёт её ответа ([`QuickOpened`] запускает следующую).
fn open_quick(
    view: &mut ViewState,
    rt: &Runtime,
    env: &ViewEnv,
    id: DocId,
    loc: Location,
    side: Side,
) {
    let cancel = rt.token().child();
    let d = ViewerDoc::new(id, loc, Placement::Panel(side), true, &view.viewer, cancel);
    view.insert(Doc::Viewer(d));
    view.quick[side.idx()] = Some(id);
    if view.qflight[side.idx()].is_none() {
        spawn_quick(view, rt, &env.vfs, side);
    }
}

/// Задача чтения документа области `side` (он в `Opening`); `qflight` — его id.
fn spawn_quick(view: &mut ViewState, rt: &Runtime, reg: &Arc<VfsRegistry>, side: Side) {
    let Some(d) = view.in_panel(side).filter(|d| !d.is_ready()) else {
        return;
    };
    let lang = urfin_i18n::current().lang().to_string();
    let opts = SourceOpts {
        follow: false,
        mmap: if view.viewer.mmap {
            crate::source::MmapPolicy::Auto
        } else {
            crate::source::MmapPolicy::Off
        },
        volume: None,
        // Поток без seek (архив) быстрый просмотр не копирует.
        spool_dir: None,
    };
    let job = OpenJob {
        doc: d.id,
        loc: d.loc.clone(),
        opts,
        tld: tld_for_lang(&lang),
        reg: reg.clone(),
    };
    let doc_cancel = d.cancel.clone();
    view.qflight[side.idx()] = Some(d.id);
    // Задача с токеном приложения: отменённую до старта пул не запускает, а ответ нужен
    // всегда — он запускает следующее чтение стороны. Отмена документа — внутри.
    rt.spawn(Lane::Interactive, "view.quick", rt.token(), move |cx| {
        let _span = tracing::debug_span!("view.quick").entered();
        let done = CancelToken::new();
        let reply = |res: QuickRes| QuickOpened {
            side,
            job_doc: job.doc,
            reg: job.reg.clone(),
            res,
        };
        if doc_cancel.is_cancelled() {
            let _ = cx.sink.send(Box::new(reply(QuickRes::Cancelled)), &done);
            return;
        }
        if is_cloud(&job.loc) {
            let _ = cx.sink.send(Box::new(reply(QuickRes::Cloud)), &done);
            return;
        }
        QUICK_READS.fetch_add(1, Ordering::Relaxed);
        let res = match open_source(&job.reg, &job.loc, &job.opts, &doc_cancel) {
            Err(VfsError::Cancelled) => {
                let _ = cx.sink.send(Box::new(reply(QuickRes::Cancelled)), &done);
                return;
            }
            Err(e) => Err(e.to_string()),
            Ok(o) if o.pump.is_some() => {
                Err(io::Error::from(io::ErrorKind::Unsupported).to_string())
            }
            Ok(o) => match probe(&*o.src, &job.name(), job.tld) {
                Ok(p) => Ok((o, Some(p))),
                Err(e) => Err(e.to_string()),
            },
        };
        let msg = reply(QuickRes::Opened(OpenedMsg {
            doc: job.doc,
            name: job.name(),
            tld: job.tld,
            res,
        }));
        let _ = cx.sink.send(Box::new(msg), &done);
    });
}

/// Облачная заглушка (атрибуты файла; `stat` скачивание не будит). Только вне UI.
fn is_cloud(loc: &Location) -> bool {
    if !loc.is_local() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        std::fs::symlink_metadata(&loc.path).is_ok_and(|m| m.file_attributes() & CLOUD_ATTRS != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = CLOUD_ATTRS;
        false
    }
}

enum QuickRes {
    /// Отменена (курсор ушёл, документ закрыт).
    Cancelled,
    Cloud,
    Opened(OpenedMsg),
}

/// Ответ задачи `view.quick`.
struct QuickOpened {
    side: Side,
    job_doc: DocId,
    reg: Arc<VfsRegistry>,
    res: QuickRes,
}

impl std::fmt::Debug for QuickOpened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickOpened")
            .field("side", &self.side)
            .field("doc", &self.job_doc)
            .finish_non_exhaustive()
    }
}

impl Msg for QuickOpened {
    fn name(&self) -> &'static str {
        "view.quick"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let QuickOpened {
            side,
            job_doc: doc,
            reg,
            res,
        } = *self;
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if view.qflight[side.idx()] == Some(doc) {
            view.qflight[side.idx()] = None;
        }
        // Документ ещё открывается (в области или уже в окне после F3) — ответ ему.
        let waiting = view.viewer(doc).is_some_and(|d| !d.is_ready());
        let in_slot = view.quick[side.idx()] == Some(doc);
        let loc = view.viewer(doc).map(|d| d.loc.clone());
        let note = match res {
            _ if !waiting => None,
            QuickRes::Cancelled => None,
            QuickRes::Cloud => Some(QuickNoteKind::Cloud),
            QuickRes::Opened(OpenedMsg { res: Err(e), .. }) => Some(QuickNoteKind::Error(e.into())),
            QuickRes::Opened(m) => {
                // Ошибок нет: общее открытие (детект, режим, первая страница).
                Box::new(m).apply(cx);
                if let Some(d) = cx
                    .state
                    .ext
                    .get_mut::<ViewState>()
                    .and_then(|v| v.viewer_mut(doc))
                    && d.kind == Some(SourceKind::Owned)
                {
                    d.start_index_quick(rt);
                }
                None
            }
        };
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let (Some(kind), Some(loc), true) = (note, loc, in_slot) {
            drop_slot(view, side);
            view.qnote[side.idx()] = Some(QuickNote { loc, kind });
        }
        // Курсор ушёл, пока шло чтение: следующее — для документа, что в области сейчас.
        if view.qflight[side.idx()].is_none() {
            spawn_quick(view, rt, &reg, side);
        }
        if !waiting {
            cx.keep_rev();
        }
    }
}

/// `viewer.dock`: вьювер окна → область пассивной панели (фокус — ему); документ области
/// с фокусом → окно.
pub(crate) fn dock(cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let rt = cx.rt;
    let passive = cx.state.active.other();
    let ours = matches!(
        cx.state.focus,
        Focus::Viewer | Focus::Overlay(OverlayKind::Find)
    );
    let view = view_mut(cx)?;
    let window_budget = (view.viewer.cache_mb.max(1) as usize) << 20;
    if let Some(side) = view.panel_focus().filter(|_| ours) {
        // Область → окно (F3 в фокусе быстрого просмотра).
        let Some(id) = view.quick[side.idx()] else {
            cx.keep_rev();
            return Ok(());
        };
        view.quick[side.idx()] = None;
        view.qfocus = None;
        if let Some(d) = view.viewer_mut(id) {
            d.relocate(Placement::Window, window_budget, rt);
        }
        view.screens.push(id);
        set_focus(cx, Focus::Viewer);
        return Ok(());
    }
    let top = view.active().filter(|id| {
        view.viewer(*id)
            .is_some_and(|d| d.placement == Placement::Window)
    });
    let Some(id) = top.filter(|_| ours) else {
        cx.keep_rev();
        return Ok(());
    };
    view.screens.retain(|d| *d != id);
    drop_slot(view, passive);
    view.qnote[passive.idx()] = None;
    view.quick[passive.idx()] = Some(id);
    if let Some(d) = view.viewer_mut(id) {
        let budget = d.budget;
        d.relocate(Placement::Panel(passive), budget, rt);
    }
    view.qfocus = Some((passive, view.screens.len()));
    set_focus(cx, Focus::Viewer);
    Ok(())
}

/// Начало прохода: быстрый просмотр выключен (`panel.mode`) — документ закрыт; фокус ушёл
/// из области — `qfocus` снят.
pub(crate) fn on_pump(cx: &mut Cx<'_>) {
    let ours = matches!(
        cx.state.focus,
        Focus::Viewer | Focus::Overlay(OverlayKind::Find)
    );
    let modes = [Side::Left, Side::Right].map(|s| cx.state.panel(s).tab().mode);
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let mut changed = false;
    for side in [Side::Left, Side::Right] {
        let i = side.idx();
        if modes[i] == PanelMode::QView {
            continue;
        }
        let quick_doc = view.in_panel(side).is_some_and(|d| d.quick);
        if quick_doc || (view.quick[i].is_none() && view.qnote[i].is_some()) {
            drop_slot(view, side);
            view.qnote[i] = None;
            changed = true;
        }
    }
    if view.qfocus.is_some() && (!ours || view.panel_focus().is_none()) {
        view.qfocus = None;
        changed = true;
    }
    if changed {
        cx.state.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_attrs_cover_recall_and_offline() {
        assert_eq!(CLOUD_ATTRS, 0x0044_1000);
    }
}
