//! Трекер Python (ADR-0024 §8): блоки — по отступам, как внешний сканер `tree-sitter-python`
//! (табуляция — до 8, `\f` — сброс), строки всех видов, f-строки с вложенными выражениями.
//! Кандидат — первая строка логической строки на своём уровне, кроме `else`, `elif`,
//! `except`, `finally` и строки после декоратора; в `[…]` и `{…}` — строка после запятой.

#![allow(
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт; флаги — состояние"
)]

use super::{
    L_BRACKET, L_GROUP, L_INTERP, L_JX_ATTR, L_LIST, L_PAREN, L_ZONE, Track, pk, word_key,
};
use crate::syntax::lex::{Cx, Lex, Map, St};

const NORMAL: u8 = 0;
const WORD: u8 = 1;
const COMMENT: u8 = 2;
/// Считаем открывающие кавычки.
const Q_OPEN: u8 = 3;
const STR: u8 = 4;
const STR_ESC: u8 = 5;
const STR_ESC_CR: u8 = 6;
/// Считаем закрывающие кавычки длинной строки.
const Q_END: u8 = 7;
/// `{` в f-строке: `{{` — текст.
const F_BRACE: u8 = 8;
/// Формат после `:` в выражении f-строки.
const F_SPEC: u8 = 9;
/// `\` вне строки.
const BS: u8 = 10;
const BS_CR: u8 = 11;

// Флаги строки.
const S_APOS: u8 = 1;
const S_LONG: u8 = 2;
const S_FMT: u8 = 4;

/// Уровней отступа в состоянии.
const IND_MAX: usize = 16;

pub(crate) struct Py {
    t: Track,
    mode: u8,
    /// Начало строки: значимых токенов на ней не было.
    first: bool,
    /// Единиц от начала строки (пока `first`).
    col: u16,
    /// Отступ строки (табуляция — до 8).
    ind: u16,
    /// Запятая в конце прошлого токена.
    comma: bool,
    /// Логическая строка кончилась: первый токен — новая логическая строка.
    fresh: bool,
    /// Прошлая логическая строка — декоратор.
    deco: bool,
    /// Текущая логическая строка начата с `@`.
    is_deco: bool,
    /// В элементе списка было `lambda` или `for`: запятая ещё не конец элемента.
    open_item: bool,
    /// Отступы уровней блока (по 8 бит), `n_ind` — сколько.
    cols: [u64; 2],
    n_ind: u8,
    /// Отступ глубже памяти.
    over: bool,
    /// Начало логической строки.
    log_at: u64,
    /// Флаги строки и счётчик кавычек.
    sf: u8,
    k: u32,
    /// Многострочная лексема: уровень открыт.
    zone: bool,
    /// Флаги строк под открытыми f-выражениями (по 4 бита).
    fs: u64,
    word: u64,
    wlen: u32,
    adj: bool,
    last: u64,
    num: bool,
    back_log: Option<u64>,
}

const fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Префикс строки: только `r`, `b`, `u`, `f`, `t`, до трёх букв. Итог: `Some(формат)`.
fn str_prefix(w: u64, len: u32) -> Option<bool> {
    if len == 0 || len > 3 {
        return None;
    }
    let mut fmt = false;
    let mut v = w;
    for _ in 0..len {
        #[allow(clippy::cast_possible_truncation, reason = "младший байт")]
        let c = (v & 0xFF) as u8;
        v >>= 8;
        match c | 0x20 {
            b'f' | b't' => fmt = true,
            b'r' | b'b' | b'u' => {}
            _ => return None,
        }
    }
    Some(fmt)
}

impl Py {
    pub(crate) fn resume(st: St) -> Py {
        let (cols, n) = (st.ext.kinds2, st.ext.pfx);
        Py {
            t: Track::resume(&st),
            mode: st.mode,
            first: st.d & 1 == 0,
            col: st.b,
            ind: st.ext.rh as u16,
            comma: st.d & 2 != 0,
            fresh: st.d & 4 == 0,
            deco: st.d & 8 != 0,
            is_deco: st.d & 16 != 0,
            open_item: st.d & 32 != 0,
            cols,
            n_ind: n,
            over: st.d & 64 != 0,
            log_at: 0,
            sf: st.a,
            k: st.c,
            zone: st.d & 128 != 0,
            fs: st.ext.fs,
            word: 0,
            wlen: 0,
            adj: false,
            last: 0,
            num: false,
            back_log: Some(st.ext.hash.into()),
        }
    }

    fn col_of(&self, i: usize) -> u16 {
        let i = i % IND_MAX;
        #[allow(clippy::cast_possible_truncation, reason = "8 бит")]
        {
            (self.cols[i / 8] >> (i % 8 * 8) & 0xFF) as u16
        }
    }

    fn set_col(&mut self, i: usize, v: u16) {
        let i = i % IND_MAX;
        let s = i % 8 * 8;
        self.cols[i / 8] = self.cols[i / 8] & !(0xFF << s) | u64::from(v.min(255)) << s;
    }

    /// Закрыты скобки: перевод строки кончает логическую строку.
    fn in_group(&self, cx: &Cx<'_>) -> bool {
        cx.depth() > u64::from(self.n_ind)
    }

    /// Новая логическая строка с отступом `self.ind`.
    fn block(&mut self, cx: &mut Cx<'_>) {
        let at = self.t.line_at;
        self.over = false;
        let top = if self.n_ind == 0 {
            0
        } else {
            self.col_of(usize::from(self.n_ind) - 1)
        };
        if self.ind > top {
            if usize::from(self.n_ind) < IND_MAX && self.ind < 256 {
                self.set_col(usize::from(self.n_ind), self.ind);
                self.n_ind += 1;
                self.t.push(cx, self.log_at, at, L_GROUP);
            } else {
                self.over = true;
            }
            return;
        }
        while self.n_ind > 0 && self.col_of(usize::from(self.n_ind) - 1) > self.ind {
            self.n_ind -= 1;
            self.t.pop(cx, at);
        }
    }

    /// Значимый токен. `w` — слово (0 — иначе), `c` — байт.
    fn lead(&mut self, w: u64, c: u8, cx: &mut Cx<'_>) {
        const ELSE: u64 = pk("else");
        const ELIF: u64 = pk("elif");
        const EXCEPT: u64 = pk("except");
        const FINALLY: u64 = pk("finally");
        let first = std::mem::take(&mut self.first);
        if first && self.fresh {
            self.fresh = false;
            self.block(cx);
            let cont = matches!(w, ELSE | ELIF | EXCEPT | FINALLY);
            let line = self.t.line_at;
            self.log_at = line;
            let deco = std::mem::take(&mut self.deco);
            self.is_deco = c == b'@';
            if !cont && !deco && !self.over && self.t.bad == 0 {
                self.t.cand(line, cx);
                self.open_item = false;
            }
        } else if first && self.comma && self.t.bad == 0 && !self.open_item {
            let top = self.t.top_of(cx);
            if matches!(top, L_BRACKET | L_LIST) && !matches!(c, b')' | b']' | b'}') {
                self.t.cand(self.t.line_at, cx);
            }
        }
        self.comma = false;
    }

    fn newline(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        if !self.in_group(cx) && !self.first {
            self.fresh = true;
            if self.is_deco {
                self.deco = true;
                self.is_deco = false;
            }
        }
        self.first = true;
        self.col = 0;
        self.ind = 0;
        self.t.line_at = m.raw(i + 1);
        self.adj = false;
    }

    fn normal(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        let c = b[i];
        match c {
            b'\n' => {
                self.newline(i, m, cx);
                i + 1
            }
            b' ' | b'\t' | b'\r' | 0x0C => {
                if self.first {
                    self.col = self.col.saturating_add(1);
                    self.ind = match c {
                        b' ' => self.ind.saturating_add(1),
                        b'\t' => self.ind.saturating_add(8),
                        0x0C => 0,
                        _ => self.ind,
                    };
                    if self.col == u16::MAX {
                        self.first = false;
                    }
                }
                self.adj = false;
                i + 1
            }
            b'#' => {
                self.mode = COMMENT;
                self.adj = false;
                i + 1
            }
            b'\\' => {
                self.mode = BS;
                self.adj = false;
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                i + 1
            }
            b'"' | b'\'' => {
                self.lead(0, c, cx);
                self.sf = if c == b'\'' { S_APOS } else { 0 };
                if self.adj && str_prefix(self.last, self.wlen) == Some(true) {
                    self.sf |= S_FMT;
                }
                self.adj = false;
                self.k = 1;
                self.mode = Q_OPEN;
                i + 1
            }
            _ if is_word(c) => {
                self.mode = WORD;
                self.word = 0;
                self.wlen = 0;
                self.num = c.is_ascii_digit();
                i
            }
            _ => {
                self.lead(0, c, cx);
                self.adj = false;
                self.punct(c, i, m, cx);
                i + 1
            }
        }
    }

    fn punct(&mut self, c: u8, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        fn brace(k: u8) -> bool {
            matches!(k, L_LIST | L_INTERP | L_JX_ATTR)
        }
        fn group(k: u8) -> bool {
            matches!(k, L_PAREN | L_BRACKET)
        }
        let (at, end) = (m.raw(i), m.raw(i + 1));
        match c {
            b'(' => self.t.push(cx, at, end, L_PAREN),
            b'[' => {
                self.t.push(cx, at, end, L_BRACKET);
                self.open_item = false;
            }
            b'{' => {
                self.t.push(cx, at, end, L_LIST);
                self.open_item = false;
            }
            b')' => {
                self.t.close(cx, at, |k| k == L_PAREN, group);
            }
            b']' => {
                self.t.close(cx, at, |k| k == L_BRACKET, group);
            }
            b'}' => match self.t.close(cx, at, brace, group) {
                Some(L_INTERP) => self.leave(),
                Some(L_JX_ATTR) => self.mode = F_SPEC,
                _ => {}
            },
            b',' => {
                if matches!(self.t.top_of(cx), L_BRACKET | L_LIST) {
                    self.comma = true;
                }
            }
            b':' => {
                let top = self.t.top_of(cx);
                if matches!(top, L_INTERP | L_JX_ATTR) {
                    self.mode = F_SPEC;
                }
                self.open_item = false;
            }
            _ => {}
        }
    }

    /// Выражение f-строки закрыто: обратно в строку.
    fn leave(&mut self) {
        #[allow(clippy::cast_possible_truncation, reason = "4 бита")]
        {
            self.sf = (self.fs & 0xF) as u8;
        }
        self.fs >>= 4;
        self.mode = STR;
    }

    /// Выражение f-строки открыто: `{` в байте `i`.
    fn enter(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>, kind: u8) {
        if kind == L_INTERP {
            self.fs = self.fs << 4 | u64::from(self.sf & 0xF);
        }
        self.t.push(cx, m.raw(i), m.raw(i + 1), kind);
        self.mode = NORMAL;
    }

    fn zone_on(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        if !self.zone {
            self.zone = true;
            let at = m.raw(i);
            self.t.push(cx, at, at, L_ZONE);
        }
    }

    fn zone_off(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        if self.zone {
            self.zone = false;
            self.t.pop(cx, m.raw(i));
        }
    }

    fn string(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        let q = if self.sf & S_APOS != 0 { b'\'' } else { b'"' };
        let fmt = self.sf & S_FMT != 0;
        let mut j = i;
        while j < b.len() {
            let Some(k) = memchr::memchr3(q, b'\\', b'\n', &b[j..]) else {
                if fmt {
                    return match memchr::memchr(b'{', &b[j..]) {
                        Some(k) => {
                            self.mode = F_BRACE;
                            j + k + 1
                        }
                        None => b.len(),
                    };
                }
                return b.len();
            };
            let e = j + k;
            if fmt && let Some(k2) = memchr::memchr(b'{', &b[j..e]) {
                self.mode = F_BRACE;
                return j + k2 + 1;
            }
            let c = b[e];
            if c == q {
                if self.sf & S_LONG != 0 {
                    self.k = 1;
                    self.mode = Q_END;
                } else {
                    self.mode = NORMAL;
                }
                return e + 1;
            }
            if c == b'\\' {
                self.mode = STR_ESC;
                return e + 1;
            }
            if self.sf & S_LONG != 0 {
                self.zone_on(e, m, cx);
                j = e + 1;
            } else {
                // Незакрытая строка кончается на переводе строки.
                self.mode = NORMAL;
                return e;
            }
        }
        b.len()
    }
}

impl Lex for Py {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let mut st = St {
            mode: self.mode,
            a: self.sf,
            b: self.col,
            c: self.k,
            d: u8::from(!self.first)
                | u8::from(self.comma) << 1
                | u8::from(!self.fresh) << 2
                | u8::from(self.deco) << 3
                | u8::from(self.is_deco) << 4
                | u8::from(self.open_item) << 5
                | u8::from(self.over) << 6
                | u8::from(self.zone) << 7,
            ..St::default()
        };
        st.ext.rh = u32::from(self.ind);
        st.ext.kinds2 = self.cols;
        st.ext.pfx = self.n_ind;
        st.ext.fs = self.fs;
        st.ext.hash = u32::try_from(at.saturating_sub(self.log_at)).unwrap_or(u32::MAX);
        if let Some(b) = self.back_log {
            st.ext.hash = u32::try_from(b).unwrap_or(u32::MAX);
        }
        self.t.save(&mut st, at);
        st
    }

    fn can_mark(&self) -> bool {
        self.mode != WORD
    }

    #[allow(clippy::too_many_lines, reason = "автомат: режимы одним match")]
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        const LAMBDA: u64 = pk("lambda");
        const FOR: u64 = pk("for");
        if b.is_empty() {
            return;
        }
        let at0 = m.raw(0);
        self.t.begin(at0, m.unit());
        if let Some(back) = self.back_log.take() {
            self.log_at = at0.saturating_sub(back);
        }
        let mut i = 0;
        while i < b.len() {
            i = match self.mode {
                NORMAL => self.normal(b, i, m, cx),
                WORD => {
                    let mut j = i;
                    while j < b.len() && is_word(b[j]) {
                        self.word = self.word << 8 | u64::from(b[j]);
                        self.wlen = self.wlen.saturating_add(1);
                        j += 1;
                    }
                    if j < b.len() {
                        let w = word_key(self.word, self.wlen);
                        self.lead(w, 0, cx);
                        self.last = if self.num { 0 } else { self.word };
                        self.adj = true;
                        self.mode = NORMAL;
                        if w == LAMBDA || w == FOR {
                            self.open_item = true;
                        }
                    }
                    j
                }
                COMMENT => match memchr::memchr(b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = NORMAL;
                        i + k
                    }
                },
                BS => {
                    let c = b[i];
                    if c == b'\r' {
                        self.mode = BS_CR;
                        i + 1
                    } else if c == b'\n' {
                        self.first = false;
                        self.mode = NORMAL;
                        i + 1
                    } else {
                        self.lead(0, b'\\', cx);
                        self.mode = NORMAL;
                        i
                    }
                }
                BS_CR => {
                    self.first = false;
                    self.mode = NORMAL;
                    if b[i] == b'\n' { i + 1 } else { i }
                }
                Q_OPEN => {
                    let q = if self.sf & S_APOS != 0 { b'\'' } else { b'"' };
                    if b[i] == q {
                        self.k += 1;
                        if self.k == 3 {
                            self.sf |= S_LONG;
                            self.mode = STR;
                        }
                        i + 1
                    } else {
                        // `""` — пустая строка.
                        self.mode = if self.k == 2 { NORMAL } else { STR };
                        i
                    }
                }
                STR => self.string(b, i, m, cx),
                STR_ESC => {
                    self.mode = if b[i] == b'\r' { STR_ESC_CR } else { STR };
                    i + 1
                }
                STR_ESC_CR => {
                    self.mode = STR;
                    if b[i] == b'\n' { i + 1 } else { i }
                }
                Q_END => {
                    let q = if self.sf & S_APOS != 0 { b'\'' } else { b'"' };
                    if b[i] == q {
                        self.k += 1;
                        if self.k == 3 {
                            self.zone_off(i, m, cx);
                            self.mode = NORMAL;
                        }
                        i + 1
                    } else {
                        self.mode = STR;
                        i
                    }
                }
                F_BRACE => {
                    if b[i] == b'{' {
                        self.mode = STR;
                        i + 1
                    } else {
                        self.enter(i, m, cx, L_INTERP);
                        i
                    }
                }
                F_SPEC => match memchr::memchr2(b'{', b'}', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'{' {
                            self.enter(j, m, cx, L_JX_ATTR);
                        } else {
                            let at = m.raw(j);
                            match self.t.close(
                                cx,
                                at,
                                |k| matches!(k, L_INTERP | L_JX_ATTR),
                                |_| false,
                            ) {
                                Some(L_JX_ATTR) => self.mode = F_SPEC,
                                _ => self.leave(),
                            }
                        }
                        j + 1
                    }
                },
                _ => {
                    self.mode = NORMAL;
                    i
                }
            };
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        if !eof {
            return;
        }
        if self.mode == WORD {
            let w = word_key(self.word, self.wlen);
            self.lead(w, 0, cx);
            self.mode = NORMAL;
        }
        // Конец текста закрывает блоки.
        while self.n_ind > 0 && cx.depth() > 0 {
            self.n_ind -= 1;
            self.t.pop(cx, end);
        }
    }
}
