//! Трекер `CMake` (ADR-0024 §8): текст — последовательность команд, поэтому кандидат — строка,
//! начатая именем команды (кроме `else`, `elseif`, `end…`). Уровни: `if`, `foreach`, `while`,
//! `function`, `macro` — настоящий заголовок из файла; `block` — вне цепочки.

#![allow(
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт; флаги — состояние"
)]

use super::{
    L_ARMS, L_BODY, L_CLASS, L_GROUP, L_LIST, L_OTHER, L_PAREN, L_ZONE, Track, pk, word_key,
};
use crate::syntax::lex::{Cx, Lex, Map, St};

const NORMAL: u8 = 0;
const WORD: u8 = 1;
/// `#`: скобочный комментарий?
const HASH: u8 = 2;
const LINE: u8 = 3;
/// Аргументы команды.
const ARGS: u8 = 4;
const QUOTE: u8 = 5;
const QUOTE_ESC: u8 = 6;
/// `[` в начале аргумента (или `#[`, флаг `comment`): считаем `=`.
const OPEN: u8 = 7;
const LONG: u8 = 8;
const LONG_END: u8 = 9;
/// `\` в аргументе.
const ARG_ESC: u8 = 10;

const fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

pub(crate) struct Cm {
    t: Track,
    mode: u8,
    first: bool,
    col: u16,
    /// Глубина скобок команды (уровни — для свёртки).
    args: u32,
    /// Команда открывает блок этого вида (`L_*`), когда закроется её `)`.
    open: u8,
    /// Длинная лексема — комментарий; `[[` возможна (начало аргумента).
    comment: bool,
    arg_start: bool,
    zone: bool,
    n: u32,
    k: u32,
    word: u64,
    wlen: u32,
    word_at: u64,
}

impl Cm {
    pub(crate) fn resume(st: St) -> Cm {
        Cm {
            t: Track::resume(&st),
            mode: st.mode,
            first: st.d & 1 == 0,
            col: st.b,
            args: st.ext.h,
            open: st.a,
            comment: st.d & 2 != 0,
            arg_start: st.d & 4 != 0,
            zone: st.d & 8 != 0,
            n: st.c,
            k: st.ext.k,
            word: 0,
            wlen: 0,
            word_at: 0,
        }
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

    /// Имя команды кончилось (вне скобок).
    fn word_end(&mut self, cx: &mut Cx<'_>) {
        const IF: u64 = pk("if");
        const ELSEIF: u64 = pk("elseif");
        const ELSE: u64 = pk("else");
        const ENDIF: u64 = pk("endif");
        const FOREACH: u64 = pk("foreach");
        const ENDFOREACH: u64 = pk("endforeach");
        const WHILE: u64 = pk("while");
        const ENDWHILE: u64 = pk("endwhile");
        const FUNCTION: u64 = pk("function");
        const ENDFUNCTION: u64 = pk("endfunction");
        const MACRO: u64 = pk("macro");
        const ENDMACRO: u64 = pk("endmacro");
        const BLOCK: u64 = pk("block");
        const ENDBLOCK: u64 = pk("endblock");
        let w = word_key(self.word, self.wlen);
        let at = self.word_at;
        let cont = matches!(
            w,
            ELSE | ELSEIF | ENDIF | ENDFOREACH | ENDWHILE | ENDFUNCTION | ENDMACRO | ENDBLOCK
        );
        if std::mem::take(&mut self.first) && !cont && self.t.bad == 0 {
            self.t.cand(self.t.line_at, cx);
        }
        self.mode = NORMAL;
        self.open = match w {
            IF => L_BODY,
            FOREACH => L_LIST,
            WHILE => L_ARMS,
            FUNCTION => L_CLASS,
            MACRO => L_GROUP,
            BLOCK => L_OTHER + 16,
            _ => 0,
        };
        let close = match w {
            ENDIF => L_BODY,
            ENDFOREACH => L_LIST,
            ENDWHILE => L_ARMS,
            ENDFUNCTION => L_CLASS,
            ENDMACRO => L_GROUP,
            ENDBLOCK => L_OTHER,
            _ => 255,
        };
        if close != 255 {
            let d = cx.depth();
            if d > 0 && (self.t.kind(d) == close || d > super::LVL_MAX) {
                self.t.pop(cx, at);
            }
        }
        if matches!(w, ELSE | ELSEIF) && self.t.bad == 0 && cx.depth() > 0 {
            // Заголовок ветви — её `if`: ветвь идёт целиком.
            self.t.bad = cx.depth();
        }
    }

    fn normal(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        let c = b[i];
        match c {
            b'\n' => {
                self.first = true;
                self.col = 0;
                self.t.line_at = m.raw(i + 1);
                i + 1
            }
            b' ' | b'\t' | b'\r' | 0x0C => {
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                i + 1
            }
            b'#' => {
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                self.mode = HASH;
                i + 1
            }
            b'(' => {
                self.first = false;
                self.t.push(cx, m.raw(i), m.raw(i + 1), L_PAREN);
                self.args = 1;
                self.arg_start = true;
                self.mode = ARGS;
                i + 1
            }
            _ if is_word(c) => {
                self.mode = WORD;
                self.word = 0;
                self.wlen = 0;
                self.word_at = m.raw(i);
                i
            }
            _ => {
                self.first = false;
                i + 1
            }
        }
    }

    fn args(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        let c = b[i];
        let start = std::mem::replace(&mut self.arg_start, false);
        match c {
            b'(' => {
                self.t.push(cx, m.raw(i), m.raw(i + 1), L_PAREN);
                self.args += 1;
                self.arg_start = true;
            }
            b')' => {
                self.t.close(cx, m.raw(i), |k| k == L_PAREN, |_| false);
                self.args = self.args.saturating_sub(1);
                if self.args == 0 {
                    self.mode = NORMAL;
                    let end = m.raw(i + 1);
                    match std::mem::take(&mut self.open) {
                        0 => {}
                        16 => self.t.push(cx, m.raw(i), end, L_OTHER),
                        k => self.t.push(cx, m.raw(i), end, k),
                    }
                }
            }
            b'"' => self.mode = QUOTE,
            b'#' => {
                self.mode = HASH;
            }
            b'\\' => self.mode = ARG_ESC,
            b'[' if start => {
                self.comment = false;
                self.k = 0;
                self.mode = OPEN;
            }
            b' ' | b'\t' | b'\r' | b'\n' => self.arg_start = true,
            _ => {}
        }
        i + 1
    }
}

impl Lex for Cm {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let mut st = St {
            mode: self.mode,
            a: self.open,
            b: self.col,
            c: self.n,
            d: u8::from(!self.first)
                | u8::from(self.comment) << 1
                | u8::from(self.arg_start) << 2
                | u8::from(self.zone) << 3,
            ..St::default()
        };
        st.ext.h = self.args;
        st.ext.k = self.k;
        self.t.save(&mut st, at);
        st
    }

    fn can_mark(&self) -> bool {
        self.mode != WORD
    }

    #[allow(clippy::too_many_lines, reason = "автомат: режимы одним match")]
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if b.is_empty() {
            return;
        }
        self.t.begin(m.raw(0), m.unit());
        let mut i = 0;
        while i < b.len() {
            i = match self.mode {
                NORMAL => self.normal(b, i, m, cx),
                WORD => {
                    let mut j = i;
                    while j < b.len() && is_word(b[j]) {
                        self.word = self.word << 8 | u64::from(b[j].to_ascii_lowercase());
                        self.wlen = self.wlen.saturating_add(1);
                        j += 1;
                    }
                    if j < b.len() {
                        self.word_end(cx);
                    }
                    j
                }
                HASH => {
                    if b[i] == b'[' {
                        self.comment = true;
                        self.k = 0;
                        self.mode = OPEN;
                        i + 1
                    } else {
                        self.mode = LINE;
                        i
                    }
                }
                LINE => match memchr::memchr(b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = if self.args > 0 { ARGS } else { NORMAL };
                        i + k
                    }
                },
                ARGS => self.args(b, i, m, cx),
                ARG_ESC => {
                    self.mode = ARGS;
                    i + 1
                }
                QUOTE => match memchr::memchr3(b'"', b'\\', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        match b[j] {
                            b'"' => {
                                self.zone_off(j, m, cx);
                                self.mode = ARGS;
                            }
                            b'\\' => self.mode = QUOTE_ESC,
                            _ => self.zone_on(j, m, cx),
                        }
                        j + 1
                    }
                },
                QUOTE_ESC => {
                    self.mode = QUOTE;
                    i + 1
                }
                OPEN => {
                    let c = b[i];
                    if c == b'=' {
                        self.k += 1;
                        i + 1
                    } else if c == b'[' {
                        self.first = false;
                        self.n = self.k;
                        self.mode = LONG;
                        i + 1
                    } else if self.comment {
                        self.mode = LINE;
                        i
                    } else {
                        self.mode = ARGS;
                        i
                    }
                }
                LONG => match memchr::memchr2(b']', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b']' {
                            self.k = 0;
                            self.mode = LONG_END;
                        } else {
                            self.zone_on(j, m, cx);
                        }
                        j + 1
                    }
                },
                LONG_END => {
                    let c = b[i];
                    if c == b'=' {
                        self.k += 1;
                        i + 1
                    } else if c == b']' {
                        if self.k == self.n {
                            self.zone_off(i, m, cx);
                            self.mode = if self.args > 0 { ARGS } else { NORMAL };
                        } else {
                            self.k = 0;
                        }
                        i + 1
                    } else {
                        self.mode = LONG;
                        i
                    }
                }
                _ => {
                    self.mode = NORMAL;
                    i
                }
            };
        }
    }

    fn flush(&mut self, _end: u64, eof: bool, cx: &mut Cx<'_>) {
        if eof && self.mode == WORD {
            self.word_end(cx);
        }
    }
}
