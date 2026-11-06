//! Трекер языков со скобками (Rust, C, C++, C#, Java, Go, JavaScript, TypeScript, TSX):
//! автомат по единицам. Режим — лексика, уровни — скобки, директивы `#if`, шаблоны, JSX и
//! многострочные комментарии и строки (ADR-0024 §8).

#![allow(
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт; флаги — состояние"
)]

use super::{
    Code, L_ARMS, L_BODY, L_BRACKET, L_CLASS, L_COND, L_GROUP, L_INTERP, L_JSX, L_JX_ATTR,
    L_JX_KID, L_LIST, L_OTHER, L_PAREN, L_PAREN_CTL, L_REP, L_ZONE, Track, pk, word_key,
};
use crate::syntax::lex::{Cx, Lex, Map, St};

// Режимы.
const NORMAL: u8 = 0;
const WORD: u8 = 1;
/// `/` — комментарий, деление или регулярка.
const SLASH: u8 = 2;
const LINE: u8 = 3;
const BLOCK: u8 = 4;
/// В блочном: `*` — конец?
const BLOCK_STAR: u8 = 5;
/// В блочном: `/` — вложенный?
const BLOCK_SLASH: u8 = 6;
const STR: u8 = 7;
const STR_ESC: u8 = 8;
/// После `\` и `\r`: `\n` — та же экранировка.
const STR_ESC_CR: u8 = 9;
/// Rust: `'` — символ или время жизни.
const CHAR0: u8 = 10;
/// Rust: первый символ после `'` прочитан.
const CHAR1: u8 = 11;
/// Rust: `r#`: считаем решётки.
const RAW_HASH: u8 = 12;
/// Rust: тело raw-строки.
const RAW: u8 = 13;
/// Rust: `"` raw-строки, считаем решётки конца.
const RAW_END: u8 = 14;
/// Go: строка в обратных кавычках.
const RAWQ: u8 = 15;
/// Java, C#: считаем `"` подряд.
const QUOTES: u8 = 16;
/// Java: текстовый блок.
const TEXT: u8 = 17;
const TEXT_ESC: u8 = 18;
/// Java: `"` в текстовом блоке, считаем.
const TEXT_Q: u8 = 19;
/// `#` директивы: слово.
const DIR_WORD: u8 = 20;
const DIR: u8 = 21;
const DIR_ESC: u8 = 22;
const DIR_ESC_CR: u8 = 23;
const DIR_SLASH: u8 = 24;
/// C++: разделитель raw-строки до `(`.
const CPP_DELIM: u8 = 25;
const CPP_RAW: u8 = 26;
/// C++: `)`, сверяем разделитель.
const CPP_END: u8 = 27;
/// JS: шаблон в обратных кавычках.
const TPL: u8 = 28;
const TPL_ESC: u8 = 29;
const TPL_DOLLAR: u8 = 30;
const REGEX: u8 = 31;
const REGEX_ESC: u8 = 32;
/// JSX: `<` в начале выражения.
const JSX_LT: u8 = 33;
/// JSX: имя и атрибуты открывающего тега.
const JSX_TAG: u8 = 34;
/// JSX: `/` в теге.
const JSX_SLASH: u8 = 35;
const JSX_TEXT: u8 = 36;
/// JSX: `<` в тексте.
const JSX_TEXT_LT: u8 = 37;
/// JSX: закрывающий тег до `>`.
const JSX_CLOSE: u8 = 38;
/// JSX: строка атрибута.
const JSX_STR: u8 = 39;
/// C#: интерполированная строка.
const CS_I: u8 = 40;
const CS_I_ESC: u8 = 41;
/// C#: `{` в интерполированной.
const CS_I_BRACE: u8 = 42;
/// C#: дословная строка (`n` — интерполяция).
const CS_V: u8 = 43;
/// C#: `"` в дословной.
const CS_V_Q: u8 = 44;
/// C#: `{` в дословной интерполированной.
const CS_V_BRACE: u8 = 45;
/// C#: сырая строка (`k` — кавычек, `n` — знаков `$`).
const CS_R: u8 = 46;
/// C#: `"` в сырой, считаем.
const CS_R_Q: u8 = 47;
/// C#: `{` в сырой интерполированной, считаем.
const CS_R_BRACE: u8 = 48;
/// `#define`: до конца имени.
const DEF_NAME: u8 = 49;
/// `#define`: сразу за именем.
const DEF_AFTER: u8 = 50;
/// `#define NAME(`: до `)`.
const DEF_PARAMS: u8 = 51;
/// `#define NAME` + пробел: значение может начаться и на следующих строках (перевод строки
/// — часть определения только вплотную за именем, так у грамматик C).
const DEF_WS: u8 = 52;

// Чем кончился прошлый токен: можно ли с новой строки начать элемент.
const E_NONE: u8 = 0;
const E_SEMI: u8 = 1;
const E_BRACE: u8 = 2;
const E_COMMA: u8 = 3;
/// Go, JS: перевод строки кончил оператор.
const E_ASI: u8 = 4;

// Прошлый значимый токен.
const P_NONE: u8 = 0;
const P_WORD: u8 = 1;
/// Слово языка, после которого идёт выражение.
const P_KW: u8 = 2;
const P_STR: u8 = 3;
const P_RPAREN: u8 = 4;
/// `)` после `if (`, `while (`, `for (`, `with (`.
const P_RPAREN_CTL: u8 = 5;
const P_RBRACKET: u8 = 6;
/// `}` тела.
const P_RBRACE: u8 = 7;
/// `}` выражения.
const P_RBRACE_X: u8 = 8;
const P_SEMI: u8 = 9;
const P_LBRACE: u8 = 10;
const P_COMMA: u8 = 11;
const P_ARROW: u8 = 12;
const P_COLON: u8 = 13;
const P_GT: u8 = 14;
const P_OP: u8 = 15;
const P_INC: u8 = 16;
const P_LPAREN: u8 = 17;
const P_LBRACKET: u8 = 18;
/// Слово, после которого `{` — тело (`else`, `do`, `try`…).
const P_BODY_KW: u8 = 19;
/// Слово, после которого `(` — управление (`if`, `while`…).
const P_CTL_KW: u8 = 20;
/// `switch`: за скобкой обязательно тело.
const P_SWITCH: u8 = 21;
/// Java, C#: `catch`, `try`, `synchronized`, `when`: то же, а `{` за словом — тело.
const P_HDR_KW: u8 = 22;
/// JS: `await`.
const P_AWAIT: u8 = 23;

// Флаги заголовка оператора.
const H_CLS: u8 = 1;
const H_ENUM: u8 = 2;
/// Go: `if`, `for`, `func`…: `{` — тело.
const H_KW: u8 = 4;
/// `->` в заголовке: возвращаемый тип.
const H_ARROW: u8 = 8;
/// `namespace`, `extern`, `module`, `declare`; Go: `const`, `var`, `import`, `type`.
const H_NS: u8 = 16;
/// Java: `new` в заголовке: `{` за `]` — значения массива. C#: `new`, `is`: `{` — выражение.
const H_NEW: u8 = 32;
/// C#: `from`: дальше запрос, `where` в нём — не ограничение.
const H_QUERY: u8 = 64;
/// C#: ограничения типа: `new()` в них — не выражение.
const H_WHERE: u8 = 128;

pub(crate) struct Brace {
    g: Code,
    t: Track,
    mode: u8,
    /// Кавычка строки; регулярка: 1 — внутри `[…]`.
    q: u8,
    /// Многострочная лексема: уровень открыт.
    zone: bool,
    /// Блочный комментарий внутри директивы.
    dir: bool,
    /// Начало строки: значимых токенов на ней не было.
    first: bool,
    /// Единиц от начала строки (пока `first`).
    col: u16,
    end: u8,
    prev: u8,
    hdr: u8,
    /// Счётчик режима: вложенность комментария, решётки, `$`.
    n: u32,
    /// Второй счётчик режима: кавычки, длина разделителя.
    k: u32,
    /// Хеш разделителя raw-строки C++; C#: счёт кавычек конца.
    hash: u32,
    /// Хеш сверяемого хвоста raw-строки C++.
    rh: u32,
    /// `<` минус `>` с прошлого кандидата.
    lt: i16,
    /// Уровни скобок, которые ждут заголовки `match`, `switch`: бит на уровень.
    h: u64,
    /// Java: то же для `enum`: между словом и `{` бывает тело лямбды в аннотации.
    en: u64,
    /// C#: уровни `{` выражения (`new {`, `is {`): после `}` выражение продолжается.
    xb: u64,
    /// Прошлый знак (вплотную).
    pc: u8,
    /// Директива: 0 — прочая, 1 — `#if`, 2 — `#else`, 3 — `#endif`.
    cond: u8,
    /// C#: `$` (младшие биты) и `@` (старший) вплотную перед строкой.
    pfx: u8,
    /// Rust: в ветви `match` уже был `=>`.
    arrow: bool,
    /// Была `@`, ждём `(` аннотации.
    ann: bool,
    word: u64,
    wlen: u32,
    /// Слово кончилось вплотную к следующей единице.
    adj: bool,
    /// Последнее слово.
    last: u64,
    num: bool,
}

const fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

const fn is_c(g: Code) -> bool {
    matches!(g, Code::C | Code::Cpp)
}

const fn is_js(g: Code) -> bool {
    matches!(g, Code::Js | Code::Ts | Code::Tsx)
}

fn is_raw_prefix(w: u64) -> bool {
    const R: u64 = pk("r");
    const BR: u64 = pk("br");
    const CR: u64 = pk("cr");
    matches!(w, R | BR | CR)
}

fn is_cpp_raw(w: u64) -> bool {
    const R: u64 = pk("R");
    const L: u64 = pk("LR");
    const U: u64 = pk("uR");
    const UU: u64 = pk("UR");
    const U8: u64 = pk("u8R");
    matches!(w, R | L | U | UU | U8)
}

fn brace_kind(k: u8) -> bool {
    matches!(
        k,
        L_BODY | L_LIST | L_ARMS | L_CLASS | L_INTERP | L_JX_ATTR | L_JX_KID
    )
}

fn group_kind(k: u8) -> bool {
    matches!(k, L_PAREN | L_PAREN_CTL | L_REP | L_GROUP | L_BRACKET)
}

/// Верхний уровень — круглые или квадратные скобки: заголовок оператора ещё идёт.
fn in_group(top: u8) -> bool {
    matches!(top, L_PAREN | L_PAREN_CTL | L_BRACKET)
}

/// C#: слова запроса: после `}` (`new { … }`, `delegate { … }`) запрос продолжается.
fn is_query_kw(w: u64) -> bool {
    const SET: [u64; 13] = [
        pk("select"),
        pk("into"),
        pk("orderby"),
        pk("group"),
        pk("join"),
        pk("let"),
        pk("where"),
        pk("on"),
        pk("equals"),
        pk("by"),
        pk("from"),
        pk("ascending"),
        pk("descending"),
    ];
    SET.contains(&w)
}

/// Слова JS, после которых идёт выражение.
fn is_js_kw(w: u64) -> bool {
    const SET: [u64; 13] = [
        pk("return"),
        pk("typeof"),
        pk("instanceof"),
        pk("in"),
        pk("of"),
        pk("new"),
        pk("delete"),
        pk("void"),
        pk("throw"),
        pk("case"),
        pk("yield"),
        pk("await"),
        pk("default"),
    ];
    SET.contains(&w)
}

/// Слова, с которых начинается оператор JS и которые не продолжают выражение.
fn is_js_stmt(w: u64) -> bool {
    const SET: [u64; 13] = [
        pk("const"),
        pk("let"),
        pk("var"),
        pk("function"),
        pk("class"),
        pk("if"),
        pk("for"),
        pk("return"),
        pk("try"),
        pk("switch"),
        pk("throw"),
        pk("export"),
        pk("import"),
    ];
    SET.contains(&w)
}

impl Brace {
    pub(crate) fn resume(g: Code, st: St) -> Brace {
        Brace {
            g,
            t: Track::resume(&st),
            mode: st.mode,
            q: st.a,
            zone: st.d & 1 != 0,
            dir: st.d & 2 != 0,
            first: st.d & 4 == 0,
            col: st.b,
            // С начала текста первый токен начинает элемент.
            end: (st.d >> 4 & 7) ^ E_SEMI,
            prev: st.ext.prev,
            hdr: st.ext.hdr,
            n: st.c,
            k: st.ext.k,
            hash: st.ext.hash,
            rh: st.ext.rh,
            lt: st.ext.lt,
            h: st.ext.fs,
            en: st.ext.kinds2[0],
            xb: st.ext.kinds2[1],
            pc: st.ext.pc,
            cond: st.ext.cond,
            pfx: st.ext.pfx,
            arrow: st.ext.hdr2 & 1 != 0,
            ann: st.ext.hdr2 & 2 != 0,
            word: 0,
            wlen: 0,
            adj: false,
            last: 0,
            num: false,
        }
    }

    /// Выражение кончилось: `/` — деление, `<` — сравнение (JS).
    #[inline]
    fn val(&self) -> bool {
        matches!(
            self.prev,
            P_WORD | P_STR | P_RPAREN | P_RBRACKET | P_RBRACE_X | P_INC
        )
    }

    #[inline]
    fn newline(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        if self.g == Code::Go {
            if matches!(
                self.prev,
                P_WORD | P_STR | P_RPAREN | P_RBRACKET | P_RBRACE | P_RBRACE_X | P_INC
            ) && matches!(self.t.top(cx), L_BODY | L_ARMS | L_GROUP)
            {
                self.end = E_ASI;
                self.prev = P_SEMI;
                self.hdr = 0;
            }
        } else if is_js(self.g) && self.end == E_NONE && self.val() {
            self.end = E_ASI;
        }
        self.first = true;
        self.col = 0;
        self.t.line_at = m.raw(i + 1);
        self.adj = false;
        self.pfx = 0;
    }

    /// Значимый токен: первый на строке — кандидат, если прошлый токен кончил элемент, а
    /// этот его продолжить не может. `w` — слово (0 — иначе), `c` — байт токена.
    fn lead(&mut self, w: u64, c: u8, cx: &mut Cx<'_>) {
        if !self.first {
            return;
        }
        self.first = false;
        if self.t.bad != 0 {
            return;
        }
        // `({a, b}) => …` в JS, аргументы аннотаций: что это — решает текст после скобки, внутри
        // скобок запятая не граница.
        let pattern = self.g != Code::Rust && self.end == E_COMMA && self.t.list_in_paren(cx);
        // C#: `[assembly: …]` — атрибут сборки только там, где выше нет операторов: решает файл.
        let attr =
            self.g == Code::CSharp && c == b'[' && (cx.depth() == 0 || self.t.top_of(cx) == L_COND);
        if !pattern && !attr && self.is_item(w, c, self.t.top(cx)) {
            self.t.cand(self.t.line_at, cx);
            self.lt = 0;
            self.arrow = false;
        }
    }

    /// После `self.end` на верхнем уровне `top` токен начинает новый элемент.
    fn is_item(&self, w: u64, c: u8, top: u8) -> bool {
        const ELSE: u64 = pk("else");
        const AS: u64 = pk("as");
        const WHERE: u64 = pk("where");
        const IN: u64 = pk("in");
        const IF: u64 = pk("if");
        const WHILE: u64 = pk("while");
        const CATCH: u64 = pk("catch");
        const FINALLY: u64 = pk("finally");
        const CASE: u64 = pk("case");
        const DEFAULT: u64 = pk("default");
        const OF: u64 = pk("of");
        const SATISFIES: u64 = pk("satisfies");
        const FROM: u64 = pk("from");
        const INSTANCEOF: u64 = pk("instanceof");
        const IS: u64 = pk("is");
        const WITH: u64 = pk("with");
        const SWITCH: u64 = pk("switch");
        if matches!(c, b')' | b']' | b'}') {
            return false;
        }
        let level = match self.end {
            E_COMMA => matches!(top, L_BRACKET | L_LIST | L_ARMS) && self.lt == 0,
            E_NONE => false,
            _ => matches!(top, L_BODY | L_CLASS | L_ARMS | L_COND | L_GROUP),
        };
        if !level {
            return false;
        }
        match self.g {
            Code::Python | Code::Lua | Code::Diff | Code::Cmake | Code::Make => false,
            Code::Css => true,
            Code::Rust => match self.end {
                E_SEMI => w != ELSE,
                // В `match`: `Foo { a }` с новой строки `if` — охрана этой ветви.
                E_BRACE if top == L_ARMS => w != 0 && !matches!(w, ELSE | AS | WHERE | IN | IF),
                E_BRACE => (w != 0 && !matches!(w, ELSE | AS | WHERE | IN)) || c == b'#',
                _ => true,
            },
            Code::Go => {
                if self.end == E_COMMA {
                    true
                } else if top == L_ARMS {
                    matches!(w, CASE | DEFAULT)
                } else {
                    w != ELSE
                }
            }
            Code::C | Code::Cpp | Code::Java | Code::CSharp | Code::Js | Code::Ts | Code::Tsx => {
                if self.end == E_COMMA {
                    return true;
                }
                if top == L_ARMS {
                    return matches!(w, CASE | DEFAULT);
                }
                if self.end == E_ASI {
                    return is_js_stmt(w);
                }
                let cont = matches!(w, ELSE | WHILE | CATCH | FINALLY)
                    || (self.g == Code::Java && w == INSTANCEOF)
                    || (self.g == Code::CSharp && matches!(w, AS | IS | WITH | SWITCH));
                let js_op =
                    is_js(self.g) && matches!(w, IN | OF | AS | SATISFIES | INSTANCEOF | FROM);
                if self.end == E_SEMI {
                    let brace = is_c(self.g) && c == b'{';
                    !(cont || js_op || brace)
                } else {
                    let query = self.g == Code::CSharp && is_query_kw(w);
                    (w != 0 && !cont && !js_op && !query)
                        || (c == b'@' && matches!(self.g, Code::Java | Code::Ts | Code::Tsx))
                        || (c == b'[' && self.g == Code::CSharp)
                        || (c == b'#' && (is_c(self.g) || self.g == Code::CSharp))
                }
            }
        }
    }

    fn normal(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        let c = b[i];
        match c {
            b'\n' => {
                self.newline(i, m, cx);
                i + 1
            }
            b' ' | b'\t' | b'\r' | 0x0C => {
                self.pc = 0;
                self.pfx = 0;
                if self.first {
                    self.col = self.col.saturating_add(1);
                    if self.col == u16::MAX {
                        self.first = false;
                    }
                }
                self.adj = false;
                i + 1
            }
            b'/' => {
                self.adj = false;
                self.mode = SLASH;
                if self.first {
                    self.col = self.col.saturating_add(1);
                }
                i + 1
            }
            _ if is_word(c) || (c == b'$' && is_js(self.g)) => {
                self.mode = WORD;
                self.word = 0;
                self.wlen = 0;
                self.num = c.is_ascii_digit();
                i
            }
            b'"' => {
                self.lead(0, c, cx);
                self.dquote();
                i + 1
            }
            b'\'' => {
                self.lead(0, c, cx);
                self.squote();
                i + 1
            }
            b'`' if self.g == Code::Go || is_js(self.g) => {
                self.lead(0, c, cx);
                self.end = E_NONE;
                self.adj = false;
                self.prev = P_STR;
                if self.g == Code::Go {
                    self.q = b'`';
                    self.mode = RAWQ;
                } else {
                    self.mode = TPL;
                }
                i + 1
            }
            b'#' if self.adj && self.g == Code::Rust && is_raw_prefix(self.last) => {
                self.adj = false;
                self.n = 1;
                self.mode = RAW_HASH;
                i + 1
            }
            b'#' if self.first && (is_c(self.g) || self.g == Code::CSharp) => {
                self.word = 0;
                self.wlen = 0;
                self.mode = DIR_WORD;
                i + 1
            }
            b'#' if is_js(self.g) && self.prev == P_NONE => {
                // `#!` в начале текста.
                self.first = false;
                self.mode = LINE;
                i + 1
            }
            b'<' if self.jsx_at() => {
                self.lead(0, c, cx);
                self.adj = false;
                self.mode = JSX_LT;
                i + 1
            }
            _ => {
                self.lead(0, c, cx);
                self.adj = false;
                self.punct(c, i, m, cx);
                self.pc = c;
                i + 1
            }
        }
    }

    /// `<` начинает элемент JSX.
    fn jsx_at(&self) -> bool {
        matches!(self.g, Code::Js | Code::Tsx) && !self.val()
    }

    fn squote(&mut self) {
        self.end = E_NONE;
        self.adj = false;
        self.prev = P_STR;
        if self.g == Code::Rust {
            self.mode = CHAR0;
        } else {
            self.q = b'\'';
            self.mode = STR;
        }
    }

    fn dquote(&mut self) {
        self.end = E_NONE;
        self.prev = P_STR;
        let adj = std::mem::take(&mut self.adj);
        match self.g {
            Code::Rust if adj && is_raw_prefix(self.last) => {
                self.n = 0;
                self.mode = RAW;
            }
            Code::Cpp if adj && is_cpp_raw(self.last) => {
                self.k = 0;
                self.hash = 0;
                self.mode = CPP_DELIM;
            }
            Code::Java | Code::CSharp => {
                self.k = 1;
                self.mode = QUOTES;
            }
            _ => {
                self.q = b'"';
                self.mode = STR;
            }
        }
    }

    /// `"` подряд (`self.k`, Java, C#) кончились: вид строки.
    fn quotes_end(&mut self) {
        let n = self.k;
        let pfx = std::mem::take(&mut self.pfx);
        let (dollars, at) = (u32::from(pfx & 0x7F), pfx & 0x80 != 0);
        self.q = b'"';
        if self.g == Code::Java {
            self.mode = match n {
                1 => STR,
                2 => NORMAL,
                _ => TEXT,
            };
            return;
        }
        if at {
            // После открывающей — пары `""`; нечётный остаток — конец строки.
            self.n = dollars;
            self.mode = if (n - 1).is_multiple_of(2) {
                CS_V
            } else {
                NORMAL
            };
        } else if n >= 3 {
            self.n = dollars;
            self.mode = CS_R;
        } else if n == 2 {
            self.mode = NORMAL;
        } else if dollars > 0 {
            self.mode = CS_I;
        } else {
            self.mode = STR;
        }
    }

    /// Вид `{` на верхнем уровне `top`, которая откроет уровень `lvl`.
    fn brace_of(&self, lvl: u32, top: u8) -> u8 {
        let arms = self.h >> (lvl % 64) & 1 != 0;
        match self.g {
            Code::Css => L_BODY,
            Code::Rust => {
                if arms {
                    L_ARMS
                } else {
                    L_BODY
                }
            }
            Code::Go => {
                if self.hdr & H_CLS != 0 && self.prev == P_BODY_KW {
                    L_BODY
                } else if arms {
                    L_ARMS
                } else if self.hdr & H_KW != 0 {
                    L_BODY
                } else {
                    L_LIST
                }
            }
            Code::Java if self.en >> (lvl % 64) & 1 != 0 => L_OTHER,
            _ => self.brace_c(arms, top),
        }
    }

    fn brace_c(&self, arms: bool, top: u8) -> u8 {
        let g = self.g;
        let class = if is_c(g) { L_CLASS } else { L_BODY };
        let named = |p| {
            if self.hdr & H_ENUM != 0 {
                if g == Code::Java { L_OTHER } else { L_LIST }
            } else if self.hdr & H_CLS != 0 {
                class
            } else if self.hdr & (H_NS | H_ARROW) != 0 {
                L_BODY
            } else {
                p
            }
        };
        match self.prev {
            P_RPAREN | P_RPAREN_CTL => {
                if arms {
                    L_ARMS
                } else {
                    named(L_BODY)
                }
            }
            P_ARROW | P_BODY_KW | P_HDR_KW | P_NONE => L_BODY,
            P_SEMI | P_RBRACE => {
                if top == L_LIST {
                    L_LIST
                } else {
                    L_BODY
                }
            }
            P_RBRACE_X if g == Code::Cpp => L_BODY,
            P_LBRACE => {
                if matches!(top, L_LIST | L_BRACKET) {
                    L_LIST
                } else {
                    L_BODY
                }
            }
            P_COLON => {
                if !is_js(g) || top == L_ARMS {
                    L_BODY
                } else {
                    L_LIST
                }
            }
            P_WORD | P_GT | P_STR | P_RBRACKET => named(match g {
                // `new int[] {` — значения; `int f()[] {`, `throws X[] {` — тело метода.
                Code::Java if self.prev == P_WORD || self.hdr & H_NEW == 0 => L_BODY,
                Code::Js | Code::Ts | Code::Tsx => L_BODY,
                Code::CSharp => L_OTHER,
                _ => L_LIST,
            }),
            _ => L_LIST,
        }
    }

    #[allow(clippy::too_many_lines, reason = "знаки — одним match")]
    fn punct(&mut self, c: u8, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        let at = m.raw(i);
        let end = m.raw(i + 1);
        self.end = E_NONE;
        // До `=>` ветвь не кончится: внутри охраны и образца границ нет; Java, C#: так же метка
        // `case` до `:` или `->`.
        if matches!(self.g, Code::Rust | Code::Java | Code::CSharp)
            && matches!(c, b'(' | b'[' | b'{')
            && !self.arrow
            && self.t.bad == 0
            && self.t.top(cx) == L_ARMS
        {
            self.t.bad = cx.depth() + 1;
        }
        match c {
            b'(' => {
                let kind = if self.g == Code::Go && self.prev == P_BODY_KW && self.hdr & H_NS != 0 {
                    L_GROUP
                } else if self.prev == P_CTL_KW {
                    L_PAREN_CTL
                } else if self.g == Code::Rust && self.pc == b'$' {
                    L_REP
                } else {
                    L_PAREN
                };
                // JS: `await (…)()` — вызов функции `await`, `await (…)` — ожидание: решает
                // текст после скобки.
                if self.prev == P_AWAIT && self.t.bad == 0 {
                    self.t.bad = cx.depth() + 1;
                }
                // За скобкой обязательно тело: одной скобкой заголовок не закрыть.
                if self.t.bad == 0 && matches!(self.prev, P_SWITCH | P_HDR_KW) {
                    self.t.bad = cx.depth() + 1;
                }
                // Аргументы аннотации: без объявления за ними заголовок — не документ.
                if std::mem::take(&mut self.ann) && self.t.bad == 0 {
                    self.t.bad = cx.depth() + 1;
                }
                self.t.push(cx, at, end, kind);
                self.prev = P_LPAREN;
                if kind == L_GROUP {
                    self.hdr = 0;
                }
            }
            b'[' => {
                self.t.push(cx, at, end, L_BRACKET);
                self.prev = P_LBRACKET;
            }
            b'{' => {
                self.ann = false;
                // Go: `{` в `[…]` — ограничение типа в заголовке функции, тело ещё впереди.
                if self.g == Code::Go && self.t.top(cx) == L_BRACKET && self.t.bad == 0 {
                    self.t.bad = cx.depth() + 1;
                }
                let lvl = u32::try_from(cx.depth() + 1).unwrap_or(0);
                let top = self.t.top(cx);
                let expr = self.g == Code::CSharp
                    && self.hdr & H_NEW != 0
                    && matches!(self.prev, P_RPAREN | P_WORD | P_GT | P_RBRACKET | P_STR);
                let kind = if expr {
                    L_OTHER
                } else {
                    self.brace_of(lvl, top)
                };
                self.xb = self.xb & !(1 << (lvl % 64)) | u64::from(expr) << (lvl % 64);
                self.h &= !(1 << (lvl % 64));
                self.en &= !(1 << (lvl % 64));
                // Скобка внутри `(` (аргумент аннотации) заголовок снаружи не кончает.
                if !in_group(top) {
                    self.hdr = 0;
                }
                self.t.push(cx, at, end, kind);
                self.lt = 0;
                self.prev = P_LBRACE;
            }
            b')' => {
                if self.g == Code::Java {
                    self.hdr &= !H_NEW;
                }
                let k = self.t.close(
                    cx,
                    at,
                    |k| matches!(k, L_PAREN | L_PAREN_CTL | L_REP | L_GROUP),
                    group_kind,
                );
                self.prev = if k == Some(L_PAREN_CTL) {
                    P_RPAREN_CTL
                } else {
                    P_RPAREN
                };
            }
            b']' => {
                self.t.close(cx, at, |k| k == L_BRACKET, group_kind);
                self.prev = P_RBRACKET;
            }
            b'}' => self.close_brace(at, cx),
            b';' => {
                self.ann = false;
                if matches!(self.t.top(cx), L_BODY | L_CLASS | L_ARMS | L_COND) {
                    self.end = E_SEMI;
                }
                self.hdr = 0;
                self.prev = P_SEMI;
            }
            b',' => {
                let top = self.t.top(cx);
                // В `switch` запятая — внутри метки `case`; ветви через запятую — только `match`.
                // Java, C#: в `[` — размерности, атрибуты, указатели на функции: не список.
                if top == L_LIST
                    || (top == L_ARMS && self.g == Code::Rust)
                    || (top == L_BRACKET && !matches!(self.g, Code::Go | Code::Java | Code::CSharp))
                {
                    self.end = E_COMMA;
                }
                if self.g == Code::Java {
                    self.hdr &= !H_NEW;
                }
                self.prev = P_COMMA;
            }
            b'<' => {
                self.lt = self.lt.saturating_add(1);
                self.prev = P_OP;
            }
            b'>' => {
                // `=>`, `->` — не скобка.
                if matches!(self.pc, b'=' | b'-') {
                    self.prev = P_ARROW;
                    self.arrow |= self.pc == b'=' || self.g == Code::Java;
                    if self.pc == b'-' {
                        self.hdr |= H_ARROW;
                    }
                } else {
                    self.lt = (self.lt - 1).max(0);
                    self.prev = P_GT;
                }
            }
            b':' => {
                self.prev = P_COLON;
                if matches!(self.g, Code::Java | Code::CSharp) && self.t.top(cx) == L_ARMS {
                    self.arrow = true;
                }
            }
            b'@' if matches!(self.g, Code::Java | Code::Ts | Code::Tsx) => {
                self.ann = true;
                self.prev = P_OP;
            }
            b'+' | b'-' if self.pc == c => {
                // Постфиксный — значение остаётся, префиксный — нет: `pc` уже съел первый
                // знак как оператор, значение смотрим по токену перед ним.
                if self.q == 1 {
                    self.prev = P_INC;
                }
            }
            b'+' | b'-' => {
                self.q = u8::from(self.val());
                self.prev = P_OP;
            }
            // Java: `...` в скобках — параметры лямбды или записи, не выражение (грамматика
            // берёт и `int... x = {`): скобкой их не закрыть.
            b'.' if self.g == Code::Java && self.pc == b'.' && self.t.bad == 0 => {
                self.t.bad = cx.depth();
                self.prev = P_OP;
            }
            b'@' if self.g == Code::CSharp => {
                self.pfx |= 0x80;
                self.prev = P_OP;
                return;
            }
            b'$' if self.g == Code::CSharp => {
                self.pfx = self.pfx & 0x80 | ((self.pfx & 0x7F) + 1).min(0x7F);
                self.prev = P_OP;
                return;
            }
            _ => self.prev = P_OP,
        }
        self.pfx = 0;
    }

    fn close_brace(&mut self, at: u64, cx: &mut Cx<'_>) {
        let cs = self.g == Code::CSharp;
        let closed = self.t.close(cx, at, brace_kind, group_kind);
        if !in_group(self.t.top(cx)) {
            self.hdr = 0;
        }
        match closed {
            Some(L_OTHER) if self.xb >> ((cx.depth() + 1) % 64) & 1 != 0 => {
                self.prev = P_RBRACE_X;
            }
            Some(L_BODY | L_ARMS | L_OTHER) => {
                self.end = E_BRACE;
                self.prev = P_RBRACE;
            }
            Some(L_INTERP) => {
                self.mode = match self.g {
                    Code::CSharp => CS_I,
                    Code::Java => {
                        self.q = b'"';
                        STR
                    }
                    _ => TPL,
                };
            }
            Some(L_JX_ATTR) => {
                self.k = 0;
                self.mode = match self.g {
                    Code::CSharp => CS_V,
                    Code::Java => TEXT,
                    _ => JSX_TAG,
                };
            }
            Some(L_JX_KID) => self.mode = if cs { CS_R } else { JSX_TEXT },
            _ => self.prev = P_RBRACE_X,
        }
    }

    /// Слово кончилось.
    fn word_end(&mut self, cx: &mut Cx<'_>) {
        let w = word_key(self.word, self.wlen);
        self.lead(w, 0, cx);
        self.end = E_NONE;
        self.pc = 0;
        self.pfx = 0;
        self.last = if self.num { 0 } else { w };
        self.adj = true;
        self.mode = NORMAL;
        let old = self.prev;
        self.prev = P_WORD;
        if !self.num {
            self.keyword(w, old, cx);
        }
    }

    /// Слова языка.
    #[allow(clippy::too_many_lines, reason = "слова — одним match")]
    fn keyword(&mut self, w: u64, old: u8, cx: &Cx<'_>) {
        const IF: u64 = pk("if");
        const FOR: u64 = pk("for");
        const WHILE: u64 = pk("while");
        const WITH: u64 = pk("with");
        const SWITCH: u64 = pk("switch");
        const SELECT: u64 = pk("select");
        const MATCH: u64 = pk("match");
        const ELSE: u64 = pk("else");
        const DO: u64 = pk("do");
        const TRY: u64 = pk("try");
        const FINALLY: u64 = pk("finally");
        const CLASS: u64 = pk("class");
        const STRUCT: u64 = pk("struct");
        const UNION: u64 = pk("union");
        const ENUM: u64 = pk("enum");
        const INTERFACE: u64 = pk("interface");
        const RECORD: u64 = pk("record");
        const NAMESPACE: u64 = pk("namespace");
        const EXTERN: u64 = pk("extern");
        const MODULE: u64 = pk("module");
        const DECLARE: u64 = pk("declare");
        const FUNC: u64 = pk("func");
        const CONST: u64 = pk("const");
        const VAR: u64 = pk("var");
        const IMPORT: u64 = pk("import");
        const TYPE: u64 = pk("type");
        const NOEXCEPT: u64 = pk("noexcept");
        const OVERRIDE: u64 = pk("override");
        const FINAL: u64 = pk("final");
        const MUTABLE: u64 = pk("mutable");
        const STATIC: u64 = pk("static");
        const UNSAFE: u64 = pk("unsafe");
        const CHECKED: u64 = pk("checked");
        const GET: u64 = pk("get");
        const SET: u64 = pk("set");
        const INIT: u64 = pk("init");
        const ADD: u64 = pk("add");
        const REMOVE: u64 = pk("remove");
        const NEW: u64 = pk("new");
        const FROM: u64 = pk("from");
        const WHERE: u64 = pk("where");
        const IS: u64 = pk("is");
        const AWAIT: u64 = pk("await");
        const CASE: u64 = pk("case");
        const CATCH: u64 = pk("catch");
        const SYNCHRONIZED: u64 = pk("synchronized");
        const WHEN: u64 = pk("when");
        const DEFAULT: u64 = pk("default");
        const LOCK: u64 = pk("lock");
        const USING: u64 = pk("using");
        const FIXED: u64 = pk("fixed");
        const FOREACH: u64 = pk("foreach");
        let bit = 1u64 << ((cx.depth() + 1) % 64);
        match self.g {
            Code::Rust => {
                if w == MATCH {
                    self.h |= bit;
                }
            }
            Code::Go => match w {
                SWITCH | SELECT => {
                    self.h |= bit;
                    self.hdr |= H_KW;
                    self.prev = P_KW;
                }
                IF | FOR | FUNC | ELSE => {
                    self.hdr |= H_KW;
                    self.prev = P_KW;
                }
                STRUCT | INTERFACE => {
                    self.hdr |= H_CLS;
                    self.prev = P_BODY_KW;
                }
                CONST | VAR | IMPORT | TYPE => {
                    self.hdr |= H_NS;
                    self.prev = P_BODY_KW;
                }
                _ => {}
            },
            _ => match w {
                SWITCH => {
                    self.h |= bit;
                    self.prev = P_SWITCH;
                }
                IF | FOR | WHILE | WITH => self.prev = P_CTL_KW,
                TRY | CATCH | SYNCHRONIZED | WHEN
                    if matches!(self.g, Code::Java | Code::CSharp) =>
                {
                    self.prev = P_HDR_KW;
                }
                ELSE | DO | TRY | FINALLY => self.prev = P_BODY_KW,
                CLASS | STRUCT | UNION | INTERFACE | RECORD => self.hdr |= H_CLS,
                ENUM => {
                    self.hdr |= H_ENUM;
                    if self.g == Code::Java {
                        self.en |= bit;
                    }
                }
                NAMESPACE | EXTERN | MODULE | DECLARE => self.hdr |= H_NS,
                CONST | NOEXCEPT | OVERRIDE | FINAL | MUTABLE
                    if self.g == Code::Cpp && old == P_RPAREN =>
                {
                    self.prev = P_RPAREN;
                }
                STATIC if self.g == Code::Java || is_js(self.g) => self.prev = P_BODY_KW,
                NEW if self.g == Code::Java
                    || (self.g == Code::CSharp && self.hdr & H_WHERE == 0) =>
                {
                    self.hdr |= H_NEW;
                }
                FROM if self.g == Code::CSharp => self.hdr |= H_QUERY,
                WHERE if self.g == Code::CSharp && self.hdr & H_QUERY == 0 => self.hdr |= H_WHERE,
                IS if self.g == Code::CSharp => self.hdr |= H_NEW,
                CASE | DEFAULT if matches!(self.g, Code::Java | Code::CSharp) => self.arrow = false,
                LOCK | USING | FIXED | FOREACH if self.g == Code::CSharp => self.prev = P_CTL_KW,
                UNSAFE | CHECKED | GET | SET | INIT | ADD | REMOVE if self.g == Code::CSharp => {
                    self.prev = P_BODY_KW;
                }
                AWAIT if is_js(self.g) => self.prev = P_AWAIT,
                _ if is_js(self.g) && is_js_kw(w) => self.prev = P_KW,
                _ => {}
            },
        }
    }

    /// Многострочная лексема на переводе строки: уровень для свёртки.
    fn zone(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        if !self.zone {
            self.zone = true;
            let at = m.raw(i);
            self.t.push(cx, at, at, L_ZONE);
        }
    }

    fn unzone(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        if self.zone {
            self.zone = false;
            self.t.pop(cx, m.raw(i));
        }
    }

    fn str_run(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        match memchr::memchr3(self.q, b'\\', b'\n', &b[i..]) {
            None => b.len(),
            Some(k) => {
                let j = i + k;
                let c = b[j];
                if c == self.q {
                    self.unzone(j, m, cx);
                    self.mode = NORMAL;
                } else if c == b'\\' {
                    self.mode = STR_ESC;
                } else if self.g == Code::Rust {
                    self.zone(j, m, cx);
                } else {
                    // Незакрытая строка кончается на переводе строки.
                    self.mode = NORMAL;
                    return j;
                }
                j + 1
            }
        }
    }

    fn block(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        match memchr::memchr3(b'*', b'/', b'\n', &b[i..]) {
            None => b.len(),
            Some(k) => {
                let j = i + k;
                match b[j] {
                    b'*' => self.mode = BLOCK_STAR,
                    b'/' if self.g == Code::Rust => self.mode = BLOCK_SLASH,
                    b'/' => {}
                    _ => self.zone(j, m, cx),
                }
                j + 1
            }
        }
    }

    fn block_end(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        self.unzone(i, m, cx);
        self.mode = if std::mem::take(&mut self.dir) {
            DIR
        } else {
            NORMAL
        };
    }

    fn raw_run(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>) -> usize {
        match memchr::memchr2(b'"', b'\n', &b[i..]) {
            None => b.len(),
            Some(k) => {
                let j = i + k;
                if b[j] == b'"' {
                    self.k = 0;
                    if self.n == 0 {
                        self.unzone(j, m, cx);
                        self.mode = NORMAL;
                    } else {
                        self.mode = RAW_END;
                    }
                } else {
                    self.zone(j, m, cx);
                }
                j + 1
            }
        }
    }

    /// Строка до байта `q` без экранирования, с переводами строк.
    fn rawq(&mut self, b: &[u8], i: usize, m: Map<'_>, cx: &mut Cx<'_>, next: u8) -> usize {
        match memchr::memchr2(self.q, b'\n', &b[i..]) {
            None => b.len(),
            Some(k) => {
                let j = i + k;
                if b[j] == self.q {
                    self.unzone(j, m, cx);
                    self.mode = next;
                } else {
                    self.zone(j, m, cx);
                }
                j + 1
            }
        }
    }

    /// Слово директивы кончилось.
    fn dir_word(&mut self, cx: &mut Cx<'_>) {
        const IF: u64 = pk("if");
        const IFDEF: u64 = pk("ifdef");
        const IFNDEF: u64 = pk("ifndef");
        const ELSE: u64 = pk("else");
        const ELIF: u64 = pk("elif");
        const ELIFDEF: u64 = pk("elifdef");
        const ELIFNDEF: u64 = pk("elifndef");
        const ENDIF: u64 = pk("endif");
        const DEFINE: u64 = pk("define");
        self.cond = match self.word {
            IF | IFDEF | IFNDEF => 1,
            ELSE | ELIF | ELIFDEF | ELIFNDEF => 2,
            ENDIF => 3,
            _ => 0,
        };
        // C#: прочие директивы — вне синтаксиса (между `}` и `else` тоже), кандидат — за ними.
        if self.cond == 1 || (self.cond == 0 && self.g != Code::CSharp) {
            self.lead(0, b'#', cx);
        }
        self.first = false;
        self.mode = if self.word == DEFINE && is_c(self.g) {
            self.wlen = 0;
            DEF_NAME
        } else {
            DIR
        };
    }

    /// Конец директивы: строка `at`, следующая строка — с `end`.
    fn dir_end(&mut self, at: u64, end: u64, cx: &mut Cx<'_>) {
        match std::mem::take(&mut self.cond) {
            1 => {
                // C#: `#if` посреди оператора или атрибутов: ветвь — не список элементов.
                if self.g == Code::CSharp
                    && self.end == E_NONE
                    && !matches!(self.prev, P_LBRACE | P_NONE)
                    && self.t.bad == 0
                {
                    self.t.bad = cx.depth() + 1;
                }
                self.t.push(cx, at, end, L_COND);
                self.lt = 0;
            }
            2 => self.t.unwind_cond(cx, at, false),
            3 => self.t.unwind_cond(cx, at, true),
            _ => {}
        }
    }

    fn jsx_pop(&mut self, i: usize, m: Map<'_>, cx: &mut Cx<'_>) {
        self.t.close(cx, m.raw(i), |k| k == L_JSX, |_| false);
        if self.t.top(cx) == L_JSX {
            self.mode = JSX_TEXT;
        } else {
            self.mode = NORMAL;
            self.prev = P_RPAREN;
        }
    }

    /// Открыть интерполяцию `{`: дальше обычный код.
    fn interp(&mut self, at: u64, end: u64, kind: u8, cx: &mut Cx<'_>) {
        self.t.push(cx, at, end, kind);
        self.mode = NORMAL;
        self.prev = P_LBRACE;
        self.end = E_NONE;
    }
}

impl Lex for Brace {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let mut st = St {
            mode: self.mode,
            a: self.q,
            b: self.col,
            c: self.n,
            d: u8::from(self.zone)
                | u8::from(self.dir) << 1
                | u8::from(!self.first) << 2
                | (self.end ^ E_SEMI) << 4,
            ..St::default()
        };
        st.ext.k = self.k;
        st.ext.hash = self.hash;
        st.ext.rh = self.rh;
        st.ext.lt = self.lt;
        st.ext.fs = self.h;
        st.ext.kinds2[0] = self.en;
        st.ext.kinds2[1] = self.xb;
        st.ext.pc = self.pc;
        st.ext.prev = self.prev;
        st.ext.hdr = self.hdr;
        st.ext.cond = self.cond;
        st.ext.pfx = self.pfx;
        st.ext.hdr2 = u8::from(self.arrow) | u8::from(self.ann) << 1;
        self.t.save(&mut st, at);
        st
    }

    fn can_mark(&self) -> bool {
        let tag_word = self.mode == JSX_TAG && self.wlen > 0;
        !tag_word && !matches!(self.mode, WORD | DIR_WORD | DEF_NAME)
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
                    while j < b.len() {
                        let c = b[j];
                        let more = is_word(c)
                            || (c == b'$' && is_js(self.g))
                            || (c == b'\'' && self.num && is_c(self.g));
                        if !more {
                            break;
                        }
                        self.word = self.word << 8 | u64::from(c);
                        self.wlen = self.wlen.saturating_add(1);
                        j += 1;
                    }
                    if j < b.len() {
                        self.word_end(cx);
                    }
                    j
                }
                SLASH => {
                    let c = b[i];
                    // CSS: `url(http://…)` — не комментарий.
                    let css_url = self.g == Code::Css && self.t.top(cx) == L_PAREN;
                    if c == b'/' && !css_url {
                        self.mode = LINE;
                        i + 1
                    } else if c == b'*' {
                        self.first = false;
                        self.n = 1;
                        self.mode = BLOCK;
                        i + 1
                    } else {
                        self.lead(0, b'/', cx);
                        self.end = E_NONE;
                        self.pfx = 0;
                        if is_js(self.g) && !self.val() {
                            self.prev = P_STR;
                            self.q = 0;
                            self.mode = REGEX;
                        } else {
                            self.prev = P_OP;
                            self.mode = NORMAL;
                        }
                        i
                    }
                }
                LINE => match memchr::memchr(b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = NORMAL;
                        i + k
                    }
                },
                BLOCK => self.block(b, i, m, cx),
                BLOCK_STAR => {
                    self.mode = BLOCK;
                    if b[i] == b'/' {
                        self.n = self.n.saturating_sub(1);
                        if self.n == 0 {
                            self.block_end(i, m, cx);
                        }
                        i + 1
                    } else {
                        i
                    }
                }
                BLOCK_SLASH => {
                    self.mode = BLOCK;
                    if b[i] == b'*' {
                        self.n += 1;
                        i + 1
                    } else {
                        i
                    }
                }
                STR => self.str_run(b, i, m, cx),
                // Java: `\{` — вставка шаблона: снова код, до парной `}`.
                STR_ESC if self.g == Code::Java && b[i] == b'{' => {
                    self.interp(m.raw(i), m.raw(i + 1), L_INTERP, cx);
                    i + 1
                }
                STR_ESC => {
                    self.mode = if b[i] == b'\r' { STR_ESC_CR } else { STR };
                    i + 1
                }
                STR_ESC_CR => {
                    self.mode = STR;
                    if b[i] == b'\n' { i + 1 } else { i }
                }
                CHAR0 => {
                    let c = b[i];
                    if c == b'\\' {
                        self.q = b'\'';
                        self.mode = STR_ESC;
                    } else if c == b'\'' {
                        // `''` — ошибка документа.
                        self.mode = NORMAL;
                    } else {
                        self.mode = CHAR1;
                    }
                    i + 1
                }
                CHAR1 => {
                    let c = b[i];
                    if c >= 0x80 {
                        i + 1
                    } else if c == b'\'' {
                        self.mode = NORMAL;
                        i + 1
                    } else {
                        // Время жизни.
                        self.mode = NORMAL;
                        i
                    }
                }
                RAW_HASH => {
                    let c = b[i];
                    if c == b'#' {
                        self.n += 1;
                        i + 1
                    } else if c == b'"' {
                        self.end = E_NONE;
                        self.prev = P_STR;
                        self.mode = RAW;
                        i + 1
                    } else {
                        // `r#ident`.
                        self.mode = NORMAL;
                        i
                    }
                }
                RAW => self.raw_run(b, i, m, cx),
                RAW_END => {
                    let c = b[i];
                    if c == b'#' {
                        self.k += 1;
                        if self.k == self.n {
                            self.unzone(i, m, cx);
                            self.mode = NORMAL;
                        }
                        i + 1
                    } else {
                        self.mode = RAW;
                        i
                    }
                }
                RAWQ => self.rawq(b, i, m, cx, NORMAL),
                QUOTES => {
                    if b[i] == b'"' {
                        self.k += 1;
                        i + 1
                    } else {
                        self.quotes_end();
                        i
                    }
                }
                TEXT => match memchr::memchr3(b'"', b'\\', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        match b[j] {
                            b'"' => {
                                self.k = 1;
                                self.mode = TEXT_Q;
                            }
                            b'\\' => self.mode = TEXT_ESC,
                            _ => self.zone(j, m, cx),
                        }
                        j + 1
                    }
                },
                TEXT_ESC => {
                    if b[i] == b'{' {
                        self.interp(m.raw(i), m.raw(i + 1), L_JX_ATTR, cx);
                    } else {
                        self.mode = TEXT;
                    }
                    i + 1
                }
                TEXT_Q => {
                    if b[i] == b'"' {
                        self.k += 1;
                        if self.k == 3 {
                            self.unzone(i, m, cx);
                            self.mode = NORMAL;
                        }
                        i + 1
                    } else {
                        self.mode = TEXT;
                        i
                    }
                }
                DIR_WORD => {
                    let c = b[i];
                    if (c == b' ' || c == b'\t') && self.wlen == 0 {
                        i + 1
                    } else if c.is_ascii_alphanumeric() {
                        self.word = self.word << 8 | u64::from(c);
                        self.wlen = self.wlen.saturating_add(1);
                        i + 1
                    } else {
                        self.dir_word(cx);
                        i
                    }
                }
                DIR => match memchr::memchr3(b'\n', b'\\', b'/', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        match b[j] {
                            b'\n' => {
                                self.mode = NORMAL;
                                let at = self.t.line_at;
                                self.dir_end(at, m.raw(j + 1), cx);
                                j
                            }
                            b'\\' if self.g != Code::CSharp => {
                                self.mode = DIR_ESC;
                                j + 1
                            }
                            b'/' if self.g != Code::CSharp => {
                                self.mode = DIR_SLASH;
                                j + 1
                            }
                            _ => j + 1,
                        }
                    }
                },
                DEF_NAME => {
                    let c = b[i];
                    if is_word(c) {
                        self.wlen = 1;
                        i + 1
                    } else if self.wlen == 0 && (c == b' ' || c == b'\t') {
                        i + 1
                    } else {
                        self.mode = if self.wlen == 0 { DIR } else { DEF_AFTER };
                        i
                    }
                }
                DEF_AFTER => match b[i] {
                    b'(' => {
                        self.mode = DEF_PARAMS;
                        i + 1
                    }
                    b'\r' => i + 1,
                    b' ' | b'\t' => {
                        self.mode = DEF_WS;
                        i + 1
                    }
                    _ => {
                        self.mode = DIR;
                        i
                    }
                },
                DEF_PARAMS => match memchr::memchr(b')', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.mode = DEF_AFTER;
                        i + k + 1
                    }
                },
                DEF_WS => match b[i] {
                    b' ' | b'\t' | b'\r' | b'\n' | 0x0C => i + 1,
                    _ => {
                        self.mode = DIR;
                        i
                    }
                },
                DIR_ESC => {
                    self.mode = if b[i] == b'\r' { DIR_ESC_CR } else { DIR };
                    i + 1
                }
                DIR_ESC_CR => {
                    self.mode = DIR;
                    if b[i] == b'\n' { i + 1 } else { i }
                }
                DIR_SLASH => {
                    if b[i] == b'*' {
                        self.dir = true;
                        self.n = 1;
                        self.mode = BLOCK;
                        i + 1
                    } else {
                        self.mode = DIR;
                        i
                    }
                }
                CPP_DELIM => {
                    let c = b[i];
                    if c == b'(' {
                        self.mode = CPP_RAW;
                        i + 1
                    } else if c == b'\n' {
                        // Не raw-строка: ошибка документа.
                        self.mode = NORMAL;
                        i
                    } else {
                        self.hash = self.hash.wrapping_mul(16_777_619) ^ u32::from(c);
                        self.k += 1;
                        i + 1
                    }
                }
                CPP_RAW => match memchr::memchr2(b')', b'\n', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b')' {
                            self.n = 0;
                            self.rh = 0;
                            self.mode = CPP_END;
                        } else {
                            self.zone(j, m, cx);
                        }
                        j + 1
                    }
                },
                CPP_END => {
                    let c = b[i];
                    if self.n == self.k {
                        if c == b'"' && self.rh == self.hash {
                            self.unzone(i, m, cx);
                            self.mode = NORMAL;
                            i + 1
                        } else {
                            self.mode = CPP_RAW;
                            i
                        }
                    } else if c == b')' {
                        self.n = 0;
                        self.rh = 0;
                        i + 1
                    } else if c == b'"' || c == b'\n' {
                        self.mode = CPP_RAW;
                        i
                    } else {
                        self.rh = self.rh.wrapping_mul(16_777_619) ^ u32::from(c);
                        self.n += 1;
                        i + 1
                    }
                }
                TPL => match memchr::memchr3(b'`', b'\\', b'$', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        match b[j] {
                            b'`' => self.mode = NORMAL,
                            b'\\' => self.mode = TPL_ESC,
                            _ => self.mode = TPL_DOLLAR,
                        }
                        j + 1
                    }
                },
                TPL_ESC => {
                    self.mode = TPL;
                    i + 1
                }
                TPL_DOLLAR => {
                    if b[i] == b'{' {
                        self.interp(m.raw(i), m.raw(i + 1), L_INTERP, cx);
                        i + 1
                    } else {
                        self.mode = TPL;
                        i
                    }
                }
                REGEX => {
                    let c = b[i];
                    match c {
                        b'\n' => {
                            // Незакрытая регулярка кончается на переводе строки.
                            self.mode = NORMAL;
                            i
                        }
                        b'\\' => {
                            self.mode = REGEX_ESC;
                            i + 1
                        }
                        b'[' => {
                            self.q = 1;
                            i + 1
                        }
                        b']' => {
                            self.q = 0;
                            i + 1
                        }
                        b'/' if self.q == 0 => {
                            self.mode = NORMAL;
                            i + 1
                        }
                        _ => i + 1,
                    }
                }
                REGEX_ESC => {
                    self.mode = REGEX;
                    i + 1
                }
                // Пробел за `<` элементу не мешает: `< foo />`.
                JSX_LT | JSX_TEXT_LT if b[i].is_ascii_whitespace() => i + 1,
                JSX_LT => {
                    let c = b[i];
                    if c == b'>' || c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
                        self.t.push(cx, m.raw(i), m.raw(i), L_JSX);
                        self.k = 0;
                        self.word = 0;
                        self.wlen = 0;
                        if c == b'>' {
                            self.mode = JSX_TEXT;
                            i + 1
                        } else {
                            self.mode = JSX_TAG;
                            i
                        }
                    } else {
                        self.prev = P_OP;
                        self.lt = self.lt.saturating_add(1);
                        self.mode = NORMAL;
                        i
                    }
                }
                JSX_TAG => {
                    let c = b[i];
                    match c {
                        b'"' | b'\'' => {
                            self.q = c;
                            self.mode = JSX_STR;
                        }
                        b'{' => self.interp(m.raw(i), m.raw(i + 1), L_JX_ATTR, cx),
                        b'/' => self.mode = JSX_SLASH,
                        // `<Foo<A, () => B> x />`: аргументы типа, не конец тега.
                        b'<' if self.g == Code::Tsx => self.k += 1,
                        b'>' if self.k > 0 => {
                            if self.pc != b'=' {
                                self.k -= 1;
                            }
                        }
                        b'>' => self.mode = JSX_TEXT,
                        b',' if self.g == Code::Tsx && self.k == 0 => {
                            // `<T,>(x) =>` — параметры типа, не элемент.
                            self.jsx_pop(i, m, cx);
                            self.mode = NORMAL;
                            self.prev = P_OP;
                            self.lt = self.lt.saturating_add(1);
                        }
                        _ if self.g != Code::Tsx => {}
                        _ if is_word(c) => {
                            self.word = self.word << 8 | u64::from(c);
                            self.wlen = self.wlen.saturating_add(1);
                        }
                        _ if self.wlen > 0 => {
                            let w = word_key(self.word, self.wlen);
                            self.word = 0;
                            self.wlen = 0;
                            // `<T extends X>(…) =>` — параметры типа, не элемент.
                            if w == pk("extends") {
                                self.jsx_pop(i, m, cx);
                                self.mode = NORMAL;
                                self.prev = P_OP;
                                self.lt = self.lt.saturating_add(1);
                            }
                        }
                        _ => {}
                    }
                    self.pc = c;
                    i + 1
                }
                JSX_SLASH => {
                    if b[i] == b'>' {
                        self.jsx_pop(i, m, cx);
                        i + 1
                    } else {
                        self.mode = JSX_TAG;
                        i
                    }
                }
                JSX_STR => self.rawq(b, i, m, cx, JSX_TAG),
                JSX_TEXT => match memchr::memchr2(b'<', b'{', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'<' {
                            self.mode = JSX_TEXT_LT;
                        } else {
                            self.interp(m.raw(j), m.raw(j + 1), L_JX_KID, cx);
                        }
                        j + 1
                    }
                },
                JSX_TEXT_LT => {
                    if b[i] == b'/' {
                        self.mode = JSX_CLOSE;
                        i + 1
                    } else {
                        self.mode = JSX_LT;
                        i
                    }
                }
                JSX_CLOSE => match memchr::memchr(b'>', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        self.jsx_pop(i + k, m, cx);
                        i + k + 1
                    }
                },
                CS_I => match memchr::memchr3(b'"', b'\\', b'{', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        match b[j] {
                            b'"' => self.mode = NORMAL,
                            b'\\' => self.mode = CS_I_ESC,
                            _ => self.mode = CS_I_BRACE,
                        }
                        j + 1
                    }
                },
                CS_I_ESC => {
                    self.mode = CS_I;
                    i + 1
                }
                CS_I_BRACE => {
                    if b[i] == b'{' {
                        self.mode = CS_I;
                        i + 1
                    } else {
                        self.interp(m.raw(i), m.raw(i), L_INTERP, cx);
                        i
                    }
                }
                CS_V => match memchr::memchr2(b'"', b'{', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'"' {
                            self.mode = CS_V_Q;
                        } else if self.n > 0 {
                            self.mode = CS_V_BRACE;
                        }
                        j + 1
                    }
                },
                CS_V_Q => {
                    if b[i] == b'"' {
                        self.mode = CS_V;
                        i + 1
                    } else {
                        self.mode = NORMAL;
                        i
                    }
                }
                CS_V_BRACE => {
                    if b[i] == b'{' {
                        self.mode = CS_V;
                        i + 1
                    } else {
                        self.interp(m.raw(i), m.raw(i), L_JX_ATTR, cx);
                        i
                    }
                }
                CS_R => match memchr::memchr2(b'"', b'{', &b[i..]) {
                    None => b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'"' {
                            self.hash = 1;
                            self.mode = CS_R_Q;
                        } else if self.n > 0 {
                            self.hash = 1;
                            self.mode = CS_R_BRACE;
                        }
                        j + 1
                    }
                },
                CS_R_Q => {
                    if b[i] == b'"' {
                        self.hash += 1;
                        i + 1
                    } else {
                        self.mode = if self.hash >= self.k { NORMAL } else { CS_R };
                        i
                    }
                }
                CS_R_BRACE => {
                    if b[i] == b'{' {
                        self.hash += 1;
                        i + 1
                    } else {
                        if self.hash >= self.n {
                            self.interp(m.raw(i), m.raw(i), L_JX_KID, cx);
                        } else {
                            self.mode = CS_R;
                        }
                        i
                    }
                }
                _ => {
                    // Порченая точка: обычный текст.
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
