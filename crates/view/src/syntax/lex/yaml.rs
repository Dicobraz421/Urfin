//! Трекер YAML (ADR-0024 §3): не красит — tree-sitter красит отрезок от строки ключа или
//! элемента. Трекер находит такие строки (`Out::line`: отступ и вид) и многострочное,
//! внутри которого строк нет: кавычки, поток `[`/`{`, блочный скаляр `|`/`>`, скаляр
//! документа (`--- |`). Свёртка — по отступам: строка ключа/элемента открывает уровень,
//! следующая строка с отступом не больше — закрывает.
//!
//! Строка копится до перевода строки (не больше [`CAP`] байт; у длинной — разбор по
//! началу, остальное — только кавычки и скобки потока).

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use super::{Cx, Lex, LineKind, Map, St};

/// Байт строки для разбора вида.
const CAP: usize = 64 << 10;

// Многострочное (`St::mode`).
const NO: u8 = 0;
const DQ: u8 = 1;
const SQ: u8 = 2;
/// Поток: `St::b` — глубина, `St::a` — кавычка внутри.
const FLOW: u8 = 3;
/// Блочный скаляр: `St::b` — отступ владельца.
const BLOCK: u8 = 4;
/// Скаляр документа: до маркера документа.
const ALL: u8 = 5;

pub(crate) struct Yaml {
    mode: u8,
    a: u8,
    b: u16,
    /// Открытые уровни свёртки по отступам (бит — отступ < 64).
    open: u64,
    /// Строка: байты и их позиции; переполнена — хвост не копится.
    line: Vec<u8>,
    raws: Vec<u64>,
    over: bool,
    /// Вид строки уже разобран (у длинной — по началу).
    done: bool,
}

fn sp(c: Option<&u8>) -> bool {
    c.is_none_or(|c| *c == b' ' || *c == b'\t')
}

fn doc_marker(l: &[u8]) -> bool {
    (l.starts_with(b"---") || l.starts_with(b"...")) && l.get(3).is_none_or(u8::is_ascii_whitespace)
}

fn skip_props(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b'&' || b[i] == b'!') {
        while i < b.len() && b[i] != b' ' {
            i += 1;
        }
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
    }
    i
}

/// Позиция `:` неявного ключа с `i` (ключ — простой или в кавычках, на этой строке).
fn key_colon(b: &[u8], i: usize) -> Option<usize> {
    let i = skip_props(b, i);
    if i >= b.len() {
        return None;
    }
    match b[i] {
        b'"' | b'\'' => {
            let q = b[i];
            let mut j = i + 1;
            while j < b.len() {
                if q == b'"' && b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if b[j] == q {
                    if q == b'\'' && b.get(j + 1) == Some(&b'\'') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            if j >= b.len() {
                return None;
            }
            let mut k = j + 1;
            while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            (b.get(k) == Some(&b':') && sp(b.get(k + 1))).then_some(k)
        }
        b'[' | b'{' | b'#' | b'|' | b'>' | b'*' | b'@' | b'`' => None,
        _ => {
            let mut j = i;
            while j < b.len() {
                if b[j] == b':' && sp(b.get(j + 1)) {
                    return Some(j);
                }
                if b[j] == b'#' && j > i && (b[j - 1] == b' ' || b[j - 1] == b'\t') {
                    return None;
                }
                j += 1;
            }
            None
        }
    }
}

impl Yaml {
    pub(crate) fn resume(st: St) -> Yaml {
        Yaml {
            mode: st.mode,
            a: st.a,
            b: st.b,
            open: st.x,
            line: Vec::new(),
            raws: Vec::new(),
            over: false,
            done: false,
        }
    }

    /// Многострочное с `i` строки `b` (режим уже стоит): до конца строки или закрытия.
    fn scan(&mut self, b: &[u8], mut i: usize) {
        while i < b.len() {
            let c = b[i];
            match self.mode {
                DQ => {
                    if c == b'\\' {
                        i += 2;
                        continue;
                    }
                    if c == b'"' {
                        self.mode = NO;
                        return;
                    }
                }
                SQ => {
                    if c == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        self.mode = NO;
                        return;
                    }
                }
                FLOW => {
                    if self.a != 0 {
                        if self.a == b'"' && c == b'\\' {
                            i += 2;
                            continue;
                        }
                        if c == self.a {
                            if self.a == b'\'' && b.get(i + 1) == Some(&b'\'') {
                                i += 2;
                                continue;
                            }
                            self.a = 0;
                        }
                    } else {
                        match c {
                            b'"' | b'\'' => self.a = c,
                            b'[' | b'{' => self.b = self.b.saturating_add(1),
                            b']' | b'}' => {
                                self.b = self.b.saturating_sub(1);
                                if self.b == 0 {
                                    self.mode = NO;
                                    return;
                                }
                            }
                            b'#' if i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t' => return,
                            _ => {}
                        }
                    }
                }
                _ => return,
            }
            i += 1;
        }
    }

    /// Значение с `i` строки: начинает ли многострочное.
    fn value(&mut self, b: &[u8], i: usize, base: usize) {
        let i = skip_props(b, i);
        if i >= b.len() {
            return;
        }
        match b[i] {
            b'"' => {
                self.mode = DQ;
                self.scan(b, i + 1);
            }
            b'\'' => {
                self.mode = SQ;
                self.scan(b, i + 1);
            }
            b'[' | b'{' => {
                self.mode = FLOW;
                self.b = 1;
                self.a = 0;
                self.scan(b, i + 1);
            }
            b'|' | b'>' => {
                self.mode = BLOCK;
                self.b = u16::try_from(base).unwrap_or(u16::MAX);
            }
            _ => {}
        }
    }

    /// Уровни свёртки: закрыть с отступом ≥ `ind`, открыть `ind` (`open` — открыть).
    fn level(&mut self, ind: usize, at: u64, open: bool, cx: &mut Cx<'_>) {
        let keep = if ind >= 64 {
            u64::MAX
        } else {
            (1u64 << ind) - 1
        };
        let mut closing = self.open & !keep;
        while closing != 0 {
            let top = closing.ilog2();
            cx.pop(at);
            closing &= !(1 << top);
        }
        self.open &= keep;
        if open && ind < 64 {
            cx.push(at, false);
            self.open |= 1 << ind;
        }
    }

    /// Разбор начала строки (вид, уровни, многострочное).
    fn classify(&mut self, cx: &mut Cx<'_>) {
        let b = std::mem::take(&mut self.line);
        let raws = std::mem::take(&mut self.raws);
        self.classify_line(&b, &raws, cx);
        self.line = b;
        self.raws = raws;
        self.done = true;
    }

    fn classify_line(&mut self, b: &[u8], raws: &[u64], cx: &mut Cx<'_>) {
        let Some(&start) = raws.first() else {
            return;
        };
        let b = b.strip_suffix(b"\r").unwrap_or(b);
        let ind = b.iter().take_while(|&&c| c == b' ').count();
        let blank = b[ind..].iter().all(u8::is_ascii_whitespace);
        match self.mode {
            BLOCK => {
                if blank || ind > usize::from(self.b) {
                    return;
                }
                self.mode = NO;
            }
            ALL => {
                if !doc_marker(b) {
                    return;
                }
                self.mode = NO;
            }
            NO => {}
            _ => {
                self.scan(b, 0);
                return;
            }
        }
        if blank || b[ind] == b'#' {
            return;
        }
        let at = raws[ind.min(raws.len() - 1)];
        if ind == 0 && doc_marker(b) {
            self.level(0, start, false, cx);
            cx.out.line(start, 0, LineKind::Doc);
            let mut r = 3;
            while r < b.len() && (b[r] == b' ' || b[r] == b'\t') {
                r += 1;
            }
            if r < b.len() && b[r] != b'#' {
                if b[r] == b'|' || b[r] == b'>' {
                    self.mode = ALL;
                } else {
                    self.value(b, r, 0);
                    if self.mode == BLOCK {
                        self.mode = ALL;
                    }
                }
            }
            return;
        }
        let c = b[ind];
        let kind = if c == b'-' && sp(b.get(ind + 1)) {
            LineKind::Item
        } else if c == b'?' && sp(b.get(ind + 1)) {
            LineKind::Key
        } else if c == b':' && sp(b.get(ind + 1)) {
            LineKind::QVal
        } else if c == b'%' && ind == 0 {
            LineKind::Other
        } else if key_colon(b, ind).is_some() {
            LineKind::Key
        } else {
            LineKind::Other
        };
        self.level(ind, at, true, cx);
        #[allow(clippy::cast_possible_truncation, reason = "отступ строки ≤ CAP")]
        cx.out.line(start, ind as u32, kind);
        // К значению: `- `, `? `, `: ` и `ключ: `.
        let mut i = ind;
        let mut base = ind;
        loop {
            if i < b.len() && matches!(b[i], b'-' | b'?' | b':') && sp(b.get(i + 1)) {
                base = i;
                i += 1;
                while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                continue;
            }
            if i < b.len()
                && let Some(k) = key_colon(b, i)
            {
                base = i;
                i = k + 1;
                while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                continue;
            }
            break;
        }
        self.value(b, i, base);
    }

    /// Хвост длинной строки: только кавычки и поток.
    fn tail(&mut self, b: &[u8]) {
        if matches!(self.mode, DQ | SQ | FLOW) {
            self.scan(b, 0);
        }
    }
}

impl Lex for Yaml {
    fn state(&self) -> St {
        St {
            mode: self.mode,
            a: self.a,
            b: self.b,
            x: self.open,
            ..St::default()
        }
    }

    fn can_mark(&self) -> bool {
        self.line.is_empty() && !self.over
    }

    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        let mut i = 0;
        while i < b.len() {
            let j = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |k| i + k);
            let piece = &b[i..j];
            if self.over {
                self.tail(piece);
            } else {
                let room = CAP - self.line.len();
                let take = piece.len().min(room);
                self.line.extend_from_slice(&piece[..take]);
                self.raws.extend((i..i + take).map(|k| m.raw(k)));
                if take < piece.len() {
                    self.over = true;
                    self.classify(cx);
                    self.tail(&piece[take..]);
                }
            }
            if j < b.len() {
                if !self.done {
                    if self.line.is_empty() {
                        // Пустая строка: позиция — сам перевод строки.
                        self.raws.push(m.raw(j));
                        self.line.push(b'\n');
                        self.line.pop();
                    }
                    self.classify(cx);
                }
                self.line.clear();
                self.raws.clear();
                self.over = false;
                self.done = false;
                i = j + 1;
            } else {
                i = j;
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        if eof {
            if !self.done && !self.line.is_empty() {
                self.classify(cx);
            }
            self.line.clear();
            self.raws.clear();
            self.level(0, end, false, cx);
        }
    }
}
