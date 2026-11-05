//! Контрольные точки подсветки (ADR-0024 §2, §4): состояние лексера не реже чем через
//! [`STEP`] байт, глубина и минимум глубины на куске до следующей точки. Проход —
//! потоком по тексту; правка — точки после неё «под вопросом» и пересчёт до совпадения
//! состояния; роли уровней стека глубже точки — по сводкам и проходу одного куска.

use std::cell::RefCell;
use std::io;

use super::lex::{Cx, Lang, NoOut, Out, Resolve, Scanner, Src, St, Stack};
use crate::edit::piece::TextEdit;

/// Шаг точек.
pub const STEP: u64 = 64 << 10;

/// Точка: позиция (байт текста), состояние, глубина на ней и минимум глубины на куске
/// до следующей точки (у последней — до `Marks::upto`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub at: u64,
    pub st: St,
    pub depth: u64,
    pub min: u64,
}

/// Точки документа.
///
/// - Чисто (`dirty == None`): все точки и сводки кусков верны, последний кусок — до
///   `upto`; `done` — проход дошёл до конца текста.
/// - После правки: точки `v[..ok]` верны (состояние и глубина), куски до `ok - 1` —
///   тоже; кусок `ok - 1` и точки `v[ok..]` (сдвинуты правкой) — «под вопросом»;
///   пересчёт сходится только на точке не раньше `dirty` (конец правленого).
#[derive(Clone, Debug)]
pub struct Marks {
    pub v: Vec<Mark>,
    pub ok: usize,
    pub upto: u64,
    pub done: bool,
    pub dirty: Option<u64>,
    /// Длина текста и его начало (за BOM).
    pub len: u64,
    pub start: u64,
    /// Шаг точек (тесты — мельче).
    pub step: u64,
}

impl Marks {
    pub fn new(start: u64, len: u64) -> Marks {
        Marks {
            v: vec![Mark {
                at: start,
                st: St::default(),
                depth: 0,
                min: 0,
            }],
            ok: 1,
            upto: start,
            done: len <= start,
            dirty: None,
            len,
            start,
            step: STEP,
        }
    }

    /// Последняя верная точка не дальше `pos`.
    pub fn last_le(&self, pos: u64) -> usize {
        self.v[..self.ok]
            .partition_point(|m| m.at <= pos)
            .saturating_sub(1)
    }

    /// Конец верной сводки куска `i` (для `i < ok`).
    pub fn chunk_end(&self, i: usize) -> u64 {
        if i + 1 < self.ok {
            self.v[i + 1].at
        } else if self.dirty.is_some() {
            self.v[i].at
        } else {
            self.upto
        }
    }

    /// Докуда точки и сводки верны.
    pub fn frontier(&self) -> u64 {
        self.chunk_end(self.ok - 1)
    }

    /// Всё посчитано: сводки верны до конца текста.
    pub fn complete(&self) -> bool {
        self.done && self.dirty.is_none()
    }

    /// Правка текста: точки после неё сдвигаются и становятся «под вопросом»; в начале текста,
    /// от которого зависит его первая строка (`Lang::head`), — все, кроме нулевой.
    pub fn edit(&mut self, e: &TextEdit, lang: Lang) {
        let shift = |x: u64| -> u64 {
            if x <= e.at {
                x
            } else if x >= e.at + e.old {
                x - e.old + e.new
            } else {
                e.at
            }
        };
        let idx = if e.at < self.start + lang.head() {
            0
        } else {
            self.v.partition_point(|m| m.at <= e.at).saturating_sub(1)
        };
        let mut keep = Vec::with_capacity(self.v.len());
        for (j, m) in self.v.iter().enumerate() {
            if j <= idx {
                keep.push(*m);
            } else if m.at >= e.at + e.old {
                keep.push(Mark {
                    at: m.at - e.old + e.new,
                    ..*m
                });
            }
        }
        self.v = keep;
        self.ok = self.ok.min(idx + 1).max(1);
        self.upto = shift(self.upto).max(self.v[self.v.len() - 1].at);
        self.len = self.len - e.old + e.new;
        let end = e.at + e.new;
        self.dirty = Some(self.dirty.map_or(end, |d| shift(d).max(end)));
        if self.upto > self.len {
            self.upto = self.len;
        }
    }
}

/// Чем кончился проход.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// Дошёл до конца текста.
    Done,
    /// Состояние совпало со старой точкой: дальше всё верно.
    Converged,
    /// Дошёл до `until`: продолжить позже.
    Paused,
}

/// Роли уровней по сводкам точек (ADR-0024 §2): уровень `hi` жив с точки `c`, если
/// минимум глубины с неё не ниже `hi`; верх стека точки его покрывает — роли оттуда;
/// иначе — кусок перед точкой, где глубина опускалась ниже `hi`: проход «только
/// глубина» по нему, последние открытия уровней.
pub(crate) struct MarksResolve<'a> {
    pub marks: &'a RefCell<Vec<Mark>>,
    /// Точка, с которой идёт текущий кусок (у окна — точка начала).
    pub last: &'a dyn Fn() -> usize,
    pub src: &'a Src<'a>,
    pub lang: Lang,
}

/// Уровней за раз не больше.
const BATCH: u64 = 1 << 15;

impl Resolve for MarksResolve<'_> {
    fn kinds(&mut self, hi: u64, min: u64) -> Vec<bool> {
        let v = self.marks.borrow();
        let mut c = (self.last)();
        let mut alive = min;
        loop {
            let m = v[c];
            alive = alive.min(m.depth);
            if hi <= alive {
                let n = u64::from(m.st.n).min(m.depth);
                if m.depth - n < hi && hi <= m.depth {
                    let lo = (m.depth - n).max(hi.saturating_sub(BATCH));
                    return (lo + 1..=hi)
                        .map(|lvl| m.st.top >> (m.depth - lvl) & 1 == 1)
                        .collect();
                }
            }
            if c == 0 {
                // Глубина у начала — 0: уровень `hi ≥ 1` открыт в куске 0.
                return vec![false];
            }
            let p = v[c - 1];
            if p.min < hi {
                let lo = p.min.max(hi.saturating_sub(BATCH));
                let end = m.at;
                drop(v);
                return last_opens(self.src, self.lang, &p, end, lo, hi);
            }
            alive = alive.min(p.min);
            c -= 1;
        }
    }
}

/// Последние открытия уровней `(lo, hi]` в куске `[p.at, end)`: роли снизу вверх.
fn last_opens(src: &Src<'_>, lang: Lang, p: &Mark, end: u64, lo: u64, hi: u64) -> Vec<bool> {
    struct Rec {
        lo: u64,
        kinds: Vec<bool>,
    }
    impl Out for Rec {
        fn open(&mut self, _at: u64, depth: u64, kind: bool) {
            if depth > self.lo
                && let Some(k) = self.kinds.get_mut((depth - self.lo - 1) as usize)
            {
                *k = kind;
            }
        }
    }
    let n = usize::try_from(hi - lo).unwrap_or(0);
    let mut rec = Rec {
        lo,
        kinds: vec![false; n],
    };
    let mut sc = Scanner::new(src, lang, p.at, p.st);
    let mut cx = Cx::new(Stack::at(p.depth, &p.st), &mut rec);
    if sc.run_to(end, &mut cx, &|| false).is_err() {
        return vec![false; n.max(1)];
    }
    drop(cx);
    if rec.kinds.is_empty() {
        rec.kinds.push(false);
    }
    rec.kinds
}

/// Проход от точки `v[ok - 1]` вперёд: новые точки через [`STEP`], сходимость со старыми
/// (после `dirty`), пауза на `until`. `stop` — отмена (`Err(Interrupted)`).
#[allow(
    clippy::too_many_lines,
    reason = "проход, сходимость и итог — одним циклом"
)]
pub(crate) fn advance(
    m: &mut Marks,
    src: &Src<'_>,
    lang: Lang,
    until: u64,
    stop: &dyn Fn() -> bool,
) -> io::Result<End> {
    let k = m.ok - 1;
    let start = m.v[k];
    let tent: Vec<Mark> = m.v.split_off(m.ok);
    let cell = RefCell::new(std::mem::take(&mut m.v));
    let last = std::cell::Cell::new(k);
    let get_last = || last.get();
    let mut res = MarksResolve {
        marks: &cell,
        last: &get_last,
        src,
        lang,
    };
    let mut out = NoOut;
    let mut sc = Scanner::new(src, lang, start.at, start.st);
    let mut cx = Cx::new(Stack::at(start.depth, &start.st), &mut out);
    cx.resolve = Some(&mut res);
    let dirty = m.dirty.unwrap_or(0);
    let mut ti = tent.partition_point(|t| t.at <= start.at);
    let every = m.step.max(1);
    let mut next = start.at + every;
    let result = loop {
        while ti < tent.len() && (tent[ti].at < sc.at || tent[ti].at < dirty) {
            ti += 1;
        }
        // За `until` — сразу до точки: по единице здесь читался бы весь кусок.
        let target = tent.get(ti).map_or(next, |t| t.at.min(next));
        let target = if until > sc.at {
            target.min(until)
        } else {
            target
        };
        if let Err(e) = sc.run_to(target, &mut cx, stop) {
            drop(cx);
            restore(m, cell.into_inner(), tent);
            return Err(e);
        }
        if sc.eof() {
            sc.flush(&mut cx);
            break End::Done;
        }
        if let Some(t) = tent.get(ti)
            && t.at == sc.at
            && sc.lex.can_mark()
            && sc.state(&cx) == t.st
            // Трекеры кода хранят номера уровней: сдвиг глубины их не чинит.
            && (!matches!(lang, Lang::Code(_)) || cx.depth() == t.depth)
        {
            let d = cx.depth();
            let mut v = cell.borrow_mut();
            if let Some(l) = v.last_mut() {
                l.min = l.min.min(cx.min);
            }
            // Глубина хранится разностями по смыслу: старые точки — со сдвигом.
            let delta = tent[ti].depth.wrapping_sub(d);
            for t in &tent[ti..] {
                v.push(Mark {
                    at: t.at,
                    st: t.st,
                    depth: t.depth.wrapping_sub(delta),
                    min: t.min.wrapping_sub(delta),
                });
            }
            break End::Converged;
        }
        if tent.get(ti).is_some_and(|t| t.at <= sc.at) {
            // На старой точке не сошлось: дальше.
            ti += 1;
        }
        if sc.at >= next || cx.dropped {
            if let Err(e) = sc.settle(&mut cx) {
                drop(cx);
                restore(m, cell.into_inner(), tent);
                return Err(e);
            }
            if sc.eof() {
                sc.flush(&mut cx);
                break End::Done;
            }
            let st = sc.state(&cx);
            let d = cx.depth();
            {
                let mut v = cell.borrow_mut();
                if let Some(l) = v.last_mut() {
                    l.min = l.min.min(cx.min);
                }
                v.push(Mark {
                    at: sc.at,
                    st,
                    depth: d,
                    min: d,
                });
                last.set(v.len() - 1);
            }
            cx.min = d;
            cx.dropped = false;
            next = sc.at + every;
            if sc.at >= until {
                break End::Paused;
            }
        }
    };
    let cur_min = cx.min;
    let at = sc.at;
    drop(cx);
    let mut v = cell.into_inner();
    match result {
        End::Done => {
            if let Some(l) = v.last_mut() {
                l.min = l.min.min(cur_min);
            }
            m.ok = v.len();
            m.v = v;
            m.upto = m.len;
            m.done = true;
            m.dirty = None;
        }
        End::Converged => {
            m.ok = v.len();
            m.v = v;
            m.dirty = None;
        }
        End::Paused => {
            m.ok = v.len();
            m.v = v;
            if m.dirty.is_some() {
                // Старые точки дальше — ещё пригодятся для сходимости.
                let from = tent.partition_point(|t| t.at <= at);
                m.v.extend_from_slice(&tent[from..]);
            } else {
                m.upto = at;
            }
        }
    }
    Ok(result)
}

/// Ошибка или отмена: точки как были.
fn restore(m: &mut Marks, v: Vec<Mark>, tent: Vec<Mark>) {
    m.v = v;
    m.ok = m.ok.min(m.v.len()).max(1);
    m.v.truncate(m.ok);
    m.v.extend(tent);
}

#[cfg(test)]
#[path = "marks_tests.rs"]
mod tests;
