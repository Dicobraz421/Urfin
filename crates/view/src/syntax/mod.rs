//! Подсветка tree-sitter (S7 §5.7, ADR-0023 §7, карточки S7-21a и S7-21): грамматики,
//! запросы, разбор текста в отрезки ролей `[syntax]` темы. Без gpui: цвета ролей выбирает UI.
//!
//! - **Грамматики в exe** (решение 2026-10-30 «M4»): JSON, TOML, YAML, INI, XML, Markdown
//!   (+ строчная грамматика Markdown). Запросы — свои, `assets/syntax/<язык>/highlights.scm`
//!   (вшиты): захваты `@keyword`, `@string`, `@number`, `@comment`, `@none` — по макетам.
//! - **Пакеты** (S7-21a): прочие языки — `grammars\<язык>.dll` рядом с `urfin.exe`
//!   (`assets/syntax/pack`), грузятся по требованию в фоне; только папка установки,
//!   без сети. Захваты их запросов сводятся к ролям по [`tag_of`].
//! - **Разбор** — только в фоне ([`job`]): текст ≤ 16 МБ целиком в UTF-8 (другие кодировки —
//!   через перекодирование с картой смещений), `Parser::parse_with_options` с отменой,
//!   захваты запроса → [`Span`] по байтам. У одного узла побеждает первый шаблон запроса,
//!   вложенный узел перекрывает внешний (как `tree-sitter-highlight`).

pub mod edit;
pub mod job;
pub mod lex;
pub mod marks;
pub mod win;

mod builtin;
mod pack;

#[cfg(test)]
mod perf_tests;
#[cfg(test)]
mod tests;

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, OnceLock};

use rustc_hash::FxHashMap;
use tree_sitter::{Language, Node, ParseOptions, Parser, Query, QueryCursor, StreamingIterator};

pub use job::{SynMarksMsg, SynMsg, SynState, SynWinMsg};

/// Роль `[syntax]` темы (docs/THEMES.md). `Plain` — цвет текста.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tag {
    #[default]
    Plain = 0,
    Keyword = 1,
    String = 2,
    Number = 3,
    Comment = 4,
}

impl Tag {
    fn from_u8(v: u8) -> Tag {
        match v {
            1 => Tag::Keyword,
            2 => Tag::String,
            3 => Tag::Number,
            4 => Tag::Comment,
            _ => Tag::Plain,
        }
    }
}

/// Отрезок подсветки: байты `start..end` (источника или текста — по месту) и роль.
/// Смещения — `u64`: окно файла любого размера (ADR-0024); отрезков — только окно.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u64,
    pub end: u64,
    pub tag: Tag,
}

/// Роль по имени захвата tree-sitter (`keyword.function` → `Keyword`). `None` — захват
/// служебный или неизвестный: не красит ничего.
pub fn tag_of(capture: &str) -> Option<Tag> {
    if capture.starts_with('_') {
        return None;
    }
    let head = capture.split('.').next().unwrap_or(capture);
    Some(match head {
        "none" | "variable" | "property" | "operator" | "punctuation" | "field" | "parameter"
        | "attribute" | "label" | "embedded" | "delimiter" | "text" => Tag::Plain,
        "keyword" | "type" | "tag" | "function" | "method" | "constructor" | "include"
        | "conditional" | "repeat" | "exception" | "storageclass" | "structure" | "namespace"
        | "module" | "preproc" | "define" | "macro" | "title" | "heading" => Tag::Keyword,
        "string" | "character" | "escape" | "regex" | "url" | "uri" => Tag::String,
        "number" | "float" | "boolean" => Tag::Number,
        "comment" => Tag::Comment,
        "constant" => {
            if capture.starts_with("constant.builtin") || capture.starts_with("constant.numeric") {
                Tag::Number
            } else {
                Tag::Plain
            }
        }
        "markup" => match capture {
            c if c.starts_with("markup.heading") => Tag::Keyword,
            c if c.starts_with("markup.raw") || c.starts_with("markup.link.url") => Tag::String,
            _ => return None,
        },
        _ => return None,
    })
}

/// Грамматика с запросом подсветки. У Markdown — ещё строчная (`inline`).
pub struct Grammar {
    pub name: &'static str,
    lang: Language,
    query: Query,
    /// Роль по индексу захвата.
    tags: Vec<Option<Tag>>,
    inline: Option<Arc<Grammar>>,
}

impl std::fmt::Debug for Grammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammar")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Почему грамматику не собрать.
#[derive(Debug, thiserror::Error)]
pub enum GrammarError {
    #[error("language: {0}")]
    Language(#[from] tree_sitter::LanguageError),
    #[error("query: {0}")]
    Query(#[from] tree_sitter::QueryError),
    #[error("pack: {0}")]
    Pack(String),
}

impl Grammar {
    /// Грамматика из языка tree-sitter и текста запроса.
    pub fn new(name: &'static str, lang: Language, query: &str) -> Result<Grammar, GrammarError> {
        // Проверка ABI: несовместимая грамматика не ставится в парсер.
        Parser::new().set_language(&lang)?;
        let query = Query::new(&lang, query)?;
        let tags = query.capture_names().iter().map(|c| tag_of(c)).collect();
        Ok(Grammar {
            name,
            lang,
            query,
            tags,
            inline: None,
        })
    }
}

/// Есть ли грамматика языка: в exe или пакетом рядом с exe. Первый вызов читает папку
/// `grammars` — только в фоне (задача открытия), не в UI-потоке.
pub fn available(name: &str) -> bool {
    builtin::has(name) || pack::has(name)
}

/// Грамматика языка: вшитая или из пакета (загрузка DLL и разбор запроса — один раз на
/// язык). Только в фоне. `None` — языка нет или пакет не подошёл (в журнал — причина).
pub fn grammar(name: &str) -> Option<Arc<Grammar>> {
    static CACHE: OnceLock<Mutex<FxHashMap<String, Option<Arc<Grammar>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(g) = cache.lock().ok()?.get(name) {
        return g.clone();
    }
    let g = match builtin::load(name).or_else(|| pack::load(name)) {
        Some(Ok(g)) => Some(Arc::new(g)),
        Some(Err(e)) => {
            tracing::warn!(target: "view.syntax", lang = name, "grammar: {e}");
            None
        }
        None => None,
    };
    cache.lock().ok()?.insert(name.to_owned(), g.clone());
    g
}

/// Разбор отменён.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// Захват: байты узла, номер шаблона (меньше — важнее) и роль.
pub(crate) struct Cap {
    start: u32,
    end: u32,
    pattern: u32,
    tag: Tag,
}

/// Захваты запроса `g` по дереву `root`; `range` — только узлы, пересекающие эти байты
/// (`QueryCursor::set_byte_range`, редактор — видимая часть).
fn captures(
    g: &Grammar,
    root: Node<'_>,
    text: &[u8],
    range: Option<std::ops::Range<usize>>,
    stop: &dyn Fn() -> bool,
    out: &mut Vec<Cap>,
) -> Result<(), Cancelled> {
    let mut cursor = QueryCursor::new();
    if let Some(r) = range {
        cursor.set_byte_range(r);
    }
    let mut it = cursor.captures(&g.query, root, text);
    let mut n = 0u32;
    while let Some((m, i)) = it.next() {
        n = n.wrapping_add(1);
        if n.is_multiple_of(4096) && stop() {
            return Err(Cancelled);
        }
        let c = m.captures[*i];
        let Some(tag) = g.tags.get(c.index as usize).copied().flatten() else {
            continue;
        };
        let r = c.node.byte_range();
        if r.start < r.end {
            out.push(Cap {
                start: r.start as u32,
                end: r.end as u32,
                pattern: m.pattern_index as u32,
                tag,
            });
        }
    }
    Ok(())
}

/// Разбор текста грамматикой `g` → отрезки ролей по байтам `text` (по возрастанию, без
/// пересечений, без `Plain`). `stop` — отмена (опрашивается во время разбора).
pub fn highlight(g: &Grammar, text: &str, stop: &dyn Fn() -> bool) -> Result<Vec<Span>, Cancelled> {
    let bytes = text.as_bytes();
    if bytes.len() > u32::MAX as usize {
        return Ok(Vec::new());
    }
    let mut parser = Parser::new();
    if parser.set_language(&g.lang).is_err() {
        return Ok(Vec::new());
    }
    let mut progress = |_: &tree_sitter::ParseState| {
        if stop() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let len = bytes.len();
    let parse =
        |p: &mut Parser, progress: &mut dyn FnMut(&tree_sitter::ParseState) -> ControlFlow<()>| {
            p.parse_with_options(
                &mut |i, _| if i < len { &bytes[i..] } else { &[][..] },
                None,
                Some(ParseOptions::new().progress_callback(&mut |s| progress(s))),
            )
        };
    let tree = parse(&mut parser, &mut progress).ok_or(Cancelled)?;
    let mut caps = Vec::new();
    captures(g, tree.root_node(), bytes, None, stop, &mut caps)?;
    // Markdown: строчные элементы — разбор каждого узла `inline` отдельно.
    if let Some(inl) = &g.inline {
        inline_caps(inl, tree.root_node(), bytes, None, stop, &mut caps)?;
    }
    if stop() {
        return Err(Cancelled);
    }
    Ok(paint(caps, len))
}

/// Узлы строчного разбора Markdown (`inline` и ячейки таблиц), пересекающие `range`, по
/// порядку.
pub(crate) fn inline_nodes<'t>(root: Node<'t>, range: &std::ops::Range<usize>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut cur = root.walk();
    'outer: loop {
        let n = cur.node();
        let hit = n.start_byte() < range.end && n.end_byte() > range.start
            || n.start_byte() == n.end_byte() && n.start_byte() == range.start;
        let leaf = matches!(n.kind(), "inline" | "pipe_table_cell");
        if hit && leaf {
            out.push(n);
        }
        if !(hit && !leaf && cur.goto_first_child()) {
            loop {
                if cur.goto_next_sibling() {
                    if cur.node().start_byte() >= range.end {
                        break 'outer;
                    }
                    break;
                }
                if !cur.goto_parent() {
                    break 'outer;
                }
            }
        }
    }
    out
}

/// Диапазоны строчного разбора узла: узел без именованных детей (маркеры `>` и
/// продолжения блока), как `MarkdownParser` крейта `tree-sitter-md`.
pub(crate) fn inline_ranges(node: Node<'_>) -> Vec<tree_sitter::Range> {
    let mut range = node.range();
    let mut out = Vec::new();
    let mut cur = node.walk();
    if cur.goto_first_child() {
        while cur.goto_next_sibling() {
            let c = cur.node();
            if !c.is_named() {
                continue;
            }
            let cr = c.range();
            out.push(tree_sitter::Range {
                start_byte: range.start_byte,
                start_point: range.start_point,
                end_byte: cr.start_byte,
                end_point: cr.start_point,
            });
            range.start_byte = cr.end_byte;
            range.start_point = cr.end_point;
        }
    }
    out.push(range);
    out
}

/// Строчная грамматика Markdown по узлам `inline` дерева `root`, пересекающим `range`
/// (`None` — все): каждый узел — свой разбор (как `MarkdownParser`: код и ссылки не
/// переходят из абзаца в абзац). Номера шаблонов — за блочными.
pub(crate) fn inline_caps(
    inl: &Grammar,
    root: Node<'_>,
    text: &[u8],
    range: Option<&std::ops::Range<usize>>,
    stop: &dyn Fn() -> bool,
    out: &mut Vec<Cap>,
) -> Result<(), Cancelled> {
    let r = range.cloned().unwrap_or(0..text.len());
    let nodes = inline_nodes(root, &r);
    if nodes.is_empty() {
        return Ok(());
    }
    let mut parser = Parser::new();
    if parser.set_language(&inl.lang).is_err() {
        return Ok(());
    }
    let len = text.len();
    for (i, n) in nodes.iter().enumerate() {
        if i % 256 == 255 && stop() {
            return Err(Cancelled);
        }
        if parser.set_included_ranges(&inline_ranges(*n)).is_err() {
            continue;
        }
        let Some(t) = parser.parse_with_options(
            &mut |i, _| if i < len { &text[i..] } else { &[][..] },
            None,
            None,
        ) else {
            continue;
        };
        let base = out.len();
        captures(inl, t.root_node(), text, range.cloned(), stop, out)?;
        for c in &mut out[base..] {
            c.pattern = c.pattern.saturating_add(1 << 20);
        }
    }
    Ok(())
}

/// Захваты → отрезки: внешний узел раньше вложенного, у одного узла последним красится
/// первый шаблон (он важнее), затем подряд идущие байты одной роли сливаются.
fn paint(mut caps: Vec<Cap>, len: usize) -> Vec<Span> {
    caps.sort_unstable_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(b.pattern.cmp(&a.pattern))
    });
    let mut out = Vec::new();
    if caps.is_empty() {
        return out;
    }
    let lo = caps[0].start as usize;
    let hi = caps
        .iter()
        .map(|c| c.end as usize)
        .max()
        .unwrap_or(lo)
        .min(len);
    let mut buf = vec![0u8; hi.saturating_sub(lo)];
    for c in &caps {
        let a = (c.start as usize).max(lo) - lo;
        let b = (c.end as usize).min(hi).saturating_sub(lo);
        if a < b {
            buf[a..b].fill(c.tag as u8);
        }
    }
    let mut i = 0;
    while i < buf.len() {
        let t = buf[i];
        let mut j = i + 1;
        while j < buf.len() && buf[j] == t {
            j += 1;
        }
        if t != 0 {
            out.push(Span {
                start: (lo + i) as u64,
                end: (lo + j) as u64,
                tag: Tag::from_u8(t),
            });
        }
        i = j;
    }
    out
}

/// Отрезки, пересекающие `start..end` (список по возрастанию, без пересечений).
pub fn spans_in(list: &[Span], start: u64, end: u64) -> &[Span] {
    let i = list.partition_point(|s| s.end <= start);
    let j = i + list[i..].partition_point(|s| s.start < end);
    &list[i..j]
}
