//! Фоновый поиск документа (S7 §3.6): задача `spawn_cpu` с токеном на поиск, прогресс в
//! атомике, ответ `SearchMsg`; после первого совпадения — счёт до `count_limit` (метки на
//! полосе прокрутки, «3 из 120»). Новый образец отменяет и поиск, и счёт.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use urfin_core::{CancelToken, Cx, Level, Msg, Runtime};

use super::scan::{self, Counted, Hit, Scan, ScanError};
use super::{Matcher, Pattern, SearchError, SourceHay};
use crate::source::ByteSource;
use crate::{Doc, DocId, ViewState};

/// Запрос поиска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchReq {
    pub pat: Pattern,
    pub back: bool,
    /// Продолжать с другого конца (`[viewer] search_wrap`).
    pub wrap: bool,
}

/// Счётчик совпадений.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CountState {
    /// Найдено (не больше `count_limit`).
    pub n: usize,
    /// Есть ещё сверх предела («1 000+»).
    pub more: bool,
    pub done: bool,
}

/// Поиск документа: образец, ход, итог, счёт и метки.
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые признаки хода поиска"
)]
#[derive(Debug)]
pub struct SearchState {
    pub req: SearchReq,
    /// Ошибка образца (в строке поиска); тогда поиска нет.
    pub error: Option<SearchError>,
    pub matcher: Option<Arc<Matcher>>,
    /// Откуда шёл последний поиск.
    pub from: u64,
    /// Просмотрено байт и сколько всего (прогресс «Поиск… 41 %»).
    pub scanned: Arc<AtomicU64>,
    pub total: u64,
    pub running: bool,
    pub hit: Option<Range<u64>>,
    /// Последний поиск перешёл через конец файла (тост «Продолжено с начала»).
    pub wrapped: bool,
    pub not_found: bool,
    /// Ошибка чтения.
    pub failed: Option<String>,
    pub count: CountState,
    /// Начала совпадений (до `count_limit`) — риски на полосе прокрутки.
    pub marks: Arc<[u64]>,
    /// Поиск по проекции Markdown-документа (S7 §3.10): `hit`, `marks` — смещения в
    /// проекции, совпадение — ещё и `MdState::hit`.
    pub proj: bool,
    /// Номер поиска: ответы старых поисков отбрасываются.
    pub(crate) seq: u64,
    /// Номер образца: счёт относится к нему.
    pub(crate) pgen: u64,
    pub(crate) cancel: CancelToken,
    pub(crate) count_cancel: CancelToken,
    pub(crate) counting: bool,
}

impl SearchState {
    pub(crate) fn new(req: SearchReq, pgen: u64) -> SearchState {
        SearchState {
            req,
            error: None,
            matcher: None,
            from: 0,
            scanned: Arc::default(),
            total: 0,
            running: false,
            hit: None,
            wrapped: false,
            not_found: false,
            failed: None,
            count: CountState::default(),
            marks: Arc::from(Vec::new()),
            proj: false,
            seq: 0,
            pgen,
            cancel: CancelToken::new(),
            count_cancel: CancelToken::new(),
            counting: false,
        }
    }

    /// Доля просмотренного, 0…1.
    #[allow(clippy::cast_precision_loss, reason = "доля для полосы прогресса")]
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        (self.scanned.load(Ordering::Relaxed) as f64 / self.total as f64).min(1.0) as f32
    }

    /// Номер текущего совпадения среди посчитанных (с 1), если оно в метках.
    pub fn ordinal(&self) -> Option<usize> {
        let h = self.hit.as_ref()?;
        self.marks.binary_search(&h.start).ok().map(|i| i + 1)
    }

    /// Останавливает поиск и счёт.
    pub fn cancel(&mut self) {
        self.cancel.cancel();
        self.count_cancel.cancel();
        self.running = false;
        self.counting = false;
    }
}

/// Событие поиска.
#[derive(Debug)]
pub enum SearchEvent {
    Found(Option<Hit>),
    Counted(Counted),
    Failed(String),
}

/// Ответ задачи поиска → документ.
#[derive(Debug)]
pub struct SearchMsg {
    pub doc: DocId,
    pub seq: u64,
    pub ev: SearchEvent,
}

impl Msg for SearchMsg {
    fn name(&self) -> &'static str {
        "view.search"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let limit = view.viewer.count_limit as usize;
        let mut wrapped = false;
        if let Some(Doc::Viewer(d)) = view.doc_mut(self.doc) {
            wrapped = matches!(&self.ev, SearchEvent::Found(Some(h)) if h.wrapped)
                && d.search.as_ref().is_some_and(|s| s.seq == self.seq);
            d.on_search(self.seq, self.ev, rt, limit);
        }
        if wrapped {
            // Поиск перешёл через конец (начало) файла (S7 §3.6).
            cx.toast(Level::Info, urfin_i18n::current().get("ui-find-wrapped"));
        }
    }
}

/// Поиск следующего совпадения на `spawn_cpu`.
#[allow(clippy::too_many_arguments, reason = "одна задача — один набор входов")]
pub(crate) fn spawn_find(
    rt: &Runtime,
    doc: DocId,
    seq: u64,
    src: Arc<dyn ByteSource>,
    m: Arc<Matcher>,
    from: u64,
    req: &SearchReq,
    cancel: CancelToken,
    scanned: Arc<AtomicU64>,
) {
    let (back, wrap) = (req.back, req.wrap);
    rt.spawn_cpu("view.search", cancel, move |cx| {
        let _span = tracing::debug_span!("view.search").entered();
        let mut hay = SourceHay::new(&*src);
        let o = Scan {
            chunk: scan::CHUNK,
            cancel: &cx.cancel,
            scanned: &scanned,
        };
        let ev = match scan::find_next(&mut hay, &m, from, back, wrap, &o) {
            Ok(h) => SearchEvent::Found(h),
            Err(ScanError::Cancelled) => return,
            Err(ScanError::Io(e)) => SearchEvent::Failed(e.to_string()),
        };
        let _ = cx
            .sink
            .send(Box::new(SearchMsg { doc, seq, ev }), &cx.cancel);
    });
}

/// Счёт совпадений образца до `limit` на `spawn_cpu`.
pub(crate) fn spawn_count(
    rt: &Runtime,
    doc: DocId,
    pgen: u64,
    src: Arc<dyn ByteSource>,
    m: Arc<Matcher>,
    limit: usize,
    cancel: CancelToken,
) {
    rt.spawn_cpu("view.search.count", cancel, move |cx| {
        let _span = tracing::debug_span!("view.search.count").entered();
        let mut hay = SourceHay::new(&*src);
        let scanned = AtomicU64::new(0);
        let o = Scan {
            chunk: scan::CHUNK,
            cancel: &cx.cancel,
            scanned: &scanned,
        };
        let ev = match scan::count(&mut hay, &m, limit, &o) {
            Ok(c) => SearchEvent::Counted(c),
            Err(ScanError::Cancelled) => return,
            Err(ScanError::Io(e)) => {
                tracing::warn!(error = %e, "view search count stopped");
                return;
            }
        };
        let _ = cx
            .sink
            .send(Box::new(SearchMsg { doc, seq: pgen, ev }), &cx.cancel);
    });
}
