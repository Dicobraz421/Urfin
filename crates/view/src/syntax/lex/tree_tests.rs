//! YAML и Markdown: окно (tree-sitter по отрезкам от безопасных строк) = полный разбор
//! (ADR-0024 §6).

#![allow(
    clippy::many_single_char_names,
    clippy::format_push_string,
    reason = "тесты: текст, точки, окно"
)]

use proptest::prelude::*;

use super::Lang;
use super::testkit::{ext_files, first_diff, marks_of, repo_files, ts_roles, win_roles};

/// Окна `cuts` = полный разбор. `Ok(false)` — в полном разборе ошибка (не сравнивается).
fn check_tree(lang: Lang, text: &str, cuts: &[(u64, u64)]) -> Result<bool, String> {
    let Some(full) = ts_roles(lang, text) else {
        return Ok(false);
    };
    let b = text.as_bytes();
    let m = marks_of(lang, b);
    for &(a, z) in cuts {
        let n = b.len() as u64;
        let (a, z) = (a.min(n), z.min(n));
        let (a, z) = (a.min(z), z.max(a));
        let w = win_roles(lang, b, &m, a, z);
        if let Some(d) = first_diff(b, &full, &w, a as usize, z as usize) {
            return Err(format!(
                "{} окно {a}..{z}: {d}\n--- text ---\n{text}",
                lang.name()
            ));
        }
    }
    Ok(true)
}

/// Окна по строкам: с начала каждой k-й строки до конца каждой j-й.
fn line_cuts(text: &str, every: usize) -> Vec<(u64, u64)> {
    let starts: Vec<u64> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i as u64 + 1))
        .collect();
    let mut out = Vec::new();
    for (i, &a) in starts.iter().enumerate().step_by(every.max(1)) {
        let z = starts.get(i + 3).copied().unwrap_or(text.len() as u64);
        out.push((a, z));
    }
    out
}

#[test]
fn yaml_samples() {
    let docs = [
        "key: value\nnum: 12\nb: true\nq: \"str\"\nlist:\n  - a\n  - 1.5\n? cplx\n: v\nflow: {a: 1, b: [x, y]}\n",
        "a: 1\nc: |\n  text\n  more: x\n\nd: >-\n  folded\ne: &anc !tag foo\nf: *anc\n",
        "items:\n- name: x\n  val: 1\n- name: \"y\"\n  val: [1,\n    2]\nnext: 'q\n  r'\n",
        "--- # doc\na: 1\n...\n---\n- x\n- y: z\n",
        "s: \"multi\n  line\"\nt: [a, \"b, ]\",\n  c]\nu: ok # c\n",
    ];
    for d in docs {
        assert_eq!(check_tree(Lang::Yaml, d, &line_cuts(d, 1)), Ok(true), "{d}");
    }
}

#[test]
fn md_samples() {
    let docs = [
        "# H1\n\nPara `code` [link](http://x) <b>x</b> &amp; \\*\n\n```rust\nfn x\n```\n\n- item\n- [x] task\n\n> quote\n\nSetext\n===\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        "- a\n  ```\n  code\n- b\n\n```\nreal fence\n# not heading\n\nmore\n```\n\n# after\n",
        "Para `code\nspans` lines\n\n<div>\n# not heading\n</div>\n\n## two\ntext\n",
        "1. one\n   ```sh\n   cargo\n   ```\n2. two\n\nlazy\ncontinuation\n- x\n# h\n",
        "<!--\ncomment\n# x\n-->\n\n[ref]: http://x\n\n*em* **strong** [ref]\n",
    ];
    for d in docs {
        assert_eq!(check_tree(Lang::Md, d, &line_cuts(d, 1)), Ok(true), "{d}");
    }
}

fn yaml_scalar() -> BoxedStrategy<String> {
    prop_oneof![
        "[a-z][a-z0-9 _.-]{0,8}",
        "-?[0-9]{1,4}(\\.[0-9]{1,2})?",
        Just("true".to_owned()),
        Just("null".to_owned()),
        Just("~".to_owned()),
        "\"[a-z :#,\\[\\]{}]{0,8}\"",
        "'[a-z :#,\"]{0,8}'",
        "&[a-z]{1,3} [a-z]{1,4}",
        "\\*[a-z]{1,3}",
        "!t [a-z]{1,4}",
    ]
    .boxed()
}

fn yaml_node(ind: usize, depth: u32) -> BoxedStrategy<String> {
    let pad = " ".repeat(ind);
    let leaf = yaml_scalar().prop_map(|s| format!(" {s}\n"));
    if depth == 0 {
        return leaf.boxed();
    }
    let p1 = pad.clone();
    let p2 = pad.clone();
    let p3 = pad.clone();
    prop_oneof![
        3 => leaf,
        1 => proptest::collection::vec(("[a-z][a-z0-9_]{0,5}", yaml_node(ind + 2, depth - 1)), 1..4)
            .prop_map(move |v| {
                let mut s = String::from("\n");
                for (k, n) in v {
                    s.push_str(&format!("{p1}  {k}:{n}"));
                }
                s
            }),
        1 => proptest::collection::vec(yaml_node(ind + 2, depth - 1), 1..4).prop_map(move |v| {
            let mut s = String::from("\n");
            for n in v {
                s.push_str(&format!("{p2}  -{n}"));
            }
            s
        }),
        1 => proptest::collection::vec("[a-z ]{0,10}", 1..4).prop_map(move |v| {
            let mut s = String::from(" |\n");
            for l in v {
                s.push_str(&format!("{p3}    {l}\n"));
            }
            s
        }),
        1 => proptest::collection::vec(yaml_scalar(), 0..4).prop_map(|v| format!(" [{}]\n", v.join(",\n    "))),
        1 => Just(" \"two\n    lines\"\n".to_owned()),
        1 => Just(" # comment only\n".to_owned()),
    ]
    .boxed()
}

fn yaml_doc() -> BoxedStrategy<String> {
    proptest::collection::vec(
        prop_oneof![
            4 => ("[a-z][a-z0-9_]{0,5}", yaml_node(0, 3)).prop_map(|(k, n)| format!("{k}:{n}")),
            1 => Just("# top comment\n".to_owned()),
            1 => Just("\n".to_owned()),
        ],
        1..8,
    )
    .prop_map(|v| v.concat())
    .boxed()
}

fn md_block() -> BoxedStrategy<String> {
    prop_oneof![
        2 => "[A-Za-zА-я][a-zA-Z `*_\\[\\]()<>&#;!-]{0,30}\n([a-zA-Z `*_\\[\\]()]{1,20}\n){0,2}",
        1 => "#{1,6} [a-zA-Z `*]{1,15}\n",
        1 => "[a-zA-Z ]{1,10}\n(===|---)\n",
        1 => "```[a-z]{0,4}\n([a-z #<>`]{0,10}\n){0,3}```\n",
        1 => "(- [a-z `\\[\\]]{1,10}\n(  [a-z#`]{1,8}\n)?){1,3}",
        1 => "1\\. [a-z]{1,8}\n   ```\n   [a-z#]{1,8}\n   ```\n2\\. [a-z]{1,5}\n",
        1 => "(> [a-z `*]{1,10}\n){1,3}",
        1 => "<div>\n[a-z #]{0,10}\n</div>\n",
        1 => "<!--\n[a-z #\\n]{0,15}\n-->\n",
        1 => "\\| [a-z]{1,4} \\| [a-z]{1,4} \\|\n\\|---\\|---\\|\n\\| [a-z`]{1,4} \\| [0-9]{1,3} \\|\n",
        1 => "    indented [a-z]{1,8}\n",
        1 => "\\[[a-z]{1,4}\\]: http://[a-z]{1,6}\n",
        1 => "---\n",
    ]
    .boxed()
}

fn md_doc() -> BoxedStrategy<String> {
    proptest::collection::vec(
        (md_block(), prop_oneof![3 => Just("\n"), 1 => Just("")]),
        1..10,
    )
    .prop_map(|v| v.into_iter().map(|(b, s)| b + s).collect())
    .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn yaml_generated(doc in yaml_doc(), every in 1usize..4) {
        let r = check_tree(Lang::Yaml, &doc, &line_cuts(&doc, every));
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn md_generated(doc in md_doc(), every in 1usize..4) {
        let r = check_tree(Lang::Md, &doc, &line_cuts(&doc, every));
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn md_random_cuts(doc in md_doc(), cuts in proptest::collection::vec((0u64..3000, 0u64..3000), 1..5)) {
        let r = check_tree(Lang::Md, &doc, &cuts);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}

#[test]
fn repo_corpus() {
    let mut n = 0;
    for (lang, ext) in [(Lang::Md, "md"), (Lang::Yaml, "yml")] {
        for p in repo_files(&[ext]) {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            if text.len() > 512 << 10 {
                continue;
            }
            match check_tree(lang, &text, &line_cuts(&text, 7)) {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) => panic!("{}: {}", p.display(), e.lines().next().unwrap_or("")),
            }
        }
    }
    assert!(n > 50, "{n}");
}

#[test]
#[ignore = "внешний корпус"]
fn ext_corpus() {
    let (mut n, mut bad) = (0, 0);
    for (lang, exts) in [(Lang::Md, &["md"][..]), (Lang::Yaml, &["yml", "yaml"][..])] {
        for p in ext_files(exts) {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            if text.len() > 1 << 20 {
                continue;
            }
            match check_tree(lang, &text, &line_cuts(&text, 5)) {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) => {
                    bad += 1;
                    if bad < 15 {
                        eprintln!("{}: {}", p.display(), e.lines().next().unwrap_or(""));
                    }
                }
            }
        }
    }
    eprintln!("tree: {n} ok, {bad} bad");
    assert_eq!(bad, 0);
}
