//! Окно кода (ADR-0024 §8): tree-sitter по кускам между границами трекера. Кусок на
//! границе вложен в уровни: перед ним — настоящие заголовки уровней из файла, после —
//! закрывающие скобки; так разбирается верный документ, из которого выброшены элементы
//! выше и ниже куска. Границы — функция текста: окно с любой точки красит те же куски.

#![allow(
    clippy::many_single_char_names,
    reason = "окно: a, b — края, k, m — точка, s, e — кусок, c — граница"
)]

use std::cell::Cell;
use std::io;

use super::lex::{Cx, Frame, Inert, Lang, LineKind, Out, Scanner, Src, Stack, code};
use super::marks::{Mark, Marks};
use super::win::{Folds, WinOut, Window, parse_cut, push_span, reach_after, read_text};
use super::{Painter, Tag, grammar};
use crate::enc::Enc;

/// Кусок длиннее не разбирается: цвета нет (порог прежнего полного разбора).
pub(crate) const SEG_MAX: u64 = 16 << 20;
/// Шаг поиска конца куска.
const SCAN: u64 = 16 << 10;

/// Уровень на границе.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Lvl {
    Known(Frame),
    /// Открыт вне цепочки (на границе таких не бывает).
    Off,
    /// Открыт раньше начала прохода.
    Unknown,
}

/// Граница и уровни на ней.
struct Cut {
    at: u64,
    lvls: Vec<Lvl>,
}

/// Уровни прохода.
#[derive(Default)]
struct Lvls {
    v: Vec<Lvl>,
    next: Option<Frame>,
}

impl Lvls {
    fn at(depth: u64) -> Lvls {
        Lvls {
            v: vec![Lvl::Unknown; usize::try_from(depth).unwrap_or(0)],
            next: None,
        }
    }

    fn open(&mut self, depth: u64) {
        let d = usize::try_from(depth).unwrap_or(usize::MAX);
        self.v.truncate(d.saturating_sub(1));
        self.v.push(self.next.take().map_or(Lvl::Off, Lvl::Known));
    }

    fn close(&mut self, depth: u64) {
        self.v
            .truncate(usize::try_from(depth).unwrap_or(usize::MAX));
        self.next = None;
    }
}

/// Вывод прохода: окно, уровни, границы.
struct CutOut<'a> {
    w: WinOut,
    lv: Lvls,
    cuts: Vec<Cut>,
    /// Конец окна и «граница за ним есть».
    b: u64,
    past: &'a Cell<bool>,
}

impl Out for CutOut<'_> {
    fn span(&mut self, start: u64, end: u64, tag: Tag) {
        self.w.span(start, end, tag);
    }

    fn inert(&mut self, z: Inert) {
        self.w.inert(z);
    }

    fn open(&mut self, at: u64, depth: u64, kind: bool) {
        self.lv.open(depth);
        self.w.open(at, depth, kind);
    }

    fn close(&mut self, at: u64, depth: u64) {
        self.lv.close(depth);
        self.w.close(at, depth);
    }

    fn line(&mut self, at: u64, ind: u32, kind: LineKind) {
        self.w.line(at, ind, kind);
    }

    fn cut(&mut self, at: u64) {
        if at >= self.b {
            self.past.set(true);
        }
        self.cuts.push(Cut {
            at,
            lvls: self.lv.v.clone(),
        });
    }

    fn frame(&mut self, f: Frame) {
        self.lv.next = Some(f);
    }
}

/// Уровни `1..=need` позиции точки `k`: по сводкам назад. Уровень открыт в куске, где
/// глубина в последний раз была ниже него (ADR-0024 §2).
fn base_frames(
    src: &Src<'_>,
    lang: Lang,
    marks: &Marks,
    k: usize,
    need: u64,
) -> io::Result<Vec<Lvl>> {
    let n = usize::try_from(need).unwrap_or(0);
    let mut out = vec![Lvl::Off; n];
    let mut need = need;
    let mut c = k;
    while need > 0 && c > 0 {
        let p = marks.v[c - 1];
        if p.min < need {
            let lv = chunk_levels(src, lang, &p, marks.v[c].at)?;
            for l in p.min..need {
                let i = usize::try_from(l).unwrap_or(0);
                if let Some(x) = lv.get(i) {
                    out[i] = *x;
                }
            }
            need = p.min;
        }
        c -= 1;
    }
    Ok(out)
}

/// Уровни в конце куска `[p.at, end)`.
fn chunk_levels(src: &Src<'_>, lang: Lang, p: &Mark, end: u64) -> io::Result<Vec<Lvl>> {
    struct Rec(Lvls);
    impl Out for Rec {
        fn open(&mut self, _at: u64, depth: u64, _kind: bool) {
            self.0.open(depth);
        }
        fn close(&mut self, _at: u64, depth: u64) {
            self.0.close(depth);
        }
        fn frame(&mut self, f: Frame) {
            self.0.next = Some(f);
        }
    }
    let mut rec = Rec(Lvls::at(p.depth));
    let mut sc = Scanner::new(src, lang, p.at, p.st);
    let mut cx = Cx::new(Stack::at(p.depth, &p.st), &mut rec);
    sc.run_to(end, &mut cx, &|| false)?;
    drop(cx);
    Ok(rec.0.v)
}

/// Заголовки уровней и закрытие уровней на конце куска.
fn context(
    src: &Src<'_>,
    enc: Enc,
    lang: Lang,
    open: &[Frame],
    close: &[Frame],
    at_start: bool,
) -> io::Result<(String, String)> {
    let mut pre = String::new();
    let mut last = 0;
    for f in open {
        let a = f.head.max(last);
        if a < f.end {
            pre.push_str(&read_text(src, enc, a, f.end)?);
            // Заголовок Lua может кончиться словом (`do`).
            if lang == Lang::Code(code::Code::Lua) {
                pre.push(' ');
            }
        }
        last = last.max(f.end);
    }
    let mut post = String::new();
    let Lang::Code(g) = lang else {
        return Ok((pre, post));
    };
    // Первый элемент уровня у грамматик C разбирается иначе, чем следующие
    // (`STACK_OF(X) *p;`): перед куском не с начала уровня — пустышка.
    pre.push_str(code::filler(
        g,
        open.last().map_or(code::L_BODY, |f| f.kind),
        at_start,
    ));
    for (i, f) in close.iter().enumerate().rev() {
        let parent = if i == 0 {
            code::L_BODY
        } else {
            close[i - 1].kind
        };
        post.push_str(code::closer(g, f.kind, parent));
    }
    Ok((pre, post))
}

/// Кадры границы: неизвестные снизу — из `base`; `None` — цепочки нет.
fn frames(c: &Cut, base: &[Lvl]) -> Option<Vec<Frame>> {
    c.lvls
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let l = if *l == Lvl::Unknown {
                base.get(i).copied()?
            } else {
                *l
            };
            match l {
                Lvl::Known(f) => Some(f),
                _ => None,
            }
        })
        .collect()
}

/// Окно `[a, b)`.
#[allow(
    clippy::too_many_lines,
    reason = "проход, куски, разбор и свёртка — одним ходом"
)]
pub(super) fn code_window(
    src: &Src<'_>,
    enc: Enc,
    lang: Lang,
    marks: &Marks,
    a: u64,
    b: u64,
    stop: &dyn Fn() -> bool,
) -> io::Result<Window> {
    let k = marks.last_le(a);
    let m = marks.v[k];
    let s0 = m.at.saturating_sub(m.st.ext.cut).max(marks.start);
    // Кусок начат слишком далеко — цвета нет, свёртка — от своей точки.
    let far = a - s0 > SEG_MAX;
    let k0 = if far { k } else { marks.last_le(s0) };
    let m0 = marks.v[k0];
    let past = Cell::new(false);
    let mut out = CutOut {
        w: WinOut::new(a, b),
        lv: Lvls::at(m0.depth),
        cuts: Vec::new(),
        b,
        past: &past,
    };
    if m0.at == marks.start {
        out.cuts.push(Cut {
            at: marks.start,
            lvls: Vec::new(),
        });
    }
    let mut sc = Scanner::new(src, lang, m0.at, m0.st);
    sc.lines = Some(Vec::new());
    let (end_at, lines) = {
        let mut cx = Cx::new(Stack::at(m0.depth, &m0.st), &mut out);
        cx.cap = false;
        sc.run_to(b, &mut cx, stop)?;
        // Конец куска — первая граница не раньше `b`.
        let limit = b.saturating_add(SEG_MAX);
        while !past.get() && !sc.eof() && sc.at < limit {
            sc.run_to(sc.at.saturating_add(SCAN), &mut cx, stop)?;
        }
        // До следующей верной точки: дальше закрытия ищутся по сводкам.
        let to = reach_after(marks, sc.at);
        sc.run_to(to, &mut cx, stop)?;
        sc.flush(&mut cx);
        let at = sc.at;
        drop(cx);
        (at, sc.lines.take().unwrap_or_default())
    };
    let eof = end_at >= marks.len;
    let cuts = std::mem::take(&mut out.cuts);
    let mut w = Window {
        range: a..b,
        by_tree: true,
        ..Window::default()
    };
    let i0 = cuts.partition_point(|c| c.at <= a);
    if i0 > 0 {
        let unknown = cuts
            .iter()
            .map(|c| c.lvls.iter().take_while(|l| **l == Lvl::Unknown).count())
            .max()
            .unwrap_or(0);
        let base = base_frames(src, lang, marks, k0, unknown as u64)?;
        let g = grammar(lang.name());
        let mut painter = g.as_deref().and_then(Painter::new);
        for i in i0 - 1..cuts.len() {
            let s = cuts[i].at;
            if s >= b {
                break;
            }
            w.anchors.push(s);
            let (e, end) = match cuts.get(i + 1) {
                Some(c) => (c.at, frames(c, &base)),
                None if eof => (marks.len, Some(Vec::new())),
                None => break,
            };
            let (Some(p), Some(open), Some(close)) = (&mut painter, frames(&cuts[i], &base), end)
            else {
                continue;
            };
            if e - s > SEG_MAX {
                continue;
            }
            let (head, tail) = context(src, enc, lang, &open, &close, s == marks.start)?;
            #[cfg(test)]
            if let Some(p) = std::env::var("HL_DUMP")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                && s <= p
                && p < e
            {
                let body = read_text(src, enc, s, e)?;
                eprintln!(
                    "<<<DUMP
{head}<<<BODY
{body}<<<TAIL
{tail}
DUMP>>>"
                );
            }
            if std::env::var_os("HL_TRACE").is_some() {
                eprintln!("--- кусок {s}..{e}\nhead: {head}\ntail: {tail}");
            }
            let spans = parse_cut(src, enc, p, s..e, a..b, (&head, &tail), stop)?;
            for x in spans {
                let (lo, hi) = (x.start.max(a), x.end.min(b));
                if lo < hi {
                    push_span(&mut w.spans, lo, hi, x.tag);
                }
            }
        }
    }
    w.inert = std::mem::take(&mut out.w.inert);
    // Якоря — и кандидаты: выше них цвета от текста ниже не зависят.
    w.anchors.extend(
        out.w
            .lines
            .iter()
            .filter(|l| l.0 >= a && l.0 <= b)
            .map(|l| l.0),
    );
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
    let (f, partial) = folds.places(&out.w.opens, &out.w.closes, a, b)?;
    w.folds = f;
    w.partial = partial;
    Ok(w)
}
