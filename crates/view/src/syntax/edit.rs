//! Подсветка и свёртка редактора (S7-35, ADR-0023 §7, S7 §5.7): разбор текста документа
//! ≤ 16 МБ с прежним деревом, запрос — только по видимой части
//! (`QueryCursor::set_byte_range`), места свёртки — узлы дерева на ней. Без gpui и `Cx`:
//! задачу и состояние документа ведёт `edit::hl` (в фоне).
//!
//! - Байты разбора — байты документа 1:1: UTF-8 как есть, UTF-16 — `parse_utf16_*`
//!   (смещения узлов — в байтах), прочие кодировки — ASCII-заглушка `a` на месте байта
//!   ≥ 0x80 (разбор по ASCII-синтаксису верен, смещения не пересчитываются).
//! - Правка: прежний текст разбора и новый → одна правка (общие начало и конец, по
//!   границам символов), её точки (строка, байт в строке) — счётом переводов строк по
//!   тексту; `Tree::edit` у копии прежнего дерева → разбор с ним. Отмена — `stop`.

use std::ops::{ControlFlow, Range};
use std::sync::Arc;

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use rustc_hash::FxHashSet;
use tree_sitter::{InputEdit, Node, ParseOptions, ParseState, Parser, Point, Tree};

use super::{Cancelled, Grammar, Span, captures, paint};

/// Как байты документа подаются разбору.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Feed {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Однобайтовые и прочие: не-ASCII байт — заглушка `a`.
    Ascii,
}

impl Feed {
    pub fn of(enc: &'static Encoding) -> Feed {
        if enc == UTF_8 {
            Feed::Utf8
        } else if enc == UTF_16LE {
            Feed::Utf16Le
        } else if enc == UTF_16BE {
            Feed::Utf16Be
        } else {
            Feed::Ascii
        }
    }

    fn unit(self) -> usize {
        match self {
            Feed::Utf16Le | Feed::Utf16Be => 2,
            Feed::Utf8 | Feed::Ascii => 1,
        }
    }
}

/// Байты документа → текст разбора (заглушки — на месте).
pub fn feed_text(mut raw: Vec<u8>, feed: Feed) -> Arc<Vec<u8>> {
    if feed == Feed::Ascii {
        for b in &mut raw {
            if *b >= 0x80 {
                *b = b'a';
            }
        }
    }
    Arc::new(raw)
}

/// Разобранный текст: дерево и текст, по которому оно построено.
pub struct Parsed {
    tree: Tree,
    text: Arc<Vec<u8>>,
    feed: Feed,
}

impl std::fmt::Debug for Parsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parsed")
            .field("len", &self.text.len())
            .field("feed", &self.feed)
            .finish_non_exhaustive()
    }
}

impl Parsed {
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }
}

/// Начала переводов строк (`\n`; у UTF-16 — единица 0x000A) в `t`, по возрастанию.
fn nls(t: &[u8], feed: Feed) -> impl DoubleEndedIterator<Item = usize> + '_ {
    memchr::memchr_iter(b'\n', t).filter_map(move |p| match feed {
        Feed::Utf8 | Feed::Ascii => Some(p),
        Feed::Utf16Le => (p % 2 == 0 && t.get(p + 1) == Some(&0)).then_some(p),
        Feed::Utf16Be => (p % 2 == 1 && t[p - 1] == 0).then(|| p - 1),
    })
}

/// Точка после байт `seg`, начавшихся в `p0`.
fn advance(p0: Point, seg: &[u8], feed: Feed) -> Point {
    let rows = nls(seg, feed).count();
    match nls(seg, feed).next_back() {
        None => Point {
            row: p0.row,
            column: p0.column + seg.len(),
        },
        Some(last) => Point {
            row: p0.row + rows,
            column: seg.len() - (last + feed.unit()),
        },
    }
}

/// Начало строки после `off` (за первым переводом строки с `off`), иначе конец текста.
fn line_after(t: &[u8], off: usize, feed: Feed) -> usize {
    nls(&t[off..], feed)
        .next()
        .map_or(t.len(), |p| off + p + feed.unit())
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    const STEP: usize = 4096;
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + STEP <= n && a[i..i + STEP] == b[i..i + STEP] {
        i += STEP;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

fn common_suffix(a: &[u8], b: &[u8], max: usize) -> usize {
    const STEP: usize = 4096;
    let (la, lb) = (a.len(), b.len());
    let mut i = 0;
    while i + STEP <= max && a[la - i - STEP..la - i] == b[lb - i - STEP..lb - i] {
        i += STEP;
    }
    while i < max && a[la - i - 1] == b[lb - i - 1] {
        i += 1;
    }
    i
}

fn is_cont(b: Option<&u8>) -> bool {
    b.is_some_and(|b| b & 0xC0 == 0x80)
}

/// Одна правка от `old` к `new` (`None` — тексты равны): общее начало и конец по
/// границам символов, точки — счётом переводов строк.
pub fn one_edit(old: &[u8], new: &[u8], feed: Feed) -> Option<InputEdit> {
    if old == new {
        return None;
    }
    let u = feed.unit();
    let mut a = common_prefix(old, new) / u * u;
    if feed == Feed::Utf8 {
        while a > 0 && (is_cont(old.get(a)) || is_cont(new.get(a))) {
            a -= 1;
        }
    }
    let max = old.len().min(new.len()) - a;
    let mut s = common_suffix(old, new, max) / u * u;
    if feed == Feed::Utf8 {
        while s > 0 && is_cont(old.get(old.len() - s)) {
            s -= 1;
        }
    }
    let (old_end, new_end) = (old.len() - s, new.len() - s);
    let start = advance(Point::default(), &new[..a], feed);
    Some(InputEdit {
        start_byte: a,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: start,
        old_end_position: advance(start, &old[a..old_end], feed),
        new_end_position: advance(start, &new[a..new_end], feed),
    })
}

/// Разбор `text` парсером (язык и диапазоны — уже в нём) с прежним деревом `old`.
fn run(
    parser: &mut Parser,
    text: &[u8],
    feed: Feed,
    old: Option<&Tree>,
    stop: &dyn Fn() -> bool,
) -> Option<Tree> {
    let mut progress = |_: &ParseState| {
        if stop() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let opts = Some(ParseOptions::new().progress_callback(&mut progress));
    match feed {
        Feed::Utf8 | Feed::Ascii => {
            let len = text.len();
            parser.parse_with_options(
                &mut |i, _| if i < len { &text[i..] } else { &[][..] },
                old,
                opts,
            )
        }
        Feed::Utf16Le | Feed::Utf16Be => {
            // Единицы в памяти — те же байты документа (порядок байт задаёт `encoding`
            // разбора), смещения узлов — в байтах документа.
            let units: Vec<u16> = text
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_ne_bytes(*c))
                .collect();
            let len = units.len();
            let mut read = |i: usize, _| if i < len { &units[i..] } else { &[][..] };
            if feed == Feed::Utf16Le {
                parser.parse_utf16_le_with_options(&mut read, old, opts)
            } else {
                parser.parse_utf16_be_with_options(&mut read, old, opts)
            }
        }
    }
}

/// Разбор текста `text` грамматикой `g`; `prev` — прежний разбор этого документа
/// (дерево правится одной правкой и переиспользуется). Тот же текст — прежнее дерево.
pub fn parse(
    g: &Grammar,
    prev: Option<&Parsed>,
    text: Arc<Vec<u8>>,
    feed: Feed,
    stop: &dyn Fn() -> bool,
) -> Result<Parsed, Cancelled> {
    let old = match prev {
        Some(p) if p.feed == feed => match one_edit(&p.text, &text, feed) {
            None => {
                return Ok(Parsed {
                    tree: p.tree.clone(),
                    text,
                    feed,
                });
            }
            Some(e) => {
                let mut t = p.tree.clone();
                t.edit(&e);
                Some(t)
            }
        },
        _ => None,
    };
    let mut parser = Parser::new();
    parser.set_language(&g.lang).map_err(|_| Cancelled)?;
    let tree = run(&mut parser, &text, feed, old.as_ref(), stop).ok_or(Cancelled)?;
    Ok(Parsed { tree, text, feed })
}

/// Узлы, пересекающие `r`, в прямом порядке; `visit` → `false` — без детей узла.
fn walk_in(root: Node<'_>, r: &Range<usize>, mut visit: impl FnMut(Node<'_>) -> bool) {
    let mut cur = root.walk();
    loop {
        let n = cur.node();
        let hit = n.start_byte() < r.end && n.end_byte() > r.start;
        if hit && visit(n) && cur.goto_first_child_for_byte(r.start).is_some() {
            continue;
        }
        loop {
            if cur.goto_next_sibling() && cur.node().start_byte() < r.end {
                break;
            }
            if !cur.goto_parent() {
                return;
            }
        }
    }
}

/// Отрезки ролей по байтам документа в `range` (по возрастанию, без пересечений).
/// Markdown: строчная грамматика — по узлам `inline` в `range`.
pub fn spans(
    g: &Grammar,
    p: &Parsed,
    range: Range<usize>,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<Span>, Cancelled> {
    let len = p.text.len();
    if len > u32::MAX as usize {
        return Ok(Vec::new());
    }
    let range = range.start.min(len)..range.end.min(len);
    let mut caps = Vec::new();
    let root = p.tree.root_node();
    captures(g, root, &p.text, Some(range.clone()), stop, &mut caps)?;
    // Markdown: строчная грамматика — по каждому узлу `inline` отдельно (как
    // `MarkdownParser`); байты UTF-16 строчный разбор не читает (Markdown в редакторе —
    // окном, ADR-0024).
    if let Some(inl) = &g.inline
        && matches!(p.feed, Feed::Utf8 | Feed::Ascii)
    {
        super::inline_caps(inl, root, &p.text, Some(&range), stop, &mut caps)?;
    }
    if stop() {
        return Err(Cancelled);
    }
    #[allow(clippy::cast_possible_truncation, reason = "текст ≤ 16 МБ")]
    let (lo, hi) = (range.start as u32, range.end as u32);
    for c in &mut caps {
        c.start = c.start.max(lo);
        c.end = c.end.min(hi);
    }
    caps.retain(|c| c.start < c.end);
    Ok(paint(caps, len))
}

/// Место свёртки: начало строки-заголовка и скрытые строки `[start, end)` (байты
/// документа).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FoldAt {
    pub head: u64,
    pub start: u64,
    pub end: u64,
}

/// Узлы, которые не сворачиваются (абзац Markdown и строчное).
const NO_FOLD: &[&str] = &["paragraph", "inline", "text", "block_continuation"];

/// Последний лист узла.
fn last_leaf(mut n: Node<'_>) -> Node<'_> {
    while let Some(c) = n
        .child_count()
        .checked_sub(1)
        .and_then(|i| u32::try_from(i).ok())
        .and_then(|i| n.child(i))
    {
        n = c;
    }
    n
}

/// Пустые строки в конце скрытого остаются видны (у TOML таблица забирает их себе).
fn trim_blank(t: &[u8], start: usize, mut end: usize, feed: Feed) -> usize {
    let u = feed.unit();
    while end > start {
        let body = end.saturating_sub(u).max(start);
        let ls = nls(&t[start..body], feed)
            .next_back()
            .map_or(start, |p| start + p + u);
        if t[ls..end]
            .iter()
            .all(|b| b.is_ascii_whitespace() || *b == 0)
        {
            end = ls;
        } else {
            break;
        }
    }
    end
}

/// Места свёртки с заголовком в `range`: на строку — внешний именованный узел, который
/// с неё начинается и занимает больше строки (кроме корня). Закрывающая скобка в начале
/// последней строки остаётся видна (`}` у JSON).
pub fn folds(p: &Parsed, range: Range<usize>) -> Vec<FoldAt> {
    let t = &p.text[..];
    let feed = p.feed;
    let range = range.start.min(t.len())..range.end.min(t.len());
    let root = p.tree.root_node();
    let mut rows = FxHashSet::default();
    let mut out = Vec::new();
    walk_in(root, &range, |n| {
        let (s, e) = (n.start_position(), n.end_position());
        let last = if e.column == 0 {
            e.row.saturating_sub(1)
        } else {
            e.row
        };
        if n != root
            && n.is_named()
            && last > s.row
            && n.start_byte() >= range.start
            && !NO_FOLD.contains(&n.kind())
            && rows.insert(s.row)
        {
            let head = n.start_byte() - s.column;
            let start = line_after(t, n.start_byte(), feed);
            let mut end = if e.column == 0 {
                n.end_byte()
            } else {
                line_after(t, n.end_byte(), feed)
            };
            let leaf = last_leaf(n);
            let lp = leaf.start_position();
            if !leaf.is_named() && lp.row == last && lp.row > s.row {
                let ls = leaf.start_byte() - lp.column;
                if t[ls..leaf.start_byte()]
                    .iter()
                    .all(|b| matches!(b, b' ' | b'\t' | 0))
                {
                    end = ls;
                }
            }
            let end = trim_blank(t, start, end, feed);
            if start < end {
                out.push(FoldAt {
                    head: head as u64,
                    start: start as u64,
                    end: end as u64,
                });
            }
        }
        true
    });
    out.sort_unstable_by_key(|f| f.head);
    out.dedup_by_key(|f| f.head);
    out
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
