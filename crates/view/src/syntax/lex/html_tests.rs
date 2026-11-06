//! HTML: лексер = tree-sitter + запрос, свёртка = узлы дерева (ADR-0024 §6). Сравнение с
//! деревом требует пакета (`HL_PACK`), окно = проход с начала и правки — нет.

#![allow(clippy::many_single_char_names, reason = "тесты: текст, точки, окно")]

use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use tree_sitter::Node;

use super::super::Lang;
use super::super::testkit::{
    Mem, cases, check_windows, ext_files, first_diff, lex_roles, marks_of, repo_files, ts_roles,
};
use super::super::{Feed, Src};
use super::{NAMES, TAGS_MAX, id};
use crate::enc::Enc;
use crate::syntax::marks::{Marks, advance};
use crate::syntax::win::{Window, window};
use crate::syntax::{Span, grammar};

const NEVER: fn() -> bool = || false;

fn has_pack() -> bool {
    grammar("html").is_some()
}

fn tree(text: &str) -> Option<tree_sitter::Tree> {
    let g = grammar("html")?;
    let mut p = tree_sitter::Parser::new();
    p.set_language(&g.lang).ok()?;
    p.parse(text, None)
}

fn whole(t: &[u8]) -> Window {
    let m = marks_of(Lang::Html, t);
    let mem = Mem(t.to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    window(&src, Enc::utf8(), Lang::Html, &m, 0, t.len() as u64, &NEVER)
        .expect("window")
        .expect("marks")
}

fn line_after(t: &[u8], off: usize) -> usize {
    memchr::memchr(b'\n', &t[off..]).map_or(t.len(), |p| off + p + 1)
}

fn line_of(t: &[u8], off: usize) -> usize {
    memchr::memrchr(b'\n', &t[..off]).map_or(0, |p| p + 1)
}

fn blank(s: &[u8]) -> bool {
    s.iter().all(|c| matches!(c, b' ' | b'\t'))
}

fn trim_blank(t: &[u8], start: usize, mut end: usize) -> usize {
    while end > start {
        let ls = line_of(t, end.saturating_sub(1).max(start)).max(start);
        if t[ls..end].iter().all(u8::is_ascii_whitespace) {
            end = ls;
        } else {
            break;
        }
    }
    end
}

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

/// Свёртка дерева — правило `edit::folds`, и одно отличие: узел без своего закрывающего
/// листа (элемент с неявным закрытием, `raw_text`), конец которого стоит первым на строке,
/// эту строку не прячет. `edit::folds` прячет её вместе с началом следующего тега: `<li>`
/// сворачивался вместе с заголовком следующего `<li>`.
fn tree_folds(text: &str, root: Node<'_>) -> Vec<(u64, u64, u64)> {
    let t = text.as_bytes();
    let mut rows = FxHashSet::default();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let mut w = n.walk();
        let kids: Vec<_> = n.children(&mut w).collect();
        stack.extend(kids.into_iter().rev());
        let (s, e) = (n.start_position(), n.end_position());
        let last = if e.column == 0 {
            e.row.saturating_sub(1)
        } else {
            e.row
        };
        if n == root || !n.is_named() || last <= s.row || n.kind() == "text" {
            continue;
        }
        if !rows.insert(s.row) {
            continue;
        }
        let start = line_after(t, n.start_byte());
        let mut end = if e.column == 0 {
            n.end_byte()
        } else {
            line_after(t, n.end_byte())
        };
        let leaf = last_leaf(n);
        let p = if leaf.is_named() || leaf.end_byte() < n.end_byte() {
            n.end_byte()
        } else {
            leaf.start_byte()
        };
        let ls = line_of(t, p);
        if ls > line_of(t, n.start_byte()) && blank(&t[ls..p]) {
            end = ls;
        }
        let end = trim_blank(t, start, end);
        if start < end {
            out.push(((n.start_byte() - s.column) as u64, start as u64, end as u64));
        }
    }
    out.sort_unstable();
    out
}

fn lex_folds(t: &[u8]) -> Vec<(u64, u64, u64)> {
    let mut v: Vec<_> = whole(t)
        .folds
        .iter()
        .map(|f| (f.head, f.start, f.end))
        .collect();
    v.sort_unstable();
    v
}

/// `<!--` сразу за кавычкой значения: сканер делает из него комментарий, если `-->` есть
/// хоть где-то ниже. Лексер красит по спецификации: значение (ADR-0024 §6 п. 4).
fn oracle_bug(root: Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "comment"
            && n.parent()
                .is_some_and(|p| p.kind() == "quoted_attribute_value")
        {
            return true;
        }
        let mut w = n.walk();
        stack.extend(n.children(&mut w));
    }
    false
}

/// Роли и свёртка = дерево; `Ok(false)` — истины нет (ошибка в дереве).
fn check(text: &str) -> Result<bool, String> {
    let Some(tree) = tree(text) else {
        return Ok(false);
    };
    let root = tree.root_node();
    if root.has_error() || oracle_bug(root) {
        return Ok(false);
    }
    let want = ts_roles(Lang::Html, text).expect("roles");
    let t = text.as_bytes();
    let got = lex_roles(Lang::Html, t);
    if let Some(d) = first_diff(t, &want, &got, 0, t.len()) {
        return Err(format!("{d}\n--- text ---\n{text}"));
    }
    let (want, got) = (tree_folds(text, root), lex_folds(t));
    if want != got {
        let only_w: Vec<_> = want.iter().filter(|x| !got.contains(x)).collect();
        let only_g: Vec<_> = got.iter().filter(|x| !want.contains(x)).collect();
        return Err(format!(
            "свёртка: дерево {only_w:?}, лексер {only_g:?}\n--- text ---\n{text}"
        ));
    }
    Ok(true)
}

fn ok(text: &str) -> bool {
    match check(text) {
        Ok(v) => v,
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn table_sorted() {
    assert!(NAMES.is_sorted());
    assert_eq!(id("A"), 1);
}

#[test]
fn samples() {
    if !has_pack() {
        return;
    }
    let mut n = 0;
    for s in [
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=utf-8>\n<title>т</title>\n<link rel=\"a\" href='b'>\n<style>\n a { color: red }\n</style>\n</head>\n<body class=x>\n<script src=\"a.js\"></script>\n<script>\n if (a < b) { c(\"</div>\"); }\n</script>\n<ul>\n  <li>a\n  <li>b<br>\n    c\n</ul>\n<p>d\n<p>e\n<table>\n<colgroup><col><col>\n<tr><td>1<td>2\n<tr><th>3\n</table>\n</body>\n</html>\n",
        "<div\n  id=a\n  class=\"b\n   c\"\n  data-x = 'y'\n>\n  t &amp; u\n  <!-- c\n   d -->\n</div\n>\n",
        "<a-b><c-d></x-y></c-d>\n<img src=x/>\n< div >t</ div >\n<br/>\n",
        "<p>a<div>b</div>\n<dl><dt>x<dd>y<dt>z</dl>\n<select><optgroup><option>1<optgroup><option>2</select>\n<ruby>a<rt>b<rp>c</ruby>\n",
        "<div <!-- c --> a=<!-- d -->b>e</div>\n<!doctype <!-- f --> html>\n",
    ] {
        n += usize::from(ok(s));
    }
    assert!(n >= 4, "{n}");
}

fn name() -> BoxedStrategy<String> {
    prop_oneof![
        5 => prop_oneof![
            Just("div"), Just("span"), Just("a"), Just("b"), Just("ul"), Just("ol"), Just("table"),
            Just("section"), Just("h1"), Just("pre"), Just("form"), Just("TD"), Just("Li"), Just("P")
        ],
        4 => prop_oneof![
            Just("li"), Just("p"), Just("td"), Just("th"), Just("tr"), Just("dt"), Just("dd"),
            Just("rb"), Just("rt"), Just("rp"), Just("optgroup"), Just("option"), Just("colgroup")
        ],
        1 => prop_oneof![Just("x-foo"), Just("my:tag"), Just("a-b"), Just("custom"), Just("имя")],
    ]
    .prop_map(str::to_owned)
    .boxed()
}

fn ws() -> BoxedStrategy<String> {
    prop_oneof![
        3 => Just(String::new()),
        2 => Just("\n".to_owned()),
        1 => Just("  ".to_owned()),
        1 => Just("\n    ".to_owned()),
        1 => Just("\r\n".to_owned()),
    ]
    .boxed()
}

fn attr() -> BoxedStrategy<String> {
    let v = prop_oneof![
        Just(String::new()),
        "=[a-zA-Z0-9/#:.;-]{1,6}",
        "=\"[a-zA-Z0-9 <>'=/\\n-]{0,10}\"",
        "='[a-zA-Z0-9 <>\"=/\\n-]{0,10}'",
        Just("=\"\"".to_owned()),
        Just(" = \"y\"".to_owned()),
        Just("\n=\n'z\nw'".to_owned()),
        Just("=<!-- c -->q".to_owned()),
    ];
    (
        prop_oneof![
            Just("id"),
            Just("class"),
            Just("data-x"),
            Just("{{"),
            Just("@c"),
            Just("a_b"),
            Just("on:x")
        ],
        v,
        prop_oneof![Just(" "), Just("\n  "), Just(" <!-- e --> ")],
    )
        .prop_map(|(n, v, s)| format!("{s}{n}{v}"))
        .boxed()
}

fn leaf() -> BoxedStrategy<String> {
    prop_oneof![
        4 => "[a-zA-Zа-я0-9 .,:;!?()\\n-]{1,12}",
        1 => Just("&amp;".to_owned()),
        1 => Just("&#x1F;".to_owned()),
        1 => "<!--[a-z <>\\n-]{0,10}-->",
        1 => Just("<!---->".to_owned()),
        1 => Just("<!-- a -- b --->".to_owned()),
        1 => Just("<br>".to_owned()),
        1 => Just("<hr/>".to_owned()),
        1 => Just("<img src=\"a.png\" alt=x>".to_owned()),
        1 => Just("<input\n  disabled\n  value='v'>".to_owned()),
        1 => Just("<x-y/>".to_owned()),
        1 => Just("</nothing>".to_owned()),
        1 => Just("<script>\n a < b; '</div>'; </sc</script> b(); </script\n>".to_owned()),
        1 => Just("<style media=x>a { b: c }\n  </STYLE>".to_owned()),
        1 => Just("<script></script>".to_owned()),
        1 => Just("<SCRIPT\n src=a\n>\n x\n y\n  </script>".to_owned()),
    ]
    .boxed()
}

fn node(depth: u32) -> BoxedStrategy<String> {
    if depth == 0 {
        return leaf();
    }
    prop_oneof![
        2 => leaf(),
        3 => element(depth - 1),
    ]
    .boxed()
}

fn element(depth: u32) -> BoxedStrategy<String> {
    (
        name(),
        proptest::collection::vec(attr(), 0..3),
        ws(),
        proptest::collection::vec((node(depth), ws()), 0..4),
        prop_oneof![
            4 => Just(0u8),
            2 => Just(1u8),
            1 => Just(2u8),
            1 => Just(3u8)
        ],
    )
        .prop_map(|(n, a, w, kids, end)| {
            let body: String = kids.into_iter().map(|(k, s)| k + &s).collect();
            let a = a.concat();
            match end {
                // Без закрывающего тега: закроет сосед или родитель.
                1 => format!("<{n}{a}>{w}{body}"),
                2 => format!("<{n}{a}{w}/>"),
                3 => format!("< {n}{a}>{w}{body}</ {n}{w}>"),
                _ => format!("<{n}{a}>{w}{body}</{n}{w}>"),
            }
        })
        .boxed()
}

fn document() -> BoxedStrategy<String> {
    (
        proptest::option::of(prop_oneof![
            Just("<!DOCTYPE html>\n"),
            Just("<!doctype html PUBLIC \"-//W3C//DTD HTML 4.01//EN\"\n \"x.dtd\">\n"),
            Just("<!-- pre -->\n<! doctype\n html\n>")
        ]),
        proptest::collection::vec(element(3), 1..4),
        prop_oneof![
            Just(("", "")),
            Just(("<html>\n<body>\n", "")),
            Just(("<html><head><title>t</title>\n<body>", "</html>\n")),
            Just(("<div>", "</div>\n")),
        ],
    )
        .prop_map(|(d, e, (a, b))| format!("{}{a}{}{b}", d.unwrap_or_default(), e.concat()))
        .boxed()
}

/// Произвольная цепочка кусков: неверные документы и все переходы автомата.
fn soup() -> BoxedStrategy<String> {
    let a = [
        "<",
        ">",
        "</",
        "/>",
        "/",
        "<!",
        "<!--",
        "-->",
        "--",
        "-",
        "=",
        "\"",
        "'",
        " ",
        "\n",
        "\t",
        "\r\n",
        "a",
        "div",
        "br",
        "li",
        "p",
        "td",
        "x-y",
        "script",
        "style",
        "<script>",
        "</script>",
        "<style>",
        "</style",
        "<br>",
        "<li>",
        "<p>",
        "</p>",
        "</li>",
        "<div ",
        "id=",
        "<!doctype",
        "doctype",
        "&amp;",
        "&",
        "я",
        "</scr",
        "<tr>",
        "<td>",
    ];
    proptest::collection::vec(0..a.len(), 0..200)
        .prop_map(move |v| v.into_iter().map(|i| a[i]).collect())
        .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(300), ..ProptestConfig::default() })]

    #[test]
    fn generated_documents(doc in document()) {
        if has_pack() {
            let r = check(&doc);
            prop_assert!(r.is_ok(), "{}", r.unwrap_err());
        }
    }

    #[test]
    fn soup_matches_tree(doc in soup()) {
        if has_pack() {
            let r = check(&doc);
            prop_assert!(r.is_ok(), "{}", r.unwrap_err());
        }
    }

    #[test]
    fn windows_match_full_pass(doc in document(), cuts in proptest::collection::vec((0u64..3000, 0u64..3000), 1..6)) {
        let r = check_windows(Lang::Html, doc.as_bytes(), &cuts);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn windows_any_bytes(doc in soup(), cuts in proptest::collection::vec((0u64..3000, 0u64..3000), 1..6)) {
        let r = check_windows(Lang::Html, doc.as_bytes(), &cuts);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}

#[test]
fn generator_is_valid_mostly() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    if !has_pack() {
        return;
    }
    let mut runner = TestRunner::deterministic();
    let mut good = 0;
    for _ in 0..300 {
        let doc = document().new_tree(&mut runner).expect("doc").current();
        if check(&doc).expect("lexer") {
            good += 1;
        }
    }
    assert!(good > 120, "только {good} из 300 без ошибок tree-sitter");
}

/// Вложенность глубже памяти лексера: окно с любой точки = проход с начала, стек на точках
/// не растёт неограниченно.
#[test]
fn deep_nesting() {
    let n = TAGS_MAX * 3;
    let mut text = "<div>\n".repeat(n);
    text.push_str("t\n");
    text.push_str(&"</nothing></div>\n".repeat(n));
    let t = text.as_bytes();
    let m = marks_of(Lang::Html, t);
    let used: usize = m.tags.iter().map(|x| x.1.len()).sum();
    assert!(used <= m.v.len() * crate::syntax::marks::TAGS_PER_MARK + 1);
    let cuts: Vec<(u64, u64)> = (0..12u64)
        .map(|i| {
            let a = t.len() as u64 * i / 12;
            (a, a + 3000)
        })
        .collect();
    check_windows(Lang::Html, t, &cuts).expect("windows");
}

/// Внешний корпус: роли и свёртка = дерево; на файлах с ошибкой в дереве — доля байт, где
/// лексер красит иначе, чем восстановление tree-sitter (только в журнал).
#[test]
fn html_corpus() {
    if !has_pack() {
        return;
    }
    let ext = ["html", "htm", "xhtml"];
    let mut files = repo_files(&ext);
    files.extend(ext_files(&ext));
    let max: u64 = std::env::var("HL_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512 << 10);
    let (same, skip) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let (err_bytes, err_diff) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let bad: Vec<String> = files
        .par_iter()
        .filter_map(|p| {
            if std::fs::metadata(p).ok()?.len() > max {
                return None;
            }
            let text = std::fs::read_to_string(p).ok()?;
            match check(&text) {
                Err(e) => Some(format!(
                    "{}: {}",
                    p.display(),
                    e.split("--- text").next().unwrap_or("")
                )),
                Ok(true) => {
                    same.fetch_add(1, Ordering::Relaxed);
                    windows(&text)
                        .err()
                        .map(|e| format!("{}: {e}", p.display()))
                }
                Ok(false) => {
                    skip.fetch_add(1, Ordering::Relaxed);
                    let (want, got) = error_roles(&text);
                    let d = want.iter().zip(&got).filter(|(a, b)| a != b).count();
                    err_bytes.fetch_add(want.len(), Ordering::Relaxed);
                    err_diff.fetch_add(d, Ordering::Relaxed);
                    if d > 0
                        && std::env::var_os("HL_ERRS").is_some()
                        && let Some(e) = first_diff(text.as_bytes(), &want, &got, 0, want.len())
                    {
                        eprintln!(
                            "ОШИБКА {} ({d}): {}",
                            p.display(),
                            e.chars().take(240).collect::<String>()
                        );
                    }
                    if let Err(e) = windows(&text) {
                        return Some(format!("{}: {e}", p.display()));
                    }
                    None
                }
            }
        })
        .collect();
    eprintln!(
        "html: файлов {} совпало {} с ошибками {} расхождений {}; на файлах с ошибкой иначе {} из {} байт",
        files.len(),
        same.load(Ordering::Relaxed),
        skip.load(Ordering::Relaxed),
        bad.len(),
        err_diff.load(Ordering::Relaxed),
        err_bytes.load(Ordering::Relaxed),
    );
    for b in bad.iter().take(20) {
        eprintln!("{b}");
    }
    assert!(bad.is_empty(), "расхождений: {}", bad.len());
}

/// Окна на мелких точках = весь текст: роли и свёртка.
fn windows(text: &str) -> Result<(), String> {
    let t = text.as_bytes();
    let n = t.len() as u64;
    let mem = Mem(t.to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    let mut m = Marks::new(0, n);
    m.step = 509;
    advance(&mut m, &src, Lang::Html, u64::MAX, &NEVER).map_err(|e| e.to_string())?;
    let full = whole(t);
    let mut x = n.wrapping_mul(2_654_435_761) ^ 0x9E37;
    for _ in 0..8 {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let a = (x >> 20) % (n + 1);
        let b = (a + 1 + ((x >> 8) & 0x3FFF)).min(n);
        if a >= b {
            continue;
        }
        let w = window(&src, Enc::utf8(), Lang::Html, &m, a, b, &NEVER)
            .map_err(|e| e.to_string())?
            .ok_or("нет точек")?;
        let inside = |s: &&Span| s.end > a && s.start < b;
        let want: Vec<(u64, u64, u8)> = full
            .spans
            .iter()
            .filter(inside)
            .map(|s| (s.start.max(a), s.end.min(b), s.tag as u8))
            .collect();
        let got: Vec<(u64, u64, u8)> = w
            .spans
            .iter()
            .map(|s| (s.start, s.end, s.tag as u8))
            .collect();
        if want != got {
            return Err(format!("окно {a}..{b}: роли"));
        }
        for f in &w.folds {
            if !full.folds.iter().any(|g| g.head == f.head && g == f) {
                return Err(format!("окно {a}..{b}: свёртка {f:?}"));
            }
        }
    }
    Ok(())
}

/// Роли полного разбора с ошибкой и лексера.
fn error_roles(text: &str) -> (Vec<u8>, Vec<u8>) {
    let g = grammar("html").expect("grammar");
    let t = text.as_bytes();
    let mut want = vec![0u8; t.len()];
    for s in crate::syntax::highlight(&g, text, &NEVER).expect("highlight") {
        want[s.start as usize..s.end as usize].fill(s.tag as u8);
    }
    let mut got = lex_roles(Lang::Html, t);
    for (i, c) in t.iter().enumerate() {
        if super::super::is_ws(*c) {
            want[i] = 0;
            got[i] = 0;
        }
    }
    (want, got)
}
