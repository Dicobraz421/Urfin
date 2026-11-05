//! Markdown-команды редактора (S7 §5.9, карточка S7-24): `editor.md.*` над кусками
//! документа — одна группа отмены на команду, текст не копируется в UI.
//!
//! Модуль-потомок `input` (нужны байты из кэшей и журнал). Команда читает из кэша окно —
//! строки выделения и по одной соседней (`editor.md.code`: ограждение вокруг), считает
//! правки по UTF-8 тексту окна ([`apply`], чистая функция — её и проверяют proptest'ы) и
//! кладёт их в документ в его кодировке. Страниц нет — действие ждёт их (`Missing`), как
//! любое другое. Контекст кнопок панели — [`context`]: разбор одной строки, без разбора
//! документа.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    BlockOut, Done, Engine, Env, Sel, Selections, encode, eol_str, missing_range, read_range,
};
use crate::cache::Missing;
use crate::edit::undo::Kind;
use crate::enc::{CharDecoder, Step};

/// Выделение длиннее команды Markdown не берут: окно читается из кэша страниц целиком.
pub const SEL_MAX: u64 = 4 << 20;
/// Контекст кнопок — по началу строки не длиннее.
const CTX_MAX: u64 = 4 << 10;
/// Столбцов в шаблоне таблицы.
const COLS: usize = 3;

/// Команда `editor.md.*` (и флажок задачи в предпросмотре).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MdAct {
    Bold,
    Italic,
    /// 0 — обычный текст, 1–3 — уровень.
    Heading(u8),
    List,
    Numbered,
    Task,
    Link,
    Code,
    Table,
    /// Клик по флажку в предпросмотре: `[ ]` ↔ `[x]` пункта, начинающегося на этом байте.
    Check(u64),
}

/// Правка окна: заменить `at` текстом `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub at: Range<usize>,
    pub text: String,
}

/// Правки по возрастанию `at` и выделение в тексте после них (пустое — курсор).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Out {
    pub edits: Vec<Edit>,
    pub sel: Range<usize>,
}

/// Состояние кнопок панели по строке курсора.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые состояния кнопок"
)]
pub struct MdCtx {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Уровень заголовка, 0 — не заголовок.
    pub heading: u8,
    pub list: bool,
    pub numbered: bool,
    pub task: bool,
}

// ------------------------------------------------------------ строки

/// Содержимое строк окна, без концов строк (`\n`, `\r\n`, `\r`).
fn lines(t: &str) -> Vec<Range<usize>> {
    let b = t.as_bytes();
    let mut out = Vec::new();
    let (mut s, mut i) = (0, 0);
    while i < b.len() {
        match b[i] {
            b'\n' => {
                out.push(s..i);
                i += 1;
                s = i;
            }
            b'\r' => {
                out.push(s..i);
                i += if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                s = i;
            }
            _ => i += 1,
        }
    }
    out.push(s..b.len());
    out
}

/// Индексы строк, задетых выделением; строка, перед которой оно кончается, — нет.
fn picked(ls: &[Range<usize>], sel: &Range<usize>) -> Range<usize> {
    let a = ls
        .partition_point(|l| l.end < sel.start)
        .min(ls.len().saturating_sub(1));
    let z = ls.partition_point(|l| l.start < sel.end);
    a..z.max(a + 1)
}

/// Вид пункта списка.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mark {
    None,
    Bullet,
    Num,
    Task,
}

/// Разбор начала строки: `lead` — отступ и `>`, `body` — начало текста за маркером.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Pre {
    lead: usize,
    mark: Mark,
    body: usize,
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    i
}

/// Отступ и цитаты `>` в начале строки.
fn lead_of(b: &[u8]) -> usize {
    let mut i = skip_ws(b, 0);
    while b.get(i) == Some(&b'>') {
        i = skip_ws(b, i + 1);
    }
    i
}

/// Конец маркера `- ` / `N. ` с `i` (с одним пробелом) и нумерованный ли он.
fn marker_at(b: &[u8], i: usize) -> Option<(usize, bool)> {
    let (end, num) = match b.get(i)? {
        b'-' | b'*' | b'+' => (i + 1, false),
        c if c.is_ascii_digit() => {
            let mut k = i;
            while b.get(k).is_some_and(u8::is_ascii_digit) {
                k += 1;
            }
            if k - i > 9 || !matches!(b.get(k), Some(b'.' | b')')) {
                return None;
            }
            (k + 1, true)
        }
        _ => return None,
    };
    matches!(b.get(end), Some(b' ' | b'\t')).then_some((end + 1, num))
}

/// `[ ] ` / `[x] ` с `i`: конец и положение знака.
fn task_at(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let tail = b.get(i..i + 3)?;
    if tail[0] != b'[' || tail[2] != b']' || !matches!(tail[1], b' ' | b'x' | b'X') {
        return None;
    }
    match b.get(i + 3) {
        Some(b' ' | b'\t') => Some((i + 4, i + 1)),
        None => Some((i + 3, i + 1)),
        _ => None,
    }
}

fn parse(line: &str) -> Pre {
    let b = line.as_bytes();
    let lead = lead_of(b);
    let Some((end, num)) = marker_at(b, lead) else {
        return Pre {
            lead,
            mark: Mark::None,
            body: lead,
        };
    };
    if !num && let Some((e, _)) = task_at(b, end) {
        return Pre {
            lead,
            mark: Mark::Task,
            body: skip_ws(b, e),
        };
    }
    Pre {
        lead,
        mark: if num { Mark::Num } else { Mark::Bullet },
        body: skip_ws(b, end),
    }
}

/// Заголовок ATX с `lead`: уровень (0 — нет) и начало текста.
fn heading_of(line: &str, lead: usize) -> (u8, usize) {
    let b = line.as_bytes();
    let n = b[lead..].iter().take_while(|&&c| c == b'#').count();
    if n == 0 || n > 6 {
        return (0, lead);
    }
    #[allow(clippy::cast_possible_truncation, reason = "1…6")]
    let level = n as u8;
    match b.get(lead + n) {
        Some(b' ' | b'\t') => (level, skip_ws(b, lead + n)),
        None => (level, lead + n),
        _ => (0, lead),
    }
}

// ------------------------------------------------------------ строчные стили

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Style {
    Strong,
    Em,
    Code,
}

/// Строчные стили одной строки: разбор pulldown-cmark только её.
fn spans(line: &str) -> Vec<(Style, Range<usize>)> {
    let mut out = Vec::new();
    for (ev, r) in Parser::new_ext(line, Options::empty()).into_offset_iter() {
        match ev {
            Event::Start(Tag::Strong) => out.push((Style::Strong, r)),
            Event::Start(Tag::Emphasis) => out.push((Style::Em, r)),
            Event::Code(_) => out.push((Style::Code, r)),
            _ => {}
        }
    }
    out
}

/// Подряд `c` с обеих сторон от `r` (меньшая из двух серий).
fn run_around(t: &str, r: &Range<usize>, c: u8) -> usize {
    let b = t.as_bytes();
    let before = b[..r.start].iter().rev().take_while(|&&x| x == c).count();
    let after = b[r.end..].iter().take_while(|&&x| x == c).count();
    before.min(after)
}

/// Подряд `c` внутри `r` с обоих краёв (края не пересекаются).
fn run_inside(t: &str, r: &Range<usize>, c: u8) -> usize {
    let b = &t.as_bytes()[r.clone()];
    let lead = b.iter().take_while(|&&x| x == c).count();
    let trail = b.iter().rev().take_while(|&&x| x == c).count();
    lead.min(trail).min(b.len() / 2)
}

/// `n` звёздочек вокруг: стиль маркера `m` включён (`**` — бит 2, `*` — бит 1).
fn styled(n: usize, m: usize) -> bool {
    if m == 2 { n % 4 >= 2 } else { n % 2 == 1 }
}

/// Слово под курсором (и вплотную к нему).
fn word_at(t: &str, ls: &[Range<usize>], c: usize) -> Option<Range<usize>> {
    let line = ls[picked(ls, &(c..c)).start].clone();
    t[line.clone()]
        .split_word_bound_indices()
        .map(|(i, w)| (line.start + i, w))
        .find(|(i, w)| {
            *i <= c && c <= *i + w.len() && w.chars().any(|x| x.is_alphanumeric() || x == '_')
        })
        .map(|(i, w)| i..i + w.len())
}

/// Выделение без пробелов по краям (из одних пробелов — как есть).
fn trimmed(t: &str, r: Range<usize>) -> Range<usize> {
    let s = &t[r.clone()];
    let a = s.trim_start();
    if a.is_empty() {
        return r;
    }
    let start = r.end - a.len();
    start..start + a.trim_end().len()
}

/// Позиция `p` после правок: `left` — у края правки остаётся слева от неё.
fn shifted(p: usize, edits: &[Edit], left: bool) -> usize {
    let mut d = 0isize;
    for e in edits {
        if p < e.at.start || (left && p == e.at.start) {
            break;
        }
        if p >= e.at.end {
            d += e.text.len().cast_signed() - e.at.len().cast_signed();
        } else {
            return (e.at.start.cast_signed() + d).cast_unsigned() + e.text.len();
        }
    }
    (p.cast_signed() + d).cast_unsigned()
}

fn del(at: Range<usize>) -> Edit {
    Edit {
        at,
        text: String::new(),
    }
}

fn ins(at: usize, text: &str) -> Edit {
    Edit {
        at: at..at,
        text: text.to_owned(),
    }
}

/// Выделение после правок (пустое — курсор).
fn sel_after(sel: &Range<usize>, edits: &[Edit]) -> Range<usize> {
    if sel.is_empty() {
        let c = shifted(sel.start, edits, false);
        c..c
    } else {
        shifted(sel.start, edits, true)..shifted(sel.end, edits, false)
    }
}

/// Снять разделители длины `m` с краёв `outer`: выделение — на том же тексте.
fn unwrap(outer: Range<usize>, m: usize, sel: &Range<usize>) -> Out {
    let edits = if outer.len() == 2 * m {
        vec![del(outer)]
    } else {
        vec![
            del(outer.start..outer.start + m),
            del(outer.end - m..outer.end),
        ]
    };
    let sel = sel_after(sel, &edits);
    Out { edits, sel }
}

/// Обернуть `r` строкой `mark`: выделение — на том же тексте (курсор — внутри).
fn wrap(r: &Range<usize>, mark: &str, sel: &Range<usize>) -> Out {
    let edits = if r.is_empty() {
        vec![ins(r.start, &mark.repeat(2))]
    } else {
        vec![ins(r.start, mark), ins(r.end, mark)]
    };
    let k = mark.len();
    let out = if sel.is_empty() {
        sel.start + k..sel.start + k
    } else {
        r.start + k..r.end + k
    };
    Out { edits, sel: out }
}

/// Отрезок стиля `st`, в котором стоит выделение (в одной строке).
fn span_of(t: &str, ls: &[Range<usize>], sel: &Range<usize>, st: Style) -> Option<Range<usize>> {
    let p = picked(ls, sel);
    if p.len() != 1 {
        return None;
    }
    let line = ls[p.start].clone();
    spans(&t[line.clone()])
        .into_iter()
        .filter(|(s, _)| *s == st)
        .map(|(_, r)| line.start + r.start..line.start + r.end)
        .find(|r| {
            if sel.is_empty() {
                r.start < sel.start && sel.start < r.end
            } else {
                r.start <= sel.start && sel.end <= r.end
            }
        })
}

/// `editor.md.bold` / `italic`: `m` — длина маркера.
fn emphasis(t: &str, sel: &Range<usize>, m: usize) -> Out {
    let ls = lines(t);
    let st = if m == 2 { Style::Strong } else { Style::Em };
    let r = if sel.is_empty() {
        word_at(t, &ls, sel.start).unwrap_or_else(|| sel.clone())
    } else {
        trimmed(t, sel.clone())
    };
    let keep = if sel.is_empty() { sel } else { &r };
    if !sel.is_empty() && styled(run_inside(t, &r, b'*'), m) {
        return unwrap(r.clone(), m, &r);
    }
    if styled(run_around(t, &r, b'*'), m) {
        return unwrap(r.start - m..r.end + m, m, keep);
    }
    // Выделение с маркером на краю — не текст внутри отрезка.
    let bare = !t[r.clone()].starts_with('*') && !t[r.clone()].ends_with('*');
    if bare && let Some(s) = span_of(t, &ls, sel, st) {
        return unwrap(s, m, keep);
    }
    wrap(&r, &"*".repeat(m), keep)
}

/// Самая длинная серия `` ` `` в тексте.
fn ticks(s: &str) -> usize {
    s.as_bytes()
        .chunk_by(|a, b| a == b)
        .filter(|c| c[0] == b'`')
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0)
}

/// Строка — ограждение кода: длина серии `` ` ``, 0 — нет.
fn fence_of(line: &str) -> usize {
    let n = line.trim_start().bytes().take_while(|&c| c == b'`').count();
    if n >= 3 { n } else { 0 }
}

/// `editor.md.code`, строка: `` `…` ``; уже код — снять.
fn code_inline(t: &str, ls: &[Range<usize>], sel: &Range<usize>) -> Out {
    let r = if sel.is_empty() {
        word_at(t, ls, sel.start).unwrap_or_else(|| sel.clone())
    } else {
        trimmed(t, sel.clone())
    };
    let keep = if sel.is_empty() { sel } else { &r };
    if !sel.is_empty() {
        let k = run_inside(t, &r, b'`');
        if k > 0 {
            return unwrap(r.clone(), k, &r);
        }
    }
    let k = run_around(t, &r, b'`');
    if k > 0 {
        return unwrap(r.start - k..r.end + k, k, keep);
    }
    if let Some(s) = span_of(t, ls, sel, Style::Code) {
        let k = t[s.clone()].bytes().take_while(|&c| c == b'`').count();
        return unwrap(s, k, keep);
    }
    wrap(&r, &"`".repeat(ticks(&t[r.clone()]) + 1), keep)
}

/// Убрать строки-ограждения `first` и `last` (со своими концами строк).
fn unfence(ls: &[Range<usize>], first: usize, last: usize) -> Out {
    let edits = vec![
        del(ls[first].start..ls[first + 1].start),
        del(ls[last - 1].end..ls[last].end),
    ];
    let c = ls[first].start;
    let end = shifted(ls[last - 1].end, &edits, true);
    Out {
        edits,
        sel: c..end.max(c),
    }
}

/// `editor.md.code`, несколько строк: ограждение вокруг, курсор — на месте языка.
fn code_block(t: &str, ls: &[Range<usize>], p: &Range<usize>, eol: &str) -> Out {
    let (first, last) = (p.start, p.end - 1);
    if fence_of(&t[ls[first].clone()]) > 0 && fence_of(&t[ls[last].clone()]) > 0 {
        return unfence(ls, first, last);
    }
    if first > 0
        && last + 1 < ls.len()
        && fence_of(&t[ls[first - 1].clone()]) > 0
        && fence_of(&t[ls[last + 1].clone()]) > 0
    {
        return unfence(ls, first - 1, last + 1);
    }
    let n = ticks(&t[ls[first].start..ls[last].end]).max(2) + 1;
    let fence = "`".repeat(n);
    let c = ls[first].start + fence.len();
    Out {
        edits: vec![
            ins(ls[first].start, &format!("{fence}{eol}")),
            ins(ls[last].end, &format!("{eol}{fence}")),
        ],
        sel: c..c,
    }
}

/// Похоже на URL или путь: `[|](выделение)`.
fn is_target(s: &str) -> bool {
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = s.to_lowercase();
    if lower.contains("://")
        || lower.starts_with("mailto:")
        || lower.starts_with("www.")
        || s.starts_with('#')
        || s.contains(['/', '\\'])
    {
        return true;
    }
    // Имя файла: `README.md`.
    s.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.starts_with(|c: char| c.is_ascii_alphabetic())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

fn link(t: &str, sel: &Range<usize>) -> Out {
    if sel.is_empty() {
        let c = sel.start + 3;
        return Out {
            edits: vec![ins(sel.start, "[]()")],
            sel: c..c,
        };
    }
    if is_target(&t[sel.clone()]) {
        let c = sel.start + 1;
        return Out {
            edits: vec![ins(sel.start, "[]("), ins(sel.end, ")")],
            sel: c..c,
        };
    }
    let c = sel.end + 3;
    Out {
        edits: vec![ins(sel.start, "["), ins(sel.end, "]()")],
        sel: c..c,
    }
}

/// Шаблон таблицы: своим абзацем, выделена первая ячейка шапки.
fn table(t: &str, sel: &Range<usize>, eol: &str, heads: &[String; COLS]) -> Out {
    let ls = lines(t);
    let line = ls[picked(&ls, &(sel.end..sel.end)).start].clone();
    let blank = t[line.clone()].trim().is_empty();
    let mut text = String::new();
    if !blank {
        text.push_str(eol);
        text.push_str(eol);
    }
    let first = text.len() + 2;
    text.push_str("| ");
    text.push_str(&heads.join(" | "));
    text.push_str(" |");
    text.push_str(eol);
    text.push('|');
    text.push_str(&"---|".repeat(COLS));
    text.push_str(eol);
    text.push('|');
    text.push_str(&"   |".repeat(COLS));
    let at = if blank {
        line.clone()
    } else {
        line.end..line.end
    };
    let c = at.start + first;
    Out {
        sel: c..c + heads[0].len(),
        edits: vec![Edit { at, text }],
    }
}

/// `editor.md.heading`, списки: префиксы строк выделения. Пустые строки пропускаются,
/// если выделены не только они.
fn prefixes(act: &MdAct, t: &str, sel: &Range<usize>) -> Option<Out> {
    let ls = lines(t);
    let p = picked(&ls, sel);
    let mut idx: Vec<usize> = p
        .clone()
        .filter(|&i| !t[ls[i].clone()].trim().is_empty())
        .collect();
    if idx.is_empty() {
        idx = p.collect();
    }
    let pres: Vec<Pre> = idx.iter().map(|&i| parse(&t[ls[i].clone()])).collect();
    let mut edits = Vec::with_capacity(idx.len());
    if let MdAct::Heading(level) = act {
        let heads: Vec<(u8, usize)> = idx
            .iter()
            .zip(&pres)
            .map(|(&i, q)| heading_of(&t[ls[i].clone()], q.lead))
            .collect();
        let off = *level == 0 || heads.iter().all(|h| h.0 == *level);
        let mark = if off {
            String::new()
        } else {
            format!("{} ", "#".repeat(usize::from(*level)))
        };
        for (k, &i) in idx.iter().enumerate() {
            let s = ls[i].start;
            edits.push(Edit {
                at: s + pres[k].lead..s + heads[k].1,
                text: mark.clone(),
            });
        }
    } else {
        let kind = match act {
            MdAct::List => Mark::Bullet,
            MdAct::Numbered => Mark::Num,
            _ => Mark::Task,
        };
        let off = pres.iter().all(|q| q.mark == kind);
        for (k, &i) in idx.iter().enumerate() {
            let s = ls[i].start;
            let text = match kind {
                _ if off => String::new(),
                Mark::Bullet => "- ".to_owned(),
                Mark::Num => format!("{}. ", k + 1),
                _ => "- [ ] ".to_owned(),
            };
            edits.push(Edit {
                at: s + pres[k].lead..s + pres[k].body,
                text,
            });
        }
    }
    edits.retain(|e| t[e.at.clone()] != *e.text);
    if edits.is_empty() {
        return None;
    }
    let sel = sel_after(sel, &edits);
    Some(Out { edits, sel })
}

/// Правки команды над окном `t` с выделением `sel` (границы символов). `eol` — конец
/// новых строк, `heads` — подписи столбцов шаблона таблицы. `None` — менять нечего.
pub fn apply(
    act: &MdAct,
    t: &str,
    sel: &Range<usize>,
    eol: &str,
    heads: &[String; COLS],
) -> Option<Out> {
    match act {
        MdAct::Bold => Some(emphasis(t, sel, 2)),
        MdAct::Italic => Some(emphasis(t, sel, 1)),
        MdAct::Link => Some(link(t, sel)),
        MdAct::Table => Some(table(t, sel, eol, heads)),
        MdAct::Code => {
            let ls = lines(t);
            let p = picked(&ls, sel);
            Some(if p.len() > 1 {
                code_block(t, &ls, &p, eol)
            } else {
                code_inline(t, &ls, sel)
            })
        }
        MdAct::Heading(_) | MdAct::List | MdAct::Numbered | MdAct::Task => prefixes(act, t, sel),
        MdAct::Check(_) => None,
    }
}

/// Флажок пункта, начинающегося в начале `t`: положение знака и новый знак.
pub(crate) fn check_of(t: &[u8]) -> Option<(usize, u8)> {
    let (end, _) = marker_at(t, 0)?;
    let (_, at) = task_at(t, skip_ws(t, end))?;
    Some((at, if t[at] == b' ' { b'x' } else { b' ' }))
}

/// Контекст кнопок по строке `line` и курсору `caret` в ней.
pub fn context(line: &str, caret: usize) -> MdCtx {
    let pre = parse(line);
    let mut ctx = MdCtx {
        heading: heading_of(line, pre.lead).0,
        list: pre.mark == Mark::Bullet,
        numbered: pre.mark == Mark::Num,
        task: pre.mark == Mark::Task,
        ..MdCtx::default()
    };
    for (st, r) in spans(line) {
        if r.start < caret && caret < r.end {
            match st {
                Style::Strong => ctx.bold = true,
                Style::Em => ctx.italic = true,
                Style::Code => ctx.code = true,
            }
        }
    }
    ctx
}

// ------------------------------------------------------------ документ

/// Окно документа текстом.
struct Win {
    text: String,
    lo: u64,
    /// `None` — UTF-8 без замен: смещение + `lo`. Иначе — начала символов.
    map: Option<Vec<(usize, u64)>>,
}

impl Win {
    /// Байт документа для позиции окна.
    fn doc(&self, i: usize) -> u64 {
        match &self.map {
            None => self.lo + i as u64,
            Some(m) => {
                let k = m.partition_point(|x| x.0 < i);
                m.get(k)
                    .map_or_else(|| m.last().map_or(self.lo, |x| x.1), |x| x.1)
            }
        }
    }

    /// Позиция окна для байта документа.
    fn at(&self, off: u64) -> usize {
        match &self.map {
            None => usize::try_from(off.saturating_sub(self.lo)).unwrap_or(usize::MAX),
            Some(m) => {
                let k = m.partition_point(|x| x.1 < off);
                m.get(k).map_or(self.text.len(), |x| x.0)
            }
        }
    }
}

#[allow(
    clippy::many_single_char_names,
    reason = "a, b — края выделения, e — правка, r — диапазон, s — сегмент"
)]
impl Engine {
    /// Окно документа текстом: байты только из кэшей.
    fn md_win(&self, r: Range<u64>) -> Result<Win, Missing> {
        let b = read_range(&self.text.view(), r.clone())?;
        if self.enc.is_utf8()
            && let Ok(s) = std::str::from_utf8(&b)
        {
            return Ok(Win {
                text: s.to_owned(),
                lo: r.start,
                map: None,
            });
        }
        let mut dec = CharDecoder::new(self.enc);
        let mut text = String::with_capacity(b.len());
        let mut map = Vec::with_capacity(b.len() + 1);
        let mut i = 0usize;
        while i < b.len() {
            let s = dec.next(&b[i..]);
            map.push((text.len(), r.start + i as u64));
            text.push(match s {
                Step::Char(c, _) => c,
                Step::Bad(_) => '\u{FFFD}',
            });
            i += s.bytes().max(1);
        }
        map.push((text.len(), r.start + i as u64));
        Ok(Win {
            text,
            lo: r.start,
            map: Some(map),
        })
    }

    /// Байты, которые команде нужны целиком: выделение с запасом на строки вокруг.
    pub(super) fn md_need(&self, m: &Missing) -> Range<u64> {
        let (a, b) = self
            .sel
            .stream()
            .map_or((self.sel.caret, self.sel.caret), |r| (r.start, r.end));
        let ml = u64::from(self.max_line);
        let r = missing_range(m);
        a.saturating_sub(ml).min(r.start)..b.saturating_add(ml).min(self.len()).max(r.end)
    }

    /// Команда Markdown одной группой отмены.
    pub(super) fn md(&mut self, act: &MdAct, env: &Env) -> Result<Done, Missing> {
        if let MdAct::Check(off) = act {
            return self.md_check(*off, env);
        }
        let (sel, back) = match self.sel.stream() {
            Some(r) => {
                let back = self.sel.caret == r.start;
                (r, back)
            }
            None => (self.sel.caret..self.sel.caret, false),
        };
        if sel.end - sel.start > SEL_MAX {
            self.block_out = Some(BlockOut::MdTooBig);
            return Ok(Done::Same);
        }
        let first = self.seg_of(sel.start)?;
        let last = self.seg_of(sel.end)?;
        // Соседние строки — для ограждения кода вокруг выделения.
        let lo = if first.start > 0 {
            self.seg_of(self.prev_char(first.start)?)?.start
        } else {
            0
        };
        let hi = if last.next > last.end && last.next < self.len() {
            self.seg_of(last.next)?.end
        } else {
            last.end
        };
        let win = self.md_win(lo..hi.max(lo))?;
        let w = win.at(sel.start)..win.at(sel.end);
        let i18n = urfin_i18n::current();
        let heads = std::array::from_fn(|n| {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("n", n + 1);
            i18n.fmt("ui-md-table-col", &a)
        });
        let Some(out) = apply(act, &win.text, &w, eol_str(env.eol_new), &heads) else {
            return Ok(Done::Same);
        };
        let (a, b) = (
            self.md_pos(&win, &out, out.sel.start),
            self.md_pos(&win, &out, out.sel.end),
        );
        let before = self.sel;
        let after = if a == b {
            Selections::at(a)
        } else {
            let (caret, anchor) = if back { (a, b) } else { (b, a) };
            Selections {
                caret,
                sel: Sel::Stream { anchor },
            }
        };
        self.undo.begin_group(Kind::Command);
        let mut changed = false;
        for e in out.edits.iter().rev() {
            let r = win.doc(e.at.start)..win.doc(e.at.end);
            let bytes = encode(&self.enc, &e.text, true).unwrap_or_default();
            let res = if bytes.is_empty() {
                Ok(self.text.remove(r))
            } else {
                self.text.replace(r, &bytes)
            };
            match res {
                Ok(sp) => {
                    changed = true;
                    self.undo
                        .record(sp.into(), Kind::Command, &before, &after, env.now);
                }
                Err(e) => tracing::warn!(error = %e, "editor: edit buffer write failed"),
            }
        }
        if !changed {
            return Ok(Done::Same);
        }
        self.goal = None;
        self.sel = after;
        self.reveal_soft();
        Ok(Done::Edited)
    }

    /// Байт документа после правок для позиции `p` в тексте после них.
    fn md_pos(&self, win: &Win, out: &Out, p: usize) -> u64 {
        let size =
            |s: &str| encode(&self.enc, s, true).map_or(0, |b| i64::try_from(b.len()).unwrap_or(0));
        let (mut bytes, mut chars) = (0i64, 0isize);
        for e in &out.edits {
            let start = (e.at.start.cast_signed() + chars).cast_unsigned();
            if p < start {
                break;
            }
            if p <= start + e.text.len() {
                let d = bytes + size(&e.text[..p - start]);
                return win.doc(e.at.start).saturating_add_signed(d);
            }
            let was = win.doc(e.at.end) - win.doc(e.at.start);
            bytes += size(&e.text) - was.cast_signed();
            chars += e.text.len().cast_signed() - e.at.len().cast_signed();
        }
        win.doc((p.cast_signed() - chars).cast_unsigned())
            .saturating_add_signed(bytes)
    }

    /// Флажок задачи: `[ ]` ↔ `[x]` пункта на `off`; курсор и выделение — на месте.
    fn md_check(&mut self, off: u64, env: &Env) -> Result<Done, Missing> {
        let to = off.saturating_add(32 * self.unit()).min(self.len());
        let win = self.md_win(off.min(to)..to)?;
        let Some((at, ch)) = check_of(win.text.as_bytes()) else {
            return Ok(Done::Same);
        };
        let r = win.doc(at)..win.doc(at + 1);
        let bytes =
            encode(&self.enc, char::from(ch).encode_utf8(&mut [0; 4]), true).unwrap_or_default();
        let sel = self.sel;
        self.undo.begin_group(Kind::Command);
        match self.text.replace(r, &bytes) {
            Ok(sp) => {
                self.undo
                    .record(sp.into(), Kind::Command, &sel, &sel, env.now);
                Ok(Done::Edited)
            }
            Err(e) => {
                tracing::warn!(error = %e, "editor: edit buffer write failed");
                Ok(Done::Same)
            }
        }
    }

    /// Контекст кнопок панели: строка курсора из кэша (страниц нет — `None`).
    pub fn md_ctx(&self) -> Option<MdCtx> {
        let s = self.seg_of(self.sel.caret).ok()?;
        let win = self
            .md_win(s.start..s.end.min(s.start + CTX_MAX * self.unit()))
            .ok()?;
        let c = win.at(self.sel.caret).min(win.text.len());
        Some(context(&win.text, c))
    }
}

#[cfg(test)]
#[path = "md_tests.rs"]
mod tests;
