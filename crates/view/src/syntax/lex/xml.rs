//! XML (`tree-sitter-xml` + `assets/syntax/xml/highlights.scm`): `<`/`</`, имя тега и
//! `>`/`/>` — `keyword`, значение атрибута — `string`, ссылки `&…;` — `number` (и внутри
//! значений), `<!-- -->` — `comment`, CDATA — `string`, `<?xml …?>`, `<?цель …?>` и
//! `<!DOCTYPE …>` целиком — `keyword` (комментарии и ссылки в значениях DTD — своими
//! ролями); `<?xml-stylesheet`/`<?xml-model` — текст (их узлы запрос не красит).
//!
//! Структура: элемент (от `<` до `</`), многострочные комментарий, CDATA и DOCTYPE —
//! уровни. Роли уровней не нужны.
//!
//! Неверный документ: `<` не перед именем, `/`, `!`, `?` — текст; ссылка без `;` — текст.

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use super::{Cx, Lex, Map, St};
use crate::syntax::Tag;

// Режимы.
const TEXT: u8 = 0;
/// После `<`.
const LT: u8 = 1;
/// Имя открывающего тега.
const SNAME: u8 = 2;
/// Внутри открывающего тега (атрибуты).
const STAG: u8 = 3;
/// Значение атрибута (`St::b` — кавычка).
const ATTV: u8 = 4;
/// `/` в теге (ждём `>`).
const SLASH: u8 = 5;
/// Закрывающий тег после `</`.
const ETAG: u8 = 6;
/// После `<!`: набираем `--`, `[CDATA[`, `DOCTYPE`.
const BANG: u8 = 7;
const COMMENT: u8 = 8;
const CDATA: u8 = 9;
/// После `<?`: цель.
const PIT: u8 = 10;
/// Содержимое PI до `?>` (`St::a` — роль: 1 — keyword).
const PIC: u8 = 11;
const DOCTYPE: u8 = 12;
/// Ссылка `&…;` (`St::a` — где: 0 — текст, 1 — значение атрибута, 2 — DTD).
const REF: u8 = 13;

const DENY_TEXT: &[u8] = b"<&]>";
const DENY_COMMENT: &[u8] = b"->";
const DENY_CDATA: &[u8] = b"]>";

pub(crate) struct Xml {
    mode: u8,
    a: u8,
    /// Кавычка значения или счётчики по месту.
    b: u16,
    /// DOCTYPE: глубина `[`, кавычка, литералов без ссылок впереди, комментарий.
    dt: u32,
    tok: u64,
    /// Начало тега `<` (уровень элемента открывается на `>`).
    tag: u64,
    /// Начало зоны / куска роли.
    zone: u64,
    /// Набранные байты (`<!…`, цель PI, слово DTD).
    buf: u64,
    blen: u32,
    /// Позиция начала ряда `-`/`]` (конец комментария, CDATA).
    run: u64,
    /// Ссылка: откуда вернуться и где кусок роли продолжится.
    ret: u8,
    resumed: bool,
}

/// Байт имени (`iswalnum`, `_:.-`, не-ASCII).
fn name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'.' | b'-') || c >= 0x80
}

fn name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || matches!(c, b'_' | b':') || c >= 0x80
}

impl Xml {
    pub(crate) fn resume(st: St) -> Xml {
        Xml {
            mode: st.mode,
            a: st.a,
            b: st.b,
            dt: st.c,
            tok: 0,
            tag: 0,
            zone: 0,
            buf: 0,
            blen: 0,
            run: 0,
            ret: 0,
            resumed: true,
        }
    }

    fn push_buf(&mut self, c: u8) {
        if self.blen < 8 {
            self.buf |= u64::from(c) << (8 * self.blen);
        }
        self.blen += 1;
    }

    fn buf_is(&self, s: &[u8]) -> bool {
        self.blen as usize == s.len() && s.len() <= 8 && self.buf.to_le_bytes()[..s.len()] == *s
    }

    /// Набранное начинается с `s` (сравниваются первые до 8 байт).
    fn buf_prefix_of(&self, s: &[u8]) -> bool {
        let n = (self.blen as usize).min(8).min(s.len());
        self.buf.to_le_bytes()[..n] == s[..n]
    }

    /// Начало ссылки `&` в `at` (из режима `from`: 0 — текст, 1 — значение, 2 — DTD).
    fn start_ref(&mut self, at: u64, from: u8) {
        self.ret = from;
        self.mode = REF;
        self.tok = at;
        self.blen = 0;
        self.buf = 0;
    }

    /// Возврат из ссылки (`ok` — ссылка верна и кончилась на `end`).
    fn end_ref(&mut self, ok: bool, end: u64, cx: &mut Cx<'_>) {
        if ok {
            cx.span(self.tok, end, Tag::Number);
        }
        let back = self.ret;
        self.mode = match back {
            1 => ATTV,
            2 => DOCTYPE,
            _ => TEXT,
        };
        if ok {
            self.zone = end;
        } else if back == 1 || back == 2 {
            // Неверная ссылка — часть куска значения.
            self.zone = self.zone.min(self.tok);
        } else {
            self.zone = end;
        }
    }

    /// Символ ссылки: `true` — съеден.
    fn ref_char(&mut self, c: u8, next: u64, cx: &mut Cx<'_>) -> bool {
        // buf: 1 — `#`, 2 — `#x`; blen — длина.
        if c == b';' {
            let ok = self.blen > 0
                && !(self.buf == 1 && self.blen == 1)
                && !(self.buf == 2 && self.blen == 2);
            self.end_ref(ok, next, cx);
            return true;
        }
        let n = self.blen;
        let good = if n == 0 {
            if c == b'#' {
                self.buf = 1;
                true
            } else {
                name_start(c)
            }
        } else if self.buf == 1 && n == 1 && (c == b'x') {
            self.buf = 2;
            true
        } else if self.buf == 1 {
            c.is_ascii_digit()
        } else if self.buf == 2 {
            c.is_ascii_hexdigit()
        } else {
            name_char(c)
        };
        if good && n < 256 {
            self.blen += 1;
            true
        } else {
            self.end_ref(false, next, cx);
            false
        }
    }
}

/// Состояние DOCTYPE (упаковано в `St::c`).
#[derive(Copy, Clone, Debug, Default)]
struct Dt {
    /// Глубина внутреннего подмножества `[`.
    sub: u8,
    /// Кавычка литерала (0 — вне).
    q: u8,
    /// Литералов без ссылок впереди (после SYSTEM — 1, PUBLIC — 2).
    plain: u8,
    /// Текущий литерал без ссылок.
    noref: bool,
    /// Комментарий: 1 — тело, 2 — после `-`, 3 — после `--`.
    com: u8,
    /// Внутри объявления `<!…>`.
    decl: bool,
    /// Набрано: 1 — `<`, 2 — `<!`, 3 — `<!-`.
    lt: u8,
}

impl Dt {
    fn pack(self) -> u32 {
        u32::from(self.sub)
            | u32::from(self.q) << 8
            | u32::from(self.plain & 0xF) << 16
            | u32::from(self.noref) << 20
            | u32::from(self.com & 3) << 21
            | u32::from(self.decl) << 23
            | u32::from(self.lt & 3) << 24
    }

    #[allow(clippy::cast_possible_truncation, reason = "поля упакованы по маскам")]
    fn unpack(c: u32) -> Dt {
        Dt {
            sub: (c & 0xFF) as u8,
            q: (c >> 8 & 0xFF) as u8,
            plain: (c >> 16 & 0xF) as u8,
            noref: c >> 20 & 1 == 1,
            com: (c >> 21 & 3) as u8,
            decl: c >> 23 & 1 == 1,
            lt: (c >> 24 & 3) as u8,
        }
    }
}

impl Xml {
    /// Символ DOCTYPE: всё — `keyword`, кроме комментариев и ссылок в литералах.
    fn doctype(&mut self, c: u8, at: u64, next: u64, cx: &mut Cx<'_>) {
        let mut d = Dt::unpack(self.dt);
        if d.com != 0 {
            d.com = match (d.com, c) {
                (1 | 2, b'-') => d.com + 1,
                (3, b'-') => 3,
                (3, b'>') => {
                    cx.span(self.zone, next, Tag::Comment);
                    self.zone = next;
                    0
                }
                _ => 1,
            };
            self.dt = d.pack();
            return;
        }
        if d.q != 0 {
            if c == d.q {
                d.q = 0;
            } else if c == b'&' && !d.noref {
                cx.span(self.zone, at, Tag::Keyword);
                self.zone = at;
                self.dt = d.pack();
                self.start_ref(at, 2);
                return;
            }
            self.dt = d.pack();
            return;
        }
        match (d.lt, c) {
            (1, b'!') => {
                d.lt = 2;
                self.dt = d.pack();
                return;
            }
            (2, b'-') => {
                d.lt = 3;
                self.dt = d.pack();
                return;
            }
            (3, b'-') => {
                d.lt = 0;
                cx.span(self.zone, self.run, Tag::Keyword);
                self.zone = self.run;
                d.com = 1;
                self.dt = d.pack();
                return;
            }
            (2 | 3, _) => {
                d.lt = 0;
                d.decl = true;
            }
            (1, _) => d.lt = 0,
            _ => {}
        }
        // Слова SYSTEM/PUBLIC: следующие литералы — без ссылок.
        if c.is_ascii_uppercase() {
            self.push_buf(c);
        } else {
            if self.buf_is(b"SYSTEM") {
                d.plain = 1;
            } else if self.buf_is(b"PUBLIC") {
                d.plain = 2;
            }
            self.buf = 0;
            self.blen = 0;
        }
        match c {
            b'"' | b'\'' => {
                d.q = c;
                d.noref = d.plain > 0;
                d.plain = d.plain.saturating_sub(1);
            }
            b'[' if !d.decl => d.sub = d.sub.saturating_add(1),
            b']' if !d.decl => d.sub = d.sub.saturating_sub(1),
            b'<' => {
                self.run = at;
                d.lt = 1;
            }
            b'>' => {
                if d.decl {
                    d.decl = false;
                    d.plain = 0;
                } else if d.sub == 0 {
                    cx.span(self.zone, next, Tag::Keyword);
                    cx.pop(at);
                    self.mode = TEXT;
                    self.dt = 0;
                    self.zone = next;
                    return;
                }
            }
            _ => {}
        }
        self.dt = d.pack();
    }
}

impl Lex for Xml {
    fn state(&self) -> St {
        St {
            mode: self.mode,
            a: self.a,
            b: self.b,
            c: self.dt,
            ..St::default()
        }
    }

    fn can_mark(&self) -> bool {
        matches!(self.mode, TEXT | STAG | ATTV | COMMENT | CDATA | DOCTYPE)
            && (self.mode != DOCTYPE || {
                let d = Dt::unpack(self.dt);
                d.com == 0 && d.lt == 0 && self.blen == 0
            })
            && (self.mode != COMMENT && self.mode != CDATA || self.b == 0)
    }

    #[allow(clippy::too_many_lines, reason = "один автомат на все режимы")]
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if self.resumed && !b.is_empty() {
            self.resumed = false;
            let p = m.raw(0);
            self.tok = p;
            self.tag = p;
            self.zone = p;
            self.run = p;
        }
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            let at = m.raw(i);
            let next = m.raw(i + 1);
            match self.mode {
                TEXT => {
                    let j = memchr::memchr2(b'<', b'&', &b[i..]).map_or(b.len(), |k| i + k);
                    if j > i {
                        i = j;
                        continue;
                    }
                    cx.inert(self.zone, at, DENY_TEXT, true);
                    if c == b'<' {
                        self.mode = LT;
                        self.tok = at;
                    } else {
                        self.start_ref(at, 0);
                    }
                    i += 1;
                }
                LT => match c {
                    b'/' => {
                        self.mode = ETAG;
                        i += 1;
                    }
                    b'!' => {
                        self.mode = BANG;
                        self.blen = 0;
                        self.buf = 0;
                        i += 1;
                    }
                    b'?' => {
                        self.mode = PIT;
                        self.blen = 0;
                        self.buf = 0;
                        i += 1;
                    }
                    _ if name_start(c) => {
                        self.mode = SNAME;
                        self.tag = self.tok;
                        i += 1;
                    }
                    _ => {
                        self.mode = TEXT;
                        self.zone = at;
                    }
                },
                SNAME => {
                    if name_char(c) {
                        i += 1;
                    } else {
                        cx.span(self.tok, at, Tag::Keyword);
                        self.mode = STAG;
                    }
                }
                STAG => {
                    match c {
                        b'>' => {
                            cx.span(at, next, Tag::Keyword);
                            cx.push(self.tag, false);
                            self.mode = TEXT;
                            self.zone = next;
                        }
                        b'/' => {
                            self.mode = SLASH;
                            self.tok = at;
                        }
                        b'"' | b'\'' => {
                            self.mode = ATTV;
                            self.b = u16::from(c);
                            self.zone = at;
                        }
                        b'<' => {
                            // Тег не закрыт: новый.
                            self.mode = TEXT;
                            self.zone = at;
                            continue;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                SLASH => {
                    if c == b'>' {
                        cx.span(self.tok, next, Tag::Keyword);
                        self.mode = TEXT;
                        self.zone = next;
                        i += 1;
                    } else {
                        self.mode = STAG;
                    }
                }
                ATTV => {
                    let q = self.b as u8;
                    if c == q {
                        cx.span(self.zone, next, Tag::String);
                        self.mode = STAG;
                        self.b = 0;
                    } else if c == b'&' {
                        cx.span(self.zone, at, Tag::String);
                        self.start_ref(at, 1);
                    }
                    i += 1;
                }
                REF => {
                    if self.ref_char(c, next, cx) {
                        i += 1;
                    }
                }
                ETAG => {
                    if c == b'>' {
                        cx.span(self.tok, next, Tag::Keyword);
                        cx.pop(self.tok);
                        self.mode = TEXT;
                        self.zone = next;
                    } else if c == b'<' {
                        self.mode = TEXT;
                        self.zone = at;
                        continue;
                    }
                    i += 1;
                }
                BANG => {
                    self.push_buf(c);
                    i += 1;
                    if self.buf_is(b"--") {
                        self.mode = COMMENT;
                        self.b = 0;
                        self.zone = next;
                        cx.push(self.tok, false);
                    } else if self.buf_is(b"[CDATA[") {
                        self.mode = CDATA;
                        self.b = 0;
                        self.a = 0;
                        self.zone = next;
                        cx.push(self.tok, false);
                    } else if self.buf_is(b"DOCTYPE") {
                        self.mode = DOCTYPE;
                        self.dt = 0;
                        self.zone = self.tok;
                        self.blen = 0;
                        self.buf = 0;
                        cx.push(self.tok, false);
                    } else if !(self.buf_prefix_of(b"--")
                        || self.buf_prefix_of(b"[CDATA[")
                        || self.buf_prefix_of(b"DOCTYPE"))
                    {
                        self.mode = TEXT;
                        self.zone = next;
                    }
                }
                COMMENT => {
                    if c == b'-' {
                        if self.b == 0 {
                            self.run = at;
                            cx.inert(self.zone, at, DENY_COMMENT, false);
                        }
                        self.b = self.b.saturating_add(1);
                    } else if c == b'>' && self.b >= 2 {
                        cx.span(self.tok, next, Tag::Comment);
                        cx.pop(self.run);
                        self.mode = TEXT;
                        self.b = 0;
                        self.zone = next;
                    } else {
                        if self.b > 0 {
                            self.zone = at;
                        }
                        self.b = 0;
                    }
                    i += 1;
                }
                CDATA => {
                    // Конец — первое `]]>` (спецификация XML; сканер tree-sitter-xml
                    // ошибается на `]]]>` и пустой CDATA — ADR-0024 §6).
                    if c == b']' {
                        if self.b == 0 {
                            self.run = at;
                            cx.inert(self.zone, at, DENY_CDATA, false);
                        }
                        self.b = self.b.saturating_add(1);
                    } else if c == b'>' && self.b >= 2 {
                        // Конец — начало последних двух `]`.
                        cx.span(self.tok, next, Tag::String);
                        cx.pop(self.run);
                        self.mode = TEXT;
                        self.b = 0;
                        self.zone = next;
                    } else {
                        if self.b > 0 {
                            self.zone = at;
                        }
                        self.b = 0;
                    }
                    i += 1;
                }
                PIT => {
                    if name_char(c) && (self.blen > 0 || name_start(c)) {
                        self.push_buf(c.to_ascii_lowercase());
                        i += 1;
                    } else {
                        // Цель: `xml` — объявление, `xml-stylesheet`/`xml-model` — текст.
                        let plain = (self.blen == 9 && self.buf_prefix_of(b"xml-mode"))
                            || (self.blen == 14 && self.buf_prefix_of(b"xml-styl"));
                        self.a = u8::from(!plain);
                        self.mode = PIC;
                        self.b = 0;
                    }
                }
                PIC => {
                    if self.b == 1 && c == b'>' {
                        if self.a == 1 {
                            cx.span(self.tok, next, Tag::Keyword);
                        }
                        self.mode = TEXT;
                        self.b = 0;
                        self.zone = next;
                    } else {
                        self.b = u16::from(c == b'?');
                    }
                    i += 1;
                }
                DOCTYPE => {
                    self.doctype(c, at, next, cx);
                    i += 1;
                }
                _ => {
                    self.mode = TEXT;
                }
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        match self.mode {
            SNAME => cx.span(self.tok, end, Tag::Keyword),
            ATTV => cx.span(self.zone, end, Tag::String),
            COMMENT => cx.span(self.tok, end, Tag::Comment),
            CDATA => cx.span(self.tok, end, Tag::String),
            PIC if self.a == 1 => cx.span(self.tok, end, Tag::Keyword),
            DOCTYPE => {
                if Dt::unpack(self.dt).com != 0 {
                    cx.span(self.zone, end, Tag::Comment);
                } else {
                    cx.span(self.zone, end, Tag::Keyword);
                }
            }
            TEXT if !eof => cx.inert(self.zone, end, DENY_TEXT, true),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "xml_tests.rs"]
mod tests;
