//! JSON: лексер = tree-sitter + `highlights.scm` (ADR-0024 §6).

use proptest::prelude::*;

use super::super::Lang;
use super::super::testkit::{check_lexer, check_windows, ext_files, repo_files};

fn ok(text: &str) {
    match check_lexer(Lang::Json, text) {
        Ok(true) => {}
        Ok(false) => panic!("tree-sitter: ошибка в образце\n{text}"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn samples() {
    ok(r#"{"a": [1, "x", {"b": null}], // c
 "c": /* x */ true}"#);
    ok(r#"[{"k": "v", "n": -1.5e10, "e": 0}, [], {}, "s\"q", "\\", false]"#);
    ok("{\n  \"a\": {\n    \"b\": [\n      1,\n      2\n    ]\n  }\n}\n");
    ok("{\"a\": /* multi\n line */ 1, \"b\"\n:\n2}");
    ok("1 2 \"x\" [true] {\"k\": {}}");
    ok("{\"esc\": \"a\\u0041\\n\\t\", \"k\\\"q\": 1}");
}

/// Корректный JSON со случайной структурой, пробелами и комментариями.
fn value(depth: u32) -> BoxedStrategy<String> {
    let leaf = prop_oneof![
        Just("null".to_owned()),
        Just("true".to_owned()),
        Just("false".to_owned()),
        "-?(0|[1-9][0-9]{0,4})(\\.[0-9]{1,3})?([eE]-?[0-9]{1,2})?",
        "\"([a-zA-Zа-я0-9 :,{}\\[\\]/*#]|\\\\[\"\\\\/bfnrt]|\\\\u00[0-9a-f]{2}){0,12}\"",
    ];
    if depth == 0 {
        return leaf.boxed();
    }
    let ws = prop_oneof![
        Just(String::new()),
        Just(" ".to_owned()),
        Just("\n  ".to_owned()),
        Just(" /* c */ ".to_owned()),
        Just(" // c\n".to_owned()),
        Just("\r\n\t".to_owned()),
    ];
    let key = "\"[a-z0-9 \\]\\[{}:,]{0,8}\"";
    prop_oneof![
        2 => leaf,
        1 => (proptest::collection::vec((value(depth - 1), ws.clone()), 0..5)).prop_map(|v| {
            let items: Vec<String> = v.into_iter().map(|(x, w)| format!("{w}{x}{w}")).collect();
            format!("[{}]", items.join(","))
        }),
        1 => (proptest::collection::vec((key, value(depth - 1), ws), 0..5)).prop_map(|v| {
            let items: Vec<String> =
                v.into_iter().map(|(k, x, w)| format!("{w}{k}{w}:{w}{x}{w}")).collect();
            format!("{{{}}}", items.join(","))
        }),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: crate::syntax::lex::testkit::cases(300), ..ProptestConfig::default() })]

    #[test]
    fn generated_documents(doc in value(4)) {
        let r = check_lexer(Lang::Json, &doc);
        prop_assert!(r == Ok(true), "{r:?}
{doc}");
    }

    /// Случайные цепочки токенов: сравниваются только разобранные без ошибок.
    #[test]
    fn token_soup(toks in proptest::collection::vec(prop_oneof![
        Just("{"), Just("}"), Just("["), Just("]"), Just(","), Just(":"), Just(" "),
        Just("\n"), Just("\"k\""), Just("\"v w\""), Just("1"), Just("-2.5"), Just("true"),
        Just("null"), Just("/* c */"), Just("// c\n"),
    ], 0..40)) {
        let doc: String = toks.concat();
        let r = check_lexer(Lang::Json, &doc);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn windows_match_full_pass(doc in value(5), cuts in proptest::collection::vec((0u64..4000, 0u64..4000), 1..6)) {
        let r = check_windows(Lang::Json, doc.as_bytes(), &cuts);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}

#[test]
fn repo_corpus() {
    let mut n = 0;
    for p in repo_files(&["json"]) {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if let Err(e) = check_lexer(Lang::Json, &text) {
            panic!("{}: {e}", p.display());
        }
        n += 1;
    }
    assert!(n > 0);
}

/// Внешний корпус (`HL_CORPUS=…`): `cargo nextest run -p urfin-view --run-ignored only`.
#[test]
#[ignore = "внешний корпус"]
fn ext_corpus() {
    let (mut n, mut bad) = (0, 0);
    for p in ext_files(&["json"]) {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if text.len() > 4 << 20 {
            continue;
        }
        match check_lexer(Lang::Json, &text) {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => {
                bad += 1;
                if bad < 10 {
                    eprintln!("{}: {}", p.display(), e.lines().next().unwrap_or(""));
                }
            }
        }
    }
    eprintln!("json: {n} ok, {bad} bad");
    assert_eq!(bad, 0);
}
