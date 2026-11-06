//! Трекер PowerShell (ADR-0024 §8). Разделитель операторов — перевод строки, поэтому
//! кандидат — строка, начатая после законченного значения или начала блока, с токена,
//! который не продолжает прошлую строку (`else`, `catch`, `{`, `|`, `.`…). Лишний отказ в
//! кандидате безвреден: кусок просто длиннее, поэтому всё сомнительное кандидатом не
//! становится. Уровни: `{` с операторами; `@{`, тело `switch`, `class`, `enum` — вне цепочки.
//!
//! Лексика — как у грамматики, а не как у PowerShell: `generic_token` (слово аргумента после
//! пробела) идёт до `& ( ) } | ; ,` и пробела, кавычки и `#` внутри него — буквы.
//! Имя команды — своё правило: кавычки в нём строки. `$( )` в строке — снова код.
//!
//! Там, где лексику решает текст дальше по файлу или трекер потерял уровни (глубже
//! [`LVL_MAX`]), он **теряет след** (`lost`): границ до конца файла нет, остаток идёт одним
//! куском, то есть как полный разбор.

#![allow(
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт; флаги — состояние"
)]

use super::{
    L_ARMS, L_BODY, L_BRACKET, L_INTERP, L_JX_ATTR, L_JX_KID, L_OTHER, L_PAREN, L_ZONE, LVL_MAX,
    Track, pk, word_key,
};
use crate::syntax::lex::{Cx, Lex, Map, St};

const NORMAL: u8 = 0;
/// Слово: имя команды, ключевое слово, имя в выражении.
const WORD: u8 = 1;
/// Слово аргумента (`generic_token`).
const BARE: u8 = 2;
/// Имя в выражении (за `.`, `::`).
const IDENT: u8 = 3;
/// После `$`.
const VAR: u8 = 4;
const VAR_NAME: u8 = 5;
/// `${…}`.
const VAR_BRACE: u8 = 6;
/// После `@`.
const AT: u8 = 7;
/// После `-`.
const DASH: u8 = 8;
const OPTION: u8 = 9;
/// До конца строки: `#`, `--%`.
const LINE: u8 = 10;
/// После `<`.
const LT: u8 = 11;
/// `<# #>`.
const BLOCK: u8 = 12;
const BLOCK_HASH: u8 = 13;
const BLOCK_TICK: u8 = 14;
/// `` ` `` в коде.
const TICK: u8 = 15;
const SQ: u8 = 16;
const DQ: u8 = 17;
const DQ_TICK: u8 = 18;
const DQ_DOLLAR: u8 = 19;
/// За `@"` или `@'`: пробелы до перевода строки (`q` — кавычка).
const HERE_OPEN: u8 = 20;
/// Тело `@" "@`.
const HDQ: u8 = 21;
/// Начало строки тела: `"` и `@` закроют.
const HDQ_LINE: u8 = 22;
const HDQ_QUOTE: u8 = 23;
const HDQ_TICK: u8 = 24;
const HDQ_DOLLAR: u8 = 25;
/// Тело `@' '@`.
const HSQ: u8 = 26;
const HSQ_LINE: u8 = 27;
const HSQ_QUOTE: u8 = 28;
/// За открытием `@'`: пробелы и переводы строк (`\s*` грамматики).
const HSQ_OPEN: u8 = 29;

/// Ждём тело вне цепочки: `switch`, `class`, `enum`.
const WAIT_BLOCK: u8 = 1;
/// Ждём тело функции: имя кончается на `{`, а не как слово аргумента.
const WAIT_FUNC: u8 = 2;

const fn word_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Конец `generic_token`.
const fn bare_end(c: u8) -> bool {
    matches!(
        c,
        b'&' | b'(' | b')' | b'}' | b'|' | b';' | b',' | b' ' | b'\t' | b'\r' | b'\n'
    )
}

/// Конец имени команды.
const fn name_end(c: u8) -> bool {
    bare_end(c) || matches!(c, b'{' | b'"' | b'\'')
}

const fn lower(c: u8) -> u8 {
    c.to_ascii_lowercase()
}

pub(crate) struct Ps {
    t: Track,
    mode: u8,
    /// Кавычка here-строки.
    q: u8,
    first: bool,
    col: u16,
    /// Прошлая строка кончилась значением.
    val: bool,
    /// Прошлый токен начинает список операторов.
    stmt: bool,
    /// Следующее слово — имя команды.
    cmd: bool,
    /// Перед словом пробел или `:`.
    sep: bool,
    /// Прошлая строка продолжена `` ` ``.
    cont: bool,
    /// Прошлый токен — `}`.
    brace: bool,
    /// Уровень многострочной лексемы открыт.
    zone: bool,
    /// Трекер потерял уровни (глубже [`LVL_MAX`]): границ до конца файла нет.
    lost: bool,
    wait: u8,
    word: u64,
    wlen: u32,
    last: u8,
    word_at: u64,
}

impl Ps {
    pub(crate) fn resume(st: St) -> Ps {
        Ps {
            t: Track::resume(&st),
            mode: st.mode,
            q: st.a,
            first: st.d & 1 == 0,
            col: st.b,
            val: st.d & 2 != 0,
            stmt: st.d & 4 == 0,
            cmd: st.d & 8 == 0,
            sep: st.d & 16 != 0,
            cont: st.d & 32 != 0,
            brace: st.d & 64 != 0,
            zone: st.d & 128 != 0,
            lost: st.c & 1 != 0,
            wait: st.ext.pfx,
            word: 0,
            wlen: 0,
            last: 0,
            word_at: 0,
        }
    }

    /// Первый токен строки: `w` — ключ слова, `c` — первый байт.
    fn lead(&mut self, w: u64, c: u8, cx: &mut Cx<'_>) {
        // `param` с начала куска стал бы блоком параметров всего текста.
        const CONT: [u64; 5] = [
            pk("else"),
            pk("elseif"),
            pk("catch"),
            pk("finally"),
            pk("param"),
        ];
        const TAIL: [u64; 2] = [pk("while"), pk("until")];
        if !std::mem::take(&mut self.first) {
            return;
        }
        let cont = std::mem::take(&mut self.cont);
        if cont || self.lost || self.t.bad != 0 || !(self.val || self.stmt) {
            return;
        }
        if self.t.top(cx) != L_BODY {
            return;
        }
        let good = c >= 0x80
            || c.is_ascii_alphanumeric()
            || matches!(c, b'$' | b'"' | b'\'' | b'[' | b'&' | b'@' | b'_');
        if good && !CONT.contains(&w) && !(self.brace && TAIL.contains(&w)) {
            self.t.cand(self.t.line_at, cx);
        }
    }

    /// Токен, после которого строка законченной не считается.
    fn op(&mut self) {
        self.val = false;
        self.stmt = false;
        self.brace = false;
        self.sep = false;
    }

    /// Токен-значение.
    fn value(&mut self) {
        self.val = true;
        self.stmt = false;
        self.brace = false;
        self.sep = false;
        self.cmd = false;
    }

    fn word_end(&mut self, cx: &mut Cx<'_>) {
        const SWITCH: u64 = pk("switch");
        const CLASS: u64 = pk("class");
        const ENUM: u64 = pk("enum");
        const FUNCTION: u64 = pk("function");
        const FILTER: u64 = pk("filter");
        const WORKFLOW: u64 = pk("workflow");
        const PARAM: u64 = pk("param");
        let w = word_key(self.word, self.wlen);
        self.lead(w, self.first_of_word(), cx);
        self.mode = NORMAL;
        self.value();
        if self.last == b':' || self.last == b'=' {
            self.val = false;
        }
        match w {
            SWITCH | CLASS | ENUM | PARAM => self.wait = WAIT_BLOCK,
            FUNCTION | FILTER | WORKFLOW => self.wait = WAIT_FUNC,
            _ => {}
        }
        self.word = 0;
        self.wlen = 0;
    }

    fn first_of_word(&self) -> u8 {
        if self.wlen == 0 { 0 } else { b'a' }
    }

    fn zone_on(&mut self, at: u64, cx: &mut Cx<'_>) {
        if !self.zone {
            self.zone = true;
            self.t.push(cx, at, at, L_ZONE);
        }
    }

    fn zone_off(&mut self, at: u64, cx: &mut Cx<'_>) {
        if std::mem::take(&mut self.zone) {
            self.t.pop(cx, at);
        }
    }

    /// Строка кончилась.
    fn string_end(&mut self, at: u64, cx: &mut Cx<'_>) {
        self.zone_off(at, cx);
        self.mode = NORMAL;
        self.value();
    }

    /// `$(` в строке: снова код; вид уровня помнит, в какую строку вернуться.
    fn sub(&mut self, at: u64, end: u64, kind: u8, cx: &mut Cx<'_>) {
        self.zone = false;
        self.t.push(cx, at, end, kind);
        self.mode = NORMAL;
        self.stmt = true;
        self.val = false;
        self.cmd = true;
    }

    fn close_paren(&mut self, at: u64, cx: &mut Cx<'_>) {
        fn want(k: u8) -> bool {
            matches!(k, L_PAREN | L_INTERP | L_JX_ATTR | L_JX_KID)
        }
        fn through(k: u8) -> bool {
            k == L_BRACKET
        }
        let deep = cx.depth() > LVL_MAX;
        let k = self.t.close(cx, at, want, through);
        self.value();
        match k {
            Some(L_INTERP) => self.mode = DQ,
            Some(L_JX_KID) => {
                self.zone = true;
                self.mode = DQ;
            }
            Some(L_JX_ATTR) => {
                self.zone = true;
                self.mode = HDQ;
            }
            Some(L_OTHER) if deep => self.lost = true,
            _ => {}
        }
    }

    /// Новая строка кода.
    fn newline(&mut self, next: u64) {
        self.first = true;
        self.col = 0;
        self.t.line_at = next;
        self.cmd = true;
        self.sep = false;
    }

    fn open_brace(&mut self, at: u64, end: u64, hash: bool, cx: &mut Cx<'_>) {
        let wait = std::mem::take(&mut self.wait);
        if hash || wait == WAIT_BLOCK {
            let d = cx.depth() + 1;
            self.t.push(cx, at, end, L_ARMS);
            if self.t.bad == 0 {
                self.t.bad = d;
            }
        } else {
            self.t.push(cx, at, end, L_BODY);
        }
        self.val = false;
        self.stmt = true;
        self.brace = false;
        self.cmd = true;
        self.sep = false;
    }

    #[allow(clippy::too_many_lines, reason = "токены одним match")]
    fn normal(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        fn body(k: u8) -> bool {
            matches!(k, L_BODY | L_ARMS)
        }
        fn group(k: u8) -> bool {
            matches!(k, L_PAREN | L_BRACKET)
        }
        let c = b[i];
        let (at, end) = (m.raw(i), m.raw(i + 1));
        match c {
            b'\n' => {
                self.newline(end);
                i + 1
            }
            b' ' | b'\t' | b'\r' | 0x0B | 0x0C => {
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                self.sep = true;
                i + 1
            }
            b'#' => {
                self.mode = LINE;
                i + 1
            }
            b'`' => {
                self.mode = TICK;
                i + 1
            }
            b'<' => {
                self.mode = LT;
                i + 1
            }
            b'$' => {
                self.lead(0, c, cx);
                self.mode = VAR;
                i + 1
            }
            b'@' => {
                self.lead(0, c, cx);
                self.mode = AT;
                i + 1
            }
            b'-' => {
                self.lead(0, c, cx);
                self.mode = DASH;
                i + 1
            }
            b'\'' => {
                self.lead(0, c, cx);
                self.mode = SQ;
                self.value();
                i + 1
            }
            b'"' => {
                self.lead(0, c, cx);
                self.mode = DQ;
                self.value();
                i + 1
            }
            b'(' => {
                self.lead(0, c, cx);
                self.t.push(cx, at, end, L_PAREN);
                self.op();
                self.cmd = true;
                i + 1
            }
            b')' => {
                self.lead(0, c, cx);
                self.close_paren(at, cx);
                i + 1
            }
            b'[' => {
                self.lead(0, c, cx);
                self.t.push(cx, at, end, L_BRACKET);
                self.op();
                self.cmd = false;
                i + 1
            }
            b']' => {
                self.lead(0, c, cx);
                self.t.close(cx, at, |k| k == L_BRACKET, |k| k == L_PAREN);
                // Атрибут над функцией или `param`: со следующей строки — тот же элемент.
                self.op();
                self.cmd = true;
                i + 1
            }
            b'{' => {
                self.lead(0, c, cx);
                self.open_brace(at, end, false, cx);
                i + 1
            }
            b'}' => {
                self.lead(0, c, cx);
                self.t.close(cx, at, body, group);
                self.value();
                self.brace = true;
                // `} else{`: слово за скобкой кончается на `{`.
                self.cmd = true;
                i + 1
            }
            b';' => {
                self.lead(0, c, cx);
                self.op();
                self.stmt = true;
                self.cmd = true;
                i + 1
            }
            b'|' | b'&' | b'=' | b',' => {
                self.lead(0, c, cx);
                self.op();
                self.cmd = c != b',';
                i + 1
            }
            b'.' | b'+' | b'*' | b'/' | b'%' | b'>' | b'!' | b'?' => {
                self.lead(0, c, cx);
                self.op();
                i + 1
            }
            b':' => {
                self.lead(0, c, cx);
                self.op();
                self.sep = true;
                i + 1
            }
            _ => {
                self.word = 0;
                self.wlen = 0;
                self.word_at = at;
                self.mode = if self.cmd || self.wait != 0 {
                    WORD
                } else if self.sep {
                    BARE
                } else {
                    IDENT
                };
                i
            }
        }
    }
}

impl Lex for Ps {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let mut st = St {
            mode: self.mode,
            a: self.q,
            b: self.col,
            c: u32::from(self.lost),
            d: u8::from(!self.first)
                | u8::from(self.val) << 1
                | u8::from(!self.stmt) << 2
                | u8::from(!self.cmd) << 3
                | u8::from(self.sep) << 4
                | u8::from(self.cont) << 5
                | u8::from(self.brace) << 6
                | u8::from(self.zone) << 7,
            ..St::default()
        };
        st.ext.pfx = self.wait;
        self.t.save(&mut st, at);
        st
    }

    fn can_mark(&self) -> bool {
        !matches!(self.mode, WORD | BARE | IDENT)
    }

    #[allow(clippy::too_many_lines, reason = "автомат: режимы одним match")]
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if b.is_empty() {
            return;
        }
        self.t.begin(m.raw(0), m.unit());
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            i = match self.mode {
                NORMAL => self.normal(b, i, m, cx),
                WORD => {
                    let mut j = i;
                    while j < b.len() && !name_end(b[j]) {
                        self.word = self.word << 8 | u64::from(lower(b[j]));
                        self.last = b[j];
                        self.wlen = self.wlen.saturating_add(1);
                        j += 1;
                    }
                    if j < b.len() {
                        self.word_end(cx);
                    }
                    j
                }
                BARE => {
                    let mut j = i;
                    while j < b.len() && !bare_end(b[j]) {
                        self.last = b[j];
                        self.wlen = self.wlen.saturating_add(1);
                        j += 1;
                    }
                    if j < b.len() {
                        self.wlen = 0;
                        self.word_end(cx);
                    }
                    j
                }
                IDENT => {
                    let mut j = i;
                    while j < b.len() && word_char(b[j]) {
                        self.last = b[j];
                        j += 1;
                    }
                    if j == i && i < b.len() {
                        // Не буква и не лексема: один байт.
                        j = i + 1;
                        self.last = c;
                    }
                    if j < b.len() || j > i {
                        self.lead(0, b'.', cx);
                        self.mode = NORMAL;
                        self.value();
                        self.val = word_char(self.last);
                    }
                    j
                }
                VAR => match c {
                    b'(' => {
                        self.t.push(cx, m.raw(i), m.raw(i + 1), L_PAREN);
                        self.mode = NORMAL;
                        self.op();
                        self.stmt = true;
                        self.cmd = true;
                        i + 1
                    }
                    b'{' => {
                        self.mode = VAR_BRACE;
                        i + 1
                    }
                    b'$' | b'^' | b'?' => {
                        self.mode = NORMAL;
                        self.value();
                        i + 1
                    }
                    _ => {
                        self.mode = VAR_NAME;
                        i
                    }
                },
                VAR_NAME => {
                    if word_char(c) || c == b':' || c == b'?' {
                        i + 1
                    } else {
                        self.mode = NORMAL;
                        self.value();
                        i
                    }
                }
                VAR_BRACE => match memchr::memchr(b'}', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = NORMAL;
                        self.value();
                        i + k + 1
                    }
                },
                AT => match c {
                    b'(' => {
                        self.t.push(cx, m.raw(i), m.raw(i + 1), L_PAREN);
                        self.mode = NORMAL;
                        self.op();
                        self.stmt = true;
                        self.cmd = true;
                        i + 1
                    }
                    b'{' => {
                        self.open_brace(m.raw(i), m.raw(i + 1), true, cx);
                        self.mode = NORMAL;
                        i + 1
                    }
                    b'"' | b'\'' => {
                        self.q = c;
                        self.mode = HERE_OPEN;
                        i + 1
                    }
                    _ => {
                        self.mode = VAR_NAME;
                        i
                    }
                },
                HERE_OPEN => match c {
                    b' ' | b'\t' | b'\r' => i + 1,
                    b'\n' => {
                        // Сразу за открытием `"@` не закрывает: перед ним нужен свой перевод.
                        self.zone_on(m.raw(i), cx);
                        self.mode = if self.q == b'"' { HDQ } else { HSQ_OPEN };
                        self.q = 0;
                        i + 1
                    }
                    _ => {
                        // Не here-строка: обычная кавычка.
                        self.mode = if self.q == b'"' { DQ } else { SQ };
                        self.q = 0;
                        self.value();
                        i
                    }
                },
                DASH => {
                    if c == b'-' {
                        self.mode = OPTION;
                        self.wlen = 1;
                        i + 1
                    } else {
                        self.wlen = 0;
                        self.mode = OPTION;
                        i
                    }
                }
                OPTION => {
                    if c == b'%' && self.wlen == 1 {
                        self.wlen = 0;
                        self.mode = LINE;
                        i + 1
                    } else if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'?' | b'-' | b'`') {
                        self.wlen = 2;
                        i + 1
                    } else {
                        self.wlen = 0;
                        self.mode = NORMAL;
                        self.op();
                        i
                    }
                }
                LINE => match memchr::memchr2(b'\n', b'\r', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = NORMAL;
                        i + k
                    }
                },
                LT => {
                    if c == b'#' {
                        self.mode = BLOCK;
                        i + 1
                    } else {
                        self.lead(0, b'<', cx);
                        self.op();
                        self.mode = NORMAL;
                        i
                    }
                }
                BLOCK => match b[i..].iter().position(|c| matches!(c, b'#' | b'`' | b'\n')) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        match b[j] {
                            b'#' => self.mode = BLOCK_HASH,
                            b'`' => self.mode = BLOCK_TICK,
                            _ => self.zone_on(m.raw(j), cx),
                        }
                        j + 1
                    }
                },
                BLOCK_HASH => match c {
                    b'>' => {
                        self.zone_off(m.raw(i), cx);
                        self.mode = NORMAL;
                        i + 1
                    }
                    b'#' => i + 1,
                    _ => {
                        self.mode = BLOCK;
                        i
                    }
                },
                BLOCK_TICK => {
                    self.mode = BLOCK;
                    if c == b'\n' {
                        self.zone_on(m.raw(i), cx);
                    }
                    i + 1
                }
                TICK => match c {
                    b'\r' => i + 1,
                    b'\n' => {
                        self.newline(m.raw(i + 1));
                        self.cont = true;
                        self.mode = NORMAL;
                        i + 1
                    }
                    _ => {
                        self.lead(0, b'`', cx);
                        self.value();
                        self.mode = NORMAL;
                        i + 1
                    }
                },
                SQ => match memchr::memchr2(b'\'', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'\'' {
                            self.string_end(m.raw(j), cx);
                        } else {
                            self.zone_on(m.raw(j), cx);
                        }
                        j + 1
                    }
                },
                DQ => {
                    match b[i..]
                        .iter()
                        .position(|c| matches!(c, b'"' | b'`' | b'$' | b'\n' | b'<'))
                    {
                        None => b.len(),
                        Some(k) => {
                            let j = i + k;
                            match b[j] {
                                b'"' => self.string_end(m.raw(j), cx),
                                b'`' => self.mode = DQ_TICK,
                                b'$' => self.mode = DQ_DOLLAR,
                                // Комментарий `<# #>` грамматика допускает и в строке, если
                                // `#>` есть где-то ниже: это решает конец файла.
                                b'<' => self.lost |= b.get(j + 1).is_none_or(|c| *c == b'#'),
                                _ => self.zone_on(m.raw(j), cx),
                            }
                            j + 1
                        }
                    }
                }
                DQ_TICK => {
                    self.mode = DQ;
                    if c == b'\n' {
                        self.zone_on(m.raw(i), cx);
                    }
                    i + 1
                }
                DQ_DOLLAR => {
                    if c == b'(' {
                        let kind = if self.zone { L_JX_KID } else { L_INTERP };
                        self.sub(m.raw(i), m.raw(i + 1), kind, cx);
                        i + 1
                    } else {
                        self.mode = DQ;
                        i
                    }
                }
                HDQ => match b[i..].iter().position(|c| matches!(c, b'`' | b'$' | b'\n')) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        self.mode = match b[j] {
                            b'`' => HDQ_TICK,
                            b'$' => HDQ_DOLLAR,
                            _ => HDQ_LINE,
                        };
                        j + 1
                    }
                },
                HDQ_LINE => match c {
                    b'"' => {
                        self.mode = HDQ_QUOTE;
                        i + 1
                    }
                    b'\r' | b'\n' => i + 1,
                    _ => {
                        self.mode = HDQ;
                        i
                    }
                },
                HDQ_QUOTE => {
                    if c == b'@' {
                        self.string_end(m.raw(i), cx);
                        i + 1
                    } else {
                        self.mode = HDQ;
                        i
                    }
                }
                HDQ_TICK => {
                    if c != b'\r' {
                        self.mode = HDQ;
                    }
                    i + 1
                }
                HDQ_DOLLAR => {
                    if c == b'(' {
                        self.sub(m.raw(i), m.raw(i + 1), L_JX_ATTR, cx);
                        i + 1
                    } else {
                        self.mode = HDQ;
                        i
                    }
                }
                HSQ_OPEN => {
                    if matches!(c, b' ' | b'\t' | b'\r' | b'\n') {
                        i + 1
                    } else {
                        self.mode = HSQ;
                        i
                    }
                }
                HSQ => match memchr::memchr(b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = HSQ_LINE;
                        i + k + 1
                    }
                },
                HSQ_LINE => match c {
                    b'\'' => {
                        self.mode = HSQ_QUOTE;
                        i + 1
                    }
                    b'\r' | b'\n' => i + 1,
                    _ => {
                        self.mode = HSQ;
                        i
                    }
                },
                _ => {
                    // HSQ_QUOTE.
                    if c == b'@' {
                        self.string_end(m.raw(i), cx);
                        i + 1
                    } else {
                        self.mode = HSQ;
                        i
                    }
                }
            };
        }
    }

    fn flush(&mut self, _end: u64, eof: bool, cx: &mut Cx<'_>) {
        if eof && matches!(self.mode, WORD | BARE | IDENT) {
            self.word_end(cx);
        }
    }
}
