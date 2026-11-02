//! TOML (`tree-sitter-toml-ng` + `assets/syntax/toml/highlights.scm`): заголовок таблицы
//! `[…]`/`[[…]]` целиком — `keyword`, строка значения — `string`, числа, даты, `true`/
//! `false`, `inf`/`nan` — `number`, `#` — `comment`; ключи (и в кавычках) — текст.
//!
//! Структура: таблица — уровень (до следующего заголовка или конца), массив и встроенная
//! таблица — уровни стека (бит: встроенная таблица — 1), многострочная строка — уровень.
//! Ключ ждём в начале строки верхнего уровня и после `{`/`,` встроенной таблицы.
//!
//! Неверный документ: число — подряд `[0-9A-Za-z_.:+-]` с цифры или знака, слово с буквы —
//! `number` только `true`/`false`/`inf`/`nan`; строка в кавычках кончается на переводе
//! строки.

use super::{Cx, Lex, Map, St};
use crate::syntax::Tag;

// Режимы (`St::mode`).
/// Ждём ключ.
const KEY: u8 = 0;
/// Ждём значение.
const VAL: u8 = 1;
/// После значения.
const AFTER: u8 = 2;
/// Заголовок таблицы.
const HDR: u8 = 3;
const COMMENT: u8 = 4;
/// Строки: `"`, `'`, `"""`, `'''`.
const BASIC: u8 = 5;
const LIT: u8 = 6;
const MLB: u8 = 7;
const MLL: u8 = 8;
/// Кавычки в начале строки значения/ключа: `St::b` — сколько.
const QUOTES: u8 = 9;
const NUM: u8 = 10;
const WORD: u8 = 11;
/// Строка ключа или заголовка с кавычкой (`St::b` — кавычка).
const KQ: u8 = 12;

// Флаги (`St::a`).
/// Внутри таблицы (уровень таблицы открыт).
const TABLE: u8 = 1;
/// Заголовок — `[[`.
const ARR: u8 = 2;
/// Строка — значение (иначе ключ).
const SVAL: u8 = 4;
/// После `\` в строке.
const ESC: u8 = 8;
/// Заголовок: после первой `]` у `[[…]]`.
const RB: u8 = 16;
/// Кавычки — `'`.
const SQ: u8 = 32;
/// Заголовок: внутри кавычек (`St::b` — кавычка).
const HQ: u8 = 64;

const DENY_B: &[u8] = b"\"\\\n\r";
const DENY_L: &[u8] = b"'\n\r";
const DENY_MLB: &[u8] = b"\"\\";
const DENY_MLL: &[u8] = b"'";
const DENY_LINE: &[u8] = b"\n\r";

pub(crate) struct Toml {
    mode: u8,
    a: u8,
    b: u16,
    tok: u64,
    zone: u64,
    /// Начало ряда кавычек в многострочной строке.
    qrun: u64,
    /// Слово: первые буквы и длина.
    word: u64,
    wlen: u32,
    /// Режим после комментария (ключ или значение — по месту).
    resumed: bool,
}

impl Toml {
    pub(crate) fn resume(st: St) -> Toml {
        Toml {
            mode: st.mode,
            a: st.a,
            b: st.b,
            tok: 0,
            zone: 0,
            qrun: 0,
            word: 0,
            wlen: 0,
            resumed: true,
        }
    }

    fn base(&self) -> u64 {
        u64::from(self.a & TABLE != 0)
    }

    /// Верхний уровень (не в массиве или встроенной таблице).
    fn top_level(&self, cx: &Cx<'_>) -> bool {
        cx.depth() <= self.base()
    }

    /// Режим после значения/перевода строки по месту.
    fn after_value(&mut self) {
        self.mode = AFTER;
    }

    fn word_tag(&self) -> Tag {
        let w = self.word.to_le_bytes();
        let n = self.wlen as usize;
        match (n, &w[..n.min(8)]) {
            (4, b"true") | (5, b"false") | (3, b"inf" | b"nan") => Tag::Number,
            _ => Tag::Plain,
        }
    }

    /// Перевод строки вне строк и комментариев.
    fn newline(&mut self, cx: &mut Cx<'_>) {
        if self.top_level(cx) {
            self.mode = KEY;
        } else if cx.top() == Some(true) {
            // Встроенная таблица в одну строку: перевод — ошибка; ждём ключ.
            self.mode = KEY;
        } else if self.mode == KEY {
            self.mode = VAL;
        }
    }

    fn key(&mut self, c: u8, at: u64, next: u64, cx: &mut Cx<'_>) {
        match c {
            b'\n' => self.newline(cx),
            b' ' | b'\t' | b'\r' => {}
            b'#' => {
                self.mode = COMMENT;
                self.tok = at;
            }
            b'[' if self.top_level(cx) => {
                if self.a & TABLE != 0 {
                    cx.pop(at);
                }
                cx.push(at, false);
                self.a = (self.a | TABLE) & !(ARR | RB | HQ);
                self.mode = HDR;
                self.tok = at;
                // Следующая единица решает `[[`.
                self.b = 2;
            }
            b'=' => self.mode = VAL,
            b'}' => {
                cx.pop(at);
                self.after_value();
            }
            b'"' | b'\'' => {
                self.mode = KQ;
                self.b = u16::from(c);
                self.tok = at;
                self.zone = next;
            }
            _ => {}
        }
    }

    fn val(&mut self, c: u8, at: u64, cx: &mut Cx<'_>) {
        match c {
            b'\n' => self.newline(cx),
            b' ' | b'\t' | b'\r' | b',' => {}
            b'#' => {
                self.mode = COMMENT;
                self.tok = at;
            }
            b'"' | b'\'' => {
                self.mode = QUOTES;
                self.tok = at;
                self.b = 1;
                self.a = (self.a | SVAL) & !ESC;
                if c == b'\'' {
                    self.a |= SQ;
                } else {
                    self.a &= !SQ;
                }
            }
            b'[' => cx.push(at, false),
            b'{' => {
                cx.push(at, true);
                self.mode = KEY;
            }
            b']' | b'}' => {
                cx.pop(at);
                self.after_value();
            }
            b'0'..=b'9' | b'+' | b'-' => {
                self.mode = NUM;
                self.tok = at;
            }
            b'a'..=b'z' | b'A'..=b'Z' => {
                self.mode = WORD;
                self.tok = at;
                self.word = u64::from(c);
                self.wlen = 1;
            }
            _ => {}
        }
    }

    fn after(&mut self, c: u8, at: u64, cx: &mut Cx<'_>) {
        match c {
            b'\n' => self.newline(cx),
            b' ' | b'\t' | b'\r' => {}
            b'#' => {
                self.mode = COMMENT;
                self.tok = at;
            }
            b',' => {
                self.mode = if cx.top() == Some(true) { KEY } else { VAL };
            }
            b']' | b'}' => {
                cx.pop(at);
            }
            b'0'..=b'9' => {
                // Время после даты через пробел (`1979-05-27 07:32:00`).
                self.mode = NUM;
                self.tok = at;
            }
            _ => {}
        }
    }

    /// Ряд кавычек в начале строки значения кончился на `c` (`None` — конец текста).
    fn quotes_done(&mut self, n: u16, at: u64, cx: &mut Cx<'_>) {
        let sq = self.a & SQ != 0;
        if n >= 3 {
            self.mode = if sq { MLL } else { MLB };
            self.zone = at;
            self.b = 0;
            cx.push(self.tok, false);
        } else if n == 2 {
            cx.span(self.tok, at, Tag::String);
            self.after_value();
        } else {
            // Одна кавычка: `at` — первая единица содержимого.
            self.mode = if sq { LIT } else { BASIC };
            self.zone = at;
        }
    }
}

impl Lex for Toml {
    fn state(&self) -> St {
        St {
            mode: self.mode,
            a: self.a,
            b: self.b,
            ..St::default()
        }
    }

    fn can_mark(&self) -> bool {
        !matches!(self.mode, QUOTES | WORD | HDR | KQ)
            && self.a & (ESC | RB) == 0
            && (!matches!(self.mode, MLB | MLL) || self.b == 0)
    }

    #[allow(clippy::too_many_lines, reason = "один автомат на все режимы")]
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if self.resumed && !b.is_empty() {
            self.resumed = false;
            self.tok = m.raw(0);
            self.zone = m.raw(0);
            self.qrun = m.raw(0);
        }
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            let at = m.raw(i);
            match self.mode {
                KEY => {
                    self.key(c, at, m.raw(i + 1), cx);
                    i += 1;
                }
                VAL => {
                    self.val(c, at, cx);
                    i += 1;
                }
                AFTER => {
                    self.after(c, at, cx);
                    i += 1;
                }
                COMMENT => {
                    let j = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |k| i + k);
                    if j < b.len() {
                        let end = m.raw(j);
                        // Комментарий кончается перед управляющими (`\r`).
                        let end = if j > 0 && b[j - 1] == b'\r' && j > i {
                            m.raw(j - 1)
                        } else {
                            end
                        };
                        cx.inert(self.tok + 1, end, DENY_LINE, true);
                        cx.span(self.tok, end, Tag::Comment);
                        self.mode = KEY;
                        self.newline(cx);
                        i = j + 1;
                    } else {
                        i = j;
                    }
                }
                HDR => {
                    if self.a & HQ != 0 {
                        // Ключ в кавычках в заголовке.
                        if self.a & ESC != 0 {
                            self.a &= !ESC;
                        } else if c == b'\\' && self.b == u16::from(b'"') {
                            self.a |= ESC;
                        } else if u16::from(c) == self.b || c == b'\n' {
                            self.a &= !HQ;
                            self.b = 0;
                        }
                        i += 1;
                        continue;
                    }
                    if self.a & RB != 0 {
                        self.a &= !RB;
                        if c == b']' {
                            cx.span(self.tok, m.raw(i + 1), Tag::Keyword);
                            self.mode = AFTER;
                            i += 1;
                        } else {
                            cx.span(self.tok, at, Tag::Keyword);
                            self.mode = AFTER;
                        }
                        continue;
                    }
                    if self.b == 2 {
                        self.b = 0;
                        if c == b'[' {
                            self.a |= ARR;
                            i += 1;
                            continue;
                        }
                    }
                    match c {
                        b']' => {
                            if self.a & ARR != 0 {
                                self.a |= RB;
                            } else {
                                cx.span(self.tok, m.raw(i + 1), Tag::Keyword);
                                self.mode = AFTER;
                            }
                        }
                        b'"' | b'\'' => {
                            self.a |= HQ;
                            self.b = u16::from(c);
                        }
                        b'\n' => {
                            cx.span(self.tok, at, Tag::Keyword);
                            self.mode = KEY;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                KQ => {
                    let q = self.b as u8;
                    if self.a & ESC != 0 {
                        self.a &= !ESC;
                        self.zone = m.raw(i + 1);
                    } else if c == b'\\' && q == b'"' {
                        cx.inert(self.zone, at, DENY_B, true);
                        self.a |= ESC;
                    } else if c == q || c == b'\n' {
                        cx.inert(self.zone, at, if q == b'"' { DENY_B } else { DENY_L }, true);
                        self.mode = KEY;
                        self.b = 0;
                        if c == b'\n' {
                            self.newline(cx);
                        }
                    }
                    i += 1;
                }
                QUOTES => {
                    let q = if self.a & SQ != 0 { b'\'' } else { b'"' };
                    if c == q && self.b < 3 {
                        self.b += 1;
                        i += 1;
                        if self.b == 3 {
                            let n = self.b;
                            self.quotes_done(n, m.raw(i), cx);
                        }
                    } else {
                        let n = self.b;
                        self.b = 0;
                        self.quotes_done(n, at, cx);
                    }
                }
                BASIC | LIT => {
                    let q = if self.mode == BASIC { b'"' } else { b'\'' };
                    let deny = if self.mode == BASIC { DENY_B } else { DENY_L };
                    if self.a & ESC != 0 {
                        self.a &= !ESC;
                        i += 1;
                        self.zone = m.raw(i);
                        continue;
                    }
                    let j = if self.mode == BASIC {
                        memchr::memchr3(b'"', b'\\', b'\n', &b[i..])
                    } else {
                        memchr::memchr2(b'\'', b'\n', &b[i..])
                    }
                    .map_or(b.len(), |k| i + k);
                    if j == b.len() {
                        i = j;
                        continue;
                    }
                    let at = m.raw(j);
                    cx.inert(self.zone, at, deny, true);
                    match b[j] {
                        x if x == q => {
                            cx.span(self.tok, m.raw(j + 1), Tag::String);
                            self.after_value();
                        }
                        b'\\' => self.a |= ESC,
                        _ => {
                            cx.span(self.tok, at, Tag::String);
                            self.mode = AFTER;
                            self.newline(cx);
                        }
                    }
                    i = j + 1;
                }
                MLB | MLL => {
                    let q = if self.mode == MLB { b'"' } else { b'\'' };
                    let deny = if self.mode == MLB { DENY_MLB } else { DENY_MLL };
                    if self.a & ESC != 0 {
                        self.a &= !ESC;
                        i += 1;
                        self.zone = m.raw(i);
                        continue;
                    }
                    if c == q {
                        if self.b == 0 {
                            self.qrun = at;
                            cx.inert(self.zone, at, deny, false);
                        }
                        self.b = self.b.saturating_add(1);
                        i += 1;
                        continue;
                    }
                    if self.b >= 3 {
                        // Ряд из ≥ 3 кавычек закрыл строку (лишние — содержимое).
                        cx.span(self.tok, at, Tag::String);
                        cx.pop(self.qrun);
                        self.b = 0;
                        self.after_value();
                        continue;
                    }
                    if self.b > 0 {
                        self.b = 0;
                        self.zone = at;
                    }
                    if c == b'\\' && self.mode == MLB {
                        cx.inert(self.zone, at, deny, false);
                        self.a |= ESC;
                        i += 1;
                        continue;
                    }
                    i += 1;
                    let j = if self.mode == MLB {
                        memchr::memchr2(b'"', b'\\', &b[i..])
                    } else {
                        memchr::memchr(b'\'', &b[i..])
                    };
                    i = j.map_or(b.len(), |k| i + k);
                }
                NUM => {
                    if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'+' | b'-') {
                        i += 1;
                    } else {
                        cx.span(self.tok, at, Tag::Number);
                        self.after_value();
                    }
                }
                WORD => {
                    if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                        if self.wlen < 8 {
                            self.word |= u64::from(c) << (8 * self.wlen);
                        }
                        self.wlen += 1;
                        i += 1;
                    } else {
                        let tag = self.word_tag();
                        cx.span(self.tok, at, tag);
                        self.after_value();
                    }
                }
                _ => {
                    self.mode = KEY;
                }
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        match self.mode {
            COMMENT => cx.span(self.tok, end, Tag::Comment),
            BASIC | LIT | MLB | MLL => {
                cx.span(self.tok, end, Tag::String);
                if !eof && self.b == 0 && self.a & ESC == 0 {
                    let deny = match self.mode {
                        BASIC => DENY_B,
                        LIT => DENY_L,
                        MLB => DENY_MLB,
                        _ => DENY_MLL,
                    };
                    cx.inert(self.zone, end, deny, matches!(self.mode, BASIC | LIT));
                }
            }
            QUOTES if eof => {
                let n = self.b;
                self.quotes_done(n, end, cx);
                if matches!(self.mode, BASIC | LIT | MLB | MLL) {
                    cx.span(self.tok, end, Tag::String);
                }
            }
            NUM => cx.span(self.tok, end, Tag::Number),
            WORD => {
                let tag = self.word_tag();
                cx.span(self.tok, end, tag);
            }
            HDR => cx.span(self.tok, end, Tag::Keyword),
            _ => {}
        }
        if eof {
            if matches!(self.mode, MLB | MLL) && self.b >= 3 {
                cx.pop(self.qrun);
            }
            if self.a & TABLE != 0 {
                while cx.depth() > 1 {
                    cx.pop(end);
                }
                cx.pop(end);
            }
        }
    }
}

#[cfg(test)]
#[path = "toml_tests.rs"]
mod tests;
