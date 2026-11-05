//! Цвет Markdown не зависит от положения окна на любом входе (ADR-0024 §6 п. 4), в том
//! числе там, где грамматика расходится с `CommonMark` в HTML-блоках: окно с любой точки =
//! проход с начала, зоны HTML-блоков трекера = `CommonMark`.

#![allow(clippy::many_single_char_names, reason = "тесты: текст, точки, окно")]

use proptest::prelude::*;

use super::super::testkit::{Mem, cases, first_diff, marks_of, win_roles};
use super::super::{Cx, Feed, Lang, Out, Scanner, Src, St, Stack};
use super::html_tests::STARTS;
use crate::syntax::Tag;
use crate::syntax::marks::{Marks, advance};
use crate::syntax::win::window;

/// Точки через `step` байт.
fn marks_step(text: &[u8], step: u64) -> Marks {
    let mem = Mem(text.to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    let mut m = Marks::new(0, mem.0.len() as u64);
    m.step = step;
    advance(&mut m, &src, Lang::Md, u64::MAX, &|| false).expect("pass");
    m
}

/// Окна `cuts` при шагах точек 31, 113 и одной точке = проход с начала.
fn same_in_windows(text: &[u8], cuts: &[(u64, u64)]) -> Result<(), String> {
    let n = text.len() as u64;
    let full = win_roles(Lang::Md, text, &marks_step(text, u64::MAX), 0, n);
    for step in [31, 113, u64::MAX] {
        let m = marks_step(text, step);
        for &(a, b) in cuts {
            let (a, b) = (a.min(b).min(n), b.max(a).min(n));
            let w = win_roles(Lang::Md, text, &m, a, b);
            if let Some(d) = first_diff(text, &full, &w, a as usize, b as usize) {
                return Err(format!(
                    "шаг {step}, окно {a}..{b}: {d}\n--- text ---\n{:?}",
                    String::from_utf8_lossy(text)
                ));
            }
        }
    }
    Ok(())
}

/// Окна с каждой строки: до конца строки, через строку и до конца текста.
fn line_cuts(text: &[u8]) -> Vec<(u64, u64)> {
    let starts: Vec<u64> = std::iter::once(0)
        .chain(memchr::memchr_iter(b'\n', text).map(|i| i as u64 + 1))
        .collect();
    let n = text.len() as u64;
    let mut out = Vec::new();
    for (i, &a) in starts.iter().enumerate() {
        out.push((a, starts.get(i + 1).copied().unwrap_or(n)));
        out.push((a, starts.get(i + 2).copied().unwrap_or(n)));
        out.push((a, n));
    }
    out
}

/// Позиции `Out::html` прохода с начала.
fn tracker_zones(text: &[u8]) -> Vec<u64> {
    #[derive(Default)]
    struct Rec(Vec<u64>);
    impl Out for Rec {
        fn html(&mut self, at: u64) {
            self.0.push(at);
        }
    }
    let mem = Mem(text.to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    let mut rec = Rec::default();
    let mut sc = Scanner::new(&src, Lang::Md, 0, St::default());
    let mut cx = Cx::new(Stack::default(), &mut rec);
    cx.cap = false;
    sc.run_to(mem.0.len() as u64, &mut cx, &|| false)
        .expect("run");
    sc.flush(&mut cx);
    drop(cx);
    rec.0
}

/// Строки HTML-блоков трекера = `CommonMark` (эталон — pulldown-cmark); строки из одних
/// маркеров контейнера у трекера пустые.
fn zones_as_commonmark(text: &str) -> Result<(), String> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line = |o: usize| starts.partition_point(|&s| s <= o) - 1;
    let mut cm = vec![false; starts.len()];
    for (ev, r) in pulldown_cmark::Parser::new(text).into_offset_iter() {
        if matches!(
            ev,
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock)
        ) && r.end > r.start
        {
            cm[line(r.start)..=line(r.end - 1)].fill(true);
        }
    }
    let mut ours = vec![false; starts.len()];
    for z in tracker_zones(text.as_bytes()) {
        ours[line(z as usize)] = true;
    }
    for l in 0..starts.len() {
        let end = starts.get(l + 1).copied().unwrap_or(text.len());
        let body = &text[starts[l]..end];
        let empty = body
            .chars()
            .all(|c| matches!(c, ' ' | '\t' | '\r' | '\n' | '>'));
        if ours[l] != cm[l] && !empty {
            return Err(format!(
                "строка {l} {body:?}: трекер {}, CommonMark {}\n--- text ---\n{text:?}",
                ours[l], cm[l]
            ));
        }
    }
    Ok(())
}

/// Роли всего текста.
fn roles(text: &str) -> Vec<u8> {
    let b = text.as_bytes();
    win_roles(Lang::Md, b, &marks_of(Lang::Md, b), 0, b.len() as u64)
}

/// Байты строки `line` текста имеют роль `tag` (не считая пробелов и маркеров цитаты).
fn assert_line(text: &str, line: usize, tag: Tag) {
    let r = roles(text);
    let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    let body = text[start..].lines().next().unwrap_or("");
    for (i, c) in body.bytes().enumerate() {
        if !matches!(c, b' ' | b'\t' | b'>') {
            assert_eq!(r[start + i], tag as u8, "{text:?}, строка {line}, байт {i}");
        }
    }
}

/// Грамматика и `CommonMark` о конце HTML-блока спорят — красим по спецификации, и строка
/// за блоком не зависит от того, где начато окно.
#[test]
fn colors_by_spec_where_grammar_differs() {
    // Блок кончился на своей же строке (грамматика тянет его дальше).
    for doc in [
        "<?>\nfoo\n",
        "<!X>\nfoo\n",
        "<!-->\nfoo\n",
        "<![CDATA[]]>\nfoo\n",
    ] {
        assert_line(doc, 0, Tag::Comment);
        assert_line(doc, 1, Tag::Plain);
    }
    // Блок идёт дальше (грамматика кончает его на пустой или не видит вовсе).
    for doc in [
        "<textarea>\n\n# x\n</textarea>\nfoo\n",
        "<!x\n\n# x\n>\nfoo\n",
        "<!X\n\n# x\na > b\nfoo\n",
    ] {
        assert_line(doc, 2, Tag::Comment);
        assert_line(doc, 4, Tag::Plain);
    }
    // В контейнерах: конец блока — по той же спецификации, контейнер цел.
    assert_line("- <?>\n  foo\n", 1, Tag::Plain);
    assert_line("> <?>\n> foo\n", 1, Tag::Plain);
    assert_line("- a\n  - <?>\n    foo\n", 2, Tag::Plain);
    assert_line(
        "> <textarea>\n>\n> # x\n> </textarea>\n> foo\n",
        2,
        Tag::Comment,
    );
    assert_line(
        "> <textarea>\n>\n> # x\n> </textarea>\n> foo\n",
        4,
        Tag::Plain,
    );
    // Контейнер закрыт — закрыт и блок.
    assert_line("> <textarea>\nfoo\n", 1, Tag::Plain);
    // Маркер пункта на строке блока — по-прежнему маркер.
    let r = roles("- <div>\n");
    assert_eq!(r[0], Tag::Keyword as u8);
    assert_eq!(r[2], Tag::Comment as u8);
    // За блоком — настоящие блоки.
    assert_line("<?>\n# h\n", 1, Tag::Keyword);
    assert_line("<?>\n---\n", 1, Tag::Comment);
    assert_line("<?>\n===\n", 1, Tag::Plain);
    assert_line("<?>\n    code\n", 1, Tag::String);
    assert_line("<?>\n```\nx\n```\n", 2, Tag::String);
}

/// UTF-16: позиции зон — в байтах источника, маска — в тексте разбора (другие смещения).
#[test]
fn zones_in_utf16() {
    let doc = "\u{44f}\n\n> <?>\n> \u{44f}\n\n<!x\n\n# \u{44f}\n>\nfoo\n\n# h\n";
    let raw: Vec<u8> = doc.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mem = Mem(raw.clone());
    let src = Src {
        text: &mem,
        feed: Feed::Utf16Le,
        start: 0,
    };
    let n = raw.len() as u64;
    let mut m = Marks::new(0, n);
    advance(&mut m, &src, Lang::Md, u64::MAX, &|| false).expect("pass");
    let enc = crate::enc::Enc::new(encoding_rs::UTF_16LE);
    let roles = |a: u64, b: u64| {
        let w = window(&src, enc, Lang::Md, &m, a, b, &|| false)
            .expect("window")
            .expect("marks");
        let mut out = vec![0u8; raw.len()];
        for s in &w.spans {
            out[s.start as usize..s.end as usize].fill(s.tag as u8);
        }
        out
    };
    let full = roles(0, n);
    let at = |t: &str| doc[..doc.find(t).expect("text")].encode_utf16().count() * 2;
    assert_eq!(full[at("<?>")], Tag::Comment as u8);
    assert_eq!(full[at("<!x")], Tag::Comment as u8);
    assert_eq!(full[at("# \u{44f}")], Tag::Comment as u8);
    assert_eq!(full[at("foo")], Tag::Plain as u8);
    assert_eq!(full[at("# h")], Tag::Keyword as u8);
    for a in (0..n).step_by(2) {
        let w = roles(a, n);
        assert_eq!(w[a as usize..], full[a as usize..], "окно с {a}");
    }
}

/// Окна с каждого байта на текстах, где грамматика расходится со спецификацией.
#[test]
fn every_start_on_divergent_samples() {
    for d in [
        "a\n\n<?>\nfoo\n\nbar `x`\n\n# h\n",
        "<textarea>\n\n# x\n\n- y\n</textarea>\n\n# h\n",
        "<!x\n\n# x\n\n```\n>\n\n# y\n```\n",
        "<!X y\n\n# x\n\na > b\n\n# y\n",
        "- <?>\n  foo\n\n  # x\n\n# y\n",
        "> <!x\n\n# x\n> y\n\n# z\n",
        "<pre>\n\n# x\n</textarea>\n\n# y\n",
        "<?>\r\nfoo\r\n\r\n# x\r\n",
    ] {
        let n = d.len() as u64;
        let cuts: Vec<(u64, u64)> = (0..=n).flat_map(|a| [(a, n), (a, a + 1)]).collect();
        assert_eq!(same_in_windows(d.as_bytes(), &cuts), Ok(()), "{d:?}");
    }
}

/// Содержимое строк вокруг начала HTML-блока: концы всех типов и то, что выглядит блоком.
const PIECES: &[&str] = &[
    "-->",
    "?>",
    ">",
    "a > b",
    "]]>",
    "</pre>",
    "</script>",
    "</style>",
    "</textarea>",
    "foo",
    "`x",
    "# h",
    "- b",
    "1. c",
    "2. d",
    "---",
    "===",
    "+++",
    "```",
    "~~~",
    "| a |",
    "|---|",
    "[r]: /u",
    "",
    "",
];

/// Начало строки: контейнеры и отступы.
const PREFIXES: &[&str] = &[
    "", "", "", "", " ", "  ", "   ", "    ", "\t", "- ", "* ", "1. ", "> ", ">", "> > ", "- > ",
    "> - ", "- - ", "  - ", "   > ",
];

/// Текст из строк, целиком нацеленный на расхождения: начала HTML-блоков и их концы в
/// контейнерах и без, вперемешку со всем, что меняет разбор ниже.
fn md_split_doc() -> BoxedStrategy<String> {
    let line = (
        0..PREFIXES.len(),
        prop_oneof![
            2 => (0..STARTS.len()).prop_map(|i| STARTS[i]),
            3 => (0..PIECES.len()).prop_map(|i| PIECES[i]),
        ],
        prop_oneof![9 => Just("\n"), 1 => Just("\r\n")],
    )
        .prop_map(|(p, s, e)| format!("{}{s}{e}", PREFIXES[p]));
    proptest::collection::vec(line, 1..24)
        .prop_map(|v| v.concat())
        .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(300), max_global_rejects: u32::MAX, ..ProptestConfig::default() })]

    /// Инвариант: любое окно = проход с начала, на любом тексте (сравнения с tree-sitter
    /// здесь нет, `md_html_agrees` ничего не отсеивает).
    #[test]
    fn window_same_on_any_text(doc in md_split_doc(),
                               cuts in proptest::collection::vec((0u64..600, 0u64..600), 1..6)) {
        let t = doc.as_bytes();
        let r = same_in_windows(t, &cuts).and_then(|()| same_in_windows(t, &line_cuts(t)));
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    /// То же на произвольных байтах (неверный UTF-8, переводы строк где попало).
    #[test]
    fn window_same_on_any_bytes(t in proptest::collection::vec(
                                    prop_oneof![
                                        4 => (0..STARTS.len()).prop_map(|i| STARTS[i].as_bytes().to_vec()),
                                        4 => (0..PIECES.len()).prop_map(|i| PIECES[i].as_bytes().to_vec()),
                                        4 => (0..PREFIXES.len()).prop_map(|i| PREFIXES[i].as_bytes().to_vec()),
                                        6 => Just(b"\n".to_vec()),
                                        1 => proptest::collection::vec(any::<u8>(), 1..4),
                                    ],
                                    1..60,
                                ).prop_map(|v| v.concat()),
                                cuts in proptest::collection::vec((0u64..600, 0u64..600), 1..6)) {
        let r = same_in_windows(&t, &cuts).and_then(|()| same_in_windows(&t, &line_cuts(&t)));
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    /// Зоны HTML-блоков трекера — по `CommonMark`, и в контейнерах.
    #[test]
    fn zones_are_commonmark(doc in md_split_doc()) {
        // Чужой тег типа 1 и конец в другом регистре блок закрывают только по спецификации
        // (`html_tests`), эталон — нет.
        prop_assume!(!doc.contains("</PRE>"));
        let low = doc.to_ascii_lowercase();
        let tags = ["script", "pre", "style", "textarea"];
        prop_assume!(tags.iter().filter(|t| low.contains(**t)).count() <= 1);
        // Строка с отступом кода — ленивое продолжение абзаца цитаты, и тип 7 за ней абзац
        // не прерывает (спецификация, cmark); эталон после неё начинает блок.
        prop_assume!(!doc.contains("	>") && !doc.contains("    >"));
        // Трекер не отличает определение ссылки от абзаца: `===` под ним для него Setext.
        prop_assume!(!(doc.contains("[r]: /u") && doc.contains("===")));
        // Метаданных в первой строке эталон не знает.
        let head = doc.trim_start_matches(' ');
        prop_assume!(!head.starts_with("---") && !head.starts_with("+++"));
        let r = zones_as_commonmark(&doc);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}
