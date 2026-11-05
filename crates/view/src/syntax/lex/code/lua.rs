//! Трекер Lua (ADR-0024 §8): разделителей операторов нет, поэтому кандидат — строка, начатая
//! словом оператора (`local`, `function`, `if`…) после законченного значения или начала
//! блока; в таблице — строка после запятой. Уровни: `function … end`, `if … end`,
//! `do … end`, `repeat … until`; ветви `else`, `elseif` идут целиком.

#![allow(
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт; флаги — состояние"
)]

use super::{L_BODY, L_BRACKET, L_CLASS, L_LIST, L_PAREN, L_ZONE, Track, pk, word_key};
use crate::syntax::lex::{Cx, Lex, Map, St};

const NORMAL: u8 = 0;
const WORD: u8 = 1;
/// `-`: комментарий?
const DASH: u8 = 2;
/// `--`: длинный комментарий?
const DASH2: u8 = 3;
/// `--[`: считаем `=`.
const COMMENT_OPEN: u8 = 4;
const COMMENT: u8 = 5;
/// `[`: длинная строка?
const BRACKET: u8 = 6;
/// Длинная лексема (`n` — уровень): тело.
const LONG: u8 = 7;
/// `]` в длинной: считаем `=`.
const LONG_END: u8 = 8;
const STR: u8 = 9;
const STR_ESC: u8 = 10;
/// `\z`: пробелы и переводы строк пропускаются.
const STR_Z: u8 = 11;

const fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

pub(crate) struct Lua {
    t: Track,
    mode: u8,
    q: u8,
    first: bool,
    col: u16,
    /// Прошлый токен — конец значения.
    val: bool,
    /// Прошлый токен начинает блок (`then`, `do`, `else`, `repeat`, `)` функции).
    stmt: bool,
    /// Запятая: в таблице с новой строки — элемент.
    comma: bool,
    /// Ждём `do` от `for`, `while`.
    dos: u32,
    /// Ждём `then`: бит на условие, 1 — `if` (откроет уровень), 0 — `elseif`.
    thens: u32,
    /// `(` на этом уровне закрывает заголовок функции (0 — нет).
    func: u32,
    zone: bool,
    /// Длинная лексема — комментарий.
    long_comment: bool,
    /// Уровень длинной лексемы и счётчик `=`.
    n: u32,
    k: u32,
    word: u64,
    wlen: u32,
    word_at: u64,
}

impl Lua {
    pub(crate) fn resume(st: St) -> Lua {
        Lua {
            t: Track::resume(&st),
            mode: st.mode,
            q: st.a,
            first: st.d & 2 == 0,
            col: st.b,
            val: st.d & 4 != 0,
            // С начала текста первое слово начинает оператор.
            stmt: st.d & 1 == 0,
            comma: st.d & 16 != 0,
            thens: st.ext.hash,
            dos: st.ext.rh,
            func: st.ext.h,
            zone: st.d & 64 != 0,
            long_comment: st.d & 128 != 0,
            n: st.c,
            k: st.ext.k,
            word: 0,
            wlen: 0,
            word_at: 0,
        }
    }

    fn lead(&mut self, w: u64, c: u8, cx: &mut Cx<'_>) {
        const STMT: [u64; 9] = [
            pk("local"),
            pk("function"),
            pk("if"),
            pk("for"),
            pk("while"),
            pk("repeat"),
            pk("do"),
            pk("return"),
            pk("goto"),
        ];
        if !std::mem::take(&mut self.first) || self.t.bad != 0 {
            return;
        }
        let top = self.t.top(cx);
        let item = if self.comma {
            top == L_LIST && !matches!(c, b')' | b']' | b'}')
        } else {
            matches!(top, L_BODY | L_CLASS)
                && (self.val || self.stmt)
                && STMT.contains(&w)
                && !(w == pk("do") && self.dos > 0)
        };
        if item {
            self.t.cand(self.t.line_at, cx);
        }
    }

    fn word_end(&mut self, cx: &mut Cx<'_>, at: u64, end: u64) {
        const FUNCTION: u64 = pk("function");
        const IF: u64 = pk("if");
        const THEN: u64 = pk("then");
        const ELSE: u64 = pk("else");
        const ELSEIF: u64 = pk("elseif");
        const DO: u64 = pk("do");
        const FOR: u64 = pk("for");
        const WHILE: u64 = pk("while");
        const REPEAT: u64 = pk("repeat");
        const UNTIL: u64 = pk("until");
        const END: u64 = pk("end");
        const AND: u64 = pk("and");
        const OR: u64 = pk("or");
        const NOT: u64 = pk("not");
        const RETURN: u64 = pk("return");
        fn body(k: u8) -> bool {
            k == L_BODY
        }
        fn rep(k: u8) -> bool {
            k == L_CLASS
        }
        fn group(k: u8) -> bool {
            matches!(k, L_PAREN | L_BRACKET)
        }
        let w = word_key(self.word, self.wlen);
        self.lead(w, 0, cx);
        self.mode = NORMAL;
        self.comma = false;
        self.val = true;
        self.stmt = false;
        match w {
            FUNCTION => {
                self.func = u32::try_from(cx.depth() + 1).unwrap_or(0);
                self.val = false;
            }
            IF => {
                self.thens = self.thens << 1 | 1;
                self.val = false;
            }
            THEN => {
                if self.thens & 1 != 0 {
                    self.t.push(cx, at, end, L_BODY);
                }
                self.thens >>= 1;
                self.val = false;
                self.stmt = true;
            }
            FOR | WHILE => {
                self.dos += 1;
                self.val = false;
            }
            DO => {
                self.dos = self.dos.saturating_sub(1);
                self.t.push(cx, at, end, L_BODY);
                self.val = false;
                self.stmt = true;
            }
            REPEAT => {
                self.t.push(cx, at, end, L_CLASS);
                self.val = false;
                self.stmt = true;
            }
            ELSE | ELSEIF => {
                // Ветвь идёт целиком: начало `if` — в заголовке, а он уже закрыт текстом.
                if self.t.bad == 0 && cx.depth() > 0 {
                    self.t.bad = cx.depth();
                }
                if w == ELSEIF {
                    self.thens <<= 1;
                }
                self.val = false;
                self.stmt = w == ELSE;
            }
            END => {
                self.t.close(cx, self.word_at, body, group);
            }
            UNTIL => {
                self.t.close(cx, self.word_at, rep, group);
                self.val = false;
            }
            AND | OR | NOT | RETURN => self.val = false,
            _ => {}
        }
    }

    fn punct(&mut self, c: u8, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        fn brace(k: u8) -> bool {
            k == L_LIST
        }
        fn group(k: u8) -> bool {
            matches!(k, L_PAREN | L_BRACKET)
        }
        let (at, end) = (m.raw(i), m.raw(i + 1));
        self.comma = false;
        self.stmt = false;
        self.val = false;
        match c {
            b'(' => self.t.push(cx, at, end, L_PAREN),
            b'{' => self.t.push(cx, at, end, L_LIST),
            b')' => {
                self.t.close(cx, at, |k| k == L_PAREN, group);
                self.val = true;
                // Заголовок функции кончился: дальше тело.
                if u64::from(self.func) == cx.depth() + 1 {
                    self.func = 0;
                    self.t.push(cx, at, end, L_BODY);
                    self.val = false;
                    self.stmt = true;
                }
            }
            b']' => {
                self.t.close(cx, at, |k| k == L_BRACKET, group);
                self.val = true;
            }
            b'}' => {
                self.t.close(cx, at, brace, group);
                self.val = true;
            }
            b',' => self.comma = self.t.top(cx) == L_LIST,
            b';' => {
                let top = self.t.top(cx);
                self.comma = top == L_LIST;
                self.stmt = matches!(top, L_BODY | L_CLASS);
            }
            _ => {}
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
                    if self.col == u16::MAX {
                        self.first = false;
                    }
                }
                i + 1
            }
            b'-' => {
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                self.mode = DASH;
                i + 1
            }
            b'[' => {
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                self.k = 0;
                self.mode = BRACKET;
                i + 1
            }
            b'#' if m.raw(i) == 0 => {
                // `#!` первой строкой.
                self.mode = COMMENT;
                self.first = false;
                i + 1
            }
            b'"' | b'\'' => {
                self.lead(0, c, cx);
                self.q = c;
                self.comma = false;
                self.stmt = false;
                self.val = true;
                self.mode = STR;
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
                self.lead(0, c, cx);
                self.punct(c, i, m, cx);
                i + 1
            }
        }
    }
}

impl Lex for Lua {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let mut st = St {
            mode: self.mode,
            a: self.q,
            b: self.col,
            c: self.n,
            d: u8::from(!self.stmt)
                | u8::from(!self.first) << 1
                | u8::from(self.val) << 2
                | u8::from(self.comma) << 4
                | u8::from(self.zone) << 6
                | u8::from(self.long_comment) << 7,
            ..St::default()
        };
        st.ext.k = self.k;
        st.ext.h = self.func;
        st.ext.hash = self.thens;
        st.ext.rh = self.dos;
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
                        self.word = self.word << 8 | u64::from(b[j]);
                        self.wlen = self.wlen.saturating_add(1);
                        j += 1;
                    }
                    if j < b.len() {
                        let (at, end) = (m.raw(j), m.raw(j));
                        self.word_end(cx, at, end);
                    }
                    j
                }
                DASH => {
                    if b[i] == b'-' {
                        self.mode = DASH2;
                        i + 1
                    } else {
                        self.lead(0, b'-', cx);
                        self.val = false;
                        self.stmt = false;
                        self.comma = false;
                        self.mode = NORMAL;
                        i
                    }
                }
                DASH2 => {
                    if b[i] == b'[' {
                        self.k = 0;
                        self.mode = COMMENT_OPEN;
                        i + 1
                    } else {
                        self.mode = COMMENT;
                        i
                    }
                }
                COMMENT_OPEN => {
                    let c = b[i];
                    if c == b'=' {
                        self.k += 1;
                        i + 1
                    } else if c == b'[' {
                        self.first = false;
                        self.long_comment = true;
                        self.n = self.k;
                        self.mode = LONG;
                        i + 1
                    } else {
                        self.mode = COMMENT;
                        i
                    }
                }
                COMMENT => match memchr::memchr(b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = NORMAL;
                        i + k
                    }
                },
                BRACKET => {
                    let c = b[i];
                    if c == b'=' {
                        self.k += 1;
                        i + 1
                    } else if c == b'[' {
                        self.lead(0, b'[', cx);
                        self.long_comment = false;
                        self.val = true;
                        self.stmt = false;
                        self.comma = false;
                        self.n = self.k;
                        self.mode = LONG;
                        i + 1
                    } else {
                        // Обычная скобка (`=` после неё — ошибка документа).
                        self.lead(0, b'[', cx);
                        self.comma = false;
                        self.stmt = false;
                        self.val = false;
                        let at = m.raw(i).saturating_sub(m.unit());
                        self.t.push(cx, at, m.raw(i), L_BRACKET);
                        self.mode = NORMAL;
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
                            self.mode = NORMAL;
                        } else {
                            self.k = 0;
                        }
                        i + 1
                    } else {
                        self.mode = LONG;
                        i
                    }
                }
                STR => match memchr::memchr3(self.q, b'\\', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        let c = b[j];
                        if c == self.q {
                            self.q = 0;
                            self.mode = NORMAL;
                            j + 1
                        } else if c == b'\\' {
                            self.mode = STR_ESC;
                            j + 1
                        } else {
                            // Незакрытая строка кончается на переводе строки.
                            self.q = 0;
                            self.mode = NORMAL;
                            j
                        }
                    }
                },
                STR_ESC => {
                    self.mode = if b[i] == b'z' { STR_Z } else { STR };
                    i + 1
                }
                STR_Z => {
                    if b[i].is_ascii_whitespace() {
                        i + 1
                    } else {
                        self.mode = STR;
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

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        if eof && self.mode == WORD {
            self.word_end(cx, end, end);
        }
    }
}
