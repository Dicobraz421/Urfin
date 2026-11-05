//! Трекеры кода (пакетные языки, ADR-0024 §8): лексическое состояние и уровни вложенности
//! на контрольных точках; красит tree-sitter по кускам между границами.
//!
//! - **Состояние** — то, от чего зависит лексика дальше: строка, комментарий, raw-строка,
//!   шаблон, директива препроцессора, стек уровней.
//! - **Кандидат** — начало строки, с которого начинается новый элемент своего уровня: всё
//!   выше на этом уровне можно выбросить, разбор останется верным документом.
//! - **Граница** (`Out::cut`) — кандидат не ближе [`cut_min`] от прошлой границы. Набор
//!   границ — функция текста от его начала: не зависит от окна.
//! - **Кадр** (`Out::frame`) — уровень, открытый на границе: начало заголовка, конец
//!   открывающей скобки, закрывающий текст. Заголовки — настоящий текст файла.
//! - **Уровень «вне цепочки»** (`bad`): заголовок длиннее [`HEAD_MAX`], глубже [`LVL_MAX`]
//!   или не список элементов: внутри него границ нет, кусок идёт целиком.
//!
//! Без грамматики (DLL) трекер работает так же: границ это не меняет.

pub(crate) mod brace;
pub(crate) mod diff;
pub(crate) mod lua;
pub(crate) mod py;

use super::{Cx, Lex, LineKind, St};

/// Диалект (имя — как в `urfin_view::lang`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    Rust,
    C,
    Cpp,
    CSharp,
    Java,
    Go,
    Js,
    Ts,
    Tsx,
    Python,
    Css,
    Lua,
    Diff,
}

impl Code {
    pub fn name(self) -> &'static str {
        match self {
            Code::Rust => "rust",
            Code::C => "c",
            Code::Cpp => "cpp",
            Code::CSharp => "csharp",
            Code::Java => "java",
            Code::Go => "go",
            Code::Js => "javascript",
            Code::Ts => "typescript",
            Code::Tsx => "tsx",
            Code::Python => "python",
            Code::Css => "css",
            Code::Lua => "lua",
            Code::Diff => "diff",
        }
    }

    pub(super) fn of(name: &str) -> Option<Code> {
        Some(match name {
            "rust" => Code::Rust,
            "go" => Code::Go,
            "python" => Code::Python,
            _ => return None,
        })
    }

    pub(super) fn lexer(self, st: St) -> Box<dyn Lex> {
        match self {
            Code::Python => Box::new(py::Py::resume(st)),
            Code::Lua => Box::new(lua::Lua::resume(st)),
            Code::Diff => Box::new(diff::Diff::resume(st)),
            _ => Box::new(brace::Brace::resume(self, st)),
        }
    }
}

/// Заголовок уровня (от кандидата до открывающей скобки) не длиннее.
pub(crate) const HEAD_MAX: u64 = 4 << 10;
/// Уровней в цепочке и в состоянии.
pub(crate) const LVL_MAX: u64 = 32;
/// Граница не ближе от прошлой.
pub(crate) const CUT_MIN: u64 = 8 << 10;

#[cfg(test)]
thread_local! {
    /// Тесты: минимум между границами (`HL_CUT_MIN=0` — граница каждый кандидат).
    pub(crate) static TEST_CUT_MIN: std::cell::Cell<u64> = std::cell::Cell::new(
        std::env::var("HL_CUT_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(CUT_MIN),
    );
}

/// Минимум между границами.
#[inline]
fn cut_min() -> u64 {
    #[cfg(test)]
    {
        TEST_CUT_MIN.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        CUT_MIN
    }
}

/// Уровень: прочее (в том числе глубже [`LVL_MAX`]).
pub(crate) const L_OTHER: u8 = 0;
/// `(`: прозрачен, своих кандидатов нет.
pub(crate) const L_PAREN: u8 = 1;
/// `[`: прозрачен, элементы — через запятую.
pub(crate) const L_BRACKET: u8 = 2;
/// `{` с операторами или членами.
pub(crate) const L_BODY: u8 = 3;
/// `{` со списком через запятую.
pub(crate) const L_LIST: u8 = 4;
/// `{` с ветвями: `match`, `switch`.
pub(crate) const L_ARMS: u8 = 5;
/// C, C++: `{` типа: конец — `;`, не `}`.
pub(crate) const L_CLASS: u8 = 6;
/// Go: `const (`, `var (`, `import (`, `type (`.
pub(crate) const L_GROUP: u8 = 7;
/// Многострочный комментарий или строка.
pub(crate) const L_ZONE: u8 = 8;
/// `#if` … `#endif`.
pub(crate) const L_COND: u8 = 9;
/// `(` после `if`, `while`, `for`: после `)` идёт оператор.
pub(crate) const L_PAREN_CTL: u8 = 10;
/// `${` шаблона JS; `{` интерполированной строки C#.
pub(crate) const L_INTERP: u8 = 11;
/// Элемент JSX.
pub(crate) const L_JSX: u8 = 12;
/// `{` в теге JSX; `{` дословной интерполированной строки C#.
pub(crate) const L_JX_ATTR: u8 = 13;
/// `{` в тексте JSX; `{` сырой интерполированной строки C#.
pub(crate) const L_JX_KID: u8 = 14;
/// Rust: `$(` в макросе: закрывается с оператором повтора.
pub(crate) const L_REP: u8 = 15;

/// Открывающий текст уровня, с которым разбирается кусок на границе.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// Начало заголовка.
    pub head: u64,
    /// Конец открывающей скобки.
    pub end: u64,
    /// Вид уровня: чем закрыть (`closer`).
    pub kind: u8,
}

/// Закрывающий текст уровня `kind` внутри уровня `parent` (корень — [`L_BODY`]): конец
/// заголовка часто не конец оператора (`let x = f(|| {`), и верный документ — с `;`.
pub(crate) fn closer(g: Code, kind: u8, parent: u8) -> &'static str {
    let group = matches!(parent, L_PAREN | L_PAREN_CTL | L_REP | L_BRACKET);
    let asi = matches!(
        g,
        Code::Go | Code::Js | Code::Ts | Code::Tsx | Code::Python | Code::Css | Code::Lua
    );
    match kind {
        L_PAREN_CTL if !group => ");",
        L_PAREN | L_PAREN_CTL if group || asi => ")",
        L_BRACKET if group || asi => "]",
        L_PAREN | L_PAREN_CTL if g == Code::Rust && parent == L_ARMS => "),",
        L_BRACKET if g == Code::Rust && parent == L_ARMS => "],",
        L_PAREN | L_PAREN_CTL => ");",
        L_BRACKET => "];",
        L_GROUP if matches!(g, Code::Python | Code::Diff) => "",
        L_BODY if g == Code::Lua => " end",
        L_CLASS if g == Code::Lua => " until true",
        L_GROUP => ")",
        L_REP => ")*",
        L_COND => "\n#endif\n",
        L_BODY | L_LIST | L_ARMS | L_CLASS => {
            let semi = match g {
                Code::Rust => parent == L_BODY,
                Code::C | Code::Cpp | Code::Java => !group,
                Code::CSharp => !group && kind == L_LIST,
                _ => false,
            };
            if semi { "};" } else { "}" }
        }
        _ => "",
    }
}

/// Текст перед куском на уровне `top` (`at_start` — начало текста): кусок идёт не первым
/// элементом уровня, как в файле.
pub(crate) fn filler(g: Code, top: u8, at_start: bool) -> &'static str {
    if at_start || !matches!(g, Code::C | Code::Cpp) {
        return "";
    }
    match top {
        L_BODY | L_CLASS | L_COND => {
            "int _;
"
        }
        _ => "",
    }
}

/// Уровни и границы — общая часть трекеров.
#[derive(Clone, Debug, Default)]
pub(crate) struct Track {
    /// Виды уровней `1..=LVL_MAX`, по 4 бита.
    kinds: [u64; 2],
    /// Уровень, с которого цепочка прервана (0 — цела).
    pub bad: u64,
    /// Уровень последнего кандидата.
    cand_lvl: u64,
    pub cut_at: u64,
    pub cand_at: u64,
    /// Начало текущей строки.
    pub line_at: u64,
    /// Кандидат уже был.
    any: bool,
    /// После возобновления: `cut_at`, `cand_at`, `line_at` назад от первого куска.
    back: Option<(u64, u64, u64)>,
}

impl Track {
    pub(crate) fn resume(st: &St) -> Track {
        Track {
            kinds: st.ext.kinds,
            bad: st.ext.bad,
            cand_lvl: st.ext.cand_lvl,
            cut_at: 0,
            cand_at: 0,
            line_at: 0,
            any: st.ext.any,
            back: Some((st.ext.cut, st.ext.cand, u64::from(st.b))),
        }
    }

    /// Первый кусок после возобновления: `at` — его позиция, `unit` — байт в единице ASCII
    /// (начало строки отстоит на `col` единиц-пробелов).
    #[inline]
    pub(crate) fn begin(&mut self, at: u64, unit: u64) {
        if let Some((cut, cand, col)) = self.back.take() {
            self.cut_at = at.saturating_sub(cut);
            self.cand_at = at.saturating_sub(cand);
            self.line_at = at.saturating_sub(col * unit);
        }
    }

    pub(crate) fn save(&self, st: &mut St, at: u64) {
        st.ext.kinds = self.kinds;
        st.ext.bad = self.bad;
        st.ext.cand_lvl = self.cand_lvl;
        st.ext.any = self.any;
        if let Some((cut, cand, _)) = self.back {
            st.ext.cut = cut;
            st.ext.cand = cand;
        } else {
            st.ext.cut = at.saturating_sub(self.cut_at);
            st.ext.cand = at.saturating_sub(self.cand_at);
        }
    }

    /// Вид уровня `lvl` (1…); глубже памяти — прочее.
    #[inline]
    pub(crate) fn kind(&self, lvl: u64) -> u8 {
        if lvl == 0 || lvl > LVL_MAX {
            return L_OTHER;
        }
        let i = (lvl - 1) as usize;
        #[allow(clippy::cast_possible_truncation, reason = "4 бита")]
        {
            (self.kinds[i / 16] >> (i % 16 * 4) & 0xF) as u8
        }
    }

    fn set_kind(&mut self, lvl: u64, k: u8) {
        if lvl == 0 || lvl > LVL_MAX {
            return;
        }
        let i = (lvl - 1) as usize;
        let s = i % 16 * 4;
        self.kinds[i / 16] = self.kinds[i / 16] & !(0xF << s) | u64::from(k) << s;
    }

    /// Кандидат относится к уровню `lvl` или к нему через прозрачные скобки.
    fn cand_ok(&self, mut lvl: u64) -> bool {
        loop {
            if self.cand_lvl == lvl {
                return true;
            }
            if lvl == 0 || !matches!(self.kind(lvl), L_PAREN | L_PAREN_CTL | L_REP | L_BRACKET) {
                return false;
            }
            lvl -= 1;
        }
    }

    /// Открыть уровень: открывающий байт `at`, текст уровня — с `end`.
    pub(crate) fn push(&mut self, cx: &mut Cx<'_>, at: u64, end: u64, kind: u8) {
        let d = cx.depth() + 1;
        let own = matches!(kind, L_BODY | L_LIST | L_ARMS | L_CLASS | L_GROUP | L_COND);
        let chain = if kind == L_COND {
            // `#if` внутри выражения грамматика не разбирает.
            matches!(self.top(cx), L_BODY | L_CLASS | L_COND)
        } else {
            own || matches!(kind, L_PAREN | L_PAREN_CTL | L_REP | L_BRACKET)
        };
        if self.bad == 0 {
            if chain && d <= LVL_MAX && self.cand_ok(d - 1) && end - self.cand_at <= HEAD_MAX {
                cx.out.frame(Frame {
                    head: self.cand_at,
                    end,
                    kind,
                });
            } else {
                self.bad = d;
            }
        }
        self.set_kind(d, kind);
        cx.push(at, false);
        if self.bad == 0 && own {
            self.cand_at = end;
            self.cand_lvl = d;
        }
    }

    /// `#else`, `#endif`: снимает незакрытое внутри ветви (скобки в `#if` без пары); `#endif`
    /// — и сам уровень, `#else` оставляет его вне цепочки: ветвь идёт целиком.
    pub(crate) fn unwind_cond(&mut self, cx: &mut Cx<'_>, at: u64, pop: bool) {
        let d = cx.depth();
        let mut l = d;
        while l > 0 && l + LVL_MAX > d {
            if self.kind(l) == L_COND {
                for _ in l..d {
                    self.pop(cx, at);
                }
                if pop {
                    self.pop(cx, at);
                } else if self.bad == 0 {
                    self.bad = l;
                }
                return;
            }
            l -= 1;
        }
    }

    /// Открыть уровень без заголовка: кусок внутри него — обычный текст.
    pub(crate) fn push_bare(&mut self, cx: &mut Cx<'_>, at: u64, kind: u8) {
        let d = cx.depth() + 1;
        if self.bad == 0 {
            if d <= LVL_MAX {
                cx.out.frame(Frame {
                    head: at,
                    end: at,
                    kind,
                });
            } else {
                self.bad = d;
            }
        }
        self.set_kind(d, kind);
        cx.push(at, false);
    }

    /// Закрыть верхний уровень.
    pub(crate) fn pop(&mut self, cx: &mut Cx<'_>, at: u64) {
        cx.pop(at);
        if cx.depth() < self.bad {
            self.bad = 0;
        }
    }

    /// Верхний уровень — список (вложенный) внутри круглых скобок.
    pub(crate) fn list_in_paren(&self, cx: &Cx<'_>) -> bool {
        let mut l = cx.depth();
        while l > 0 {
            match self.kind(l) {
                L_LIST | L_BRACKET => l -= 1,
                L_PAREN | L_PAREN_CTL => return true,
                _ => return false,
            }
        }
        false
    }

    /// Вид верхнего уровня (корень — прочее).
    #[inline]
    pub(crate) fn top_of(&self, cx: &Cx<'_>) -> u8 {
        self.kind(cx.depth())
    }

    /// Вид верхнего уровня; корень — как тело.
    #[inline]
    pub(crate) fn top(&self, cx: &Cx<'_>) -> u8 {
        let d = cx.depth();
        if d == 0 { L_BODY } else { self.kind(d) }
    }

    /// Закрывающая скобка: снимает уровни сверху до вида `want` (через виды `through`);
    /// такого нет — скобка лишняя (ошибка документа), глубина не меняется. Итог — вид
    /// закрытого.
    pub(crate) fn close(
        &mut self,
        cx: &mut Cx<'_>,
        at: u64,
        want: fn(u8) -> bool,
        through: fn(u8) -> bool,
    ) -> Option<u8> {
        let d = cx.depth();
        let mut l = d;
        while l > 0 && l + LVL_MAX > d {
            let k = self.kind(l);
            if want(k) || k == L_OTHER {
                for _ in l..=d {
                    self.pop(cx, at);
                }
                return Some(k);
            }
            if !through(k) {
                return None;
            }
            l -= 1;
        }
        None
    }

    /// Начало строки `at` — кандидат; граница, если от прошлой далеко (первый элемент
    /// текста границей не становится: у границы есть элементы выше).
    pub(crate) fn cand(&mut self, at: u64, cx: &mut Cx<'_>) {
        self.cand_at = at;
        self.cand_lvl = cx.depth();
        // Выше кандидата цвета от текста ниже не зависят: это и якорь правки (§4).
        cx.out.line(at, 0, LineKind::Safe);
        if std::mem::replace(&mut self.any, true)
            && at > self.cut_at
            && at - self.cut_at >= cut_min()
        {
            self.cut_at = at;
            cx.out.cut(at);
        }
    }
}

/// Ключ слова: восемь последних байт, у длинных — и длина.
pub(crate) const fn word_key(last8: u64, len: u32) -> u64 {
    if len > 8 {
        last8 ^ (len as u64) << 56
    } else {
        last8
    }
}

/// Ключ слова `s`.
pub(crate) const fn pk(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut v = 0u64;
    let mut i = 0;
    while i < b.len() {
        v = v << 8 | b[i] as u64;
        i += 1;
    }
    #[allow(clippy::cast_possible_truncation, reason = "слова языка короткие")]
    word_key(v, b.len() as u32)
}

#[cfg(test)]
#[path = "code/tests.rs"]
mod tests;
