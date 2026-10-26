//! When-выражения привязок (S3 §2.4, S3-02): атомы фокуса, флаги, `!`, `&&`, `||`,
//! скобки, `*`.
//!
//! Выражение компилируется в постфиксный код: вычисление — проход по массиву со
//! стеком на стеке, без кучи (бюджет <50 нс). Специфичность — число атомов в
//! сработавшей конъюнкции; `global` — 0 (побеждает более точная привязка).
//!
//! `global` истинен в `Panel`, `Cmdline`, `Viewer`, `Editor`, `Overlay(*)`; в
//! `Terminal` и `Prompt` — нет (ADR-0015).

use compact_str::CompactString;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use urfin_core::{AppState, Focus, OverlayKind, Row};

/// Флаг: функция состояния. Регистрируют крейты (`cmdline_empty` — S6, `in_archive` —
/// S8).
pub type FlagFn = fn(&AppState) -> bool;

/// Реестр флагов.
#[derive(Clone)]
pub struct FlagRegistry {
    flags: FxHashMap<&'static str, FlagFn>,
}

impl std::fmt::Debug for FlagRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<_> = self.flags.keys().collect();
        names.sort();
        f.debug_struct("FlagRegistry")
            .field("flags", &names)
            .finish()
    }
}

impl FlagRegistry {
    /// Пустой реестр.
    pub fn empty() -> Self {
        FlagRegistry {
            flags: FxHashMap::default(),
        }
    }

    /// Встроенные: `qs`, `has_selection`, `tree`, `on_parent_row`, `cmdline_empty`,
    /// `cmdline_menu`, `cmdline_ghost` (заглушки до S6), `os_windows`, `os_macos`, `os_linux`.
    pub fn with_builtin() -> Self {
        let mut r = Self::empty();
        r.register("qs", |st| {
            st.focus == Focus::Overlay(OverlayKind::QuickSearch)
        });
        r.register("has_selection", |st| !st.active_tab().selection.is_empty());
        // Активная панель — дерево каталогов (S2-18): стрелки и Enter ведут дерево.
        r.register("tree", |st| {
            st.active_tab().mode == urfin_core::PanelMode::Tree
        });
        r.register("on_parent_row", |st| {
            matches!(st.active_tab().current(), Some(Row::Parent))
        });
        // Пока нет командной строки S6, она всегда пуста; S6 регистрирует настоящий.
        r.register("cmdline_empty", |_| true);
        // Список над cmdline и ghost-текст (S6-16, S6-17): настоящие — у S6.
        r.register("cmdline_menu", |_| false);
        r.register("cmdline_ghost", |_| false);
        r.register("os_windows", |_| cfg!(windows));
        r.register("os_macos", |_| cfg!(target_os = "macos"));
        r.register("os_linux", |_| cfg!(all(unix, not(target_os = "macos"))));
        r
    }

    /// Флаг `name` (повторная регистрация заменяет прежний).
    pub fn register(&mut self, name: &'static str, f: FlagFn) {
        self.flags.insert(name, f);
    }

    fn get(&self, name: &str) -> Option<FlagFn> {
        self.flags.get(name).copied()
    }
}

/// Ошибка разбора: позиция (байт) и вид.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("at {pos}: {kind}")]
pub struct WhenError {
    pub pos: usize,
    pub kind: WhenErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WhenErrorKind {
    #[error("empty expression")]
    Empty,
    /// Ни атом фокуса, ни зарегистрированный флаг.
    #[error("unknown flag '{0}'")]
    UnknownFlag(CompactString),
    #[error("unknown overlay '{0}'")]
    UnknownOverlay(CompactString),
    #[error("expected {0}")]
    Expected(&'static str),
    #[error("'*' must stand alone")]
    StarNotAlone,
    #[error("expression is too deep")]
    TooDeep,
}

/// Фокусы — биты маски атома.
const F_PANEL: u16 = 1;
const F_CMDLINE: u16 = 1 << 1;
const F_PROMPT: u16 = 1 << 2;
const F_VIEWER: u16 = 1 << 3;
const F_EDITOR: u16 = 1 << 4;
const F_TERMINAL: u16 = 1 << 5;
const F_OV_MENU: u16 = 1 << 6;
const F_OV_QS: u16 = 1 << 7;
const F_OV_EDIT: u16 = 1 << 8;
const F_OV_DRAFT: u16 = 1 << 9;
const F_OV_PALETTE: u16 = 1 << 10;
const F_OV_FIND: u16 = 1 << 11;
const F_OVERLAY: u16 = F_OV_MENU | F_OV_QS | F_OV_EDIT | F_OV_DRAFT | F_OV_PALETTE | F_OV_FIND;
/// ADR-0015.
const F_GLOBAL: u16 = F_PANEL | F_CMDLINE | F_VIEWER | F_EDITOR | F_OVERLAY;

fn focus_bit(f: Focus) -> u16 {
    match f {
        Focus::Panel => F_PANEL,
        Focus::Cmdline => F_CMDLINE,
        Focus::Prompt(_) => F_PROMPT,
        Focus::Viewer => F_VIEWER,
        Focus::Editor => F_EDITOR,
        Focus::Terminal => F_TERMINAL,
        Focus::Overlay(k) => match k {
            OverlayKind::Menu => F_OV_MENU,
            OverlayKind::QuickSearch => F_OV_QS,
            OverlayKind::InlineEdit => F_OV_EDIT,
            OverlayKind::Draft => F_OV_DRAFT,
            OverlayKind::Palette => F_OV_PALETTE,
            OverlayKind::Find => F_OV_FIND,
        },
    }
}

/// `overlay:<kind>`.
const OVERLAYS: [(&str, u16); 6] = [
    ("menu", F_OV_MENU),
    ("quick_search", F_OV_QS),
    ("inline_edit", F_OV_EDIT),
    ("draft", F_OV_DRAFT),
    ("palette", F_OV_PALETTE),
    ("find", F_OV_FIND),
];

const FOCUS_ATOMS: [(&str, u16); 8] = [
    ("panel", F_PANEL),
    ("cmdline", F_CMDLINE),
    ("prompt", F_PROMPT),
    ("viewer", F_VIEWER),
    ("editor", F_EDITOR),
    ("terminal", F_TERMINAL),
    ("overlay", F_OVERLAY),
    ("global", F_GLOBAL),
];

/// Инструкция постфиксного кода.
#[derive(Copy, Clone)]
enum Op {
    /// Фокус в маске; специфичность атома (у `global` — 0).
    Focus(u16, u8),
    /// Флаг и его номер в `When::flags` (для анализа пересечений).
    Flag(FlagFn, u8),
    Not,
    And,
    Or,
}

impl std::fmt::Debug for Op {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Op::Focus(m, s) => write!(f, "Focus({m:#x}, {s})"),
            Op::Flag(_, i) => write!(f, "Flag({i})"),
            Op::Not => f.write_str("Not"),
            Op::And => f.write_str("And"),
            Op::Or => f.write_str("Or"),
        }
    }
}

/// Глубина стека вычисления (и вложенности выражения).
const MAX_DEPTH: usize = 16;

/// Скомпилированное when-выражение.
#[derive(Clone, Debug)]
pub struct When {
    code: SmallVec<[Op; 8]>,
    /// `*` — любой контекст (только для снятия привязок, S3 §2.4).
    any: bool,
    src: CompactString,
    /// Имена флагов выражения по номеру `Op::Flag`.
    flags: SmallVec<[CompactString; 2]>,
}

impl When {
    /// `*`: любой контекст.
    pub fn any() -> Self {
        When {
            code: SmallVec::new(),
            any: true,
            src: "*".into(),
            flags: SmallVec::new(),
        }
    }

    /// Разбор: атомы фокуса, `overlay:<kind>`, флаги `flags`, `!`, `&&`, `||`, скобки.
    pub fn parse(s: &str, flags: &FlagRegistry) -> Result<Self, WhenError> {
        let trimmed = s.trim();
        if trimmed == "*" {
            return Ok(Self::any());
        }
        let mut p = Parser {
            s,
            pos: 0,
            flags,
            code: SmallVec::new(),
            depth: 0,
            names: SmallVec::new(),
        };
        p.skip_ws();
        if p.eof() {
            return Err(p.err(WhenErrorKind::Empty));
        }
        p.or()?;
        p.skip_ws();
        if !p.eof() {
            let kind = if p.peek() == Some('*') {
                WhenErrorKind::StarNotAlone
            } else {
                WhenErrorKind::Expected("'&&', '||' or end")
            };
            return Err(p.err(kind));
        }
        Ok(When {
            code: p.code,
            any: false,
            src: trimmed.into(),
            flags: p.names,
        })
    }

    /// `*`.
    pub fn is_any(&self) -> bool {
        self.any
    }

    /// Исходный текст (для предупреждений и `keymap.show`).
    pub fn source(&self) -> &str {
        &self.src
    }

    /// Истинно ли в состоянии `st`.
    pub fn eval(&self, st: &AppState) -> bool {
        self.matches(st).is_some()
    }

    /// Специфичность сработавшей конъюнкции, если выражение истинно: число атомов в
    /// ней (`global` — 0, `*` — 0).
    pub fn matches(&self, st: &AppState) -> Option<u8> {
        self.matches_as(st, st.focus)
    }

    /// Как [`When::matches`], но атомы фокуса — для `focus`, а не `st.focus` (выход из
    /// быстрого поиска: клавиша выполняется так, будто фокус уже на панели).
    pub fn matches_as(&self, st: &AppState, focus: Focus) -> Option<u8> {
        self.run(focus_bit(focus), |f, _| f(st))
    }

    /// Вычисление при фокусе `bit`; значение флага даёт `flag` (функция и номер).
    #[inline]
    fn run(&self, bit: u16, flag: impl Fn(FlagFn, u8) -> bool) -> Option<u8> {
        if self.any {
            return Some(0);
        }
        let mut stack = [(false, 0u8); MAX_DEPTH];
        let mut sp = 0usize;
        for op in &self.code {
            match *op {
                Op::Focus(mask, spec) => {
                    stack[sp] = (mask & bit != 0, spec);
                    sp += 1;
                }
                Op::Flag(f, i) => {
                    stack[sp] = (flag(f, i), 1);
                    sp += 1;
                }
                Op::Not => {
                    let (v, s) = stack[sp - 1];
                    stack[sp - 1] = (!v, s);
                }
                Op::And => {
                    sp -= 1;
                    let (b, sb) = stack[sp];
                    let (a, sa) = stack[sp - 1];
                    stack[sp - 1] = (a && b, sa.saturating_add(sb));
                }
                Op::Or => {
                    sp -= 1;
                    let (b, sb) = stack[sp];
                    let (a, sa) = stack[sp - 1];
                    stack[sp - 1] = match (a, b) {
                        (true, true) => (true, sa.max(sb)),
                        (true, false) => (true, sa),
                        (false, true) => (true, sb),
                        (false, false) => (false, 0),
                    };
                }
            }
        }
        let (v, s) = stack[0];
        v.then_some(s)
    }

    /// Могут ли оба выражения быть истинны одновременно с равной специфичностью
    /// (предупреждение `Ambiguous`, S3 §2.5). Перебор всех фокусов и значений флагов:
    /// флаги считаются независимыми.
    pub fn ambiguous_with(&self, other: &When) -> bool {
        let mut names: SmallVec<[&str; 8]> = SmallVec::new();
        for n in self.flags.iter().chain(&other.flags) {
            if !names.contains(&n.as_str()) {
                names.push(n);
            }
        }
        if names.len() > 12 {
            return false;
        }
        let slot = |w: &When, i: u8| {
            let name = &w.flags[usize::from(i)];
            names.iter().position(|n| *n == name.as_str()).unwrap_or(0)
        };
        (0..12).any(|f| {
            let bit = 1u16 << f;
            (0u32..1 << names.len()).any(|set| {
                let a = self.run(bit, |_, i| set & (1 << slot(self, i)) != 0);
                let b = other.run(bit, |_, i| set & (1 << slot(other, i)) != 0);
                matches!((a, b), (Some(x), Some(y)) if x == y)
            })
        })
    }

    /// Наибольшая специфичность выражения (без состояния): для сравнения привязок при
    /// компиляции (S3-04).
    pub fn specificity(&self) -> u8 {
        if self.any {
            return 0;
        }
        let mut stack = [0u8; MAX_DEPTH];
        let mut sp = 0;
        for op in &self.code {
            match *op {
                Op::Focus(_, s) => {
                    stack[sp] = s;
                    sp += 1;
                }
                Op::Flag(..) => {
                    stack[sp] = 1;
                    sp += 1;
                }
                Op::Not => {}
                Op::And => {
                    sp -= 1;
                    stack[sp - 1] = stack[sp - 1].saturating_add(stack[sp]);
                }
                Op::Or => {
                    sp -= 1;
                    stack[sp - 1] = stack[sp - 1].max(stack[sp]);
                }
            }
        }
        stack[0]
    }
}

struct Parser<'a> {
    s: &'a str,
    pos: usize,
    flags: &'a FlagRegistry,
    code: SmallVec<[Op; 8]>,
    depth: usize,
    names: SmallVec<[CompactString; 2]>,
}

impl Parser<'_> {
    fn err(&self, kind: WhenErrorKind) -> WhenError {
        WhenError {
            pos: self.pos,
            kind,
        }
    }

    fn eof(&self) -> bool {
        self.pos >= self.s.len()
    }

    fn peek(&self) -> Option<char> {
        self.s[self.pos..].chars().next()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += self.peek().map_or(0, char::len_utf8);
        }
    }

    fn eat(&mut self, t: &str) -> bool {
        self.skip_ws();
        if self.s[self.pos..].starts_with(t) {
            self.pos += t.len();
            true
        } else {
            false
        }
    }

    /// Глубина стека: каждый атом занимает ячейку до свёртки — не глубже `MAX_DEPTH`.
    fn push(&mut self, op: Op) -> Result<(), WhenError> {
        if matches!(op, Op::Focus(..) | Op::Flag(..)) {
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                return Err(self.err(WhenErrorKind::TooDeep));
            }
        }
        if matches!(op, Op::And | Op::Or) {
            self.depth -= 1;
        }
        self.code.push(op);
        Ok(())
    }

    fn or(&mut self) -> Result<(), WhenError> {
        self.and()?;
        while self.eat("||") {
            self.and()?;
            self.push(Op::Or)?;
        }
        Ok(())
    }

    fn and(&mut self) -> Result<(), WhenError> {
        self.unary()?;
        while self.eat("&&") {
            self.unary()?;
            self.push(Op::And)?;
        }
        Ok(())
    }

    fn unary(&mut self) -> Result<(), WhenError> {
        if self.eat("!") {
            self.unary()?;
            return self.push(Op::Not);
        }
        if self.eat("(") {
            self.or()?;
            if !self.eat(")") {
                return Err(self.err(WhenErrorKind::Expected("')'")));
            }
            return Ok(());
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<(), WhenError> {
        self.skip_ws();
        let start = self.pos;
        let len = self.s[start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(self.s.len() - start);
        if len == 0 {
            let kind = if self.peek() == Some('*') {
                WhenErrorKind::StarNotAlone
            } else {
                WhenErrorKind::Expected("context or flag")
            };
            return Err(self.err(kind));
        }
        let name = &self.s[start..start + len];
        self.pos = start + len;
        let lower = name.to_ascii_lowercase();
        if let Some(kind) = lower.strip_prefix("overlay:") {
            let Some((_, bit)) = OVERLAYS.iter().find(|(n, _)| *n == kind) else {
                self.pos = start;
                return Err(self.err(WhenErrorKind::UnknownOverlay(kind.into())));
            };
            return self.push(Op::Focus(*bit, 1));
        }
        if let Some((n, bit)) = FOCUS_ATOMS.iter().find(|(n, _)| *n == lower) {
            let spec = u8::from(*n != "global");
            return self.push(Op::Focus(*bit, spec));
        }
        if let Some(f) = self.flags.get(name) {
            let idx = self
                .names
                .iter()
                .position(|n| n == name)
                .unwrap_or_else(|| {
                    self.names.push(name.into());
                    self.names.len() - 1
                });
            self.push(Op::Flag(f, u8::try_from(idx).unwrap_or(u8::MAX)))
        } else {
            self.pos = start;
            Err(self.err(WhenErrorKind::UnknownFlag(name.into())))
        }
    }
}

#[cfg(test)]
#[path = "when_tests.rs"]
mod tests;
