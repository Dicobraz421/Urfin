//! Трекер Bash (ADR-0024 §8). Кандидат — начало строки в списке операторов (корень, `{ }`,
//! тело `then`, тело `do`) после законченной команды. Лексика — как у `tree-sitter-bash`:
//! heredoc (конец — строка, первое непробельное которой начинается с разделителя),
//! `$( )`, `${ }`, `$(( ))`, обратные кавычки, кавычки внутри них, `case`, `[[ ]]`, массивы.
//!
//! Уровни строгие: всё, что открыто, закрывается своим словом. Несовпадение значит, что
//! трекер не понял текст (или в тексте ошибка), и он **теряет след** (`lost`): границ до конца
//! файла нет, остаток идёт одним куском, то есть как полный разбор. Так же — там, где лексика
//! зависит от того, что здесь не повторено (`'` в `${ }` внутри строки, `#` в начале слова в
//! `[[ ]]`, второй heredoc на строке, глубже [`LVL_MAX`]). Цвет от этого не меняется, только
//! размер куска.
//!
//! `if`, `while`, `for` до `then` и `do` — уровень условия вне цепочки. Ветви `elif`, `else`,
//! `case`, скобки, подстановки — вне цепочки: внутри них границ нет.

#![allow(
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт; флаги — состояние"
)]

use super::{
    L_ARMS, L_BODY, L_BRACKET, L_CLASS, L_INTERP, L_JSX, L_JX_ATTR, L_JX_KID, L_LIST, L_OTHER,
    L_PAREN_CTL, L_ZONE, LVL_MAX, Track, pk, word_key,
};
use crate::syntax::lex::{Cx, Lex, Map, St};

// Виды уровней (свои значения общих констант).
const PAREN: u8 = L_JX_ATTR;
const ARITH: u8 = L_JX_KID;
const EXP: u8 = L_INTERP;
const BT: u8 = L_JSX;
const TEST: u8 = L_OTHER;
const ARRAY: u8 = L_BRACKET;
const CASE: u8 = L_CLASS;
const COND: u8 = L_PAREN_CTL;

// Режимы.
const NORMAL: u8 = 0;
const COMMENT: u8 = 1;
const ESC: u8 = 2;
const SQ: u8 = 3;
const DQ: u8 = 4;
const DQ_ESC: u8 = 5;
/// После `$` (откуда — `dctx`).
const DOLLAR: u8 = 6;
/// После `$(`.
const DOLLAR_P: u8 = 7;
/// `$'…'`.
const ANSI: u8 = 8;
/// `${ }`.
const EXPAND: u8 = 10;
const EXP_ESC: u8 = 11;
/// `$(( ))`, `(( ))`.
const MATH: u8 = 12;
const MATH_ESC: u8 = 13;
/// После `(` у начала команды: `((`?
const LP: u8 = 14;
const SEMI: u8 = 15;
const AMP: u8 = 16;
const PIPE: u8 = 17;
const LT: u8 = 18;
const GT: u8 = 19;
/// После `<<`.
const LT2: u8 = 20;
const HD_WS: u8 = 21;
const HD_WORD: u8 = 22;
const HD_ESC: u8 = 23;
const HD_QUOTE: u8 = 24;
const HD_Q_ESC: u8 = 25;
/// Тело heredoc.
const BODY: u8 = 26;
const BODY_ESC: u8 = 27;
const BODY_D: u8 = 28;

// Куда вернуться с уровня.
const R_NORMAL: u8 = 0;
/// Строка, начатая в коде, в `${ }`, в арифметике.
const R_DQ: u8 = 1;
const R_DQ_EXP: u8 = 2;
const R_DQ_MATH: u8 = 3;
const R_HERE: u8 = 4;
const R_MATH: u8 = 5;
const R_EXP: u8 = 6;
/// Подоболочка: за `)` слова нет.
const R_SUB: u8 = 7;
/// Строка с открытым уровнем многострочной лексемы.
const R_ZONE: u8 = 8;

const FNV: u64 = 0x0000_0100_0000_01B3;
const OFFSET: u64 = 0xCBF2_9CE4_8422_2325;

const IF: u64 = pk("if");
const THEN: u64 = pk("then");
const ELIF: u64 = pk("elif");
const ELSE: u64 = pk("else");
const FI: u64 = pk("fi");
const FOR: u64 = pk("for");
const SELECT: u64 = pk("select");
const WHILE: u64 = pk("while");
const UNTIL: u64 = pk("until");
const DO: u64 = pk("do");
const DONE: u64 = pk("done");
const CASE_W: u64 = pk("case");
const ESAC: u64 = pk("esac");
const IN: u64 = pk("in");
const FUNCTION: u64 = pk("function");
const OPEN: u64 = pk("{");
const CLOSE: u64 = pk("}");
const TEST_OPEN: u64 = pk("[[");
const TEST_CLOSE: u64 = pk("]]");
const BANG: u64 = pk("!");

pub(crate) struct Bash {
    t: Track,
    mode: u8,
    /// Кавычка разделителя heredoc.
    q: u8,
    first: bool,
    col: u16,
    /// Слово начато.
    in_word: bool,
    /// Слово из простых букв: может быть ключевым.
    plain: bool,
    /// Следующее слово — начало команды.
    cmd: bool,
    /// Прошлый токен требует продолжения (`|`, `&&`, `||`).
    op: bool,
    /// Строка продолжена `\`.
    cont: bool,
    /// За `f()` или `function f`: тело ещё впереди.
    fn_wait: bool,
    /// Прошлое слово — имя команды: `(` за ним — функция.
    was_cmd: bool,
    /// `(` сразу за именем команды, внутри пока пусто.
    fn_paren: bool,
    /// За `function`.
    func: bool,
    wait_in: bool,
    wait_do: bool,
    /// Образец `case`.
    pat: bool,
    /// Следующее слово — цель перенаправления.
    redir: bool,
    /// Уровень многострочной лексемы открыт.
    zone: bool,
    lost: bool,
    /// Разделитель heredoc ждёт конца строки.
    pend: bool,
    raw: bool,
    /// Откуда `$` и откуда текущая строка.
    dctx: u8,
    sret: u8,
    dret: u8,
    /// В слове есть `=`.
    eq: bool,
    /// Присваиваний в строке (до двух) и строка из них одних.
    la: u8,
    mixed: bool,
    /// Строка из двух присваиваний и строки присваиваний за ней. Грамматика пропускает здесь
    /// перевод строки (`variable_assignments`, `command`): следующая строка для неё —
    /// продолжение, даже если это `if`. Кандидата в такой цепочке нет.
    chain: bool,
    /// Куда вернуться с уровня: 4 бита на уровень `1..=LVL_MAX`.
    rets: [u64; 2],
    /// Разделитель: хеш и длина; разбираемая строка: сколько сравнено и хеш.
    hash: u64,
    dlen: u32,
    head: bool,
    n: u32,
    h: u64,
    word: u64,
    wlen: u32,
    word_at: u64,
    last: u8,
}

impl Bash {
    pub(crate) fn resume(st: St) -> Bash {
        let f = st.c;
        Bash {
            t: Track::resume(&st),
            mode: st.mode,
            q: st.a,
            first: f & 1 == 0,
            col: st.b,
            in_word: f & 2 != 0,
            plain: f & 4 != 0,
            cmd: f & 8 == 0,
            op: f & 16 != 0,
            cont: f & 32 != 0,
            fn_wait: f & 64 != 0,
            was_cmd: f & 128 != 0,
            fn_paren: f & 256 != 0,
            func: f & 512 != 0,
            wait_in: f & 1024 != 0,
            wait_do: f & 2048 != 0,
            pat: f & 4096 != 0,
            redir: f & 8192 != 0,
            zone: f & 16384 != 0,
            lost: f & 32768 != 0,
            eq: st.ext.cond & 16 != 0,
            la: st.ext.cond & 3,
            mixed: st.ext.cond & 4 != 0,
            chain: st.ext.cond & 8 != 0,
            pend: f & 65536 != 0,
            raw: f & 131_072 != 0,
            head: f & 262_144 != 0,
            dctx: st.ext.pc,
            sret: st.ext.prev,
            dret: st.ext.hdr,
            rets: st.ext.kinds2,
            hash: st.ext.fs,
            dlen: st.ext.k,
            n: st.ext.rh,
            h: 0,
            word: 0,
            wlen: 0,
            word_at: 0,
            last: st.ext.hdr2,
        }
    }

    fn ret_of(&self, lvl: u64) -> u8 {
        if lvl == 0 || lvl > LVL_MAX {
            return R_NORMAL;
        }
        let i = (lvl - 1) as usize;
        #[allow(clippy::cast_possible_truncation, reason = "4 бита")]
        {
            (self.rets[i / 16] >> (i % 16 * 4) & 0xF) as u8
        }
    }

    fn set_ret(&mut self, lvl: u64, r: u8) {
        if lvl == 0 || lvl > LVL_MAX {
            return;
        }
        let i = (lvl - 1) as usize;
        let s = i % 16 * 4;
        self.rets[i / 16] = self.rets[i / 16] & !(0xF << s) | u64::from(r) << s;
    }

    /// Кавычка кончилась в коде: слово продолжается.
    fn in_code(&mut self) -> u8 {
        self.in_word = true;
        self.plain = false;
        NORMAL
    }

    /// Открыть уровень; `r` — куда вернуться.
    fn enter(&mut self, at: u64, end: u64, kind: u8, r: u8, cx: &mut Cx<'_>) {
        let d = cx.depth() + 1;
        if d >= LVL_MAX {
            self.lost = true;
        }
        self.set_ret(d, r);
        self.t.push(cx, at, end, kind);
        if matches!(kind, ARRAY | CASE | COND) && self.t.bad == 0 {
            self.t.bad = d;
        }
    }

    /// Закрыть верхний уровень; итог — куда вернуться.
    fn leave(&mut self, at: u64, cx: &mut Cx<'_>) -> u8 {
        let r = self.ret_of(cx.depth());
        self.t.pop(cx, at);
        r
    }

    /// Вернуться в режим `r`.
    fn resume_mode(&mut self, r: u8, cx: &mut Cx<'_>) {
        let _ = cx;
        self.zone = r & R_ZONE != 0;
        match r & !R_ZONE {
            R_DQ => {
                self.dret = R_NORMAL;
                self.mode = DQ;
            }
            R_DQ_EXP => {
                self.dret = R_EXP;
                self.mode = DQ;
            }
            R_DQ_MATH => {
                self.dret = R_MATH;
                self.mode = DQ;
            }
            R_HERE => {
                self.head = false;
                self.mode = BODY;
            }
            R_MATH => self.mode = MATH,
            R_EXP => self.mode = EXPAND,
            R_SUB => {
                self.mode = NORMAL;
                self.in_word = false;
                self.cmd = false;
            }
            _ => {
                self.mode = NORMAL;
                self.in_word = true;
                self.plain = false;
                self.cmd = false;
            }
        }
    }

    fn top(&self, cx: &Cx<'_>) -> u8 {
        self.t.top(cx)
    }

    /// Первый токен строки (`w` — ключ слова, 0 — не простое слово).
    fn lead(&mut self, w: u64, cx: &mut Cx<'_>) {
        const NOT: [u64; 10] = [THEN, DO, FI, DONE, ELSE, ELIF, ESAC, CLOSE, IN, TEST_CLOSE];
        if !std::mem::take(&mut self.first) {
            return;
        }
        let cont = std::mem::take(&mut self.cont);
        if cont
            || self.lost
            || self.chain
            // Со строки присваиваний кусок не начинается: она не всегда конец оператора.
            || self.eq
            || self.t.bad != 0
            || self.op
            || self.fn_wait
            || self.func
            || self.wait_in
            || self.wait_do
            || self.pend
            || !self.cmd
            || NOT.contains(&w)
        {
            return;
        }
        if matches!(self.t.top(cx), L_BODY | L_LIST | L_ARMS) {
            self.t.cand(self.t.line_at, cx);
        }
    }

    /// Начать слово байтом `c`.
    fn word_start(&mut self, at: u64) {
        self.in_word = true;
        self.plain = true;
        self.word = 0;
        self.wlen = 0;
        self.word_at = at;
    }

    /// В слове не только буквы: ключевым не будет.
    fn deplain(&mut self, at: u64, cx: &mut Cx<'_>) {
        if !self.in_word {
            self.word_start(at);
            self.plain = false;
            self.lead(0, cx);
        } else if self.plain {
            self.plain = false;
            self.lead(0, cx);
        }
        self.was_cmd = false;
        self.fn_paren = false;
    }

    /// Закрыть уровень `want` словом в `at`.
    fn close(&mut self, want: u8, at: u64, cx: &mut Cx<'_>) {
        if cx.depth() > 0 && self.top(cx) == want {
            self.t.pop(cx, at);
        } else {
            self.lost = true;
        }
        self.cmd = false;
    }

    #[allow(clippy::too_many_lines, reason = "ключевые слова одним match")]
    fn word_end(&mut self, end: u64, cx: &mut Cx<'_>) {
        if !std::mem::take(&mut self.in_word) {
            return;
        }
        let w = if self.plain {
            word_key(self.word, self.wlen)
        } else {
            0
        };
        if self.plain {
            self.lead(w, cx);
        }
        if std::mem::take(&mut self.eq) {
            self.la = (self.la + 1).min(2);
        } else {
            self.mixed = true;
        }
        self.op = false;
        let fn_wait = std::mem::take(&mut self.fn_wait);
        if std::mem::take(&mut self.redir) {
            return;
        }
        if self.top(cx) == TEST {
            if w == TEST_CLOSE {
                self.t.pop(cx, self.word_at);
                self.cmd = false;
            }
            return;
        }
        if self.wait_in {
            if w == IN {
                self.wait_in = false;
                self.enter(self.word_at, end, CASE, R_NORMAL, cx);
                self.pat = true;
                self.cmd = false;
            }
            return;
        }
        if self.pat {
            if w == ESAC {
                self.pat = false;
                self.close(CASE, self.word_at, cx);
            }
            return;
        }
        if self.func {
            self.func = false;
            self.fn_wait = true;
            self.cmd = true;
            return;
        }
        if self.wait_do && w == IN {
            self.wait_do = false;
            return;
        }
        if !self.cmd && (!self.wait_do || w != DO) {
            return;
        }
        self.was_cmd = false;
        match w {
            IF | WHILE | UNTIL => {
                self.enter(self.word_at, end, COND, R_NORMAL, cx);
            }
            FOR | SELECT => {
                self.enter(self.word_at, end, COND, R_NORMAL, cx);
                self.wait_do = true;
                self.cmd = false;
            }
            THEN => {
                let top = self.top(cx);
                if top == COND {
                    self.t.pop(cx, self.word_at);
                    self.enter(self.word_at, end, L_LIST, R_NORMAL, cx);
                } else if top != L_LIST {
                    self.lost = true;
                }
            }
            ELIF | ELSE => {
                // Ветвь идёт целиком: начала `if` в заголовке нет.
                if self.top(cx) == L_LIST {
                    if self.t.bad == 0 {
                        self.t.bad = cx.depth();
                    }
                } else {
                    self.lost = true;
                }
            }
            FI => self.close(L_LIST, self.word_at, cx),
            DO => {
                self.wait_do = false;
                if self.top(cx) == COND {
                    self.t.pop(cx, self.word_at);
                    self.enter(self.word_at, end, L_ARMS, R_NORMAL, cx);
                } else {
                    self.lost = true;
                }
                self.cmd = true;
            }
            DONE => self.close(L_ARMS, self.word_at, cx),
            CASE_W => {
                self.wait_in = true;
                self.cmd = false;
            }
            ESAC => self.close(CASE, self.word_at, cx),
            FUNCTION => {
                self.func = true;
                self.cmd = false;
            }
            OPEN => self.enter(self.word_at, end, L_BODY, R_NORMAL, cx),
            CLOSE => self.close(L_BODY, self.word_at, cx),
            TEST_OPEN => {
                self.enter(self.word_at, end, TEST, R_NORMAL, cx);
                self.cmd = false;
            }
            BANG => {}
            _ => {
                self.cmd = false;
                self.was_cmd = !fn_wait;
            }
        }
    }

    fn newline(&mut self, next: u64, cx: &Cx<'_>) {
        if self.mixed {
            self.chain = false;
        } else if self.la == 2 || (self.la == 1 && self.chain) {
            self.chain = true;
        }
        self.la = 0;
        self.mixed = false;
        self.first = true;
        self.col = 0;
        self.t.line_at = next;
        if self.top(cx) != ARRAY && !self.pat && !self.wait_in {
            self.cmd = true;
        }
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

    /// Код возврата в текущую строку.
    fn dq_ret(&self) -> u8 {
        let r = match self.dret {
            R_EXP => R_DQ_EXP,
            R_MATH => R_DQ_MATH,
            _ => R_DQ,
        };
        if self.zone { r | R_ZONE } else { r }
    }

    /// Войти на уровень из строки: уровень лексемы остаётся ниже.
    fn leave_dq(&mut self) -> u8 {
        let r = self.dq_ret();
        self.zone = false;
        r
    }

    /// `(` в коде.
    #[allow(clippy::too_many_lines, reason = "все формы скобки")]
    fn open_paren(&mut self, c_at: u64, end: u64, cx: &mut Cx<'_>) {
        let top = self.top(cx);
        if top == TEST {
            return;
        }
        if self.in_word {
            match self.last {
                b'=' => {
                    self.deplain(c_at, cx);
                    self.enter(c_at, end, ARRAY, R_NORMAL, cx);
                    self.cmd = false;
                    self.in_word = false;
                    return;
                }
                b'@' | b'?' | b'+' | b'*' | b'!' => {
                    self.deplain(c_at, cx);
                    self.enter(c_at, end, PAREN, R_NORMAL, cx);
                    self.in_word = false;
                    self.cmd = false;
                    return;
                }
                _ => self.word_end(c_at, cx),
            }
        } else if self.first {
            self.lead(0, cx);
        }
        if self.pat && top == CASE {
            return;
        }
        self.fn_paren = std::mem::take(&mut self.was_cmd) || self.fn_wait;
        if self.cmd && !self.fn_paren {
            self.mode = LP;
            return;
        }
        self.enter(c_at, end, PAREN, R_SUB, cx);
        self.cmd = true;
        self.op = false;
    }

    fn close_paren(&mut self, at: u64, cx: &mut Cx<'_>) {
        self.word_end(at, cx);
        match self.top(cx) {
            PAREN => {
                let fun = std::mem::take(&mut self.fn_paren);
                let r = self.leave(at, cx);
                self.resume_mode(r, cx);
                if fun {
                    self.fn_wait = true;
                    self.cmd = true;
                }
            }
            ARRAY => {
                let r = self.leave(at, cx);
                self.resume_mode(r, cx);
            }
            CASE => {
                if self.pat {
                    self.pat = false;
                    self.cmd = true;
                } else {
                    self.lost = true;
                }
            }
            TEST => {}
            _ => self.lost = true,
        }
    }

    #[allow(clippy::too_many_lines, reason = "токены одним match")]
    fn normal(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        let c = b[i];
        let (at, end) = (m.raw(i), m.raw(i + 1));
        match c {
            b'\n' => {
                self.word_end(at, cx);
                self.fn_paren = false;
                if self.pend {
                    self.pend = false;
                    self.zone_on(at, cx);
                    self.mode = BODY;
                    self.head = true;
                    self.n = 0;
                } else {
                    self.newline(end, cx);
                }
                i + 1
            }
            b' ' | b'\t' | b'\r' | 0x0B | 0x0C => {
                self.word_end(at, cx);
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                i + 1
            }
            b'#' if !self.in_word => {
                if self.top(cx) == TEST {
                    self.lost = true;
                }
                self.mode = COMMENT;
                i + 1
            }
            b'\\' => {
                self.deplain(at, cx);
                self.mode = ESC;
                i + 1
            }
            b'\'' => {
                self.deplain(at, cx);
                self.sret = R_NORMAL;
                self.mode = SQ;
                i + 1
            }
            b'"' => {
                self.deplain(at, cx);
                self.dret = R_NORMAL;
                self.mode = DQ;
                i + 1
            }
            b'$' => {
                self.deplain(at, cx);
                self.dctx = R_NORMAL;
                self.mode = DOLLAR;
                i + 1
            }
            b'`' => {
                if self.top(cx) == BT {
                    self.word_end(at, cx);
                    let r = self.leave(at, cx);
                    self.resume_mode(r, cx);
                } else {
                    self.deplain(at, cx);
                    self.enter(at, end, BT, R_NORMAL, cx);
                    self.in_word = false;
                    self.cmd = true;
                }
                i + 1
            }
            b'(' => {
                self.open_paren(at, end, cx);
                i + 1
            }
            b')' => {
                self.first = false;
                self.close_paren(at, cx);
                i + 1
            }
            b';' => {
                self.word_end(at, cx);
                self.first = false;
                self.mixed = true;
                self.mode = SEMI;
                i + 1
            }
            b'&' => {
                self.word_end(at, cx);
                self.first = false;
                self.mixed = true;
                self.mode = AMP;
                i + 1
            }
            b'|' => {
                self.word_end(at, cx);
                self.first = false;
                self.mixed = true;
                self.mode = PIPE;
                i + 1
            }
            b'<' | b'>' if self.top(cx) != TEST => {
                self.word_end(at, cx);
                if self.first {
                    self.lead(0, cx);
                }
                self.mode = if c == b'<' { LT } else { GT };
                i + 1
            }
            b'<' | b'>' => {
                self.word_end(at, cx);
                i + 1
            }
            _ => {
                if self.in_word {
                    self.was_cmd = false;
                } else {
                    self.word_start(at);
                }
                self.fn_paren = false;
                if self.plain {
                    self.word = self.word << 8 | u64::from(c);
                    self.wlen = self.wlen.saturating_add(1);
                }
                self.eq |= c == b'=';
                self.last = c;
                i + 1
            }
        }
    }

    /// Разделитель heredoc прочитан.
    fn hd_end(&mut self) {
        if self.dlen == 0 {
            self.lost = true;
        } else {
            self.pend = true;
        }
        self.mode = NORMAL;
    }

    fn hd_byte(&mut self, c: u8) {
        self.hash = (self.hash ^ u64::from(c)).wrapping_mul(FNV);
        self.dlen = self.dlen.saturating_add(1);
    }

    /// Конец heredoc: первое непробельное строки начинается с разделителя.
    fn body_end(&mut self, end: u64, cx: &mut Cx<'_>) {
        self.zone_off(end, cx);
        self.head = false;
        self.mode = NORMAL;
        self.first = false;
        self.op = false;
        self.cmd = false;
    }
}

/// Пробел `iswspace`.
const fn space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x0B | 0x0C)
}

impl Lex for Bash {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let f = u32::from(!self.first)
            | u32::from(self.in_word) << 1
            | u32::from(self.plain) << 2
            | u32::from(!self.cmd) << 3
            | u32::from(self.op) << 4
            | u32::from(self.cont) << 5
            | u32::from(self.fn_wait) << 6
            | u32::from(self.was_cmd) << 7
            | u32::from(self.fn_paren) << 8
            | u32::from(self.func) << 9
            | u32::from(self.wait_in) << 10
            | u32::from(self.wait_do) << 11
            | u32::from(self.pat) << 12
            | u32::from(self.redir) << 13
            | u32::from(self.zone) << 14
            | u32::from(self.lost) << 15
            | u32::from(self.pend) << 16
            | u32::from(self.raw) << 17
            | u32::from(self.head) << 18;
        let mut st = St {
            mode: self.mode,
            a: self.q,
            b: self.col,
            c: f,
            ..St::default()
        };
        st.ext.pc = self.dctx;
        st.ext.prev = self.sret;
        st.ext.hdr = self.dret;
        st.ext.hdr2 = self.last;
        st.ext.cond = self.la
            | u8::from(self.mixed) << 2
            | u8::from(self.chain) << 3
            | u8::from(self.eq) << 4;
        st.ext.kinds2 = self.rets;
        st.ext.fs = self.hash;
        st.ext.k = self.dlen;
        st.ext.rh = self.n;
        self.t.save(&mut st, at);
        st
    }

    #[allow(clippy::nonminimal_bool, reason = "условия по смыслу")]
    fn can_mark(&self) -> bool {
        !(self.in_word && self.plain)
            && !matches!(
                self.mode,
                LP | SEMI
                    | AMP
                    | PIPE
                    | LT
                    | GT
                    | LT2
                    | HD_WS
                    | HD_WORD
                    | HD_ESC
                    | HD_QUOTE
                    | HD_Q_ESC
                    | DOLLAR
                    | DOLLAR_P
                    | BODY_D
            )
            && !(self.mode == BODY && self.head && self.n > 0)
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
                COMMENT => match memchr::memchr(b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = NORMAL;
                        i + k
                    }
                },
                ESC => {
                    self.mode = NORMAL;
                    if c == b'\r' {
                        self.mode = ESC;
                    } else if c == b'\n' {
                        // Продолжение строки: слово не кончилось, строка новая.
                        self.first = true;
                        self.cont = true;
                        self.col = 0;
                        self.t.line_at = m.raw(i + 1);
                    }
                    i + 1
                }
                SQ => match memchr::memchr2(b'\'', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'\'' {
                            self.zone_off(m.raw(j), cx);
                            self.mode = match self.sret {
                                R_EXP => EXPAND,
                                R_MATH => MATH,
                                _ => self.in_code(),
                            };
                        } else {
                            self.zone_on(m.raw(j), cx);
                        }
                        j + 1
                    }
                },
                DQ => {
                    match b[i..]
                        .iter()
                        .position(|c| matches!(c, b'"' | b'\\' | b'$' | b'`' | b'\n'))
                    {
                        None => b.len(),
                        Some(k) => {
                            let j = i + k;
                            let at = m.raw(j);
                            match b[j] {
                                b'"' => {
                                    self.zone_off(at, cx);
                                    self.mode = match self.dret {
                                        R_EXP => EXPAND,
                                        R_MATH => MATH,
                                        _ => self.in_code(),
                                    };
                                }
                                b'\\' => self.mode = DQ_ESC,
                                b'$' => {
                                    self.dctx = R_DQ;
                                    self.mode = DOLLAR;
                                }
                                b'`' => {
                                    let r = self.leave_dq();
                                    self.enter(at, m.raw(j + 1), BT, r, cx);
                                    self.mode = NORMAL;
                                    self.in_word = false;
                                    self.cmd = true;
                                }
                                _ => self.zone_on(at, cx),
                            }
                            j + 1
                        }
                    }
                }
                DQ_ESC => {
                    self.mode = DQ;
                    if c == b'\n' {
                        self.zone_on(m.raw(i), cx);
                    }
                    i + 1
                }
                DOLLAR => {
                    let back = |s: &Self| match s.dctx {
                        R_DQ => DQ,
                        R_EXP => EXPAND,
                        R_MATH => MATH,
                        _ => NORMAL,
                    };
                    match c {
                        b'(' => {
                            self.mode = DOLLAR_P;
                            i + 1
                        }
                        b'{' => {
                            let r = match self.dctx {
                                R_DQ => self.leave_dq(),
                                r => r,
                            };
                            self.enter(m.raw(i), m.raw(i + 1), EXP, r, cx);
                            self.mode = EXPAND;
                            i + 1
                        }
                        b'\'' if matches!(self.dctx, R_NORMAL | R_EXP) => {
                            self.sret = self.dctx;
                            self.mode = ANSI;
                            i + 1
                        }
                        b'"' if self.dctx == R_NORMAL => {
                            self.dret = R_NORMAL;
                            self.mode = DQ;
                            i + 1
                        }
                        b'#' | b'$' | b'?' | b'!' | b'@' | b'*' | b'-' | b'0'..=b'9' => {
                            self.mode = back(self);
                            i + 1
                        }
                        _ => {
                            self.mode = back(self);
                            i
                        }
                    }
                }
                DOLLAR_P => {
                    let (at, end) = (m.raw(i), m.raw(i));
                    let r = match self.dctx {
                        R_DQ => self.leave_dq(),
                        r => r,
                    };
                    if c == b'(' {
                        self.enter(at, end, ARITH, r, cx);
                        self.enter(at, end, ARITH, R_MATH, cx);
                        self.mode = MATH;
                        i + 1
                    } else {
                        self.enter(at, end, PAREN, r, cx);
                        self.mode = NORMAL;
                        self.in_word = false;
                        self.cmd = true;
                        self.op = false;
                        i
                    }
                }
                ANSI => match memchr::memchr2(b'\'', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'\n' {
                            self.zone_on(m.raw(j), cx);
                            self.last = 0;
                        } else if self.last == b'\\' || (j > 0 && b[j - 1] == b'\\') {
                            // `\'` не закрывает; `\\'` — решает текст дальше по файлу.
                            if j > 1 && b[j - 2] == b'\\' {
                                self.lost = true;
                            }
                            self.last = 0;
                        } else {
                            self.zone_off(m.raw(j), cx);
                            self.mode = if self.sret == R_EXP {
                                EXPAND
                            } else {
                                self.in_code()
                            };
                        }
                        j + 1
                    }
                },
                EXPAND => match c {
                    b'}' => {
                        let r = self.leave(m.raw(i), cx);
                        self.resume_mode(r, cx);
                        i + 1
                    }
                    b'\\' => {
                        self.mode = EXP_ESC;
                        i + 1
                    }
                    b'"' => {
                        self.dret = R_EXP;
                        self.mode = DQ;
                        i + 1
                    }
                    b'\'' => {
                        let r = self.ret_of(cx.depth()) & !R_ZONE;
                        if !matches!(r, R_NORMAL) {
                            self.lost = true;
                        }
                        self.sret = R_EXP;
                        self.mode = SQ;
                        i + 1
                    }
                    b'$' => {
                        self.dctx = R_EXP;
                        self.mode = DOLLAR;
                        i + 1
                    }
                    b'`' => {
                        self.enter(m.raw(i), m.raw(i + 1), BT, R_EXP, cx);
                        self.mode = NORMAL;
                        self.in_word = false;
                        self.cmd = true;
                        i + 1
                    }
                    _ => i + 1,
                },
                EXP_ESC => {
                    // `\'` в `${ }`: грамматика читает `'` как начало строки до следующей.
                    self.lost |= c == b'\'';
                    self.mode = EXPAND;
                    i + 1
                }
                MATH => match c {
                    b'(' => {
                        self.enter(m.raw(i), m.raw(i + 1), ARITH, R_MATH, cx);
                        i + 1
                    }
                    b')' => {
                        if self.top(cx) == ARITH {
                            let r = self.leave(m.raw(i), cx);
                            self.resume_mode(r, cx);
                        } else {
                            self.lost = true;
                        }
                        i + 1
                    }
                    b'"' => {
                        self.dret = R_MATH;
                        self.mode = DQ;
                        i + 1
                    }
                    b'\'' => {
                        self.sret = R_MATH;
                        self.mode = SQ;
                        i + 1
                    }
                    b'$' => {
                        self.dctx = R_MATH;
                        self.mode = DOLLAR;
                        i + 1
                    }
                    b'`' => {
                        self.enter(m.raw(i), m.raw(i + 1), BT, R_MATH, cx);
                        self.mode = NORMAL;
                        self.in_word = false;
                        self.cmd = true;
                        i + 1
                    }
                    b'\\' => {
                        self.mode = MATH_ESC;
                        i + 1
                    }
                    b'#' if self.last != 0 => {
                        self.last = 0;
                        i + 1
                    }
                    b' ' | b'\t' | b'\n' | b'\r' => {
                        self.last = 0;
                        i + 1
                    }
                    b'#' => {
                        self.lost = true;
                        i + 1
                    }
                    _ => {
                        self.last = c;
                        i + 1
                    }
                },
                MATH_ESC => {
                    self.mode = MATH;
                    i + 1
                }
                LP => {
                    let at = m.raw(i).saturating_sub(m.unit());
                    if c == b'(' {
                        self.enter(at, m.raw(i + 1), ARITH, R_SUB, cx);
                        self.enter(at, m.raw(i + 1), ARITH, R_MATH, cx);
                        self.mode = MATH;
                        self.last = 0;
                        i + 1
                    } else {
                        self.enter(at, m.raw(i), PAREN, R_SUB, cx);
                        self.mode = NORMAL;
                        self.cmd = true;
                        self.op = false;
                        i
                    }
                }
                SEMI => {
                    self.mode = NORMAL;
                    self.op = false;
                    self.was_cmd = false;
                    if matches!(c, b';' | b'&') {
                        if self.top(cx) == CASE {
                            self.pat = true;
                            self.cmd = false;
                        }
                        if c == b';' {
                            self.mode = SEMI;
                        }
                        i + 1
                    } else {
                        if !self.pat {
                            self.cmd = true;
                        }
                        i
                    }
                }
                AMP => {
                    self.mode = NORMAL;
                    self.was_cmd = false;
                    match c {
                        b'&' => {
                            self.op = true;
                            self.cmd = true;
                            i + 1
                        }
                        b'>' => {
                            self.mode = GT;
                            i + 1
                        }
                        _ => {
                            self.op = false;
                            self.cmd = true;
                            i
                        }
                    }
                }
                PIPE => {
                    self.mode = NORMAL;
                    self.was_cmd = false;
                    if self.pat {
                        if c == b'|' { i + 1 } else { i }
                    } else {
                        self.op = true;
                        self.cmd = true;
                        if matches!(c, b'|' | b'&') { i + 1 } else { i }
                    }
                }
                LT => match c {
                    b'<' => {
                        self.mode = LT2;
                        i + 1
                    }
                    b'(' => {
                        self.mode = NORMAL;
                        self.deplain(m.raw(i), cx);
                        self.enter(m.raw(i), m.raw(i + 1), PAREN, R_NORMAL, cx);
                        self.in_word = false;
                        self.cmd = true;
                        i + 1
                    }
                    b'&' => {
                        self.mode = NORMAL;
                        self.redir = true;
                        i + 1
                    }
                    _ => {
                        self.mode = NORMAL;
                        self.redir = true;
                        i
                    }
                },
                GT => match c {
                    b'(' => {
                        self.mode = NORMAL;
                        self.deplain(m.raw(i), cx);
                        self.enter(m.raw(i), m.raw(i + 1), PAREN, R_NORMAL, cx);
                        self.in_word = false;
                        self.cmd = true;
                        i + 1
                    }
                    b'>' | b'&' | b'|' => {
                        self.mode = NORMAL;
                        self.redir = true;
                        i + 1
                    }
                    _ => {
                        self.mode = NORMAL;
                        self.redir = true;
                        i
                    }
                },
                LT2 => match c {
                    b'<' => {
                        self.mode = NORMAL;
                        self.redir = true;
                        i + 1
                    }
                    b'-' => {
                        self.mode = HD_WS;
                        i + 1
                    }
                    _ => {
                        self.mode = HD_WS;
                        i
                    }
                },
                HD_WS => {
                    if space(c) {
                        i + 1
                    } else {
                        if self.pend {
                            self.lost = true;
                        }
                        self.hash = OFFSET;
                        self.dlen = 0;
                        self.raw = matches!(c, b'\'' | b'"' | b'\\');
                        if matches!(c, b'\'' | b'"') {
                            self.q = c;
                            self.mode = HD_QUOTE;
                            i + 1
                        } else {
                            self.mode = HD_WORD;
                            i
                        }
                    }
                }
                HD_WORD => {
                    if space(c) {
                        self.hd_end();
                        i
                    } else if c == b'\\' {
                        self.mode = HD_ESC;
                        i + 1
                    } else {
                        self.hd_byte(c);
                        i + 1
                    }
                }
                HD_ESC => {
                    self.hd_byte(c);
                    self.mode = HD_WORD;
                    i + 1
                }
                HD_QUOTE => {
                    if c == self.q {
                        self.q = 0;
                        self.hd_end();
                        i + 1
                    } else if matches!(c, b'\r' | b'\n') {
                        self.q = 0;
                        self.hd_end();
                        i
                    } else if c == b'\\' {
                        self.mode = HD_Q_ESC;
                        i + 1
                    } else {
                        self.hd_byte(c);
                        i + 1
                    }
                }
                HD_Q_ESC => {
                    self.hd_byte(c);
                    self.mode = HD_QUOTE;
                    i + 1
                }
                BODY => {
                    if self.head {
                        if self.n == 0 && space(c) {
                            i + 1
                        } else {
                            if self.n == 0 {
                                self.h = OFFSET;
                            }
                            self.n += 1;
                            self.h = (self.h ^ u64::from(c)).wrapping_mul(FNV);
                            if self.n == self.dlen {
                                self.head = false;
                                if self.h == self.hash {
                                    self.body_end(m.raw(i + 1), cx);
                                    i + 1
                                } else {
                                    i
                                }
                            } else if matches!(c, b'\\' | b'$' | b'\n') {
                                self.head = false;
                                i
                            } else {
                                i + 1
                            }
                        }
                    } else {
                        match b[i..]
                            .iter()
                            .position(|c| matches!(c, b'\\' | b'$' | b'\n'))
                        {
                            None => b.len(),
                            Some(k) => {
                                let j = i + k;
                                match b[j] {
                                    b'\\' => self.mode = BODY_ESC,
                                    b'$' if !self.raw => self.mode = BODY_D,
                                    b'\n' => {
                                        self.head = true;
                                        self.n = 0;
                                    }
                                    _ => {}
                                }
                                j + 1
                            }
                        }
                    }
                }
                BODY_ESC => {
                    self.mode = BODY;
                    i + 1
                }
                BODY_D => match c {
                    b'(' => {
                        let r = if self.zone { R_HERE | R_ZONE } else { R_HERE };
                        self.zone = false;
                        self.enter(m.raw(i), m.raw(i + 1), PAREN, r, cx);
                        self.mode = NORMAL;
                        self.in_word = false;
                        self.cmd = true;
                        self.op = false;
                        i + 1
                    }
                    b'{' => {
                        let r = if self.zone { R_HERE | R_ZONE } else { R_HERE };
                        self.zone = false;
                        self.enter(m.raw(i), m.raw(i + 1), EXP, r, cx);
                        self.mode = EXPAND;
                        i + 1
                    }
                    _ => {
                        self.mode = BODY;
                        i
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
        if eof {
            self.word_end(end, cx);
        }
    }
}
