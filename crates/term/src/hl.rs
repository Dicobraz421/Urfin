//! Подсветка вывода терминала (F03 §3.3, F03-10): правила — один многошаблонный regex
//! по тексту строки снапшота; результат — оверлей поверх ячеек копии снапшота, `Term`
//! не меняется.
//!
//! - **Красится только неокрашенный текст:** ячейки с [`Cell::plain`] (fg и bg по
//!   умолчанию, без инверсии). Цвета программы (cargo, git, ls) и окрашенный промпт не
//!   трогаются. В alt-screen (TUI) подсветки нет.
//! - **Кэш по хэшу строки:** regex идёт только по строкам, которых ещё не было
//!   (изменённые строки кадра); бюджет — ≤ 0,3 мс на кадр при 50 изменённых строках
//!   (бенч `grid/highlight_50_changed`).
//! - Роли — токены темы: ошибки `danger`, предупреждения `context_remote`, успех
//!   `added`, пути/URL/IP — подчёркнуты `accent`, даты и хэши — `secondary`; цвета роли
//!   ([`Palette`]) даёт UI.
//!
//! Те же правила (ошибки, предупреждения, даты) — уровни в логах вьювера: [`spans_of`].

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use regex_automata::meta::Regex;
use regex_automata::util::syntax;
use serde::{Deserialize, Serialize};

use crate::vt::grid::{Cell, Snapshot};

/// `[term.highlight]` (F03 §2.9): общий для всех терминалов.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HighlightConfig {
    pub enabled: bool,
    pub rules: Rules,
}

impl Default for HighlightConfig {
    fn default() -> Self {
        HighlightConfig {
            enabled: true,
            rules: Rules::default(),
        }
    }
}

/// Включённые правила (`rules = { errors = true, … }`).
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые выключатели правил"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub errors: bool,
    pub warnings: bool,
    pub success: bool,
    pub paths: bool,
    pub urls: bool,
    pub ip: bool,
    pub dates: bool,
    pub hashes: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            errors: true,
            warnings: true,
            success: true,
            paths: true,
            urls: true,
            ip: true,
            dates: true,
            hashes: false,
        }
    }
}

impl Rules {
    /// Уровни в логах вьювера (F03 §3.3): только ошибки, предупреждения и даты.
    pub const LOG: Rules = Rules {
        errors: true,
        warnings: true,
        success: false,
        paths: false,
        urls: false,
        ip: false,
        dates: true,
        hashes: false,
    };
}

/// Роль отрезка подсветки (цвет — токен темы, [`Palette`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// `danger`.
    Error,
    /// `context_remote`.
    Warning,
    /// `added`.
    Success,
    /// Путь, URL, IP: `accent` с подчёркиванием.
    Link,
    /// Дата, хэш: `secondary`.
    Dim,
}

impl Role {
    /// Короткая метка для снимков разметки (`[e:error]`).
    pub fn code(self) -> char {
        match self {
            Role::Error => 'e',
            Role::Warning => 'w',
            Role::Success => 's',
            Role::Link => 'u',
            Role::Dim => 'd',
        }
    }
}

/// Отрезок: `start..end` — колонки (оверлей ячеек) или байты (текст вьювера).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub role: Role,
}

/// Цвета ролей под тему (RGB).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Palette {
    pub error: [u8; 3],
    pub warning: [u8; 3],
    pub success: [u8; 3],
    pub link: [u8; 3],
    pub dim: [u8; 3],
}

impl Palette {
    fn of(&self, r: Role) -> [u8; 3] {
        match r {
            Role::Error => self.error,
            Role::Warning => self.warning,
            Role::Success => self.success,
            Role::Link => self.link,
            Role::Dim => self.dim,
        }
    }
}

/// Вид шаблона — для разбора совпадения (обрезка хвоста, проверки без lookaround).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Url,
    Date,
    Ip4,
    Ip6,
    Path,
    Hash,
    Error,
    Warning,
    Success,
}

impl Kind {
    fn role(self) -> Role {
        match self {
            Kind::Url | Kind::Ip4 | Kind::Ip6 | Kind::Path => Role::Link,
            Kind::Date | Kind::Hash => Role::Dim,
            Kind::Error => Role::Error,
            Kind::Warning => Role::Warning,
            Kind::Success => Role::Success,
        }
    }
}

/// Граница слова ASCII: кириллица рядом — не слово (Unicode `\b` медленнее в DFA).
const B: &str = r"(?-u:\b)";

/// Шаблоны по порядку: при совпадениях с одного места побеждает первый (дата раньше
/// IP и пути, путь раньше хэша).
fn patterns(r: Rules) -> Vec<(Kind, String)> {
    let mut v = Vec::new();
    if r.urls {
        v.push((
            Kind::Url,
            format!(r#"{B}(?:https?|ssh|sftp|ftp|git|file)://[^\s"'<>()\[\]{{}}`]+"#),
        ));
    }
    if r.dates {
        // ISO 8601, `2026/10/23 14:02:31`, syslog `Oct 23 13:58:40`.
        v.push((
            Kind::Date,
            format!(
                r"{B}\d{{4}}-\d{{2}}-\d{{2}}(?:[T ]\d{{2}}:\d{{2}}(?::\d{{2}}(?:[.,]\d{{1,9}})?)?(?:Z|[+-]\d{{2}}:?\d{{2}})?)?{B}"
            ),
        ));
        v.push((
            Kind::Date,
            format!(r"{B}\d{{4}}/\d{{2}}/\d{{2}}(?: \d{{2}}:\d{{2}}(?::\d{{2}})?)?{B}"),
        ));
        v.push((
            Kind::Date,
            format!(
                r"{B}(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) [ \d]\d \d{{2}}:\d{{2}}:\d{{2}}{B}"
            ),
        ));
    }
    if r.ip {
        v.push((
            Kind::Ip4,
            format!(r"{B}(?:\d{{1,3}}\.){{3}}\d{{1,3}}(?::\d{{1,5}})?{B}"),
        ));
        // `[::1]:22`, полная форма и сжатая с `::` (границы — проверкой в коде).
        v.push((
            Kind::Ip6,
            r"\[[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*\](?::\d{1,5})?".to_owned(),
        ));
        v.push((
            Kind::Ip6,
            r"(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}|(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4})*)?::(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4})*)?".to_owned(),
        ));
    }
    if r.paths {
        // `C:\x`, `\\srv\x`; `/abs/x`, `~/x` (перед ними — не буква и не цифра);
        // относительный — от двух разделителей (`crates\ops\src\copy.rs:212:9`).
        v.push((
            Kind::Path,
            format!(
                r#"{B}[A-Za-z]:\\[^\s"'<>|*?()\[\]]*|\\\\[A-Za-z0-9_.$-]+\\[^\s"'<>|*?()\[\]]*"#
            ),
        ));
        v.push((
            Kind::Path,
            r"(?-u:\B)~?/[A-Za-z0-9_.~][A-Za-z0-9_.~+@%-]*(?:/[A-Za-z0-9_.~+@%-]+)*/?(?::\d+(?::\d+)?)?"
                .to_owned(),
        ));
        v.push((
            Kind::Path,
            format!(r"{B}[A-Za-z0-9_.-]+(?:[\\/][A-Za-z0-9_.-]+){{2,}}(?::\d+(?::\d+)?)?"),
        ));
    }
    if r.hashes {
        v.push((Kind::Hash, format!(r"{B}[0-9a-f]{{7,64}}{B}")));
    }
    if r.errors {
        v.push((
            Kind::Error,
            format!(r"{B}(?:error|ERROR|FAILED|failed|fatal|panicked|[A-Za-z]*Exception){B}"),
        ));
    }
    if r.warnings {
        v.push((Kind::Warning, format!(r"{B}(?:warn|WARN|warning){B}")));
    }
    if r.success {
        v.push((
            Kind::Success,
            format!(r"active \(running\)|{B}(?:ok|PASSED|succeeded){B}"),
        ));
    }
    v
}

/// Скомпилированные правила.
#[derive(Debug)]
pub struct Matcher {
    re: Option<Regex>,
    kinds: Vec<Kind>,
}

impl Matcher {
    pub fn new(rules: Rules) -> Matcher {
        let pats = patterns(rules);
        if pats.is_empty() {
            return Matcher {
                re: None,
                kinds: Vec::new(),
            };
        }
        let kinds = pats.iter().map(|(k, _)| *k).collect();
        let src: Vec<&str> = pats.iter().map(|(_, p)| p.as_str()).collect();
        // Шаблоны свои и проверены тестом: ошибка сборки — без подсветки, не паника.
        let re = Regex::builder()
            .syntax(syntax::Config::new().unicode(true))
            .build_many(&src)
            .map_err(|e| tracing::warn!(error = %e, "term.highlight: rules"))
            .ok();
        Matcher { re, kinds }
    }

    /// Отрезки текста в байтах, по возрастанию, без пересечений.
    pub fn find(&self, text: &str, out: &mut Vec<Span>) {
        let Some(re) = &self.re else {
            return;
        };
        let bytes = text.as_bytes();
        for m in re.find_iter(text) {
            let kind = self.kinds[m.pattern().as_usize()];
            let (s, mut e) = (m.start(), m.end());
            match kind {
                Kind::Url | Kind::Path => {
                    // Хвостовая пунктуация — не часть пути и адреса.
                    while e > s && matches!(bytes[e - 1], b'.' | b',' | b';' | b':' | b'!' | b'?') {
                        e -= 1;
                    }
                }
                Kind::Ip6 => {
                    if !ip6_ok(bytes, s, e) {
                        continue;
                    }
                }
                Kind::Ip4 => {
                    // Версия `1.2.3.4.5` и `1.2.3.4a` — не адрес.
                    if bytes
                        .get(e)
                        .is_some_and(|b| *b == b'.' || b.is_ascii_alphanumeric())
                        && bytes.get(e + 1).is_some_and(u8::is_ascii_digit)
                    {
                        continue;
                    }
                }
                Kind::Hash => {
                    let t = &bytes[s..e];
                    if !t.iter().any(u8::is_ascii_digit) || !t.iter().any(u8::is_ascii_lowercase) {
                        continue;
                    }
                }
                _ => {}
            }
            if e > s {
                out.push(Span {
                    start: u32::try_from(s).unwrap_or(u32::MAX),
                    end: u32::try_from(e).unwrap_or(u32::MAX),
                    role: kind.role(),
                });
            }
        }
    }
}

/// IPv6 без lookaround: вокруг — не слово, не `:` и не `.`; групп хотя бы две или `::`
/// с группой (`::1`), иначе это `a::b` из путей Rust и C++.
fn ip6_ok(b: &[u8], s: usize, e: usize) -> bool {
    if b[s] == b'[' {
        return true;
    }
    let edge = |c: u8| c.is_ascii_alphanumeric() || c == b':' || c == b'.' || c == b'_';
    if s > 0 && edge(b[s - 1]) || b.get(e).is_some_and(|c| edge(*c)) {
        return false;
    }
    let t = &b[s..e];
    let groups = t.split(|c| *c == b':').filter(|g| !g.is_empty()).count();
    let has_digit = t.iter().any(u8::is_ascii_digit);
    has_digit && (groups >= 2 || (groups == 1 && t.windows(2).any(|w| w == b"::")))
}

/// Отрезки строки текста по правилам (уровни в логах вьювера): байты строки.
pub fn spans_of(m: &Matcher, line: &str) -> Vec<Span> {
    let mut v = Vec::new();
    m.find(line, &mut v);
    v
}

/// Хэш уже готовый (ключ кэша — сам хэш строки).
#[derive(Default)]
struct Id(u64);

impl Hasher for Id {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0.rotate_left(8)) ^ u64::from(*b);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

/// Строк в кэше до сброса: экран и ещё много кадров прокрутки.
const CACHE_MAX: usize = 4096;

/// Подсветка снапшотов одного вида: правила и кэш отрезков по хэшу строки.
#[derive(Debug)]
pub struct Highlighter {
    rules: Rules,
    matcher: Matcher,
    cache: HashMap<u64, Box<[Span]>, BuildHasherDefault<Id>>,
    /// Буферы строки на промахе кэша: текст и колонка каждого байта.
    text: String,
    cols: Vec<u32>,
    found: Vec<Span>,
}

impl Highlighter {
    pub fn new(rules: Rules) -> Highlighter {
        Highlighter {
            rules,
            matcher: Matcher::new(rules),
            cache: HashMap::default(),
            text: String::new(),
            cols: Vec::new(),
            found: Vec::new(),
        }
    }

    pub fn rules(&self) -> Rules {
        self.rules
    }

    /// Правила сменились (настройки): пересобрать и сбросить кэш.
    pub fn set_rules(&mut self, rules: Rules) {
        if rules != self.rules {
            *self = Highlighter::new(rules);
        }
    }

    /// Отрезки строки ячеек в колонках (из кэша, если строка уже была).
    pub fn spans(&mut self, line: &[Cell]) -> &[Span] {
        let (key, _) = self.ensure(line);
        &self.cache[&key]
    }

    /// Ключ строки в кэше; `true` — посчитана сейчас (промах).
    fn ensure(&mut self, line: &[Cell]) -> (u64, bool) {
        let key = line_hash(line);
        if self.cache.contains_key(&key) {
            return (key, false);
        }
        if self.cache.len() >= CACHE_MAX {
            self.cache.clear();
        }
        let spans = self.compute(line);
        self.cache.insert(key, spans);
        (key, true)
    }

    fn compute(&mut self, line: &[Cell]) -> Box<[Span]> {
        self.text.clear();
        self.cols.clear();
        for (c, cell) in line.iter().enumerate() {
            if cell.spacer {
                continue;
            }
            let col = u32::try_from(c).unwrap_or(u32::MAX);
            self.text.push(cell.ch);
            self.cols
                .extend(std::iter::repeat_n(col, cell.ch.len_utf8()));
        }
        let end = u32::try_from(line.len()).unwrap_or(u32::MAX);
        self.found.clear();
        if self.text.trim_end().is_empty() {
            return Box::default();
        }
        self.matcher.find(&self.text, &mut self.found);
        let col = |b: u32| self.cols.get(b as usize).copied().unwrap_or(end);
        self.found
            .iter()
            .map(|s| Span {
                start: col(s.start),
                end: col(s.end),
                role: s.role,
            })
            .collect()
    }

    /// Оверлей на копию снапшота: неокрашенные ячейки отрезков — цвет роли, ссылки
    /// подчёркнуты. В alt-screen — ничего. Возвращает число строк, пересчитанных
    /// regex (промахи кэша).
    pub fn apply(&mut self, snap: &mut Snapshot, pal: &Palette) -> usize {
        if snap.alt {
            return 0;
        }
        let _span = tracing::trace_span!("term.highlight").entered();
        let mut misses = 0;
        for line in &mut snap.cells {
            if !line.iter().any(|c| c.plain && c.ch != ' ') {
                continue;
            }
            let (key, miss) = self.ensure(line);
            misses += usize::from(miss);
            for s in &self.cache[&key] {
                let color = pal.of(s.role);
                let (a, b) = (s.start as usize, (s.end as usize).min(line.len()));
                for cell in line.iter_mut().take(b).skip(a) {
                    if cell.plain {
                        cell.fg = color;
                        if s.role == Role::Link {
                            cell.underline = true;
                        }
                    }
                }
            }
        }
        misses
    }
}

/// Хэш строки ячеек: символы (без заполнителей широких) — FxHash-подобное смешивание.
fn line_hash(line: &[Cell]) -> u64 {
    const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
    let mut h: u64 = line.len() as u64;
    for c in line {
        if !c.spacer {
            h = (h.rotate_left(5) ^ u64::from(u32::from(c.ch))).wrapping_mul(K);
        }
    }
    h
}

/// Разметка строки для снимков: `[e:error]` вокруг отрезков (колонки).
pub fn markup(line: &[Cell], spans: &[Span]) -> String {
    let mut out = String::new();
    let mut i = 0;
    for s in spans {
        let (a, b) = (s.start as usize, (s.end as usize).min(line.len()));
        for c in &line[i.min(a)..a] {
            if !c.spacer {
                out.push(c.ch);
            }
        }
        out.push('[');
        out.push(s.role.code());
        out.push(':');
        for c in &line[a..b] {
            if !c.spacer {
                out.push(c.ch);
            }
        }
        out.push(']');
        i = b;
    }
    for c in &line[i.min(line.len())..] {
        if !c.spacer {
            out.push(c.ch);
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
#[path = "hl_tests.rs"]
mod tests;
