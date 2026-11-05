//! Подсветка окном в редакторе (ADR-0024): встроенные языки любого размера. Точки
//! (`syntax::marks`) — по снимку в фоне; окно — видимое ± запас (`win::margins`). На UI-потоке —
//! только сдвиг и обрезка готовых отрезков по правкам (ADR-0024 §4):
//!
//! - правка внутри инертной зоны (строка, комментарий) байтами без запрещённых —
//!   отрезки, места свёртки и зоны сдвигаются: состояние лексера за зоной то же;
//! - иначе отрезки остаются только до «якоря» перед правкой (начало строки у лексеров,
//!   начало отрезка разбора у YAML и Markdown), дальше — без цвета до нового окна.
//!
//! Задача — одна за раз (`Lane::Interactive`): правки с точек → пересчёт точек до
//! совпадения состояния или до окна → окно. Не сошлось — пересчёт до конца в
//! выделенном потоке, его точки принимаются, если за это время новых не было.

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use urfin_core::{CancelToken, Cx, Lane, Msg, Runtime};

use super::piece::{TextEdit, TextSnap};
use crate::cache::ByteView;
use crate::enc::Enc;
use crate::syntax::Span;
use crate::syntax::edit::FoldAt;
use crate::syntax::lex::{Feed, Inert, Lang, Src, Text};
use crate::syntax::marks::{Marks, advance};
use crate::syntax::win::{Window, guard_of, margins, window};
use crate::{DocId, ViewState};

/// Видимое длиннее — окно по началу (строки без переноса).
const VIS_MAX: u64 = 4 << 20;
/// Вставка длиннее — не проверяется на инертность (окно заново).
const INS_MAX: u64 = 256;

impl Text for TextSnap {
    fn len(&self) -> u64 {
        TextSnap::len(self)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        TextSnap::read_at(self, off, buf)
    }
}

/// Подсветка окном документа редактора.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "флаги задач и экрана — независимы"
)]
pub struct EdWin {
    pub lang: Lang,
    pub feed: Feed,
    pub enc: Enc,
    pass: u64,
    cancel: CancelToken,
    /// Идёт задача окна.
    busy: bool,
    /// Идёт пересчёт точек до конца (выделенный поток).
    tail: bool,
    /// Точки и счётчик правок, на котором они верны.
    marks: Option<(Arc<Marks>, u64)>,
    /// Правки после точек (`None` — счёт потерян: точки заново).
    log: Option<Vec<TextEdit>>,
    /// Отрезки ролей окна (по возрастанию, без пересечений) — верны в `covered`.
    pub spans: Arc<[Span]>,
    pub folds: Arc<[FoldAt]>,
    inert: Vec<Inert>,
    anchors: Vec<u64>,
    by_tree: bool,
    covered: Range<u64>,
    /// Окно — по тексту после последней правки и видимому.
    fresh: bool,
    /// Свёртка неполна: окно заново, когда точки дойдут.
    partial: bool,
    /// Окно посчитано только для кусков экрана ([`keep_only`]): его верх и сдвиг вправо.
    sparse: Option<(u64, u32, u32)>,
    /// Правка, ждущая новых цветов, и сколько ждала прошлая (мс).
    pending_since: Option<Instant>,
    pub last_ms: Option<f32>,
    pub fresh_n: u64,
}

impl EdWin {
    pub fn new(lang: Lang, feed: Feed, enc: Enc, cancel: CancelToken) -> EdWin {
        EdWin {
            lang,
            feed,
            enc,
            pass: 0,
            cancel,
            busy: false,
            tail: false,
            marks: None,
            log: Some(Vec::new()),
            spans: Arc::from(Vec::new()),
            folds: Arc::from(Vec::new()),
            inert: Vec::new(),
            anchors: Vec::new(),
            by_tree: lang.by_tree(),
            covered: 0..0,
            fresh: false,
            partial: false,
            sparse: None,
            pending_since: None,
            last_ms: None,
            fresh_n: 0,
        }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub(crate) fn cancel_job(&self) {
        self.cancel.cancel();
    }

    /// Счёт правок потерян (новое дерево кусков): точки и цвета — заново.
    fn lost(&mut self) {
        self.log = None;
        self.marks = None;
        self.drop_all();
    }

    fn drop_all(&mut self) {
        self.spans = Arc::from(Vec::new());
        self.folds = Arc::from(Vec::new());
        self.inert.clear();
        self.anchors.clear();
        self.covered = 0..0;
    }

    /// Правки на экране: инертная — сдвиг, иначе — цвета до якоря (ADR-0024 §4).
    /// `ins` — вставленные байты правки, если известны (одна правка).
    fn on_edit(&mut self, e: &TextEdit, ins: Option<&[u8]>) {
        let inert = ins.is_some_and(|b| self.inert_ok(e, b));
        if inert {
            self.shift(e);
        } else {
            self.cut(e);
        }
    }

    /// Правка внутри одной зоны, вставлено без запрещённых байт, удалять можно.
    fn inert_ok(&self, e: &TextEdit, ins: &[u8]) -> bool {
        self.inert.iter().any(|z| {
            z.start <= e.at
                && e.at + e.old <= z.end
                && (e.old == 0 || z.del)
                && !ins.iter().any(|b| z.deny.contains(b))
        })
    }

    fn shift(&mut self, e: &TextEdit) {
        self.spans = shift_spans(&self.spans, e).into();
        self.folds = self
            .folds
            .iter()
            .filter_map(|f| shift_fold(*f, e))
            .collect::<Vec<_>>()
            .into();
        for z in &mut self.inert {
            if e.at + e.old <= z.start && !(e.old == 0 && e.at == z.start) {
                z.start = z.start - e.old + e.new;
                z.end = z.end - e.old + e.new;
            } else if e.at <= z.end && z.start <= e.at {
                z.end = z.end - e.old + e.new;
            }
        }
        for a in &mut self.anchors {
            *a = shift_off(*a, e);
        }
        self.covered = shift_off(self.covered.start, e)..shift_off(self.covered.end, e);
    }

    /// Цвета — только до якоря не дальше правки.
    fn cut(&mut self, e: &TextEdit) {
        let q = guard_of(&self.anchors, self.by_tree, e.at)
            .unwrap_or(self.covered.start)
            .max(self.covered.start)
            .min(self.covered.end);
        let mut sp: Vec<Span> = Vec::with_capacity(self.spans.len());
        for s in self.spans.iter() {
            if s.end <= q {
                sp.push(*s);
            } else if s.start < q {
                sp.push(Span {
                    start: s.start,
                    end: q,
                    tag: s.tag,
                });
            }
        }
        self.spans = sp.into();
        self.folds = self
            .folds
            .iter()
            .filter(|f| f.end <= q)
            .copied()
            .collect::<Vec<_>>()
            .into();
        self.inert.retain(|z| z.end < q);
        self.anchors.retain(|&a| a <= q);
        self.covered = self.covered.start..q.max(self.covered.start);
    }

    /// Итог окна: на экран; правки после его снимка — по тем же правилам.
    fn show(&mut self, w: Window, later: &[TextEdit], ins: Option<&[u8]>) {
        self.spans = w.spans.into();
        self.folds = w.folds.into();
        self.inert = w.inert;
        self.anchors = w.anchors;
        self.covered = w.range;
        self.partial = w.partial;
        let single = later.len() == 1;
        for e in later {
            self.on_edit(e, if single { ins } else { None });
        }
    }
}

/// Сдвиг позиции правкой: внутри убранного — к её началу.
fn shift_off(x: u64, e: &TextEdit) -> u64 {
    if x <= e.at {
        x
    } else if x >= e.at + e.old {
        x - e.old + e.new
    } else {
        e.at
    }
}

/// Отрезки после инертной правки: набранное внутри отрезка — в нём, после — сдвиг.
fn shift_spans(list: &[Span], e: &TextEdit) -> Vec<Span> {
    let end = e.at + e.old;
    let mut out = Vec::with_capacity(list.len());
    for s in list {
        if s.end <= e.at {
            out.push(*s);
        } else if s.start >= end && !(s.start == e.at && e.old > 0) {
            out.push(Span {
                start: s.start - e.old + e.new,
                end: s.end - e.old + e.new,
                tag: s.tag,
            });
        } else {
            // Правка внутри отрезка.
            out.push(Span {
                start: s.start.min(e.at),
                end: (s.end.max(end) - e.old + e.new).max(s.start.min(e.at)),
                tag: s.tag,
            });
        }
    }
    out.retain(|s| s.start < s.end);
    out
}

/// Место свёртки после инертной правки: внутри — растёт или сжимается.
fn shift_fold(mut f: FoldAt, e: &TextEdit) -> Option<FoldAt> {
    let end = e.at + e.old;
    if end <= f.head && !(e.old == 0 && e.at == f.head) {
        f.head = f.head - e.old + e.new;
        f.start = f.start - e.old + e.new;
        f.end = f.end - e.old + e.new;
        Some(f)
    } else if e.at >= f.end {
        Some(f)
    } else if e.at >= f.start {
        f.end = f.end - e.old + e.new;
        Some(f)
    } else {
        // Правка на строке-заголовке: место — новым окном.
        None
    }
}

/// Что нужно задаче окна.
struct Job {
    doc: DocId,
    pass: u64,
    /// Правок текста у снимка; правок журнала, применённых к точкам.
    edits: u64,
    log_len: usize,
    snap: TextSnap,
    marks: Option<Arc<Marks>>,
    log: Vec<TextEdit>,
    range: Range<u64>,
    /// Цвета только этих кусков (см. [`SPARSE_MIN`]).
    keep: Option<Vec<Range<u64>>>,
    lang: Lang,
    feed: Feed,
    enc: Enc,
}

/// Видимое длиннее этого — ряды одной длинной строки (на экране — их начала):
/// окно считается целиком, но на экран идут только цвета видимых кусков. Иначе правка
/// сдвигает сотни тысяч отрезков на UI-потоке (3,7 мс на нажатие на JSON в одну строку).
const SPARSE_MIN: u64 = 256 << 10;

/// Оставляет отрезки и инертные зоны, задевающие куски `keep` (по возрастанию).
fn keep_only(w: &mut Window, keep: &[Range<u64>]) {
    let hit = |a: u64, b: u64| {
        let i = keep.partition_point(|k| k.end <= a);
        keep.get(i).is_some_and(|k| k.start < b.max(a + 1))
    };
    w.spans.retain(|s| hit(s.start, s.end));
    w.inert.retain(|z| hit(z.start, z.end));
}

fn work(job: &Job, stop: &dyn Fn() -> bool) -> Option<(Marks, Option<Window>)> {
    let src = Src {
        text: &job.snap,
        feed: job.feed,
        start: 0,
    };
    let len = job.snap.len();
    let mut m = match &job.marks {
        Some(m) => {
            let mut m = (**m).clone();
            for e in &job.log {
                m.edit(e);
            }
            m
        }
        None => Marks::new(0, len),
    };
    // Точки — до окна (или до сходимости и конца).
    while !(m.complete() || m.frontier() >= job.range.start) {
        advance(&mut m, &src, job.lang, job.range.start, stop).ok()?;
    }
    let mut w = window(
        &src,
        job.enc,
        job.lang,
        &m,
        job.range.start,
        job.range.end,
        stop,
    )
    .ok()?;
    if let (Some(k), Some(w)) = (&job.keep, &mut w) {
        keep_only(w, k);
    }
    Some((m, w))
}

fn spawn(rt: &Runtime, job: Job, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "edit.syntax.window", cancel, move |cx| {
        let _span = tracing::debug_span!("edit.syntax.window", lang = job.lang.name()).entered();
        let stop = || cx.cancel.is_cancelled();
        let Some((marks, win)) = work(&job, &stop) else {
            return;
        };
        let msg = WinMsg {
            doc: job.doc,
            pass: job.pass,
            edits: job.edits,
            log_len: job.log_len,
            marks: Arc::new(marks),
            win,
        };
        let _ = cx.sink.send(Box::new(msg), &cx.cancel);
    });
}

/// Пересчёт точек до конца в выделенном потоке.
#[allow(
    clippy::too_many_arguments,
    reason = "снимок, точки, язык и кодировка задачи"
)]
fn spawn_tail(
    rt: &Runtime,
    doc: DocId,
    pass: u64,
    edits: u64,
    snap: TextSnap,
    marks: Arc<Marks>,
    lang: Lang,
    feed: Feed,
    cancel: CancelToken,
) {
    rt.spawn_dedicated(format!("edit.syntax.tail.{}", doc.0), cancel, move |cx| {
        let _span = tracing::debug_span!("edit.syntax.tail", lang = lang.name()).entered();
        let src = Src {
            text: &snap,
            feed,
            start: 0,
        };
        let mut m = (*marks).clone();
        let stop = || cx.cancel.is_cancelled();
        if advance(&mut m, &src, lang, u64::MAX, &stop).is_err() {
            return;
        }
        let msg = TailMsg {
            doc,
            pass,
            edits,
            marks: Arc::new(m),
        };
        let _ = cx.sink.send(Box::new(msg), &cx.cancel);
    });
}

#[derive(Debug)]
struct WinMsg {
    doc: DocId,
    pass: u64,
    edits: u64,
    log_len: usize,
    marks: Arc<Marks>,
    win: Option<Window>,
}

impl Msg for WinMsg {
    fn name(&self) -> &'static str {
        "edit.syntax.window"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(d) = view.editor_mut(self.doc) {
            d.win_apply(*self, rt);
        }
    }
}

#[derive(Debug)]
struct TailMsg {
    doc: DocId,
    pass: u64,
    edits: u64,
    marks: Arc<Marks>,
}

impl Msg for TailMsg {
    fn name(&self) -> &'static str {
        "edit.syntax.tail"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(d) = view.editor_mut(self.doc) {
            d.win_tail(*self, rt);
        }
    }
}

/// Байты `[at, at + n)` текста из страниц в памяти (`None` — нет или длинно).
fn bytes_at(v: &impl ByteView, at: u64, n: u64) -> Option<Vec<u8>> {
    if n > INS_MAX {
        return None;
    }
    let mut out = Vec::with_capacity(usize::try_from(n).ok()?);
    let mut p = at;
    while p < at + n {
        let c = v.chunk(p).ok()?;
        if c.is_empty() {
            return None;
        }
        let k = usize::try_from((at + n - p).min(c.len() as u64)).ok()?;
        out.extend_from_slice(&c[..k]);
        p += k as u64;
    }
    Some(out)
}

impl super::EditorDoc {
    /// Окно для этого документа: встроенный язык и кодировка без состояния.
    pub(crate) fn win_lang(&self) -> Option<(Lang, Feed)> {
        let lang = Lang::of(self.lang?)?;
        let enc = self.engine.as_deref().map_or(self.enc, |e| e.enc);
        Some((lang, Feed::of(enc)?))
    }

    /// Правки → экран, видимое или текст ушли от посчитанного — задача (одна за раз).
    pub(crate) fn win_sync(&mut self, lang: Lang, feed: Feed, rt: &Runtime) {
        let id = self.id;
        let doc_cancel = self.cancel.clone();
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        let enc = e.enc;
        if self
            .win
            .as_ref()
            .is_none_or(|w| w.lang != lang || w.feed != feed || w.enc != enc)
        {
            if let Some(w) = self.win.take() {
                w.cancel_job();
            }
            self.win = Some(EdWin::new(lang, feed, enc, doc_cancel.child()));
        }
        let w = self.win.as_mut().expect("win");
        let log = e.text.take_log();
        match &log {
            None => {
                w.lost();
                w.fresh = false;
                w.pending_since.get_or_insert_with(Instant::now);
            }
            Some(l) if !l.is_empty() => {
                let ins = if l.len() == 1 {
                    bytes_at(&e.text.view(), l[0].at, l[0].new)
                } else {
                    None
                };
                let single = l.len() == 1;
                for ed in l {
                    w.on_edit(ed, if single { ins.as_deref() } else { None });
                }
                if let Some(lg) = &mut w.log {
                    lg.extend_from_slice(l);
                }
                w.fresh = false;
                w.pending_since.get_or_insert_with(Instant::now);
            }
            Some(_) => {}
        }
        if w.busy {
            return;
        }
        let vis = e.visible_range();
        let vis = vis.start..vis.end.min(vis.start.saturating_add(VIS_MAX));
        let len = e.len();
        let (margin, edge) = margins(lang);
        let need = vis.start.saturating_sub(edge)..vis.end.saturating_add(edge).min(len);
        let shown = (e.top, e.top_sub, e.hscroll);
        if w.fresh
            && w.covered.start <= need.start
            && need.end <= w.covered.end
            && w.sparse.is_none_or(|k| k == shown)
        {
            return;
        }
        let keep = (vis.end - vis.start >= SPARSE_MIN)
            .then(|| e.shown_slices())
            .flatten();
        w.sparse = keep.is_some().then_some(shown);
        let range = vis.start.saturating_sub(margin)..vis.end.saturating_add(margin).min(len);
        let edits = e.text.edits();
        let (marks, log) = match (&w.marks, &w.log) {
            (Some((m, _)), Some(l)) => (Some(m.clone()), l.clone()),
            _ => (None, Vec::new()),
        };
        let log_len = w.log.as_ref().map_or(0, Vec::len);
        w.pass += 1;
        w.busy = true;
        let job = Job {
            doc: id,
            pass: w.pass,
            edits,
            log_len,
            snap: e.text.snap(),
            marks,
            log,
            range,
            keep,
            lang,
            feed,
            enc,
        };
        spawn(rt, job, w.cancel.clone());
    }

    /// Итог окна: точки — новые, окно — на экран (правки после снимка — сверху).
    fn win_apply(&mut self, m: WinMsg, rt: &Runtime) {
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        let Some(w) = self.win.as_mut() else {
            return;
        };
        if m.pass != w.pass {
            return;
        }
        w.busy = false;
        // Правки журнала, вошедшие в точки задачи, — прочь.
        match &mut w.log {
            Some(l) => {
                l.drain(..m.log_len.min(l.len()));
            }
            None if m.edits == e.text.edits() => {
                // Счёт был потерян до задачи: точки задачи — с нуля по её снимку, правок
                // после нет.
                w.log = Some(Vec::new());
            }
            None => {
                // Правки после снимка не записаны: точки — заново.
                w.fresh = false;
                self.bump();
                self.hl_sync(rt);
                return;
            }
        }
        let complete = m.marks.complete();
        w.marks = Some((m.marks.clone(), m.edits));
        let later: Vec<TextEdit> = w.log.clone().unwrap_or_default();
        match m.win {
            Some(win) => {
                let ins = if later.len() == 1 {
                    bytes_at(&e.text.view(), later[0].at, later[0].new)
                } else {
                    None
                };
                w.show(win, &later, ins.as_deref());
                if later.is_empty() {
                    w.fresh = true;
                    if let Some(t) = w.pending_since.take() {
                        w.last_ms = Some(t.elapsed().as_secs_f32() * 1000.0);
                        w.fresh_n += 1;
                    }
                }
            }
            None => w.fresh = false,
        }
        if !complete && !w.tail && later.is_empty() {
            w.tail = true;
            spawn_tail(
                rt,
                self.id,
                w.pass,
                m.edits,
                e.text.snap(),
                m.marks,
                w.lang,
                w.feed,
                w.cancel.clone(),
            );
        }
        self.bump();
        self.hl_sync(rt);
    }

    /// Точки до конца: если новее не было — принять; свёртка неполна — окно заново.
    fn win_tail(&mut self, m: TailMsg, rt: &Runtime) {
        let Some(w) = self.win.as_mut() else {
            return;
        };
        w.tail = false;
        let _ = m.pass;
        if w.marks.as_ref().is_some_and(|(_, v)| *v == m.edits) {
            w.marks = Some((m.marks, m.edits));
            if w.partial {
                w.fresh = false;
            }
        }
        self.hl_sync(rt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Tag;

    fn span(start: u64, end: u64) -> Span {
        Span {
            start,
            end,
            tag: Tag::Plain,
        }
    }

    /// На экран — только отрезки видимых кусков; задетый краем остаётся.
    #[test]
    fn keeps_only_shown_slices() {
        let mut w = Window {
            range: 0..1000,
            spans: vec![span(0, 10), span(10, 20), span(95, 105), span(500, 510)],
            folds: Vec::new(),
            inert: Vec::new(),
            anchors: Vec::new(),
            by_tree: false,
            partial: false,
        };
        keep_only(&mut w, &[5..12, 100..110]);
        assert_eq!(w.spans, [span(0, 10), span(10, 20), span(95, 105)]);
    }
}
