//! INI (`tree-sitter-ini` + `assets/syntax/ini/highlights.scm`): заголовок секции
//! `[…]` — `keyword`, `;`/`#` в начале строки — `comment`, ключи и значения — текст.
//! Секция — уровень структуры (свёртка до следующей секции).
//!
//! Неверный документ: строка, не начатая `[`, `;`, `#`, — настройка (текст).

use super::{Cx, Lex, Map, St};
use crate::syntax::Tag;

/// Начало строки (до первого непробельного).
const BOL: u8 = 0;
const COMMENT: u8 = 1;
/// Заголовок секции до `]`.
const SEC: u8 = 2;
/// Остаток строки — текст.
const REST: u8 = 3;

const DENY_LINE: &[u8] = b"\n\r";

pub(crate) struct Ini {
    mode: u8,
    /// Внутри секции (уровень открыт).
    sec: bool,
    tok: u64,
    resumed: bool,
}

impl Ini {
    pub(crate) fn resume(st: St) -> Ini {
        Ini {
            mode: st.mode,
            sec: st.a != 0,
            tok: 0,
            resumed: true,
        }
    }
}

impl Lex for Ini {
    fn state(&self) -> St {
        St {
            mode: self.mode,
            a: u8::from(self.sec),
            ..St::default()
        }
    }

    fn can_mark(&self) -> bool {
        true
    }

    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if self.resumed && !b.is_empty() {
            self.resumed = false;
            self.tok = m.raw(0);
        }
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            match self.mode {
                BOL => {
                    match c {
                        b' ' | b'\t' | b'\r' | b'\n' => {}
                        b';' | b'#' => {
                            self.mode = COMMENT;
                            self.tok = m.raw(i);
                        }
                        b'[' => {
                            if self.sec {
                                cx.pop(m.raw(i));
                            }
                            cx.push(m.raw(i), false);
                            self.sec = true;
                            self.mode = SEC;
                            self.tok = m.raw(i);
                        }
                        _ => self.mode = REST,
                    }
                    i += 1;
                }
                COMMENT => {
                    let j = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |k| i + k);
                    if j < b.len() {
                        let at = m.raw(j);
                        cx.inert(self.tok + 1, at, DENY_LINE, true);
                        cx.span(self.tok, at, Tag::Comment);
                        self.mode = BOL;
                        i = j + 1;
                    } else {
                        i = j;
                    }
                }
                SEC => {
                    match c {
                        b']' => {
                            cx.span(self.tok, m.raw(i + 1), Tag::Keyword);
                            self.mode = REST;
                        }
                        b'[' => {
                            // Ошибка: `[` внутри заголовка — дальше текст.
                            cx.span(self.tok, m.raw(i), Tag::Keyword);
                            self.mode = REST;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                _ => {
                    let j = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |k| i + k);
                    if j < b.len() {
                        self.mode = BOL;
                        i = j + 1;
                    } else {
                        i = j;
                    }
                }
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        match self.mode {
            COMMENT => cx.span(self.tok, end, Tag::Comment),
            SEC => cx.span(self.tok, end, Tag::Keyword),
            _ => {}
        }
        if eof && self.sec {
            cx.pop(end);
            self.sec = false;
        }
    }
}

#[cfg(test)]
#[path = "ini_tests.rs"]
mod tests;
