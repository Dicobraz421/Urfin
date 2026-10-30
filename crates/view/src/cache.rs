//! `PageCache`, `Page`, `Loader`: страницы по 64 КБ, LRU по бюджету, коалесцированные
//! запросы и прогрев вне UI-потока (S7 §0, §2). Карточка S7-03.
//!
//! UI рисует только из кэша: `get`/[`ByteView`] отдают срезы резидентных страниц или
//! `Missing`. Кадр просит видимый диапазон `want` (последний запрос побеждает), `pump`
//! превращает его в задачу `Loader` на `Lane::Interactive` (не больше [`MAX_INFLIGHT`]
//! в полёте), ответ приходит `Msg` [`PagesMsg`]. Страница mmap отдаётся без копии, но
//! только после того, как loader её «потрогал» (page fault — в воркере).

use std::cell::Cell;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use memmap2::Mmap;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use urfin_core::{CancelToken, Cx, Lane, Msg, Runtime};

use crate::source::{ByteSource, read_full};
use crate::{Doc, DocId, ViewState};

/// Размер страницы.
pub const PAGE: u64 = 1 << 16;
const PAGE_US: usize = 1 << 16;
/// Запросов loader'а в полёте на документ.
pub const MAX_INFLIGHT: usize = 2;
/// Шаг «прикосновения» к странице mmap (страница ОС).
const TOUCH_STEP: usize = 4096;

/// Страница кэша: копия или окно отображения.
#[derive(Clone)]
pub enum Page {
    Owned(Arc<[u8]>),
    Mapped {
        map: Arc<Mmap>,
        off: usize,
        len: usize,
    },
}

impl Page {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Page::Owned(b) => b,
            Page::Mapped { map, off, len } => &map[*off..*off + *len],
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Page::Owned(b) => b.len(),
            Page::Mapped { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Page::Owned(_) => "Owned",
            Page::Mapped { .. } => "Mapped",
        };
        write!(f, "Page::{kind}({})", self.len())
    }
}

/// Приоритет запроса.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Prio {
    /// Видимый экран.
    #[default]
    Visible,
    /// Фоновый (быстрый просмотр без фокуса).
    Background,
}

/// Нет страниц `pages` (номера страниц): на их месте — скелетон до `PagesMsg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub pages: Range<u64>,
}

impl Missing {
    fn at(off: u64) -> Missing {
        let p = off / PAGE;
        Missing { pages: p..p + 1 }
    }
}

/// Байты диапазона: 1–2 смежных среза (диапазон не длиннее страницы).
#[derive(Copy, Clone, Debug, Default)]
pub struct Bytes<'a> {
    pub head: &'a [u8],
    pub tail: &'a [u8],
}

impl Bytes<'_> {
    pub fn len(&self) -> usize {
        self.head.len() + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<u8> {
        match self.head.get(i) {
            Some(b) => Some(*b),
            None => self.tail.get(i - self.head.len()).copied(),
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len());
        v.extend_from_slice(self.head);
        v.extend_from_slice(self.tail);
        v
    }
}

/// Байты документа для раскладки и индекса: кэш страниц или срез в памяти (тесты, mmap).
#[allow(
    clippy::len_without_is_empty,
    reason = "длина источника, пустота не нужна"
)]
pub trait ByteView {
    fn len(&self) -> u64;
    /// Смежные байты от `off` до границы страницы; пусто только при `off >= len`.
    fn chunk(&self, off: u64) -> Result<&[u8], Missing>;
    /// Смежные байты, кончающиеся на `end`, от начала страницы; пусто только при `end == 0`.
    fn chunk_before(&self, end: u64) -> Result<&[u8], Missing>;
}

impl ByteView for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn chunk(&self, off: u64) -> Result<&[u8], Missing> {
        Ok(self.get(off as usize..).unwrap_or(&[]))
    }

    fn chunk_before(&self, end: u64) -> Result<&[u8], Missing> {
        Ok(&self[..(end as usize).min(<[u8]>::len(self))])
    }
}

struct Slot {
    page: Page,
    used: Cell<u64>,
}

/// Запрос loader'у.
#[derive(Clone, Debug)]
pub struct LoadReq {
    pub id: u64,
    pub pages: SmallVec<[u64; 32]>,
    pub prio: Prio,
}

/// Ответ loader'а.
#[derive(Debug)]
pub struct Loaded {
    pub id: u64,
    /// Все страницы запроса (в том числе не прочитанные) — снимаются с «в полёте».
    pub req: SmallVec<[u64; 32]>,
    pub pages: Vec<(u64, Page)>,
    pub error: Option<String>,
}

/// Читает страницы источника. Только вне UI-потока.
#[derive(Clone, Debug)]
pub struct Loader {
    src: Arc<dyn ByteSource>,
}

impl Loader {
    pub fn new(src: Arc<dyn ByteSource>) -> Loader {
        Loader { src }
    }

    pub fn src(&self) -> &Arc<dyn ByteSource> {
        &self.src
    }

    /// Страницы запроса; mmap — без копии, но после касания каждой страницы ОС.
    pub fn load(&self, req: &LoadReq, cancel: &CancelToken) -> Loaded {
        crate::source::assert_off_ui();
        let mut out = Loaded {
            id: req.id,
            req: req.pages.clone(),
            pages: Vec::with_capacity(req.pages.len()),
            error: None,
        };
        let len = self.src.len();
        for &no in &req.pages {
            if cancel.is_cancelled() {
                break;
            }
            let off = no * PAGE;
            if off >= len {
                continue;
            }
            let n = (len - off).min(PAGE) as usize;
            let page = if let Some(map) = self.src.map() {
                let at = off as usize;
                let bytes = &map[at..at + n];
                let mut x = 0u8;
                for i in (0..n).step_by(TOUCH_STEP) {
                    x ^= bytes[i];
                }
                std::hint::black_box(x);
                Page::Mapped {
                    map: map.clone(),
                    off: at,
                    len: n,
                }
            } else {
                let mut buf = vec![0u8; n];
                match read_full(&*self.src, off, &mut buf) {
                    Ok(k) => {
                        buf.truncate(k);
                        Page::Owned(buf.into())
                    }
                    Err(e) => {
                        out.error = Some(e.to_string());
                        break;
                    }
                }
            };
            out.pages.push((no, page));
        }
        out
    }
}

/// Кэш страниц документа (UI-поток): LRU по бюджету, «в полёте», отложенный запрос.
pub struct PageCache {
    slots: FxHashMap<u64, Slot>,
    /// Бюджет в страницах.
    cap: usize,
    clock: Cell<u64>,
    len: u64,
    fin: bool,
    pending: Option<(Range<u64>, Prio)>,
    inflight: FxHashSet<u64>,
    reqs: usize,
    next_id: u64,
    loader: Option<Loader>,
}

impl fmt::Debug for PageCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageCache")
            .field("pages", &self.slots.len())
            .field("cap", &self.cap)
            .field("len", &self.len)
            .field("reqs", &self.reqs)
            .finish_non_exhaustive()
    }
}

impl Default for PageCache {
    fn default() -> Self {
        PageCache::new(8 << 20, 0, true)
    }
}

impl PageCache {
    /// Кэш с бюджетом `budget` байт (8 МБ — документ, 1 МБ — быстрый просмотр).
    pub fn new(budget: usize, len: u64, fin: bool) -> PageCache {
        PageCache {
            slots: FxHashMap::default(),
            cap: (budget / PAGE_US).max(2),
            clock: Cell::new(0),
            len,
            fin,
            pending: None,
            inflight: FxHashSet::default(),
            reqs: 0,
            next_id: 1,
            loader: None,
        }
    }

    /// Кэш над источником (`pump` запускает loader).
    pub fn with_source(budget: usize, src: Arc<dyn ByteSource>) -> PageCache {
        let mut c = PageCache::new(budget, src.len(), src.is_final());
        c.loader = Some(Loader::new(src));
        c
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_final(&self) -> bool {
        self.fin
    }

    /// Новая длина (Spool, follow): неполная последняя страница перечитывается.
    pub fn set_len(&mut self, len: u64, fin: bool) {
        if len != self.len {
            let last = self.len / PAGE;
            if self
                .slots
                .get(&last)
                .is_some_and(|s| (s.page.len() as u64) < PAGE)
            {
                self.slots.remove(&last);
            }
            if len < self.len {
                self.slots.retain(|no, _| no * PAGE < len);
            }
            self.len = len;
        }
        self.fin = fin;
    }

    /// Резидентных байт (бюджет — `budget`).
    pub fn resident(&self) -> usize {
        self.slots.values().map(|s| s.page.len()).sum()
    }

    /// Резидентных страниц.
    pub fn pages(&self) -> usize {
        self.slots.len()
    }

    /// Бюджет в страницах.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Запросов в полёте.
    pub fn inflight(&self) -> usize {
        self.reqs
    }

    fn tick(&self) -> u64 {
        let t = self.clock.get() + 1;
        self.clock.set(t);
        t
    }

    /// Резидентная страница `no`, если она покрывает байты до `need` (не включая).
    fn slot(&self, no: u64, need: u64) -> Option<&[u8]> {
        let s = self.slots.get(&no)?;
        let b = s.page.bytes();
        if (b.len() as u64) < need - no * PAGE {
            return None;
        }
        s.used.set(self.tick());
        Some(b)
    }

    /// Байты `range` (не длиннее страницы, обрезается по длине документа).
    pub fn get(&self, range: Range<u64>) -> Result<Bytes<'_>, Missing> {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        if start == end {
            return Ok(Bytes::default());
        }
        debug_assert!(
            end - start <= PAGE,
            "PageCache::get: range longer than a page"
        );
        let (p0, p1) = (start / PAGE, (end - 1) / PAGE);
        let first = self
            .slot(p0, end.min((p0 + 1) * PAGE))
            .ok_or(Missing { pages: p0..p1 + 1 })?;
        let lo = (start - p0 * PAGE) as usize;
        if p0 == p1 {
            let hi = (end - p0 * PAGE) as usize;
            return Ok(Bytes {
                head: &first[lo..hi],
                tail: &[],
            });
        }
        let second = self.slot(p1, end).ok_or(Missing { pages: p1..p1 + 1 })?;
        Ok(Bytes {
            head: &first[lo..],
            tail: &second[..(end - p1 * PAGE) as usize],
        })
    }

    /// Просьба кадра: видимый диапазон. Копится до `take_request`; последняя побеждает.
    pub fn want(&mut self, range: Range<u64>, prio: Prio) {
        self.pending = Some((range, prio));
    }

    /// Нужна ли странице загрузка.
    fn needs(&self, no: u64) -> bool {
        let off = no * PAGE;
        if off >= self.len || self.inflight.contains(&no) {
            return false;
        }
        let full = (self.len - off).min(PAGE) as usize;
        self.slots.get(&no).is_none_or(|s| s.page.len() < full)
    }

    /// Отложенный запрос → задание loader'у: видимые страницы, затем прогрев на экран ниже
    /// и выше; уже резидентные и «в полёте» — нет. `None` — нечего грузить или в полёте
    /// уже [`MAX_INFLIGHT`] (тогда запрос ждёт).
    pub fn take_request(&mut self) -> Option<LoadReq> {
        if self.reqs >= MAX_INFLIGHT {
            return None;
        }
        let (r, prio) = self.pending.take()?;
        let end = r.end.min(self.len);
        let start = r.start.min(end);
        let span = (end - start).max(1);
        let pages = |a: u64, b: u64| (a / PAGE)..b.div_ceil(PAGE);
        let vis = pages(start, end.max(start + 1));
        let below = pages(end, end.saturating_add(span).min(self.len));
        let above = pages(start.saturating_sub(span), start);
        let limit = (self.cap / 2).max(1);
        let mut want: SmallVec<[u64; 32]> = SmallVec::new();
        for no in vis.chain(below).chain(above.rev()) {
            if want.len() >= limit {
                break;
            }
            if self.needs(no) && !want.contains(&no) {
                want.push(no);
            }
        }
        if want.is_empty() {
            return None;
        }
        self.inflight.extend(want.iter().copied());
        self.reqs += 1;
        let id = self.next_id;
        self.next_id += 1;
        Some(LoadReq {
            id,
            pages: want,
            prio,
        })
    }

    /// Ответ loader'а: страницы в кэш, LRU-вытеснение сверх бюджета.
    pub fn insert(&mut self, loaded: Loaded) {
        self.reqs = self.reqs.saturating_sub(1);
        for no in &loaded.req {
            self.inflight.remove(no);
        }
        if let Some(e) = &loaded.error {
            tracing::warn!(error = %e, "view page load failed");
        }
        for (no, page) in loaded.pages {
            let used = Cell::new(self.tick());
            self.slots.insert(no, Slot { page, used });
        }
        while self.slots.len() > self.cap {
            let Some(old) = self
                .slots
                .iter()
                .min_by_key(|(_, s)| s.used.get())
                .map(|(no, _)| *no)
            else {
                break;
            };
            self.slots.remove(&old);
        }
    }

    /// Запускает loader для отложенного запроса (UI-поток, после `want` и после
    /// `PagesMsg`). Без источника — ничего.
    pub fn pump(&mut self, rt: &Runtime, doc: DocId, cancel: &CancelToken) {
        let Some(loader) = self.loader.clone() else {
            return;
        };
        while let Some(req) = self.take_request() {
            let loader = loader.clone();
            rt.spawn(Lane::Interactive, "view.load", cancel.clone(), move |cx| {
                let out = loader.load(&req, &cx.cancel);
                let _ = cx
                    .sink
                    .send(Box::new(PagesMsg { doc, loaded: out }), &cx.cancel);
            });
        }
    }
}

impl ByteView for PageCache {
    fn len(&self) -> u64 {
        self.len
    }

    fn chunk(&self, off: u64) -> Result<&[u8], Missing> {
        if off >= self.len {
            return Ok(&[]);
        }
        let no = off / PAGE;
        let b = self.slot(no, off + 1).ok_or_else(|| Missing::at(off))?;
        let end = b.len().min((self.len - no * PAGE) as usize);
        Ok(&b[(off - no * PAGE) as usize..end])
    }

    fn chunk_before(&self, end: u64) -> Result<&[u8], Missing> {
        let end = end.min(self.len);
        if end == 0 {
            return Ok(&[]);
        }
        let no = (end - 1) / PAGE;
        let b = self.slot(no, end).ok_or_else(|| Missing::at(end - 1))?;
        Ok(&b[..(end - no * PAGE) as usize])
    }
}

/// Страницы от loader'а → кэш документа; следом — отложенный запрос.
#[derive(Debug)]
pub struct PagesMsg {
    pub doc: DocId,
    pub loaded: Loaded,
}

impl Msg for PagesMsg {
    fn name(&self) -> &'static str {
        "view.pages"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let PagesMsg { doc, loaded } = *self;
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(Doc::Viewer(d)) = view.doc_mut(doc) {
            d.pages.insert(loaded);
            d.pages.pump(rt, doc, &d.cancel);
        }
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
