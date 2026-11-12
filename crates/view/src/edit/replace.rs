//! Поиск и замена в документе на движке (ADR-0023 §5; S7 §5.2). Карточка S7-30.
//!
//! - Поиск — `Matcher` и проход S7-09 (`search::scan`) по [`TextHay`] снимка: литерал
//!   `memmem`, regex по байтам, HIR-перевод для однобайтовых, transcode для UTF-16 и
//!   многобайтовых, `^`/`$` по строкам с CRLF. Фоном (`spawn_cpu`), как во вьювере:
//!   совпадение → выделение и вывод курсора; затем счёт до `count_limit` (метки, «N из
//!   M»). Правка документа после старта — итог отбрасывается, поиск повторяется.
//! - «Заменить» (Enter) — обычная правка одной группой, затем поиск дальше.
//! - «Заменить все» — задача по снимку (Bulk, прогресс в атомике, отмена); документ на
//!   время задачи только для чтения (`Engine::busy`). Итог — одна группа `Op::Root`:
//!   ≤ [`PIECES_MAX`] совпадений — новые куски над источником замен (без групп — один
//!   кусок на все совпадения), больше — результат потоком во временный файл `Rewrite`.
//!   Байты замен задача кладёт в свой источник `Rewrite` (память до [`REPL_MEM`], иначе
//!   временный файл): UI-поток в буфер правок `Add` не пишет (S7-43).
//! - Группы `$1`, `${name}`, `$$` в «Заменить на» (regex): совпадение декодируется,
//!   образец (Unicode) сопоставляется с ним целиком, подстановка кодируется в кодировку
//!   документа (непредставимые — `?`).

use std::io;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use regex_automata::meta;
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input};
use urfin_core::{CancelToken, CmdError, Command, Cx, Lane, Level, Msg, Runtime};

use super::input::{Done, Sel, Selections};
use super::piece::{
    Lf, PIECE_MAX, Piece, Pieces, SpillSource, Src, TextDoc, TextHay, TextSnap, Tree, create_temp,
};
use super::undo::{Kind, Op};
use super::write::{RewriteOut, Rewritten};
use super::{EdState, EditorDoc};
use crate::enc::Enc;
use crate::index::{Sep, scan as scan_seps};
use crate::search::job::SearchEvent;
use crate::search::scan::{self, Counted, Hit, Scan, ScanError, Walk};
use crate::search::{Matcher, Pattern, SearchError, SearchReq, SearchState, compile};
use crate::source::{ByteSource, OwnedSource, assert_off_ui};
use crate::{DocId, ViewState, sys};

/// Совпадений не больше — итог «Заменить все» куском дерева; больше — `Rewrite`.
pub const PIECES_MAX: usize = 64 << 10;

/// Байт замен не больше — их источник в памяти; больше — временный файл.
pub const REPL_MEM: usize = 1 << 20;

/// «Заменить на»: байты в кодировке документа или подстановка групп.
#[derive(Debug)]
pub enum Repl {
    /// Одни и те же байты для каждого совпадения.
    Plain(Vec<u8>),
    /// Группы regex: образец (Unicode) и шаблон.
    Groups {
        re: Box<meta::Regex>,
        tmpl: String,
        enc: Enc,
    },
}

impl Repl {
    /// Шаблон `with` для образца `pat` в кодировке документа `enc`.
    pub fn new(pat: &Pattern, with: &str, enc: Enc) -> Result<Repl, SearchError> {
        let plain = || Repl::Plain(super::input::encode(&enc, with, true).unwrap_or_default());
        let Pattern::Regex { s, case } = pat else {
            return Ok(plain());
        };
        if !with.contains('$') {
            return Ok(plain());
        }
        let re = meta::Regex::builder()
            .syntax(
                syntax::Config::new()
                    .case_insensitive(!*case)
                    .multi_line(true)
                    .crlf(true),
            )
            .build(s)
            .map_err(|e| SearchError::Regex {
                pos: 0,
                msg: e.to_string(),
            })?;
        Ok(Repl::Groups {
            re: Box::new(re),
            tmpl: with.to_owned(),
            enc,
        })
    }

    /// Байты замены совпадения `matched` (байты документа) — в `out`.
    pub fn expand(&self, matched: &[u8], out: &mut Vec<u8>) {
        match self {
            Repl::Plain(b) => out.extend_from_slice(b),
            Repl::Groups { re, tmpl, enc } => {
                let text = enc.enc.decode_without_bom_handling(matched).0;
                let mut caps = re.create_captures();
                re.captures(Input::new(&*text).anchored(Anchored::Yes), &mut caps);
                let mut dst = String::new();
                if caps.is_match() {
                    caps.interpolate_string_into(&text, tmpl, &mut dst);
                } else {
                    // Совпадение зависит от контекста вокруг (`\b`, `^`): группы пусты.
                    let empty = re.create_captures();
                    empty.interpolate_string_into(&text, tmpl, &mut dst);
                }
                out.extend(super::input::encode(enc, &dst, true).unwrap_or_default());
            }
        }
    }
}

/// Итог «Заменить все».
#[derive(Debug)]
pub enum Replaced {
    /// Совпадения и замены. Байты замен — в `src`, готовом источнике `Rewrite` (`None` —
    /// замены пустые); `parts` — куски над ним (источник кусков — `Src::Rewrite(0)`, номер
    /// ставит документ): один набор на все совпадения (без групп) или по набору на
    /// совпадение.
    Small {
        hits: Vec<Range<u64>>,
        src: Option<Arc<dyn ByteSource>>,
        parts: Vec<Pieces>,
    },
    /// Результат потоком: новый источник; отметка — курсор.
    Big { rw: Rewritten, count: usize },
}

impl Replaced {
    pub fn count(&self) -> usize {
        match self {
            Replaced::Small { hits, .. } => hits.len(),
            Replaced::Big { count, .. } => *count,
        }
    }
}

/// Байты снимка `r` (вне UI-потока).
fn read(snap: &TextSnap, r: Range<u64>, buf: &mut Vec<u8>) -> io::Result<()> {
    buf.resize((r.end - r.start) as usize, 0);
    let n = snap.read_at(r.start, buf)?;
    buf.truncate(n);
    Ok(())
}

/// Байты `r` окна `bytes`, начатого на `base`.
fn part(bytes: &[u8], base: u64, r: Range<u64>) -> &[u8] {
    &bytes[(r.start - base) as usize..(r.end - base) as usize]
}

/// Ошибка прохода замены.
#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl From<ScanError> for ReplaceError {
    fn from(e: ScanError) -> Self {
        match e {
            ScanError::Cancelled => ReplaceError::Cancelled,
            ScanError::Io(e) => ReplaceError::Io(e),
        }
    }
}

/// Куски части `r` источника замен `bytes` (`Src::Rewrite(0)`), переводы строк — по `sep`.
fn part_pieces(bytes: &[u8], r: Range<usize>, sep: Sep) -> Pieces {
    let mut out = Pieces::new();
    let (mut at, end) = (r.start as u64, r.end as u64);
    while at < end {
        let to = end.min(at + PIECE_MAX);
        let mut n = 0u32;
        scan_seps(&bytes[at as usize..to as usize], at, sep, |_| n += 1);
        out.push(Piece {
            src: Src::Rewrite(0),
            start: at,
            len: to - at,
            lf: Lf::Known(n),
        });
        at = to;
    }
    out
}

/// Источник замен (`None` — замены пустые) и куски над его частями.
type ReplSrc = (Option<Arc<dyn ByteSource>>, Vec<Pieces>);

/// Байты замен → источник `Rewrite` и куски над частями `parts` (смещения в `bytes`).
/// Только вне UI-потока: разбор переводов строк и запись временного файла — здесь, UI
/// получает готовое (S7-43).
fn repl_source(
    bytes: Vec<u8>,
    parts: &[Range<usize>],
    sep: Sep,
    temp: &TempAt,
) -> io::Result<ReplSrc> {
    assert_off_ui();
    let pieces = parts
        .iter()
        .map(|p| part_pieces(&bytes, p.clone(), sep))
        .collect();
    if bytes.is_empty() {
        return Ok((None, pieces));
    }
    let src: Arc<dyn ByteSource> = if bytes.len() <= REPL_MEM {
        Arc::new(OwnedSource::new(bytes))
    } else {
        let file = create_temp(&temp.dir, &temp.name, "rw")?;
        sys::write_all_at(&file, &bytes, 0)?;
        Arc::new(SpillSource::sealed(file, bytes.len() as u64))
    };
    Ok((Some(src), pieces))
}

/// Где временный файл результата.
#[derive(Debug, Clone)]
pub struct TempAt {
    pub dir: std::path::PathBuf,
    pub name: String,
}

/// «Заменить все» по снимку (вне UI-потока): совпадения по порядку без перекрытий;
/// ≤ [`PIECES_MAX`] — [`Replaced::Small`], больше — второй проход с записью результата
/// ([`Replaced::Big`]); `caret` — смещение курсора, его новое место — в `Big`.
pub fn replace_all(
    snap: &TextSnap,
    m: &Matcher,
    repl: &Repl,
    o: &Scan<'_>,
    caret: u64,
    temp: &TempAt,
) -> Result<Replaced, ReplaceError> {
    replace_all_max(snap, m, repl, o, caret, temp, PIECES_MAX)
}

/// [`replace_all`] со своим порогом кусков (тесты потокового пути).
pub fn replace_all_max(
    snap: &TextSnap,
    m: &Matcher,
    repl: &Repl,
    o: &Scan<'_>,
    caret: u64,
    temp: &TempAt,
    max: usize,
) -> Result<Replaced, ReplaceError> {
    let _span = tracing::debug_span!("edit.replace_all", len = snap.len()).entered();
    let mut hits = Vec::new();
    let mut bytes = Vec::new();
    let mut offs = matches!(repl, Repl::Groups { .. }).then(Vec::new);
    let mut buf = Vec::new();
    let mut err: Option<io::Error> = None;
    let mut hay = TextHay::new(snap);
    let all = scan::each(&mut hay, m, o, &mut |r| {
        if hits.len() == max {
            return false;
        }
        if let Some(offs) = &mut offs {
            offs.push(bytes.len());
            if let Err(e) = read(snap, r.clone(), &mut buf) {
                err = Some(e);
                return false;
            }
            repl.expand(&buf, &mut bytes);
        }
        hits.push(r);
        true
    })?;
    if let Some(e) = err {
        return Err(e.into());
    }
    if all {
        if hits.is_empty() {
            return Ok(Replaced::Small {
                hits,
                src: None,
                parts: Vec::new(),
            });
        }
        let parts: Vec<Range<usize>> = if let Some(offs) = &mut offs {
            offs.push(bytes.len());
            offs.windows(2).map(|w| w[0]..w[1]).collect()
        } else {
            if let Repl::Plain(b) = repl {
                bytes.clone_from(b);
            }
            std::iter::once(0..bytes.len()).collect()
        };
        let (src, parts) = repl_source(bytes, &parts, snap.sep(), temp)?;
        return Ok(Replaced::Small { hits, src, parts });
    }
    // Больше порога: второй проход, результат — потоком.
    drop(hits);
    o.scanned.store(0, Ordering::Relaxed);
    let mut out = RewriteOut::new(snap.sep(), snap.len(), &temp.dir, &temp.name)?;
    let mut at = 0u64;
    let mut count = 0usize;
    let mut new_caret = None;
    let mut rbuf = Vec::new();
    // Совпадения идут десятками миллионов: байты между ними — из окна прохода, без второго
    // чтения файла; запись копится в `RewriteOut` (S7-30, #310).
    let mut hay = TextHay::new(snap);
    scan::walk(&mut hay, m, o, &mut |ev| {
        let res = match ev {
            Walk::Hit { range, bytes, base } => (|| -> io::Result<()> {
                if new_caret.is_none() && caret < range.end {
                    new_caret = Some(out.len() + caret.saturating_sub(at).min(range.start - at));
                }
                out.write(part(bytes, base, at..range.start))?;
                rbuf.clear();
                repl.expand(part(bytes, base, range.clone()), &mut rbuf);
                out.write(&rbuf)?;
                at = range.end;
                count += 1;
                Ok(())
            })(),
            Walk::Past { upto, bytes, base } if at < upto => (|| -> io::Result<()> {
                if new_caret.is_none() && caret < upto {
                    new_caret = Some(out.len() + caret.saturating_sub(at));
                }
                out.write(part(bytes, base, at..upto))?;
                at = upto;
                Ok(())
            })(),
            Walk::Past { .. } => Ok(()),
        };
        match res {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        }
    })?;
    if let Some(e) = err {
        return Err(e.into());
    }
    if new_caret.is_none() {
        new_caret = Some(out.len() + caret.saturating_sub(at));
    }
    let rw = out.finish(vec![new_caret.unwrap_or(0)])?;
    Ok(Replaced::Big { rw, count })
}

/// Новое дерево по совпадениям (UI-поток): куски снимка между совпадениями и куски
/// замен `add` — над источником `slot`; новое место курсора.
fn rebuild(
    tree: &Tree,
    hits: &[Range<u64>],
    add: &[Pieces],
    slot: Option<Src>,
    caret: u64,
) -> (Tree, u64) {
    let mut out: Vec<Piece> = Vec::with_capacity(hits.len() * 3 + 4);
    let mut pieces = tree.iter().map(|(s, p)| (s, *p)).peekable();
    // Куски снимка в [from, to).
    let mut take = |from: u64, to: u64, out: &mut Vec<Piece>| {
        while let Some(&(s, p)) = pieces.peek() {
            let e = s + p.len;
            if e <= from {
                pieces.next();
                continue;
            }
            if s >= to {
                break;
            }
            let a = from.max(s);
            let b = to.min(e);
            out.push(Piece {
                src: p.src,
                start: p.start + (a - s),
                len: b - a,
                lf: if a == s && b == e { p.lf } else { Lf::Unknown },
            });
            if e <= to {
                pieces.next();
            } else {
                break;
            }
        }
    };
    // `at` — конец предыдущего совпадения во входе, `len` — длина выхода до него.
    let (mut at, mut len) = (0u64, 0u64);
    let mut new_caret = None;
    for (i, r) in hits.iter().enumerate() {
        if new_caret.is_none() && caret < r.end {
            // Курсор внутри совпадения — в начало замены.
            new_caret = Some(len + caret.clamp(at, r.start) - at);
        }
        take(at, r.start, &mut out);
        let rp = &add[i.min(add.len().saturating_sub(1))];
        out.extend(rp.iter().map(|p| Piece {
            src: slot.unwrap_or(p.src),
            ..*p
        }));
        len += (r.start - at) + super::piece::bytes_of(rp);
        at = r.end;
    }
    take(at, tree.len(), &mut out);
    let caret = new_caret.unwrap_or(len + caret.max(at) - at);
    (Tree::from_pieces(out), caret)
}

/// Итог «Заменить все» → новое дерево документа и место курсора `caret` (UI-поток, без
/// I/O: и замены, и поток — готовые источники `Rewrite`; в буфер правок `Add` ничего не
/// пишется, S7-43). Дерево не ставится.
pub fn apply(text: &mut TextDoc, res: Replaced, caret: u64) -> (Tree, u64) {
    match res {
        Replaced::Small { hits, src, parts } => {
            let slot = src.map(|s| text.push_rewrite(s));
            rebuild(text.tree(), &hits, &parts, slot, caret)
        }
        Replaced::Big { rw, .. } => {
            let src = text.push_rewrite(rw.src);
            let t = Tree::from_pieces(rw.pieces.into_iter().map(|p| Piece { src, ..p }));
            (t, rw.marks.first().copied().unwrap_or(0))
        }
    }
}

/// Задача «Заменить все» документа.
#[derive(Debug)]
pub struct ReplaceJob {
    pub scanned: Arc<AtomicU64>,
    pub total: u64,
    cancel: CancelToken,
    pub started: Instant,
}

impl ReplaceJob {
    /// Доля, 0…1 (статус «Замена 41 %», не чаще 30 раз в секунду — перерисовка UI).
    #[allow(clippy::cast_precision_loss, reason = "доля для статуса")]
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        (self.scanned.load(Ordering::Relaxed) as f64 / self.total as f64).min(1.0) as f32
    }
}

/// Поиск документа на движке.
#[derive(Debug)]
pub struct EdSearch {
    pub st: SearchState,
    /// Правок документа при старте поиска и счёта.
    find_edits: u64,
    count_edits: u64,
}

impl EditorDoc {
    /// `editor.search text=…`: новый поиск от выделения (вперёд — от конца, назад — от
    /// начала).
    pub(crate) fn find(&mut self, req: SearchReq, rt: &Runtime) {
        let Some(e) = self.engine.as_deref() else {
            return;
        };
        let enc = e.enc;
        if let Some(s) = &mut self.search {
            s.st.cancel();
        }
        self.search_gen += 1;
        let mut st = SearchState::new(req, self.search_gen);
        match compile(&st.req.pat, enc) {
            Ok(m) => st.matcher = Some(Arc::new(m)),
            Err(e) => st.error = Some(e),
        }
        self.search = Some(EdSearch {
            st,
            find_edits: 0,
            count_edits: u64::MAX,
        });
        self.run_find(rt);
    }

    /// Shift+F7 / Alt+F7: тот же образец. `false` — поиска не было.
    pub(crate) fn find_again(&mut self, back: bool, rt: &Runtime) -> bool {
        let Some(s) = &mut self.search else {
            return false;
        };
        s.st.req.back = back;
        self.run_find(rt);
        true
    }

    /// Esc: поиск, счёт и «Заменить все» останавливаются (метки и итог остаются).
    pub(crate) fn find_cancel(&mut self) {
        if let Some(s) = &mut self.search {
            s.st.cancel.cancel();
            s.st.running = false;
        }
        if let Some(j) = self.replace.take() {
            j.cancel.cancel();
            if let Some(e) = self.engine.as_deref_mut() {
                e.busy = false;
            }
            if self.state == EdState::Replacing {
                self.state = EdState::Ready;
            }
        }
        self.bump();
    }

    fn run_find(&mut self, rt: &Runtime) {
        let (Some(eng), Some(srch)) = (self.engine.as_deref(), self.search.as_mut()) else {
            return;
        };
        let Some(mat) = srch.st.matcher.clone() else {
            self.rev += 1;
            return;
        };
        srch.st.cancel.cancel();
        srch.st.cancel = self.cancel.child();
        srch.st.seq += 1;
        let from = match (eng.sel.stream(), srch.st.req.back) {
            (Some(r), false) => r.end,
            (Some(r), true) => r.start,
            (None, _) => eng.sel.caret,
        };
        srch.st.from = from;
        srch.st.scanned.store(0, Ordering::Relaxed);
        srch.st.total = eng.len();
        srch.st.running = true;
        srch.st.not_found = false;
        srch.st.wrapped = false;
        srch.st.failed = None;
        srch.find_edits = eng.text.edits();
        let (doc, seq, snap) = (self.id, srch.st.seq, eng.text.snap());
        let (back, wrap, scanned) = (srch.st.req.back, srch.st.req.wrap, srch.st.scanned.clone());
        rt.spawn_cpu("edit.search", srch.st.cancel.clone(), move |cx| {
            let _span = tracing::debug_span!("edit.search").entered();
            let mut hay = TextHay::new(&snap);
            let o = Scan {
                chunk: scan::CHUNK,
                cancel: &cx.cancel,
                scanned: &scanned,
            };
            let ev = match scan::find_next(&mut hay, &mat, from, back, wrap, &o) {
                Ok(h) => SearchEvent::Found(h),
                Err(ScanError::Cancelled) => return,
                Err(ScanError::Io(e)) => SearchEvent::Failed(e.to_string()),
            };
            let _ = cx.sink.send(Box::new(FindMsg { doc, seq, ev }), &cx.cancel);
        });
        self.rev += 1;
    }

    fn start_count(&mut self, rt: &Runtime, limit: usize) {
        let (Some(eng), Some(srch)) = (self.engine.as_deref(), self.search.as_mut()) else {
            return;
        };
        let Some(mat) = srch.st.matcher.clone() else {
            return;
        };
        srch.st.count_cancel.cancel();
        srch.st.count_cancel = self.cancel.child();
        srch.st.counting = true;
        srch.count_edits = eng.text.edits();
        let (doc, pgen, snap) = (self.id, srch.st.pgen, eng.text.snap());
        rt.spawn_cpu(
            "edit.search.count",
            srch.st.count_cancel.clone(),
            move |cx| {
                let _span = tracing::debug_span!("edit.search.count").entered();
                let mut hay = TextHay::new(&snap);
                let scanned = AtomicU64::new(0);
                let o = Scan {
                    chunk: scan::CHUNK,
                    cancel: &cx.cancel,
                    scanned: &scanned,
                };
                let ev = match scan::count(&mut hay, &mat, limit, &o) {
                    Ok(c) => SearchEvent::Counted(c),
                    Err(ScanError::Cancelled) => return,
                    Err(ScanError::Io(e)) => {
                        tracing::warn!(error = %e, "editor search count stopped");
                        return;
                    }
                };
                let _ = cx
                    .sink
                    .send(Box::new(FindMsg { doc, seq: pgen, ev }), &cx.cancel);
            },
        );
    }

    /// Ответ задачи поиска или счёта.
    fn on_find(&mut self, seq: u64, ev: SearchEvent, rt: &Runtime, limit: usize) {
        let edits = self.engine.as_deref().map_or(0, |e| e.text.edits());
        let Some(s) = self.search.as_mut() else {
            return;
        };
        match ev {
            SearchEvent::Counted(Counted { starts, more }) => {
                if seq != s.st.pgen {
                    return;
                }
                s.st.counting = false;
                if s.count_edits != edits {
                    // Правили, пока считали: метки — заново.
                    self.start_count(rt, limit);
                    return;
                }
                s.st.count.n = starts.len();
                s.st.count.more = more;
                s.st.count.done = true;
                s.st.marks = Arc::from(starts);
            }
            SearchEvent::Found(h) => {
                if seq != s.st.seq {
                    return;
                }
                if s.find_edits != edits {
                    // Правили, пока искали: совпадение могло сдвинуться — ещё раз.
                    self.run_find(rt);
                    return;
                }
                s.st.running = false;
                match h {
                    None => s.st.not_found = true,
                    Some(Hit { range, wrapped }) => {
                        s.st.hit = Some(range.clone());
                        s.st.wrapped = wrapped;
                        let recount =
                            !s.st.counting && (!s.st.count.done || s.count_edits != edits);
                        if let Some(e) = self.engine.as_deref_mut() {
                            e.undo.break_group();
                            e.sel = Selections {
                                caret: range.end,
                                sel: Sel::Stream {
                                    anchor: range.start,
                                },
                            };
                            if let Err(m) = e.reveal() {
                                e.need_reveal = true;
                                e.need = Some(super::input::missing_range(&m));
                            }
                            self.cursor = e.cursor();
                        }
                        if recount {
                            self.start_count(rt, limit);
                        }
                        self.settle(rt);
                    }
                }
            }
            SearchEvent::Failed(err) => {
                if seq != s.st.seq {
                    return;
                }
                s.st.running = false;
                s.st.failed = Some(err);
            }
        }
        self.bump();
    }

    /// Правка документа: метки поиска устарели (счёт — заново при следующем поиске).
    pub(crate) fn search_edited(&mut self) {
        if let Some(s) = &mut self.search
            && s.st.count.done
        {
            s.st.count = crate::search::CountState::default();
            s.st.marks = Arc::from(Vec::new());
        }
    }

    /// `editor.replace text=… with=…` (Enter): выделение — совпадение образца → заменяется
    /// одной группой; затем поиск следующего. `false` — нечего делать.
    pub(crate) fn replace_one(&mut self, req: SearchReq, with: &str, rt: &Runtime) -> bool {
        let env = self.env();
        let same = self
            .search
            .as_ref()
            .is_some_and(|s| s.st.req.pat == req.pat);
        let edits = self.engine.as_deref().map_or(0, |e| e.text.edits());
        let hit = self
            .search
            .as_ref()
            .filter(|s| same && s.find_edits == edits)
            .and_then(|s| s.st.hit.clone());
        let Some(e) = self.engine.as_deref_mut() else {
            return false;
        };
        let sel = e.sel.stream();
        if hit.is_some() && hit == sel {
            let r = hit.unwrap_or_default();
            let repl = match Repl::new(&req.pat, with, e.enc) {
                Ok(r) => r,
                Err(err) => {
                    if let Some(s) = &mut self.search {
                        s.st.error = Some(err);
                    }
                    self.bump();
                    return true;
                }
            };
            let Some(matched) = e.bytes(r.clone()) else {
                // Страниц совпадения нет в кэше: сначала поиск ещё раз (выводит его).
                self.find(req, rt);
                return true;
            };
            let mut bytes = Vec::new();
            repl.expand(&matched, &mut bytes);
            if e.replace_range(r, &bytes, &env) == Done::Same {
                return false;
            }
            self.after(&Done::Edited);
            if let Some(s) = &mut self.search {
                s.st.hit = None;
            }
            self.search_edited();
        }
        if same {
            if let Some(s) = &mut self.search {
                s.st.req.back = req.back;
            }
            self.run_find(rt);
        } else {
            self.find(req, rt);
        }
        true
    }

    /// `editor.replace … all=true` (Ctrl+Alt+Enter): задача по снимку; документ — только
    /// для чтения до итога.
    pub(crate) fn start_replace_all(&mut self, req: &SearchReq, with: &str, rt: &Runtime) {
        if !self.is_ready() || self.readonly || self.ask.is_some() {
            return;
        }
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        if e.busy {
            return;
        }
        let m = match compile(&req.pat, e.enc) {
            Ok(m) => m,
            Err(err) => {
                self.search_error(req, err);
                return;
            }
        };
        let repl = match Repl::new(&req.pat, with, e.enc) {
            Ok(r) => r,
            Err(err) => {
                self.search_error(req, err);
                return;
            }
        };
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        e.busy = true;
        let snap = e.text.snap();
        let caret = e.sel.caret;
        let edits = e.text.edits();
        let job = ReplaceJob {
            scanned: Arc::new(AtomicU64::new(0)),
            total: snap.len(),
            cancel: self.cancel.child(),
            started: Instant::now(),
        };
        let (doc, scanned, cancel) = (self.id, job.scanned.clone(), job.cancel.clone());
        self.replace = Some(job);
        self.state = EdState::Replacing;
        let temp = TempAt {
            dir: std::env::temp_dir().join("Urfin"),
            name: format!("edit-{}-{}", std::process::id(), doc.0),
        };
        rt.spawn(Lane::Bulk, "edit.replace_all", cancel, move |cx| {
            let o = Scan {
                chunk: scan::CHUNK,
                cancel: &cx.cancel,
                scanned: &scanned,
            };
            let res = replace_all(&snap, &m, &repl, &o, caret, &temp);
            let res = match res {
                Err(ReplaceError::Cancelled) => return,
                r => r.map_err(|e| e.to_string()),
            };
            let msg = ReplacedMsg { doc, edits, res };
            let _ = cx.sink.send(Box::new(msg), &cx.cancel);
        });
        self.bump();
    }

    fn search_error(&mut self, req: &SearchReq, err: SearchError) {
        self.search_gen += 1;
        let mut st = SearchState::new(req.clone(), self.search_gen);
        st.error = Some(err);
        if let Some(s) = &mut self.search {
            s.st.cancel();
        }
        self.search = Some(EdSearch {
            st,
            find_edits: 0,
            count_edits: u64::MAX,
        });
        self.bump();
    }

    /// Итог «Заменить все»: новое дерево одной группой `Op::Root`; число замен.
    fn on_replaced(
        &mut self,
        edits: u64,
        res: Result<Replaced, String>,
    ) -> Result<Option<usize>, String> {
        if self.replace.take().is_none() {
            // Отменено (Esc) — ответ опоздал.
            return Ok(None);
        }
        if self.state == EdState::Replacing {
            self.state = EdState::Ready;
        }
        let Some(e) = self.engine.as_deref_mut() else {
            return Ok(None);
        };
        e.busy = false;
        let res = res?;
        if e.text.edits() != edits {
            return Err("document changed during replace".into());
        }
        let n = res.count();
        if n == 0 {
            self.bump();
            return Ok(Some(0));
        }
        let before = e.text.tree().clone();
        let sel_before = e.sel;
        let (after, new_caret) = apply(&mut e.text, res, e.sel.caret);
        e.text.set_tree(after.clone());
        e.sel = Selections::at(new_caret.min(after.len()));
        e.undo.begin_group(Kind::Command);
        e.undo.record(
            Op::Root {
                before,
                after,
                enc: None,
                eol: None,
            },
            Kind::Command,
            &sel_before,
            &e.sel,
            Instant::now(),
        );
        if let Err(m) = e.reveal() {
            e.need_reveal = true;
            e.need = Some(super::input::missing_range(&m));
        }
        self.after(&Done::Edited);
        self.search_edited();
        if let Some(s) = &mut self.search {
            s.st.hit = None;
        }
        Ok(Some(n))
    }
}

/// Ответ поиска или счёта документа на движке.
#[derive(Debug)]
struct FindMsg {
    doc: DocId,
    seq: u64,
    ev: SearchEvent,
}

impl Msg for FindMsg {
    fn name(&self) -> &'static str {
        "edit.search"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let limit = view.viewer.count_limit as usize;
        let wrapped = matches!(&self.ev, SearchEvent::Found(Some(h)) if h.wrapped);
        let Some(d) = view.editor_mut(self.doc) else {
            cx.keep_rev();
            return;
        };
        let current = d.search.as_ref().is_some_and(|s| s.st.seq == self.seq);
        d.on_find(self.seq, self.ev, rt, limit);
        if wrapped && current {
            cx.toast(Level::Info, urfin_i18n::current().get("ui-find-wrapped"));
        }
    }
}

/// Итог «Заменить все».
#[derive(Debug)]
struct ReplacedMsg {
    doc: DocId,
    edits: u64,
    res: Result<Replaced, String>,
}

impl Msg for ReplacedMsg {
    fn name(&self) -> &'static str {
        "edit.replace_all"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let ReplacedMsg { doc, edits, res } = *self;
        let rt = cx.rt;
        let res = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(doc))
            .map(|d| {
                let res = d.on_replaced(edits, res);
                // Новому дереву нужны страницы экрана и счётчик изменений: без этого
                // документ до следующего события стоит на скелетоне (S7-36, #310).
                d.settle(rt);
                res
            });
        match res {
            Some(Ok(Some(n))) if n > 0 => {
                let mut a = urfin_i18n::FluentArgs::new();
                a.set("n", n as u64);
                cx.toast(
                    Level::Info,
                    urfin_i18n::current().fmt("ui-edit-replaced", &a),
                );
            }
            Some(Ok(Some(_))) => cx.toast(Level::Info, urfin_i18n::current().get("ui-find-none")),
            Some(Err(e)) => super::engine::report_failed(cx, "editor.replace", e),
            Some(Ok(None)) | None => cx.keep_rev(),
        }
    }
}

/// `editor.search` / `editor.replace` с `text`, `editor.search_next` / `_prev` после
/// поиска на движке, `editor.search_cancel` — у документа на движке. `false` — команда
/// для строки поиска UI (прежний путь или без образца).
pub(crate) fn command(cmd: &Command, cx: &mut Cx<'_>) -> Result<bool, CmdError> {
    let rt = cx.rt;
    let id = cmd.id.as_str();
    let a = &cmd.args;
    let view = crate::handler::view_mut(cx)?;
    let wrap = view.viewer.search_wrap;
    let Some(doc) = view.active_editor() else {
        return Ok(false);
    };
    let Some(d) = view.editor_mut(doc) else {
        return Ok(false);
    };
    if d.engine.is_none() {
        return Ok(false);
    }
    match id {
        "editor.search" | "editor.replace" => {
            let Some(text) = a.str("text") else {
                return Ok(false);
            };
            let back = a.bool("back").unwrap_or(false);
            let pat = match crate::handler::search_pattern(a, text)? {
                Ok(p) => p,
                Err(e) => {
                    let req = SearchReq {
                        pat: Pattern::Hex(Vec::new()),
                        back,
                        wrap,
                    };
                    d.search_error(&req, e);
                    return Ok(true);
                }
            };
            let req = SearchReq { pat, back, wrap };
            if id == "editor.search" {
                d.find(req, rt);
            } else {
                let with = a.str("with").unwrap_or_default().to_owned();
                if a.bool("all").unwrap_or(false) {
                    d.start_replace_all(&req, &with, rt);
                } else {
                    d.replace_one(req, &with, rt);
                }
            }
            Ok(true)
        }
        "editor.search_next" | "editor.search_prev" => {
            Ok(d.find_again(id == "editor.search_prev", rt))
        }
        _ => {
            // Esc: поиск и «Заменить все» останавливаются; строку закрывает UI.
            d.find_cancel();
            Ok(false)
        }
    }
}

#[cfg(test)]
#[path = "replace_tests.rs"]
mod tests;
