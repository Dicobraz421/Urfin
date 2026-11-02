//! JSON (`tree-sitter-json` + `assets/syntax/json/highlights.scm`): строка — `string`,
//! ключ пары — как текст, число и `true`/`false`/`null` — `number`, `//` и `/* */` —
//! `comment`. Ключ — строка в объекте там, где ждут ключ (после `{` и `,`): роль задаёт
//! начало токена. Стек: бит роли уровня — объект (1) / массив (0).
//!
//! Неверный документ (свои правила, ADR-0024 §6): строка кончается на переводе строки
//! (как токен tree-sitter), число — подряд идущие `[0-9eE.+-]`, слово — подряд идущие
//! буквы (`true`/`false`/`null` — число, прочее — текст), незакрытый `/*` — до конца.

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use super::{Cx, Lex, Map, St};
use crate::syntax::Tag;

/// Режимы (`St::mode`).
const CODE: u8 = 0;
const STR: u8 = 1;
const LINE: u8 = 2;
const BLOCK: u8 = 3;
const SLASH: u8 = 4;
const NUM: u8 = 5;
const WORD: u8 = 6;

/// Флаги (`St::a`).
/// Ждём ключ (в объекте после `{` или `,`).
const KEY: u8 = 1;
/// Строка — ключ.
const SKEY: u8 = 2;
/// После `\` в строке.
const ESC: u8 = 4;
/// В блочном комментарии после `*`.
const STAR: u8 = 8;

const DENY_STR: &[u8] = b"\"\\\n\r";
const DENY_LINE: &[u8] = b"\n\r";
const DENY_BLOCK: &[u8] = b"*/\n\r";

pub(crate) struct Json {
    mode: u8,
    a: u8,
    /// Начало токена (байт источника).
    tok: u64,
    /// Начало инертной зоны.
    zone: u64,
    /// Слово: до 8 первых букв и длина.
    word: u64,
    wlen: u32,
    /// Последняя `*` блочного комментария.
    star: u64,
    /// Лексер создан с точки: начатый токен начинается с первой единицы.
    resumed: bool,
}

impl Json {
    pub(crate) fn resume(st: St) -> Json {
        Json {
            mode: st.mode,
            a: st.a,
            tok: 0,
            zone: 0,
            word: 0,
            wlen: 0,
            star: 0,
            resumed: true,
        }
    }

    /// Слово-литерал по первой букве (`true`, `false`, `null`): токены tree-sitter без
    /// границы слова (`truetrue` — два литерала).
    fn keyword(c: u8) -> Option<&'static [u8]> {
        match c {
            b't' => Some(b"true"),
            b'f' => Some(b"false"),
            b'n' => Some(b"null"),
            _ => None,
        }
    }

    /// После значения: ключ не ждём.
    fn value_done(&mut self) {
        self.a &= !KEY;
    }

    fn code(&mut self, c: u8, at: u64, next: u64, cx: &mut Cx<'_>) {
        match c {
            b'"' => {
                self.mode = STR;
                self.tok = at;
                self.zone = next;
                self.a = if self.a & KEY != 0 { SKEY } else { 0 };
            }
            b'{' => {
                cx.push(at, true);
                self.a = KEY;
            }
            b'[' => {
                cx.push(at, false);
                self.a = 0;
            }
            b'}' | b']' => {
                cx.pop(at);
                self.a = 0;
            }
            b',' => {
                self.a = if cx.top() == Some(true) { KEY } else { 0 };
            }
            b':' => self.a = 0,
            b'/' => {
                self.mode = SLASH;
                self.tok = at;
            }
            b'-' | b'0'..=b'9' => {
                self.mode = NUM;
                self.tok = at;
                self.value_done();
            }
            _ if Self::keyword(c).is_some() => {
                self.mode = WORD;
                self.tok = at;
                self.word = u64::from(c);
                self.wlen = 1;
                self.value_done();
            }
            _ => {}
        }
    }
}

impl Lex for Json {
    fn state(&self) -> St {
        St {
            mode: self.mode,
            a: self.a,
            ..St::default()
        }
    }

    fn can_mark(&self) -> bool {
        !matches!(self.mode, SLASH | WORD) && self.a & (ESC | STAR) == 0
    }

    #[allow(clippy::too_many_lines, reason = "один автомат на все режимы")]
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        let mut i = 0;
        if self.resumed && !b.is_empty() {
            // Токен, начатый до точки: красится с неё.
            self.resumed = false;
            self.tok = m.raw(0);
            self.zone = m.raw(0);
            self.star = m.raw(0);
        }
        while i < b.len() {
            let c = b[i];
            match self.mode {
                CODE => {
                    if !super::is_ws(c) {
                        self.code(c, m.raw(i), m.raw(i + 1), cx);
                    }
                    i += 1;
                }
                STR => {
                    if self.a & ESC != 0 {
                        self.a &= !ESC;
                        i += 1;
                        if c == b'\n' {
                            // `\` + перевод строки: строка кончилась ошибкой.
                            let tag = if self.a & SKEY != 0 {
                                Tag::Plain
                            } else {
                                Tag::String
                            };
                            cx.span(self.tok, m.raw(i - 1), tag);
                            self.mode = CODE;
                            self.a = 0;
                        } else {
                            self.zone = m.raw(i);
                        }
                        continue;
                    }
                    // Быстро до кавычки, `\` или перевода строки.
                    let j = memchr::memchr3(b'"', b'\\', b'\n', &b[i..]).map_or(b.len(), |k| i + k);
                    if j == b.len() {
                        i = j;
                        continue;
                    }
                    let at = m.raw(j);
                    cx.inert(self.zone, at, DENY_STR, true);
                    let tag = if self.a & SKEY != 0 {
                        Tag::Plain
                    } else {
                        Tag::String
                    };
                    match b[j] {
                        b'"' => {
                            cx.span(self.tok, m.raw(j + 1), tag);
                            self.mode = CODE;
                            self.a = 0;
                        }
                        b'\\' => self.a |= ESC,
                        _ => {
                            cx.span(self.tok, at, tag);
                            self.mode = CODE;
                            self.a = 0;
                        }
                    }
                    i = j + 1;
                }
                LINE => {
                    let j = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |k| i + k);
                    if j < b.len() {
                        let at = m.raw(j);
                        cx.inert(self.zone, at, DENY_LINE, true);
                        cx.span(self.tok, at, Tag::Comment);
                        self.mode = CODE;
                    }
                    i = j;
                }
                BLOCK => {
                    if self.a & STAR != 0 && c == b'/' {
                        let at = m.raw(i + 1);
                        cx.inert(self.zone, self.star, DENY_BLOCK, false);
                        cx.span(self.tok, at, Tag::Comment);
                        cx.pop(self.star);
                        self.mode = CODE;
                        self.a &= !STAR;
                        i += 1;
                        continue;
                    }
                    if c == b'*' {
                        if self.a & STAR == 0 {
                            self.star = m.raw(i);
                        }
                        self.a |= STAR;
                    } else {
                        self.a &= !STAR;
                    }
                    i += 1;
                }
                SLASH => match c {
                    b'/' => {
                        self.mode = LINE;
                        self.zone = m.raw(i + 1);
                        i += 1;
                    }
                    b'*' => {
                        self.mode = BLOCK;
                        self.zone = m.raw(i + 1);
                        self.a &= !STAR;
                        cx.push(self.tok, false);
                        i += 1;
                    }
                    _ => self.mode = CODE,
                },
                NUM => {
                    if matches!(c, b'0'..=b'9' | b'e' | b'E' | b'.' | b'+' | b'-') {
                        i += 1;
                    } else {
                        cx.span(self.tok, m.raw(i), Tag::Number);
                        self.mode = CODE;
                    }
                }
                WORD => {
                    // `word` — первая буква литерала, `wlen` — совпало букв.
                    let kw = Self::keyword(self.word as u8).unwrap_or(b"");
                    if kw.get(self.wlen as usize) == Some(&c) {
                        self.wlen += 1;
                        i += 1;
                        if self.wlen as usize == kw.len() {
                            cx.span(self.tok, m.raw(i), Tag::Number);
                            self.mode = CODE;
                        }
                    } else {
                        // Не литерал: ошибка, текст.
                        self.mode = CODE;
                    }
                }
                _ => {
                    self.mode = CODE;
                }
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        match self.mode {
            STR => {
                let tag = if self.a & SKEY != 0 {
                    Tag::Plain
                } else {
                    Tag::String
                };
                cx.span(self.tok, end, tag);
                if !eof {
                    cx.inert(self.zone, end, DENY_STR, true);
                }
            }
            LINE | BLOCK => cx.span(self.tok, end, Tag::Comment),
            NUM => cx.span(self.tok, end, Tag::Number),
            _ => {}
        }
        if eof {
            self.mode = CODE;
        }
    }
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod tests;
