//! Документ из кусков: дерево, источники и их кэши страниц (`TextDoc`), чтение на
//! UI-потоке по кэшам ([`TextView`]: `ByteView` для раскладки), снимок для фоновых задач
//! ([`TextSnap`]) и окно поиска S7-09 ([`TextHay`]).

use std::cell::RefCell;
use std::fmt;
use std::io;
use std::ops::Range;
use std::sync::{Arc, mpsc};

use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use super::add::AddSnap;
use super::{AddBuf, BLOCK, CountLf, Lf, PIECE_MAX, Piece, Pieces, Splice, Src, Tree};
use crate::cache::{ByteView, LoadReq, Loaded, Loader, Missing, PAGE, PageCache, Prio};
use crate::edit::fold::Folds;
use crate::index::{LineGuess, LineIndex, Sep, count_seps, goto_line, scan};
use crate::search::Hay;
use crate::source::{ByteSource, read_full};

/// Кэш страниц исходника (файл больше 16 МБ; меньший — в памяти целиком).
pub const ORIG_CACHE: usize = 16 << 20;
/// Кэш страниц временного файла `Rewrite`.
const REWRITE_CACHE: usize = 2 << 20;
/// Переводы строк в диапазоне правок считаются по байтам, только если он не длиннее
/// (у исходника — только по индексу).
const DIRECT_MAX: u64 = 64 << 10;

/// Временный файл массовой правки и его кэш.
struct Rewrite {
    src: Arc<dyn ByteSource>,
    pages: PageCache,
}

/// Источники документа и их кэши (UI-поток).
pub(crate) struct Store {
    pub(crate) sep: Sep,
    pub(crate) orig: Arc<dyn ByteSource>,
    pub(crate) pages: PageCache,
    pub(crate) add: AddBuf,
    rw: Vec<Rewrite>,
    rw_src: Arc<Vec<Arc<dyn ByteSource>>>,
    /// Индекс строк исходника (его координаты; S7-27).
    pub(crate) index: LineIndex,
    /// Точные номера строк исходника на краях кусков (смещение → строка): досчёт края
    /// не ждёт страниц, ушедших из кэша.
    pub(crate) lines_at: RefCell<FxHashMap<u64, u64>>,
}

/// Номеров строк на краях кусков помнится не больше (дальше — заново).
const LINES_AT_MAX: usize = 1 << 16;

impl Store {
    /// Байты источника с `off` (до границы страницы или блока).
    fn chunk(&self, src: Src, off: u64) -> Result<&[u8], Missing> {
        match src {
            Src::Original => match self.orig.mapped() {
                Some(m) => m.chunk(off),
                None => self.pages.chunk(off),
            },
            Src::Add => match self.add.mem_chunk(off) {
                Some(b) => Ok(b),
                None => self.add.pages.chunk(off),
            },
            Src::Rewrite(n) => {
                let r = &self.rw[usize::from(n)];
                match r.src.mapped() {
                    Some(m) => m.chunk(off),
                    None => r.pages.chunk(off),
                }
            }
        }
    }

    /// Байты источника, кончающиеся на `end`.
    fn chunk_before(&self, src: Src, end: u64) -> Result<&[u8], Missing> {
        match src {
            Src::Original => match self.orig.mapped() {
                Some(m) => m.chunk_before(end),
                None => self.pages.chunk_before(end),
            },
            Src::Add => match self.add.mem_chunk_before(end) {
                Some(b) => Ok(b),
                None => self.add.pages.chunk_before(end),
            },
            Src::Rewrite(n) => {
                let r = &self.rw[usize::from(n)];
                match r.src.mapped() {
                    Some(m) => m.chunk_before(end),
                    None => r.pages.chunk_before(end),
                }
            }
        }
    }

    /// Вид источника в его координатах (счёт строк, поиск разделителей).
    pub(crate) fn view(&self, src: Src) -> SrcViewOf<'_> {
        SrcViewOf { store: self, src }
    }

    /// Переводов строк в диапазоне источника по байтам, которые есть без I/O.
    pub(crate) fn count_bytes(&self, src: Src, r: Range<u64>) -> Option<u32> {
        let n = count_seps(&self.view(src), r, self.sep).ok()?;
        u32::try_from(n).ok()
    }

    /// Индекс исходника покрывает смещение `off`.
    fn covers(&self, off: u64) -> bool {
        off <= self.index.upto
    }

    /// Точный номер строки исходника в `off`: метка индекса и досчёт ≤ 1024 строк по
    /// байтам без I/O; `None` — индекс не дошёл или страниц нет.
    pub(crate) fn orig_line(&self, off: u64) -> Option<u64> {
        if !self.covers(off) {
            return None;
        }
        if let Some(n) = self.lines_at.borrow().get(&off) {
            return Some(*n);
        }
        let Ok(LineGuess::Exact(n)) =
            self.index
                .line_of_in(&self.view(Src::Original), off, self.sep)
        else {
            return None;
        };
        self.remember(off, n);
        Some(n)
    }

    /// Запоминает номер строки исходника в `off`.
    pub(crate) fn remember(&self, off: u64, line: u64) {
        let mut m = self.lines_at.borrow_mut();
        if m.len() >= LINES_AT_MAX {
            m.clear();
        }
        m.insert(off, line);
    }

    /// Переводов строк в диапазоне источника: у исходника — по индексу (две метки и
    /// досчёт), иначе — по байтам, если диапазон короткий.
    pub(crate) fn count_lf(&self, src: Src, r: Range<u64>) -> Option<u32> {
        if src == Src::Original {
            let n = self.orig_line(r.end)? - self.orig_line(r.start)?;
            return u32::try_from(n).ok();
        }
        if r.end - r.start > DIRECT_MAX {
            return None;
        }
        self.count_bytes(src, r)
    }

    /// Смещение источника сразу за `n`-м (с 1) разделителем в `r`; `None` — не узнать
    /// без I/O или в `r` их меньше.
    pub(crate) fn nth_sep(&self, src: Src, r: Range<u64>, n: u64) -> Option<u64> {
        let view = self.view(src);
        let found = if src == Src::Original && self.covers(r.end) {
            let target = self.orig_line(r.start)? + n;
            goto_line(&view, self.index.mark_before_line(target), target, self.sep).ok()?
        } else {
            let mut left = n;
            let mut pos = r.start;
            let mut found = None;
            while found.is_none() && pos < r.end {
                let c = view.chunk(pos).ok()?;
                if c.is_empty() {
                    break;
                }
                let c = &c[..c.len().min((r.end - pos) as usize)];
                scan(c, pos, self.sep, |s| {
                    if found.is_none() {
                        left -= 1;
                        if left == 0 {
                            found = Some(s);
                        }
                    }
                });
                pos += c.len() as u64;
            }
            found
        };
        found.filter(|&f| f <= r.end)
    }
}

/// Один источник как `ByteView` (координаты источника).
#[derive(Clone, Copy)]
pub(crate) struct SrcViewOf<'a> {
    store: &'a Store,
    src: Src,
}

impl ByteView for SrcViewOf<'_> {
    fn len(&self) -> u64 {
        match self.src {
            Src::Original => self.store.orig.len(),
            Src::Add => self.store.add.len(),
            Src::Rewrite(n) => self.store.rw[usize::from(n)].src.len(),
        }
    }

    fn chunk(&self, off: u64) -> Result<&[u8], Missing> {
        if off >= self.len() {
            return Ok(&[]);
        }
        self.store.chunk(self.src, off)
    }

    fn chunk_before(&self, end: u64) -> Result<&[u8], Missing> {
        if end == 0 {
            return Ok(&[]);
        }
        self.store.chunk_before(self.src, end)
    }
}

/// Куски `Original` над исходником `[bom, len)` (не длиннее [`PIECE_MAX`]); переводы
/// строк — там, куда дошёл индекс.
fn orig_tree(store: &Store, bom: u64) -> Tree {
    let len = store.orig.len();
    let mut pieces = Vec::new();
    let mut at = bom.min(len);
    while at < len {
        let end = len.min(at + PIECE_MAX);
        let lf = store
            .count_lf(Src::Original, at..end)
            .map_or(Lf::Unknown, Lf::Known);
        pieces.push(Piece {
            src: Src::Original,
            start: at,
            len: end - at,
            lf,
        });
        at = end;
    }
    Tree::from_pieces(pieces)
}

/// Документ редактора из кусков: дерево и источники (UI-поток).
pub struct TextDoc {
    pub(crate) tree: Tree,
    pub(crate) store: Store,
    /// Правок текста (не счёта строк): сохранение сравнивает со снимком (S7-31).
    edits: u64,
    /// Свёрнутое (S7-35): слой отображения, правка сдвигает или разворачивает.
    pub folds: Folds,
    /// Правки по порядку для сдвига подсветки до нового разбора (S7-35); `None` — счёт
    /// потерян (новое дерево, переполнение): подсветка ждёт разбора.
    log: Option<Vec<TextEdit>>,
}

/// Правка текста: на `at` убрано `old` байт, вставлено `new`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub at: u64,
    pub old: u64,
    pub new: u64,
}

/// Правок в журнале подсветки не больше (дальше — «счёт потерян»).
const LOG_MAX: usize = 4096;

impl fmt::Debug for TextDoc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextDoc")
            .field("len", &self.tree.len())
            .field("pieces", &self.tree.sums().pieces)
            .field("sep", &self.store.sep)
            .field("add", &self.store.add)
            .finish_non_exhaustive()
    }
}

impl TextDoc {
    /// Документ над исходником: байты `[bom, len)` — куски `Original` (не длиннее
    /// [`PIECE_MAX`]). Переводы строк кусков известны там, куда дошёл индекс `index`
    /// (файл до 16 МБ индексируется на открытии), иначе `Unknown`.
    pub fn new(
        orig: Arc<dyn ByteSource>,
        bom: u64,
        sep: Sep,
        add: AddBuf,
        index: LineIndex,
    ) -> TextDoc {
        let pages = PageCache::with_source(ORIG_CACHE, orig.clone());
        let store = Store {
            sep,
            orig,
            pages,
            add,
            rw: Vec::new(),
            rw_src: Arc::new(Vec::new()),
            index,
            lines_at: RefCell::default(),
        };
        TextDoc {
            tree: orig_tree(&store, bom),
            store,
            edits: 0,
            folds: Folds::default(),
            log: Some(Vec::new()),
        }
    }

    /// Сохранено потоком (S7-31): исходник — новый файл `src` (байты `[bom, len)` = текст
    /// документа), индекс строк — его. Прежний исходник остаётся источником для отмены —
    /// `Rewrite(n)` (его handle держит байты старой версии, место на диске освобождается
    /// при закрытии документа); куски журнала переименовывает вызывающий
    /// (`Journal::rename_src(Original, n)`). Ответы кэша прежнего исходника не кладутся.
    pub fn rebase(&mut self, src: Arc<dyn ByteSource>, bom: u64, index: LineIndex) -> Src {
        let old = std::mem::replace(&mut self.store.orig, src.clone());
        self.store.pages.replace_source(src);
        let slot = self.push_rewrite(old);
        self.store.index = index;
        self.store.lines_at.borrow_mut().clear();
        self.tree = orig_tree(&self.store, bom);
        self.edits += 1;
        slot
    }

    /// Счётчик правок текста: тот же — текст тот же, что у снимка того момента (счёт
    /// переводов строк его не меняет).
    pub fn edits(&self) -> u64 {
        self.edits
    }

    pub fn len(&self) -> u64 {
        self.tree.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn sep(&self) -> Sep {
        self.store.sep
    }

    pub fn add(&self) -> &AddBuf {
        &self.store.add
    }

    /// Новое дерево целиком (отмена и повтор массовой правки): свёрнутое разворачивается.
    pub fn set_tree(&mut self, tree: Tree) {
        self.tree = tree;
        self.edits += 1;
        self.folds.clear();
        self.log = None;
    }

    /// Правки с прошлого вызова; `None` — счёт потерян (новое дерево, переполнение).
    pub fn take_log(&mut self) -> Option<Vec<TextEdit>> {
        self.log.replace(Vec::new())
    }

    /// Новый разделитель (перевод кодировки, отмена перевода): куски дерева — уже в нём.
    pub fn set_sep(&mut self, sep: Sep) {
        self.store.sep = sep;
    }

    /// Временный файл массовой правки — новый источник `Rewrite(n)`.
    pub fn push_rewrite(&mut self, src: Arc<dyn ByteSource>) -> Src {
        let n = u16::try_from(self.store.rw.len()).expect("rewrite sources fit u16");
        let pages = PageCache::with_source(REWRITE_CACHE, src.clone());
        Arc::make_mut(&mut self.store.rw_src).push(src.clone());
        self.store.rw.push(Rewrite { src, pages });
        Src::Rewrite(n)
    }

    /// Вид для раскладки и рендера: байты только из кэшей (без I/O).
    pub fn view(&self) -> TextView<'_> {
        TextView {
            tree: &self.tree,
            store: &self.store,
        }
    }

    /// Снимок (O(1), `Send + Sync`): поиск, сохранение, подсветка — в фоне.
    pub fn snap(&self) -> TextSnap {
        TextSnap {
            tree: self.tree.clone(),
            orig: self.store.orig.clone(),
            add: self.store.add.snap(),
            rw: self.store.rw_src.clone(),
            sep: self.store.sep,
        }
    }

    /// Просьба кадра: страницы источников под диапазон документа (последняя побеждает).
    pub fn want(&mut self, range: Range<u64>, prio: Prio) {
        let mut hull: SmallVec<[(Src, Range<u64>); 4]> = SmallVec::new();
        for (ps, p) in self.tree.iter_from(range.start) {
            if ps >= range.end {
                break;
            }
            let a = p.start + range.start.saturating_sub(ps);
            let b = p.start + (range.end - ps).min(p.len);
            match hull.iter_mut().find(|(s, _)| *s == p.src) {
                Some((_, r)) => *r = r.start.min(a)..r.end.max(b),
                None => hull.push((p.src, a..b)),
            }
        }
        for (src, r) in hull {
            match src {
                Src::Original => {
                    if self.store.orig.mapped().is_none() {
                        self.store.pages.want(r, prio);
                    }
                }
                Src::Add => {
                    let spilled = self.store.add.spilled();
                    if r.start < spilled {
                        self.store.add.pages.want(r.start..r.end.min(spilled), prio);
                    }
                }
                Src::Rewrite(n) => {
                    let rw = &mut self.store.rw[usize::from(n)];
                    if rw.src.mapped().is_none() {
                        rw.pages.want(r, prio);
                    }
                }
            }
        }
    }

    /// Запросы кэшей к loader'ам (после [`TextDoc::want`]): источник, loader, запрос.
    pub(crate) fn take_requests(&mut self) -> Vec<(Src, Loader, LoadReq)> {
        let mut out = Vec::new();
        if self.store.orig.mapped().is_none() {
            while let Some(req) = self.store.pages.take_request() {
                out.push((Src::Original, Loader::new(self.store.orig.clone()), req));
            }
        }
        if let Some(spill) = self.store.add.spill_src() {
            while let Some(req) = self.store.add.pages.take_request() {
                out.push((Src::Add, Loader::new(spill.clone()), req));
            }
        }
        for (i, r) in self.store.rw.iter_mut().enumerate() {
            if r.src.mapped().is_some() {
                continue;
            }
            let n = u16::try_from(i).expect("rewrite sources fit u16");
            while let Some(req) = r.pages.take_request() {
                out.push((Src::Rewrite(n), Loader::new(r.src.clone()), req));
            }
        }
        out
    }

    /// Ответ loader'а → кэш источника.
    pub(crate) fn insert_pages(&mut self, src: Src, loaded: Loaded) {
        match src {
            Src::Original => self.store.pages.insert(loaded),
            Src::Add => self.store.add.pages.insert(loaded),
            Src::Rewrite(n) => {
                if let Some(r) = self.store.rw.get_mut(usize::from(n)) {
                    r.pages.insert(loaded);
                }
            }
        }
    }

    /// Исходник — другой источник с теми же байтами («Править копию», ADR-0023 §8 п. 3;
    /// новый файл после сохранения, S7-31): кэш страниц — заново.
    pub fn replace_orig(&mut self, src: Arc<dyn ByteSource>) {
        self.store.pages.replace_source(src.clone());
        self.store.orig = src;
    }

    /// Источник исходника.
    pub fn orig(&self) -> &Arc<dyn ByteSource> {
        &self.store.orig
    }

    /// Подкачка страниц кэшей под диапазон здесь же (тесты; в приложении — `Loader` в
    /// фоне).
    #[cfg(test)]
    pub(crate) fn load_now(&mut self, range: Range<u64>) {
        let cancel = urfin_core::CancelToken::new();
        let len = range.end.min(self.len());
        let mut at = range.start;
        while at < len {
            let end = len.min(at + (256 << 10));
            self.want(at..end, Prio::Visible);
            let spill = self.store.add.spill_src();
            let mut caches: Vec<(&mut PageCache, Arc<dyn ByteSource>)> = Vec::new();
            if self.store.orig.mapped().is_none() {
                caches.push((&mut self.store.pages, self.store.orig.clone()));
            }
            if let Some(s) = spill {
                caches.push((&mut self.store.add.pages, s));
            }
            for r in &mut self.store.rw {
                caches.push((&mut r.pages, r.src.clone()));
            }
            for (c, src) in caches {
                while let Some(req) = c.take_request() {
                    c.insert(crate::cache::Loader::new(src.clone()).load(&req, &cancel));
                }
            }
            at = end;
        }
    }

    /// Куски байт `data`, дописанных в `Add` на `r`: по блокам, переводы строк — `scan`
    /// с `Sep` документа.
    fn add_pieces(&self, r: Range<u64>, data: &[u8]) -> Pieces {
        let mut out = Pieces::new();
        let mut at = r.start;
        while at < r.end {
            let end = r.end.min((at / BLOCK + 1) * BLOCK);
            let mut n = 0u32;
            let b = &data[(at - r.start) as usize..(end - r.start) as usize];
            scan(b, at, self.store.sep, |_| n += 1);
            out.push(Piece {
                src: Src::Add,
                start: at,
                len: end - at,
                lf: Lf::Known(n),
            });
            at = end;
        }
        out
    }

    /// Байты — в буфер правок одним куском; куски над частями `parts` (смещения в
    /// `bytes`): «Заменить все» (S7-30), замена `i` — `parts[i]`.
    pub fn push_add_parts(
        &mut self,
        bytes: &[u8],
        parts: &[Range<usize>],
    ) -> io::Result<Vec<Pieces>> {
        let r = self.store.add.push(bytes)?;
        Ok(parts
            .iter()
            .map(|p| {
                let a = r.start + p.start as u64..r.start + p.end as u64;
                self.add_pieces(a, &bytes[p.clone()])
            })
            .collect())
    }

    /// Заменяет `range` байтами `bytes` (в кодировке документа). Вставка конца строки
    /// перед таким же концом строки сдвигается за него («Enter» в конце строки — новая
    /// строка `Add`, а не правка текущей): байты те же, полоса изменений точнее.
    pub fn replace(&mut self, range: Range<u64>, bytes: &[u8]) -> io::Result<Splice> {
        let mut at = range.start;
        let mut rotated = Vec::new();
        let mut data = bytes;
        if range.is_empty()
            && let Some(p) = self.eol_prefix(at, bytes)
        {
            rotated.extend_from_slice(&bytes[p..]);
            rotated.extend_from_slice(&bytes[..p]);
            data = &rotated;
            at += p as u64;
        }
        let r = self.store.add.push(data)?;
        let inserted = self.add_pieces(r, data);
        let removed = self.splice(at, range.end - range.start, &inserted);
        Ok(Splice {
            at,
            removed,
            inserted,
        })
    }

    /// Вставка `bytes` на `at`.
    pub fn insert(&mut self, at: u64, bytes: &[u8]) -> io::Result<Splice> {
        self.replace(at..at, bytes)
    }

    /// Удаление `range`.
    pub fn remove(&mut self, range: Range<u64>) -> Splice {
        let at = range.start;
        let removed = self.splice(at, range.end - range.start, &[]);
        Splice {
            at,
            removed,
            inserted: Pieces::new(),
        }
    }

    /// Убирает `len` байт с `at` и вставляет готовые куски; возвращает убранные.
    pub fn splice(&mut self, at: u64, len: u64, pieces: &[Piece]) -> Pieces {
        let store = &self.store;
        let count: CountLf<'_> = &|s, r| store.count_lf(s, r);
        self.edits += 1;
        let new = super::bytes_of(pieces);
        self.folds.edit(at, len, new);
        if let Some(log) = &mut self.log {
            if log.len() < LOG_MAX {
                log.push(TextEdit { at, old: len, new });
            } else {
                self.log = None;
            }
        }
        let removed = self.tree.remove(at..at + len, count);
        self.tree.insert(at, pieces, count);
        self.tree.seam(at, count);
        self.tree.seam(at + new, count);
        removed
    }

    /// Длина начального конца строки в `bytes`, если он же стоит в документе на `at`.
    fn eol_prefix(&self, at: u64, bytes: &[u8]) -> Option<usize> {
        let sep = self.store.sep;
        let u = sep.unit() as usize;
        let mut p = None;
        scan(&bytes[..bytes.len().min(2 * u)], at, sep, |s| {
            p.get_or_insert((s - at) as usize);
        });
        let p = p?;
        let eol = |c: &[u8]| match sep {
            Sep::Byte(_) => matches!(c, [b'\r' | b'\n']),
            Sep::Le(_) => matches!(c, [b'\r' | b'\n', 0]),
            Sep::Be(_) => matches!(c, [0, b'\r' | b'\n']),
        };
        if !bytes[..p].chunks(u).all(eol) {
            return None;
        }
        let mut here = [0u8; 4];
        let view = self.view();
        let mut got = 0;
        while got < p {
            let c = view.chunk(at + got as u64).ok()?;
            if c.is_empty() {
                return None;
            }
            let k = c.len().min(p - got);
            here[got..got + k].copy_from_slice(&c[..k]);
            got += k;
        }
        (here[..p] == bytes[..p]).then_some(p)
    }
}

/// Байты документа на UI-потоке: куски по дереву, байты — из кэшей страниц
/// (`Missing` — страниц нет; номера страниц — в координатах документа).
#[derive(Clone, Copy)]
pub struct TextView<'a> {
    tree: &'a Tree,
    store: &'a Store,
}

impl fmt::Debug for TextView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextView")
            .field("len", &self.tree.len())
            .finish_non_exhaustive()
    }
}

fn missing(off: u64) -> Missing {
    let p = off / PAGE;
    Missing { pages: p..p + 1 }
}

impl ByteView for TextView<'_> {
    fn len(&self) -> u64 {
        self.tree.len()
    }

    fn chunk(&self, off: u64) -> Result<&[u8], Missing> {
        let Some((ps, p)) = self.tree.find(off) else {
            return Ok(&[]);
        };
        let rel = off - ps;
        let b = self
            .store
            .chunk(p.src, p.start + rel)
            .map_err(|_| missing(off))?;
        let n = b.len().min((p.len - rel) as usize);
        if n == 0 {
            return Err(missing(off));
        }
        Ok(&b[..n])
    }

    fn chunk_before(&self, end: u64) -> Result<&[u8], Missing> {
        let end = end.min(self.tree.len());
        if end == 0 {
            return Ok(&[]);
        }
        let (ps, p) = self.tree.find(end - 1).expect("end - 1 < len");
        let rel = end - ps;
        let b = self
            .store
            .chunk_before(p.src, p.start + rel)
            .map_err(|_| missing(end - 1))?;
        let n = b.len().min(rel as usize);
        if n == 0 {
            return Err(missing(end - 1));
        }
        Ok(&b[b.len() - n..])
    }

    /// Строка на весь файл: на ряд — десятки КиБ `memchr`, рядов — сотни на кадр вместе с
    /// мини-картой, а счёт кусков уже всё знает.
    fn no_sep(&self, r: Range<u64>, sep: Sep) -> bool {
        const PIECES: usize = 8;
        if sep != self.store.sep {
            return false;
        }
        self.tree
            .iter_from(r.start)
            .take(PIECES)
            .take_while(|(_, p)| p.lf == Lf::Known(0))
            .any(|(ps, p)| ps + p.len >= r.end)
    }
}

/// Снимок документа: дерево и источники (`Clone` — O(1), `Send + Sync`). Читает
/// источники напрямую — только вне UI-потока.
#[derive(Clone)]
pub struct TextSnap {
    tree: Tree,
    orig: Arc<dyn ByteSource>,
    add: AddSnap,
    rw: Arc<Vec<Arc<dyn ByteSource>>>,
    sep: Sep,
}

impl fmt::Debug for TextSnap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextSnap")
            .field("len", &self.tree.len())
            .field("pieces", &self.tree.sums().pieces)
            .finish_non_exhaustive()
    }
}

impl TextSnap {
    pub fn len(&self) -> u64 {
        self.tree.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn sep(&self) -> Sep {
        self.sep
    }

    /// Чтение источника с `off` (не всё сразу: до конца блока `Add`).
    pub fn read_src(&self, src: Src, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        match src {
            Src::Original => self.orig.read_at(off, buf),
            Src::Add => self.add.read_at(off, buf),
            Src::Rewrite(n) => self.rw[usize::from(n)].read_at(off, buf),
        }
    }

    /// Читает `buf.len()` байт источника с `off` (меньше — источник кончился).
    pub fn read_src_full(&self, src: Src, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if src == Src::Original {
            return read_full(&*self.orig, off, buf);
        }
        let mut done = 0;
        while done < buf.len() {
            match self.read_src(src, off + done as u64, &mut buf[done..]) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    /// Байты документа с `off` в `buf` (по кускам); меньше `buf.len()` — конец документа.
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut done = 0usize;
        for (ps, p) in self.tree.iter_from(off) {
            if done == buf.len() {
                break;
            }
            let rel = (off + done as u64) - ps;
            let k = ((p.len - rel) as usize).min(buf.len() - done);
            let n = self.read_src_full(p.src, p.start + rel, &mut buf[done..done + k])?;
            if n < k {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "piece source is shorter than the piece",
                ));
            }
            done += k;
        }
        Ok(done)
    }
}

/// Запас под перекрытие окон в начале буфера [`TextHay`].
const HAY_HEAD: usize = 256 << 10;

type HayBlock = (io::Result<()>, Vec<u8>);

/// Поток чтения впереди прохода: читает следующее окно, пока идёт поиск по текущему.
#[derive(Debug)]
struct HayReader {
    ask: mpsc::Sender<(Range<u64>, Vec<u8>)>,
    got: mpsc::Receiver<HayBlock>,
    /// Что читается сейчас.
    busy: Option<Range<u64>>,
}

impl HayReader {
    fn new(snap: TextSnap) -> io::Result<HayReader> {
        let (ask, jobs) = mpsc::channel::<(Range<u64>, Vec<u8>)>();
        let (done, got) = mpsc::channel();
        std::thread::Builder::new()
            .name("edit-hay".into())
            .spawn(move || {
                for (r, mut b) in jobs {
                    let res = hay_read(&snap, r.start, (r.end - r.start) as usize, &mut b);
                    if done.send((res, b)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(HayReader {
            ask,
            got,
            busy: None,
        })
    }

    /// Прочитанное впереди, если это `r`; другое — выбрасывается.
    fn take(&mut self, r: &Range<u64>) -> io::Result<Option<Vec<u8>>> {
        let Some(busy) = self.busy.take() else {
            return Ok(None);
        };
        let (res, b) = self.got.recv().map_err(io::Error::other)?;
        if busy != *r {
            return Ok(None);
        }
        res?;
        Ok(Some(b))
    }

    fn ask(&mut self, r: Range<u64>, b: Vec<u8>) {
        if self.ask.send((r.clone(), b)).is_ok() {
            self.busy = Some(r);
        }
    }
}

/// `n` байт снимка с `off` в `b` за запасом [`HAY_HEAD`].
fn hay_read(snap: &TextSnap, off: u64, n: usize, b: &mut Vec<u8>) -> io::Result<()> {
    if b.len() != HAY_HEAD + n {
        b.resize(HAY_HEAD + n, 0);
    }
    let k = snap.read_at(off, &mut b[HAY_HEAD..])?;
    b.truncate(HAY_HEAD + k);
    Ok(())
}

/// Окно поиска S7-09 по снимку: байты кусков собираются в буфер через `read_at`
/// источников (не через кэш страниц — видимые страницы не вытесняются). Проход вперёд
/// упирается в чтение (поиск по памяти на порядок быстрее): со второго окна следующее
/// читает свой поток, перекрытие окон не копируется заново.
#[derive(Debug)]
pub struct TextHay<'a> {
    snap: &'a TextSnap,
    buf: Vec<u8>,
    /// Начало окна в `buf` и его место в документе.
    head: usize,
    at: Range<u64>,
    reader: Option<HayReader>,
}

impl<'a> TextHay<'a> {
    pub fn new(snap: &'a TextSnap) -> TextHay<'a> {
        TextHay {
            snap,
            buf: Vec::new(),
            head: HAY_HEAD,
            at: 0..0,
            reader: None,
        }
    }

    /// Окно `w` — продолжение прошлого: хвост прошлого в запасе нового буфера, остальное
    /// — от потока чтения, он получает следующее окно. `false` — не продолжение.
    fn forward(&mut self, w: &Range<u64>) -> io::Result<bool> {
        let at = self.at.clone();
        if !(at.start < w.start && w.start <= at.end && at.end < w.end) {
            return Ok(false);
        }
        let keep = (at.end - w.start) as usize;
        if keep > HAY_HEAD {
            return Ok(false);
        }
        let reader = match &mut self.reader {
            Some(r) => r,
            None => self.reader.insert(HayReader::new(self.snap.clone())?),
        };
        let fresh = at.end..w.end;
        let step = fresh.end - fresh.start;
        let mut next = match reader.take(&fresh)? {
            Some(b) => b,
            None => {
                let mut b = Vec::new();
                hay_read(self.snap, fresh.start, step as usize, &mut b)?;
                b
            }
        };
        let tail = self.buf.len() - keep;
        next[HAY_HEAD - keep..HAY_HEAD].copy_from_slice(&self.buf[tail..]);
        let old = std::mem::replace(&mut self.buf, next);
        self.head = HAY_HEAD - keep;
        self.at = w.start..w.start + (self.buf.len() - self.head) as u64;
        let ahead = w.end..w.end.saturating_add(step).min(self.snap.len());
        if !ahead.is_empty() {
            reader.ask(ahead, old);
        }
        Ok(true)
    }
}

impl Hay for TextHay<'_> {
    fn len(&self) -> u64 {
        self.snap.len()
    }

    fn window(&mut self, r: Range<u64>) -> io::Result<&[u8]> {
        let end = r.end.min(self.snap.len());
        let start = r.start.min(end);
        let w = start..end;
        if !self.forward(&w)? {
            hay_read(self.snap, start, (end - start) as usize, &mut self.buf)?;
            self.head = HAY_HEAD;
            self.at = start..start + (self.buf.len() - HAY_HEAD) as u64;
        }
        Ok(&self.buf[self.head..])
    }
}
