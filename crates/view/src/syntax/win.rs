//! Окно подсветки (ADR-0024 §3, §5): видимое ± запас от ближайшей верной точки — роли,
//! места свёртки, инертные зоны и «якоря» (до них цвета верны при правке). JSON, TOML,
//! INI, XML — лексер; YAML и Markdown — tree-sitter по отрезкам от безопасных строк.

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use std::cell::{Cell, RefCell};
use std::io;
use std::ops::Range;

use super::edit::FoldAt;
use super::lex::{Cx, Feed, Inert, Lang, LineKind, Out, Scanner, Src, St, Stack, md};
use super::marks::{Mark, Marks, MarksResolve};
use super::{HtmlZones, Painter, Span, Tag, grammar};
use crate::enc::Enc;
use crate::search::transcode::{OffMap, decode};

/// Итог окна.
#[derive(Clone, Debug, Default)]
pub struct Window {
    /// Посчитанные байты.
    pub range: Range<u64>,
    /// Роли по байтам текста (по возрастанию, без пересечений, без `Plain`).
    pub spans: Vec<Span>,
    pub folds: Vec<FoldAt>,
    pub inert: Vec<Inert>,
    /// Начала мест, с которых пересчёт не меняет цвета выше (строки — у лексеров,
    /// начала отрезков — у YAML и Markdown), по возрастанию.
    pub anchors: Vec<u64>,
    /// Якоря — строки разбора (YAML, Markdown): правка на самой строке меняет и строки
    /// выше её отрезка — берётся якорь раньше.
    pub by_tree: bool,
    /// Свёртка неполна: точки ещё не дошли (позже — окно заново).
    pub partial: bool,
}

/// Запас окна по краям видимого и порог нового окна (ADR-0024 §7): лексеры — 64 и
/// 16 кБ (окно — доли мс), YAML — 8 и 2 кБ, Markdown — 2 кБ и 512 байт (разбор
/// tree-sitter ≈ 1–5 МБ/с).
pub fn margins(lang: Lang) -> (u64, u64) {
    match lang {
        // Строчный разбор — свой на каждый абзац (≈ 50 мкс): запас меньше.
        Lang::Md => (2 << 10, 512),
        Lang::Yaml => (8 << 10, 2 << 10),
        _ => (64 << 10, 16 << 10),
    }
}

impl Window {
    /// Позиция, до которой цвета окна верны после правки с `at` (ADR-0024 §4); `None`
    /// — якоря нет (от начала окна).
    pub fn guard(&self, at: u64) -> Option<u64> {
        guard_of(&self.anchors, self.by_tree, at)
    }
}

/// Якорь не дальше `at`; у строк разбора — предыдущий.
pub fn guard_of(anchors: &[u64], by_tree: bool, at: u64) -> Option<u64> {
    let i = anchors.partition_point(|&x| x <= at);
    let skip = usize::from(by_tree);
    (i > skip).then(|| anchors[i - 1 - skip])
}

/// Окно `[a, b)`. `None` — проход ещё не дошёл до `a` (окно ждёт).
pub(crate) fn window(
    src: &Src<'_>,
    enc: Enc,
    lang: Lang,
    marks: &Marks,
    a: u64,
    b: u64,
    stop: &dyn Fn() -> bool,
) -> io::Result<Option<Window>> {
    let a = a.max(marks.start).min(marks.len);
    let b = b.max(a).min(marks.len);
    if a > marks.frontier() && !marks.complete() {
        return Ok(None);
    }
    if lang.by_tree() {
        tree_window(src, enc, lang, marks, a, b, stop).map(Some)
    } else {
        lex_window(src, lang, marks, a, b, stop).map(Some)
    }
}

/// Вывод окна лексера.
struct WinOut {
    a: u64,
    b: u64,
    spans: Vec<Span>,
    inert: Vec<Inert>,
    /// Открытия с начала прохода: позиция и глубина после.
    opens: Vec<(u64, u64)>,
    /// Закрытие открытия `i` (если было).
    closes: Vec<Option<u64>>,
    open_stack: Vec<usize>,
    lines: Vec<(u64, u32, LineKind)>,
    /// Якоря внутри строк (в окне).
    anchors: Vec<u64>,
    /// Markdown: строки HTML-блоков (`Out::html`), `>` в них (`Out::html_quote`) и
    /// подчёркивания, которые Setext не делают (`Out::no_setext`).
    html: Vec<u64>,
    html_quote: Vec<u64>,
    no_setext: Vec<u64>,
}

impl WinOut {
    fn new(a: u64, b: u64) -> WinOut {
        WinOut {
            a,
            b,
            spans: Vec::new(),
            inert: Vec::new(),
            opens: Vec::new(),
            closes: Vec::new(),
            open_stack: Vec::new(),
            lines: Vec::new(),
            anchors: Vec::new(),
            html: Vec::new(),
            html_quote: Vec::new(),
            no_setext: Vec::new(),
        }
    }

    /// Решения трекера на отрезке `r`.
    fn zones(&self, r: &Range<u64>) -> SegZones<'_> {
        let within = |v: &'_ [u64]| {
            let i = v.partition_point(|&z| z < r.start);
            i..i + v[i..].partition_point(|&z| z < r.end)
        };
        SegZones {
            html: &self.html[within(&self.html)],
            quotes: &self.html_quote[within(&self.html_quote)],
            no_setext: &self.no_setext[within(&self.no_setext)],
        }
    }
}

impl Out for WinOut {
    fn span(&mut self, start: u64, end: u64, tag: Tag) {
        let (s, e) = (start.max(self.a), end.min(self.b));
        if s < e {
            push_span(&mut self.spans, s, e, tag);
        }
    }

    fn inert(&mut self, z: Inert) {
        if z.end >= self.a && z.start <= self.b {
            self.inert.push(z);
        }
    }

    fn open(&mut self, at: u64, depth: u64, _kind: bool) {
        self.opens.push((at, depth));
        self.closes.push(None);
        self.open_stack.push(self.opens.len() - 1);
    }

    fn close(&mut self, at: u64, _depth: u64) {
        if let Some(i) = self.open_stack.pop() {
            self.closes[i] = Some(at);
        }
    }

    fn line(&mut self, at: u64, ind: u32, kind: LineKind) {
        self.lines.push((at, ind, kind));
    }

    fn anchor(&mut self, at: u64) {
        if at >= self.a && at <= self.b {
            self.anchors.push(at);
        }
    }

    fn html(&mut self, at: u64) {
        self.html.push(at);
    }

    fn html_quote(&mut self, at: u64) {
        self.html_quote.push(at);
    }

    fn no_setext(&mut self, at: u64) {
        self.no_setext.push(at);
    }
}

/// Отрезок в конец списка: подряд одной роли — слить.
fn push_span(v: &mut Vec<Span>, s: u64, e: u64, tag: Tag) {
    if let Some(l) = v.last_mut()
        && l.end == s
        && l.tag == tag
    {
        l.end = e;
        return;
    }
    if let Some(l) = v.last()
        && s < l.end
    {
        // Лексер красит по порядку; на всякий случай — без пересечений.
        let s = l.end;
        if s < e {
            v.push(Span {
                start: s,
                end: e,
                tag,
            });
        }
        return;
    }
    v.push(Span {
        start: s,
        end: e,
        tag,
    });
}

/// Сканер с точки `k` с разрешением стека по точкам.
fn lex_window(
    src: &Src<'_>,
    lang: Lang,
    marks: &Marks,
    a: u64,
    b: u64,
    stop: &dyn Fn() -> bool,
) -> io::Result<Window> {
    let k0 = marks.last_le(a);
    let m0 = marks.v[k0];
    let mut out = WinOut::new(a, b);
    let (end_at, lines) = {
        let cell = RefCell::new(marks.v[..marks.ok].to_vec());
        let last = Cell::new(k0);
        let get_last = || last.get();
        let mut res = MarksResolve {
            marks: &cell,
            last: &get_last,
            src,
            lang,
        };
        let mut sc = Scanner::new(src, lang, m0.at, m0.st);
        sc.lines = Some(Vec::new());
        let mut cx = Cx::new(Stack::at(m0.depth, &m0.st), &mut out);
        cx.resolve = Some(&mut res);
        cx.cap = false;
        sc.run_to(b, &mut cx, stop)?;
        // До следующей верной точки: дальше закрытия ищутся по сводкам.
        let to = reach_after(marks, sc.at);
        sc.run_to(to, &mut cx, stop)?;
        sc.flush(&mut cx);
        let end_at = sc.at;
        drop(cx);
        (end_at, sc.lines.take().unwrap_or_default())
    };
    let mut w = Window {
        range: a..b,
        spans: std::mem::take(&mut out.spans),
        inert: std::mem::take(&mut out.inert),
        anchors: Vec::new(),
        by_tree: false,
        folds: Vec::new(),
        partial: false,
    };
    // Якоря — начала строк окна (и само начало прохода, если это начало текста).
    if m0.at == marks.start {
        w.anchors.push(m0.at);
    }
    w.anchors.extend(lines.iter().copied().filter(|&p| p <= b));
    w.anchors.append(&mut out.anchors);
    w.anchors.sort_unstable();
    w.anchors.dedup();
    let mut lines_all = Vec::with_capacity(lines.len() + 1);
    if m0.at == marks.start {
        lines_all.push(m0.at);
    }
    lines_all.extend(lines);
    let folds = Folds {
        src,
        lang,
        marks,
        lines: &lines_all,
        scan: m0.at..end_at,
    };
    let (f, partial) = folds.places(&out.opens, &out.closes, a, b)?;
    w.folds = f;
    w.partial = partial;
    Ok(w)
}

/// Места свёртки по событиям структуры.
struct Folds<'a> {
    src: &'a Src<'a>,
    lang: Lang,
    marks: &'a Marks,
    /// Начала строк, известные проходу (по возрастанию).
    lines: &'a [u64],
    scan: Range<u64>,
}

/// Байт строки дальше не ищем.
const LINE_MAX: u64 = 16 << 20;

impl Folds<'_> {
    /// Места с заголовком в `[a, b)`; `true` — есть открытия без найденного закрытия.
    fn places(
        &self,
        opens: &[(u64, u64)],
        closes: &[Option<u64>],
        a: u64,
        b: u64,
    ) -> io::Result<(Vec<FoldAt>, bool)> {
        let mut partial = false;
        // Окно с середины строки — с её начала: место свёртки строки (первое открытие)
        // не зависит от того, где режет окно.
        let a = match self.lines.partition_point(|&l| l <= a) {
            0 => a,
            k => self.lines[k - 1],
        };
        // Закрытия далеко за проходом — по сводкам точек.
        let mut far: Vec<Option<u64>> = closes.to_vec();
        let pend: Vec<usize> = (0..opens.len())
            .filter(|&i| closes[i].is_none() && opens[i].0 >= a && opens[i].0 < b)
            .collect();
        if !pend.is_empty() {
            let found = self.far_closes(&pend.iter().map(|&i| opens[i].1).collect::<Vec<_>>())?;
            for (j, &i) in pend.iter().enumerate() {
                far[i] = found[j];
                if found[j].is_none() {
                    partial = true;
                }
            }
        }
        let mut out: Vec<FoldAt> = Vec::new();
        let mut last_head = None;
        // Начало строки после последнего известного (одно на все открытия хвоста).
        let mut tail_next: Option<Option<u64>> = None;
        for (i, &(at, _)) in opens.iter().enumerate() {
            if at < a || at >= b {
                continue;
            }
            // Заголовок — только у строк, начало которых проход видел (все открытия
            // строки известны: «первое открытие строки» одинаково в любом окне).
            let k = self.lines.partition_point(|&l| l <= at);
            if k == 0 {
                continue;
            }
            let head = self.lines[k - 1];
            if last_head == Some(head) {
                continue;
            }
            let Some(close) = far[i] else {
                continue;
            };
            let start = match self.lines.get(k) {
                Some(&l) if l <= self.scan.end => l,
                _ => {
                    // Перевода строки после `at` в пройденном нет: закрытие в нём — на
                    // той же строке.
                    if close <= self.scan.end {
                        continue;
                    }
                    let t = if let Some(t) = tail_next {
                        t
                    } else {
                        let t = probe_next_line(self.src, at)?;
                        tail_next = Some(t);
                        t
                    };
                    match t {
                        Some(l) => l,
                        None => continue,
                    }
                }
            };
            if close < start {
                continue;
            }
            last_head = Some(head);
            let Some(cls) = self.line_start(close)? else {
                continue;
            };
            let mut end = if self.only_ws(cls, close)? {
                cls
            } else {
                match self.next_line(close)? {
                    Some(e) => e,
                    None => continue,
                }
            };
            end = self.trim_blank(start, end)?;
            if start < end {
                out.push(FoldAt { head, start, end });
            }
        }
        Ok((out, partial))
    }

    /// Закрытия уровней с глубиной после открытия `depths` (вложенные: глубже — раньше)
    /// после конца прохода.
    fn far_closes(&self, depths: &[u64]) -> io::Result<Vec<Option<u64>>> {
        let m = self.marks;
        let mut out = vec![None; depths.len()];
        let mut order: Vec<usize> = (0..depths.len()).collect();
        order.sort_by(|&x, &y| depths[y].cmp(&depths[x]));
        let mut c = m.v[..m.ok].partition_point(|k| k.at < self.scan.end);
        // Сводки — только с точки, где кончился проход (иначе между ними — не видно).
        if self.scan.end >= m.len || c >= m.ok || m.v[c].at != self.scan.end {
            return Ok(out);
        }
        for &j in &order {
            let target = depths[j].saturating_sub(1);
            loop {
                if c >= m.ok {
                    break;
                }
                let end = m.chunk_end(c);
                if end <= m.v[c].at && !(m.complete() && c + 1 == m.ok) {
                    break;
                }
                if m.v[c].min <= target {
                    if let Some(p) = self.close_in(&m.v[c], end, target)? {
                        out[j] = Some(p);
                    }
                    break;
                }
                c += 1;
            }
            if out[j].is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Первое закрытие до глубины `target` в куске точки `k` (до `end`).
    fn close_in(&self, k: &Mark, end: u64, target: u64) -> io::Result<Option<u64>> {
        struct Rec {
            target: u64,
            hit: Option<u64>,
        }
        impl Out for Rec {
            fn close(&mut self, at: u64, depth: u64) {
                if self.hit.is_none() && depth <= self.target {
                    self.hit = Some(at);
                }
            }
        }
        let mut rec = Rec { target, hit: None };
        let mut sc = Scanner::new(self.src, self.lang, k.at, k.st);
        let mut cx = Cx::new(Stack::at(k.depth, &k.st), &mut rec);
        sc.run_to(end, &mut cx, &|| false)?;
        if sc.eof() {
            sc.flush(&mut cx);
        }
        drop(cx);
        Ok(rec.hit)
    }

    /// Начало строки с `pos` (`None` — дальше [`LINE_MAX`] или до известного начала).
    fn line_start(&self, pos: u64) -> io::Result<Option<u64>> {
        let i = self.lines.partition_point(|&l| l <= pos);
        if i > 0 {
            let l = self.lines[i - 1];
            // Строки известны только в пройденном: дальше (закрытие по сводкам точек) —
            // назад от `pos`, не раньше последней известной.
            if pos < self.scan.end {
                return Ok(Some(l));
            }
            return probe_line_start(self.src, pos, l);
        }
        probe_line_start(self.src, pos, self.marks.start)
    }

    /// Начало следующей строки после `pos` (конец текста — `len`).
    fn next_line(&self, pos: u64) -> io::Result<Option<u64>> {
        let i = self.lines.partition_point(|&l| l <= pos);
        if let Some(&l) = self.lines.get(i)
            && pos < self.scan.end
        {
            return Ok(Some(l));
        }
        probe_next_line(self.src, pos)
    }

    fn only_ws(&self, a: u64, b: u64) -> io::Result<bool> {
        if b <= a {
            return Ok(true);
        }
        if b - a > 4096 {
            return Ok(false);
        }
        let u = units(self.src, a, b)?;
        Ok(u.iter().all(|&c| c == b' ' || c == b'\t'))
    }

    /// Пустые строки в конце `[start, end)` остаются видны.
    fn trim_blank(&self, start: u64, mut end: u64) -> io::Result<u64> {
        let unit = self.src.feed.unit();
        while end > start {
            let Some(ls) = probe_line_start(self.src, end.saturating_sub(unit).max(start), start)?
            else {
                break;
            };
            let ls = ls.max(start);
            if end - ls > 4096 {
                break;
            }
            let u = units(self.src, ls, end)?;
            if u.iter().all(|c| super::lex::is_ws(*c)) {
                end = ls;
            } else {
                break;
            }
        }
        Ok(end)
    }
}

/// Единицы `[a, b)` источника (UTF-16 — по единице, прочие — байты как есть).
fn units(src: &Src<'_>, a: u64, b: u64) -> io::Result<Vec<u8>> {
    let n = usize::try_from(b.saturating_sub(a)).unwrap_or(0);
    let mut raw = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match src.text.read_at(a + got as u64, &mut raw[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    raw.truncate(got);
    Ok(match src.feed {
        Feed::Utf16Le | Feed::Utf16Be => {
            let be = src.feed == Feed::Utf16Be;
            raw.as_chunks::<2>()
                .0
                .iter()
                .map(|c| {
                    let u = if be {
                        u16::from_be_bytes(*c)
                    } else {
                        u16::from_le_bytes(*c)
                    };
                    #[allow(clippy::cast_possible_truncation, reason = "u < 0x80")]
                    if u < 0x80 { u as u8 } else { 0x80 }
                })
                .collect()
        }
        _ => raw,
    })
}

/// Начало строки с `pos`: назад до перевода строки (не дальше `lo` и [`LINE_MAX`]).
fn probe_line_start(src: &Src<'_>, pos: u64, lo: u64) -> io::Result<Option<u64>> {
    let unit = src.feed.unit();
    let lo = lo.max(src.start);
    let mut end = pos;
    while end > lo {
        if pos - end > LINE_MAX {
            return Ok(None);
        }
        let a = end.saturating_sub(4096).max(lo);
        // Выравнивание по единице от начала текста.
        let a = a + (a - src.start) % unit;
        let u = units(src, a, end)?;
        if let Some(i) = memchr::memrchr(b'\n', &u) {
            return Ok(Some(a + (i as u64 + 1) * unit));
        }
        if a == lo {
            break;
        }
        end = a;
    }
    Ok(Some(lo))
}

/// Начало следующей строки после `pos`; конец текста — `len`.
fn probe_next_line(src: &Src<'_>, pos: u64) -> io::Result<Option<u64>> {
    let unit = src.feed.unit();
    let len = src.len();
    let mut a = pos;
    while a < len {
        if a - pos > LINE_MAX {
            return Ok(None);
        }
        let b = (a + 4096).min(len);
        let u = units(src, a, b)?;
        if let Some(i) = memchr::memchr(b'\n', &u) {
            return Ok(Some(a + (i as u64 + 1) * unit));
        }
        a = b;
    }
    Ok(Some(len))
}

// ---- YAML и Markdown: tree-sitter по отрезкам ----

/// Строки трекера от точки `k` до `to` (и чуть дальше — до конца строки `to`).
fn track(
    src: &Src<'_>,
    lang: Lang,
    k: &Mark,
    to: u64,
    out: &mut WinOut,
    stop: &dyn Fn() -> bool,
) -> io::Result<(u64, Vec<u64>, St)> {
    let mut sc = Scanner::new(src, lang, k.at, k.st);
    sc.lines = Some(Vec::new());
    let mut cx = Cx::new(Stack::at(k.depth, &k.st), out);
    cx.cap = false;
    sc.run_to(to, &mut cx, stop)?;
    sc.flush(&mut cx);
    Ok((sc.at, sc.lines.take().unwrap_or_default(), sc.state(&cx)))
}

fn tree_window(
    src: &Src<'_>,
    enc: Enc,
    lang: Lang,
    marks: &Marks,
    a: u64,
    b: u64,
    stop: &dyn Fn() -> bool,
) -> io::Result<Window> {
    let g = grammar(lang.name());
    let mut k = marks.last_le(a);
    let mut reach = reach_after(marks, b);
    let (out, segs, k, end_scan, phys, last) = loop {
        let m = marks.v[k];
        let mut out = WinOut::new(a, b);
        let (at_end, phys, last) = track(src, lang, &m, reach, &mut out, stop)?;
        let eof = at_end >= marks.len;
        let from_start = m.at == marks.start;
        match segments(
            lang,
            &out.lines,
            a,
            b,
            from_start,
            marks.start,
            eof,
            marks.len,
        ) {
            Seg::NoStart if k > 0 => k -= 1,
            Seg::NoStart => {
                break (
                    out,
                    std::iter::once(marks.start..marks.len).collect(),
                    k,
                    at_end,
                    phys,
                    last,
                );
            }
            Seg::More => {
                let grow = (reach - m.at).max(4 * super::marks::STEP);
                let next = reach_after(marks, reach.saturating_add(grow));
                reach = if next > reach { next } else { marks.len };
            }
            Seg::Ok(v) => break (out, v, k, at_end, phys, last),
        }
    };
    let mut w = Window {
        range: a..b,
        by_tree: true,
        ..Window::default()
    };
    let mut painter = g.as_deref().and_then(Painter::new);
    for seg in &segs {
        w.anchors.push(seg.start);
        if let Some(p) = &mut painter {
            let spans = parse_seg(src, enc, p, seg.clone(), &out.zones(seg), a..b, stop)?;
            for s in spans {
                let (x, y) = (s.start.max(a), s.end.min(b));
                if x < y {
                    push_span(&mut w.spans, x, y, s.tag);
                }
            }
        }
    }
    // Якоря — и все строки, с которых разбор можно начать (до них цвета от текста ниже
    // не зависят; правка на самой строке — от якоря раньше, ADR-0024 §4).
    w.anchors.extend(
        out.lines
            .iter()
            .filter(|l| {
                l.0 >= a
                    && l.0 <= b
                    && match lang {
                        Lang::Md => l.2 == LineKind::Safe,
                        _ => matches!(l.2, LineKind::Key | LineKind::Item | LineKind::Doc),
                    }
            })
            .map(|l| l.0),
    );
    w.anchors.sort_unstable();
    w.anchors.dedup();
    // Закрытие ограды в начале текста сделает его метаданными: строки в нём не якоря.
    if lang == Lang::Md && md::open_head(&last) {
        let head = marks.start + lang.head();
        w.anchors.retain(|&x| x >= head);
    }
    // Свёртка — по событиям трекера и началам строк.
    let mut lines_all = phys;
    if marks.v[k].at == marks.start {
        lines_all.insert(0, marks.start);
    }
    lines_all.sort_unstable();
    lines_all.dedup();
    let folds = Folds {
        src,
        lang,
        marks,
        lines: &lines_all,
        scan: marks.v[k].at..end_scan,
    };
    let (f, partial) = folds.places(&out.opens, &out.closes, a, b)?;
    w.folds = f;
    w.partial = partial;
    Ok(w)
}

/// Позиция первой верной точки не раньше `pos`; нет — конец текста, если всё посчитано,
/// иначе `pos` (не дальше конца).
fn reach_after(marks: &Marks, pos: u64) -> u64 {
    let kb = marks.v[..marks.ok].partition_point(|m| m.at < pos);
    if kb < marks.ok {
        marks.v[kb].at
    } else if marks.complete() {
        marks.len
    } else {
        pos.min(marks.len)
    }
}

/// Отрезки окна.
enum Seg {
    Ok(Vec<Range<u64>>),
    /// Начала ≤ a в строках нет: точкой раньше.
    NoStart,
    /// Конец отрезка за пройденным: идти дальше.
    More,
}

/// Отрезки разбора, покрывающие `[a, b)` (ADR-0024 §3).
#[allow(
    clippy::too_many_arguments,
    reason = "строки, окно, начало и конец текста"
)]
fn segments(
    lang: Lang,
    lines: &[(u64, u32, LineKind)],
    a: u64,
    b: u64,
    from_start: bool,
    text_start: u64,
    eof: bool,
    len: u64,
) -> Seg {
    if lang == Lang::Md {
        let s = match lines
            .iter()
            .rev()
            .find(|l| l.0 <= a && l.2 == LineKind::Safe)
        {
            Some(l) => l.0,
            None if from_start => text_start,
            None => return Seg::NoStart,
        };
        let e = match lines
            .iter()
            .find(|l| l.0 >= b && l.0 > s && l.2 == LineKind::Safe)
        {
            Some(l) => l.0,
            None if eof => len,
            None => return Seg::More,
        };
        // Каждый кусок между безопасными строками — свой разбор (ADR-0024 §6 п. 4): цвет
        // строки не зависит от того, с какой строки начато окно, на любом тексте.
        let mut out = Vec::new();
        let mut p = s;
        for l in lines.iter().filter(|l| l.2 == LineKind::Safe) {
            if l.0 > s && l.0 < e {
                out.push(p..l.0);
                p = l.0;
            }
        }
        out.push(p..e);
        return Seg::Ok(out);
    }
    // YAML: самая глубокая открытая строка ≤ a — начало; отрезок — до строки с меньшим
    // отступом, того же отступа другого вида или маркера документа.
    let mut open: Vec<usize> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.0 > a {
            break;
        }
        close_open(lines, &mut open, i);
        if matches!(l.2, LineKind::Key | LineKind::Item | LineKind::Doc) {
            open.push(i);
        }
    }
    let mut idx = match open.last() {
        Some(&i) => Some(i),
        None if from_start => None,
        None => return Seg::NoStart,
    };
    let mut start = idx.map_or(text_start, |i| lines[i].0);
    let mut out = Vec::new();
    loop {
        let Some((end, next)) = seg_end(lines, idx, b, eof, len) else {
            return Seg::More;
        };
        out.push(start..end);
        if end >= b || end >= len {
            break;
        }
        match next {
            Some(n) => {
                start = lines[n].0;
                idx = Some(n);
            }
            None => return Seg::More,
        }
    }
    Seg::Ok(out)
}

/// Строки, отрезок которых кончился на строке `i`, уходят из открытых.
fn close_open(lines: &[(u64, u32, LineKind)], open: &mut Vec<usize>, i: usize) {
    let (_, ind, kind) = lines[i];
    if kind == LineKind::Doc {
        open.clear();
        return;
    }
    let kind = norm(kind);
    while let Some(&j) = open.last() {
        let (_, jind, jk) = lines[j];
        if jk == LineKind::Doc {
            break;
        }
        let _ = kind;
        if ind <= jind {
            open.pop();
        } else {
            break;
        }
    }
}

fn norm(k: LineKind) -> LineKind {
    match k {
        LineKind::QVal => LineKind::Key,
        x => x,
    }
}

/// Конец отрезка со строки `idx` (`None` — с начала текста: до маркера документа) и
/// строка, с которой начать следующий; `None` — конец за пройденными строками.
fn seg_end(
    lines: &[(u64, u32, LineKind)],
    idx: Option<usize>,
    b: u64,
    eof: bool,
    len: u64,
) -> Option<(u64, Option<usize>)> {
    let from = idx.map_or(0, |i| i + 1);
    let (ind, kind) = idx.map_or((0, LineKind::Doc), |i| (lines[i].1, norm(lines[i].2)));
    for (j, l) in lines.iter().enumerate().skip(from) {
        if l.2 == LineKind::Doc {
            return Some((l.0, Some(j)));
        }
        if kind == LineKind::Doc {
            continue;
        }
        let k = norm(l.2);
        if l.1 < ind || (l.1 == ind && k != kind) {
            return Some((l.0, Some(j)));
        }
        // Брат того же отступа и вида за концом окна — разрез.
        if l.0 >= b && l.1 == ind && k == kind && l.2 != LineKind::QVal {
            return Some((l.0, Some(j)));
        }
    }
    eof.then_some((len, None))
}

/// Строки HTML-блоков для разбора (`zones` — позиции `Out::html` в `text`, по возрастанию):
/// от позиции до конца строки — пустой заголовок `#` той же длины. Для грамматики это, как
/// и строка HTML-блока, блок в одну строку: контейнеры те же, абзац прерван, на следующую
/// строку ничего не переходит (ADR-0024 §6 п. 4). Итог — начала масок.
fn mask_html(text: &mut [u8], zones: &[usize]) -> Vec<usize> {
    let mut out = Vec::with_capacity(zones.len());
    let mut done = 0;
    for &z in zones {
        if z >= text.len() || z < done {
            continue;
        }
        let e = memchr::memchr(b'\n', &text[z..]).map_or(text.len(), |i| z + i);
        done = e;
        let e = if e > z && text[e - 1] == b'\r' {
            e - 1
        } else {
            e
        };
        if z < e {
            text[z..e].fill(b' ');
            text[z] = b'#';
            out.push(z);
        }
    }
    out
}

/// Подчёркивания под абзацем из определений ссылок (`zones` — позиции `Out::no_setext` в
/// `text`). Грамматика считает такой абзац текстом и делает заголовок Setext, а `CommonMark`
/// — нет: `===` и `--` здесь текст абзаца, `---` — разрыв. Она видит их без подчёркивания:
/// текст — буквы, разрыв — `***`, той же длины.
fn mask_setext(text: &mut [u8], zones: &[usize]) {
    for &z in zones {
        if z >= text.len() {
            continue;
        }
        let e = memchr::memchr(b'\n', &text[z..]).map_or(text.len(), |i| z + i);
        let line = &mut text[z..e];
        let to = if memchr::memchr_iter(b'-', line).count() >= 3 {
            b'*'
        } else {
            b'x'
        };
        for c in line.iter_mut().filter(|c| matches!(c, b'=' | b'-')) {
            *c = to;
        }
    }
}

/// Позиции `at` байт `raw` (по возрастанию) → позиции в `from_utf8_lossy(raw)`.
fn raw_to_lossy(raw: &[u8], at: &mut [usize]) {
    let mut i = 0;
    let (mut r, mut l) = (0, 0);
    for c in raw.utf8_chunks() {
        let n = c.valid().len();
        while i < at.len() && at[i] <= r + n {
            at[i] = at[i] - r + l;
            i += 1;
        }
        r += n + c.invalid().len();
        l += n + if c.invalid().is_empty() { 0 } else { 3 };
    }
}

/// Что трекер решил за грамматику на отрезке (позиции по возрастанию).
struct SegZones<'a> {
    html: &'a [u64],
    quotes: &'a [u64],
    no_setext: &'a [u64],
}

/// Разбор отрезка `[s, e)` → роли по байтам текста.
#[allow(clippy::too_many_arguments, reason = "текст, отрезок, зоны и окно")]
fn parse_seg(
    src: &Src<'_>,
    enc: Enc,
    p: &mut Painter<'_>,
    r: Range<u64>,
    seg: &SegZones<'_>,
    win: Range<u64>,
    stop: &dyn Fn() -> bool,
) -> io::Result<Vec<Span>> {
    let n = usize::try_from(r.end - r.start).unwrap_or(0);
    let mut raw = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match src.text.read_at(r.start + got as u64, &mut raw[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    raw.truncate(got);
    let (text, map) = if enc.is_utf8() {
        (raw, None)
    } else {
        let mut out = Vec::new();
        let mut map = OffMap::default();
        decode(&raw, enc, &mut out, &mut map);
        (out, Some(map))
    };
    // Маски — на копии: карта смещений читает настоящий текст.
    let at = |z: u64| {
        let z = usize::try_from(z - r.start).unwrap_or(0);
        map.as_ref().map_or(z, |m| m.to_utf8(&text, z))
    };
    let mut zones = HtmlZones::default();
    let masked = (!seg.html.is_empty() || !seg.no_setext.is_empty()).then(|| {
        let mut t = text.clone();
        let html: Vec<usize> = seg.html.iter().map(|&z| at(z)).collect();
        zones.heads = mask_html(&mut t, &html);
        zones.quotes = seg.quotes.iter().map(|&z| at(z)).collect();
        let under: Vec<usize> = seg.no_setext.iter().map(|&z| at(z)).collect();
        mask_setext(&mut t, &under);
        t
    });
    let parsed = masked.as_deref().unwrap_or(&text);
    let s = String::from_utf8_lossy(parsed);
    if s.len() != parsed.len() {
        raw_to_lossy(parsed, &mut zones.heads);
        raw_to_lossy(parsed, &mut zones.quotes);
    }
    // Окно в координатах текста разбора (у неверного UTF-8 — весь текст: замены меняют
    // смещения).
    let lo = win.start.max(r.start) - r.start;
    let hi = win.end.min(r.end).max(win.start.max(r.start)) - r.start;
    let range = if s.len() == parsed.len() {
        match &map {
            None => Some(usize::try_from(lo).unwrap_or(0)..usize::try_from(hi).unwrap_or(0)),
            Some(m) => Some(
                m.to_utf8(&text, usize::try_from(lo).unwrap_or(0))
                    ..m.to_utf8(&text, usize::try_from(hi).unwrap_or(0)),
            ),
        }
    } else {
        None
    };
    let spans = p
        .run(&s, range.as_ref(), &zones, stop)
        .map_err(|_| io::Error::from(io::ErrorKind::Interrupted))?;
    // `from_utf8_lossy` меняет длину только у неверного UTF-8: тогда смещения — по
    // замене; без замены — 1:1.
    let same = s.len() == parsed.len();
    Ok(spans
        .into_iter()
        .filter_map(|sp| {
            let (x, y) = if same {
                (sp.start, sp.end)
            } else {
                (lossy_to_raw(parsed, sp.start), lossy_to_raw(parsed, sp.end))
            };
            let (x, y) = match &map {
                None => (x, y),
                Some(m) => (
                    m.to_src(&text, usize::try_from(x).unwrap_or(0)) as u64,
                    m.to_src(&text, usize::try_from(y).unwrap_or(0)) as u64,
                ),
            };
            (x < y).then_some(Span {
                start: r.start + x,
                end: r.start + y,
                tag: sp.tag,
            })
        })
        .collect())
}

/// Смещение в строке после `from_utf8_lossy` → смещение в байтах `raw`.
fn lossy_to_raw(raw: &[u8], target: u64) -> u64 {
    let mut i = 0usize;
    let mut out = 0u64;
    while i < raw.len() && out < target {
        match std::str::from_utf8(&raw[i..]) {
            Ok(_) => {
                return (i as u64 + (target - out)).min(raw.len() as u64);
            }
            Err(e) => {
                let ok = e.valid_up_to();
                if out + ok as u64 >= target {
                    return i as u64 + (target - out);
                }
                out += ok as u64;
                i += ok;
                let bad = e.error_len().unwrap_or(raw.len() - i);
                // U+FFFD — 3 байта строки на `bad` байт источника.
                out += 3;
                i += bad;
            }
        }
    }
    i as u64
}
