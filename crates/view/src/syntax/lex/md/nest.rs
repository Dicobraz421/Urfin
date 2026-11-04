//! Свёртка списков и цитат Markdown (решение 2026-11-03): контейнеры `CommonMark` —
//! цитаты `>` и пункты списков любого уровня, а также список верхнего уровня целиком.
//! Только события структуры (`Cx::push`/`pop`), подсветку и безопасные строки не трогает.
//!
//! Уровень открывается на маркере (`>`, `-`, `1.`), закрывается на первой строке, которая
//! его не продолжает (пункт — отступ меньше колонки содержимого, цитата — нет `>`; пустая
//! строка пункт продолжает, цитату — нет; ленивое продолжение абзаца — продолжает всё).
//! Список верхнего уровня (вне контейнеров) — уровень под первым пунктом: тянется, пока
//! следующий пункт верхнего уровня того же типа. Пункт «в одну строку» места свёртки не
//! даёт (`win::Folds` — только больше строки).
//!
//! Состояние — в точке: до [`DEPTH`] контейнеров (глубже — текст), колонка пункта ≤
//! [`COL_MAX`], длина ограждения — до [`FLEN_MAX`].

use super::{Cx, fence_open, heading, html_kind, list_marker, thematic};

/// Контейнеров в состоянии точки (по байту в `St::x`, биты 8..64).
const DEPTH: usize = 7;
/// Колонка содержимого пункта (7 бит).
const COL_MAX: usize = 127;
/// Длина ограждения в состоянии (6 бит): длиннее — как эта.
const FLEN_MAX: usize = 63;
/// Байт контейнера: цитата (иначе — колонка содержимого пункта).
const QUOTE: u8 = 0x80;

/// Контейнеры строки.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Nest {
    /// Снизу вверх.
    v: [u8; DEPTH],
    n: u8,
    /// Тип маркера открытого списка верхнего уровня (`list_type`; 0 — нет).
    list: u8,
    /// Открыт абзац (возможно ленивое продолжение; порядковый не с 1 и пустой пункт его
    /// не прерывают).
    para: bool,
    /// Ограждение внутри контейнера: символ (0 — нет) и длина.
    fence: u8,
    flen: u8,
}

/// Позиция в строке: байт и колонка (табуляция — до кратного 4).
#[derive(Copy, Clone)]
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
    col: usize,
}

impl Cur<'_> {
    /// Пробелы с текущей позиции: колонки и байты.
    fn ws(&self) -> (usize, usize) {
        let mut c = self.col;
        let mut k = self.i;
        while let Some(&x) = self.b.get(k) {
            match x {
                b' ' => c += 1,
                b'\t' => c = (c / 4 + 1) * 4,
                _ => break,
            }
            k += 1;
        }
        (c - self.col, k - self.i)
    }

    fn rest(&self) -> &[u8] {
        &self.b[self.i.min(self.b.len())..]
    }

    fn blank(&self) -> bool {
        self.rest().iter().all(u8::is_ascii_whitespace)
    }

    /// Вперёд на `n` байт.
    fn adv(&mut self, n: usize) {
        for _ in 0..n {
            match self.b.get(self.i) {
                Some(b'\t') => self.col = (self.col / 4 + 1) * 4,
                Some(_) => self.col += 1,
                None => return,
            }
            self.i += 1;
        }
    }
}

/// Тип маркера: 1–3 — `-` `+` `*`, 4–5 — `.` `)`.
fn list_type(t: &[u8], mw: usize, ord: bool) -> u8 {
    if ord {
        if t.get(mw - 1) == Some(&b')') { 5 } else { 4 }
    } else {
        match t[0] {
            b'-' => 1,
            b'+' => 2,
            _ => 3,
        }
    }
}

/// Новый контейнер на строке.
enum Open {
    Quote,
    /// Пункт: тип маркера, ширина маркера с пробелами, колонка содержимого.
    Item {
        ty: u8,
        len: usize,
        col: usize,
    },
}

impl Nest {
    pub(super) fn unpack(x: u64, c: u32) -> Nest {
        let mut v = [0u8; DEPTH];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (x >> (8 + 8 * i) & 0xFF) as u8;
        }
        let c = c >> 16;
        Nest {
            v,
            n: (c & 7) as u8,
            list: (c >> 3 & 7) as u8,
            para: c >> 6 & 1 == 1,
            fence: match c >> 7 & 3 {
                1 => b'`',
                2 => b'~',
                _ => 0,
            },
            flen: (c >> 9 & 0x3F) as u8,
        }
    }

    /// В состояние: биты 8..64 `St::x` и 16..32 `St::c`.
    pub(super) fn pack(&self) -> (u64, u32) {
        let mut x = 0u64;
        for (i, &b) in self.v.iter().enumerate().take(usize::from(self.n)) {
            x |= u64::from(b) << (8 + 8 * i);
        }
        let f = match self.fence {
            b'`' => 1u32,
            b'~' => 2,
            _ => 0,
        };
        let c = u32::from(self.n)
            | u32::from(self.list) << 3
            | u32::from(self.para) << 6
            | f << 7
            | u32::from(self.flen) << 9;
        (x, c << 16)
    }

    /// Открыт абзац (после прошлой строки).
    pub(super) fn para(&self) -> bool {
        self.para
    }

    /// Закрыть все уровни на `at` (заголовок раздела, ограждение или HTML верхнего
    /// уровня, конец текста).
    pub(super) fn reset(&mut self, at: u64, cx: &mut Cx<'_>) {
        for _ in 0..self.n {
            cx.pop(at);
        }
        if self.list != 0 {
            cx.pop(at);
        }
        *self = Nest::default();
    }

    /// Сколько уровней снизу строка продолжает; `cur` — за их маркерами и отступами.
    fn continued(&self, cur: &mut Cur<'_>) -> usize {
        let mut m = 0;
        while m < usize::from(self.n) {
            let c = self.v[m];
            if c & QUOTE != 0 {
                let (wc, wb) = cur.ws();
                if wc >= 4 || cur.b.get(cur.i + wb) != Some(&b'>') {
                    break;
                }
                cur.adv(wb + 1);
                if matches!(cur.b.get(cur.i), Some(b' ' | b'\t')) {
                    cur.adv(1);
                }
            } else if !cur.blank() {
                let w = usize::from(c);
                let (wc, _) = cur.ws();
                if cur.col + wc < w {
                    break;
                }
                while cur.col < w && cur.i < cur.b.len() {
                    cur.adv(1);
                }
            }
            m += 1;
        }
        m
    }

    /// Строка `b` (без перевода; позиции байтов — `raws`).
    pub(super) fn line(&mut self, b: &[u8], raws: &[u64], cx: &mut Cx<'_>) {
        if raws.is_empty() {
            return;
        }
        let pos = |i: usize| raws[i.min(raws.len() - 1)];
        let first = b
            .iter()
            .position(|c| *c != b' ' && *c != b'\t')
            .unwrap_or(b.len());
        let line_at = pos(first);
        let mut cur = Cur { b, i: 0, col: 0 };
        let n = usize::from(self.n);
        let m = self.continued(&mut cur);
        let blank = cur.blank();
        if self.fence != 0 {
            if m == n {
                let (wc, wb) = cur.ws();
                let t = &cur.rest()[wb..];
                let k = t.iter().take_while(|&&c| c == self.fence).count();
                if wc < 4
                    && k >= usize::from(self.flen)
                    && t[k..].iter().all(u8::is_ascii_whitespace)
                {
                    self.fence = 0;
                    self.flen = 0;
                }
                self.para = false;
                return;
            }
            // Ограждение закрывается с контейнером; ленивого продолжения у кода нет.
            self.fence = 0;
            self.flen = 0;
            self.para = false;
        }
        // Ленивое продолжение абзаца: все уровни открыты.
        if m < n && self.para && !blank && !Self::interrupts(cur) {
            return;
        }
        for _ in m..n {
            cx.pop(line_at);
        }
        self.n = u8::try_from(m).unwrap_or(0);
        // Абзац может продолжиться (и его прервать) — только если уровни не менялись.
        let mut para = self.para && m == n;
        loop {
            let open = if usize::from(self.n) < DEPTH {
                Self::open_at(cur, para)
            } else {
                None
            };
            // Список верхнего уровня продолжает только пункт того же типа.
            if self.n == 0
                && self.list != 0
                && !matches!(open, Some(Open::Item { ty, .. }) if ty == self.list)
            {
                cx.pop(line_at);
                self.list = 0;
            }
            let Some(open) = open else {
                break;
            };
            let (_, wb) = cur.ws();
            cur.adv(wb);
            let at = pos(cur.i);
            match open {
                Open::Quote => {
                    cur.adv(1);
                    if matches!(b.get(cur.i), Some(b' ' | b'\t')) {
                        cur.adv(1);
                    }
                    cx.push(at, false);
                    self.v[usize::from(self.n)] = QUOTE;
                }
                Open::Item { ty, len, col } => {
                    if self.n == 0 && self.list == 0 {
                        cx.push(at, false);
                        self.list = ty;
                    }
                    cur.adv(len);
                    cx.push(at, false);
                    #[allow(clippy::cast_possible_truncation, reason = "col ≤ COL_MAX")]
                    {
                        self.v[usize::from(self.n)] = col as u8;
                    }
                }
            }
            self.n += 1;
            para = false;
        }
        // Содержимое строки.
        let (wc, wb) = cur.ws();
        let t = &cur.rest()[wb..];
        if t.iter().all(u8::is_ascii_whitespace) {
            self.para = false;
        } else if wc >= 4 {
            // Отступ кода — или продолжение абзаца.
            self.para = para;
        } else if let Some((c, k)) = fence_open(t) {
            self.fence = c;
            self.flen = u8::try_from(k.min(FLEN_MAX)).unwrap_or(3);
            self.para = false;
        } else {
            self.para = heading(t).is_none() && !thematic(t);
        }
    }

    /// Строка с позиции `cur` начинает блок (прерывает абзац, ленивым продолжением не
    /// бывает).
    fn interrupts(cur: Cur<'_>) -> bool {
        let (wc, wb) = cur.ws();
        let t = &cur.rest()[wb..];
        if wc >= 4 || t.is_empty() {
            return false;
        }
        if t[0] == b'>' || heading(t).is_some() || thematic(t) || fence_open(t).is_some() {
            return true;
        }
        // Уровень выше не продолжен: абзац не последний открытый, пункт может быть любым.
        if Self::open_at(cur, false).is_some() {
            return true;
        }
        !matches!(html_kind(t), 0 | 7)
    }

    /// Контейнер, который начинается с `cur`; `para` — открыт абзац (его прерывают не
    /// все пункты).
    fn open_at(cur: Cur<'_>, para: bool) -> Option<Open> {
        let (wc, wb) = cur.ws();
        let t = &cur.rest()[wb..];
        if wc >= 4 {
            return None;
        }
        if t.first() == Some(&b'>') {
            return Some(Open::Quote);
        }
        if thematic(t) {
            return None;
        }
        let (mw, ord, one) = list_marker(t)?;
        let after = &t[mw..];
        let empty = after.iter().all(u8::is_ascii_whitespace);
        if para && (empty || (ord && !one)) {
            return None;
        }
        // Пробелы после маркера: 1–4; больше (код в пункте) или пусто — один.
        let (sw, sb) = Cur {
            b: cur.b,
            i: cur.i + wb + mw,
            col: cur.col + wc + mw,
        }
        .ws();
        let (sp, len) = if empty {
            (1, mw)
        } else if sw > 4 {
            (1, mw + 1)
        } else {
            (sw, mw + sb)
        };
        let col = cur.col + wc + mw + sp;
        (col <= COL_MAX).then(|| Open::Item {
            ty: list_type(t, mw, ord),
            len,
            col,
        })
    }
}
