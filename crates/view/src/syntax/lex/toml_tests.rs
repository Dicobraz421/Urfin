//! TOML: лексер = tree-sitter + `highlights.scm` (ADR-0024 §6).

use proptest::prelude::*;

use super::super::Lang;
use super::super::testkit::{check_lexer, check_windows, ext_files, repo_files};

fn ok(text: &str) {
    match check_lexer(Lang::Toml, text) {
        Ok(true) => {}
        Ok(false) => panic!("tree-sitter: ошибка в образце\n{text}"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn samples() {
    ok(
        "[workspace]\nresolver = \"3\"\n\n[profile.dev.package.\"*\"]\ndebug = false\nlto = 1 # UI\n[[bin]]\n",
    );
    ok("a = 1\nb.c = 'lit'\n\"q k\" = [1, 2,\n  3, # c\n]\n[t]\nx = { y = 1, z = \"s\" }\n");
    ok("s = \"\"\"\nline \"one\"\nline \"\"two\"\"\"\"\"\nl = '''\n'x'\n'''\ne = \"\"\n");
    ok(
        "d1 = 1979-05-27T07:32:00Z\nd2 = 1979-05-27 07:32:00-07:00\nd3 = 1979-05-27\nt = 07:32:00.999\n",
    );
    ok("i = +1_000\nh = 0xDEAD_beef\no = 0o755\nb = 0b1101\nf = -3.14e-2\nn = nan\np = +inf\n");
    ok("[[ a . \"b\" ]] # c\n[ x ]\n");
    ok("k = \"esc \\\" \\\\ \\u00E9 \\U0001F600\"\nm = \"\"\"a \\\n  b\"\"\"\n");
}

fn key() -> BoxedStrategy<String> {
    prop_oneof![
        "[a-zA-Z_][a-zA-Z0-9_-]{0,6}",
        "\"[a-z .=#\\[\\]]{0,6}\"",
        "'[a-z .=#]{0,6}'",
        ("[a-z]{1,4}", "[a-z]{1,4}").prop_map(|(a, b)| format!("{a}.{b}")),
        ("[a-z]{1,4}", "[a-z]{1,4}").prop_map(|(a, b)| format!("{a} . \"{b}\"")),
    ]
    .boxed()
}

fn scalar() -> BoxedStrategy<String> {
    prop_oneof![
        "\"([a-zA-Zа-я0-9 #=\\[\\]{}',]|\\\\[\"\\\\nt]|\\\\u00[0-9A-F]{2}){0,10}\"",
        "'[a-zA-Z0-9 #=\\[\\]\"\\\\]{0,10}'",
        "\"\"\"([a-z \\n#]|\"[a-z]|\"\"[a-z]){0,10}\"\"\"",
        "'''([a-z \\n#]|'[a-z]){0,10}'''",
        "[+-]?(0|[1-9](_?[0-9]){0,4})",
        "0x[0-9a-fA-F](_?[0-9a-fA-F]){0,3}",
        "0o[0-7]{1,4}",
        "0b[01]{1,6}",
        "[+-]?(0|[1-9][0-9]{0,3})(\\.[0-9]{1,3})?([eE][+-]?[0-9]{1,2})?",
        "[+-]?(inf|nan)",
        Just("true".to_owned()),
        Just("false".to_owned()),
        "19[0-9]{2}-0[1-9]-[12][0-9][Tt ][01][0-9]:[0-5][0-9]:[0-5][0-9](\\.[0-9]{1,3})?(Z|[+-]0[0-9]:[0-5][0-9])?",
        "20[0-9]{2}-1[0-2]-0[1-9]",
        "[01][0-9]:[0-5][0-9]:[0-5][0-9]",
    ]
    .boxed()
}

fn value(depth: u32) -> BoxedStrategy<String> {
    if depth == 0 {
        return scalar();
    }
    let sep = prop_oneof![
        Just(", ".to_owned()),
        Just(",\n  ".to_owned()),
        Just(", # c\n".to_owned()),
        Just(",".to_owned()),
    ];
    prop_oneof![
        3 => scalar(),
        1 => (proptest::collection::vec((value(depth - 1), sep), 0..4), proptest::bool::ANY)
            .prop_map(|(v, nl)| {
                let mut s = String::from("[");
                if nl {
                    s.push('\n');
                }
                for (i, (x, sp)) in v.iter().enumerate() {
                    s.push_str(x);
                    if i + 1 < v.len() {
                        s.push_str(sp);
                    }
                }
                s.push(']');
                s
            }),
        1 => proptest::collection::vec((key(), value(depth - 1)), 0..3).prop_map(|v| {
            let items: Vec<String> = v.into_iter().map(|(k, x)| format!("{k} = {x}")).collect();
            if items.is_empty() {
                "{}".to_owned()
            } else {
                format!("{{ {} }}", items.join(", "))
            }
        }),
    ]
    .boxed()
}

fn line() -> BoxedStrategy<String> {
    prop_oneof![
        4 => (key(), value(2), prop_oneof![Just(""), Just(" # c"), Just("  #x=1")])
            .prop_map(|(k, v, c)| format!("{k} = {v}{c}\n")),
        1 => Just("\n".to_owned()),
        1 => Just("# comment [x] = \"y\"\n".to_owned()),
    ]
    .boxed()
}

fn document() -> BoxedStrategy<String> {
    let table = (
        prop_oneof![Just(false), Just(true)],
        key(),
        proptest::collection::vec(line(), 0..5),
    )
        .prop_map(|(arr, k, lines)| {
            let h = if arr {
                format!("[[{k}]]\n")
            } else {
                format!("[{k}]\n")
            };
            h + &lines.concat()
        });
    (
        proptest::collection::vec(line(), 0..4),
        proptest::collection::vec(table, 0..4),
    )
        .prop_map(|(root, tables)| root.concat() + &tables.concat())
        .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn generated_documents(doc in document()) {
        let r = check_lexer(Lang::Toml, &doc);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn token_soup(toks in proptest::collection::vec(prop_oneof![
        Just("["), Just("]"), Just("[["), Just("]]"), Just("{"), Just("}"), Just(","),
        Just("="), Just(" "), Just("\n"), Just("k"), Just("\"s\""), Just("'l'"), Just("\"\"\""),
        Just("'''"), Just("1"), Just("1979-05-27"), Just("true"), Just("# c"), Just("."),
    ], 0..40)) {
        let doc: String = toks.concat();
        let r = check_lexer(Lang::Toml, &doc);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn windows_match_full_pass(doc in document(), cuts in proptest::collection::vec((0u64..3000, 0u64..3000), 1..6)) {
        let r = check_windows(Lang::Toml, doc.as_bytes(), &cuts);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}

/// Сколько сгенерированных документов разобрал tree-sitter (генератор не врёт).
#[test]
fn generator_is_valid_mostly() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut good = 0;
    for _ in 0..200 {
        let doc = document().new_tree(&mut runner).expect("doc").current();
        if check_lexer(Lang::Toml, &doc).expect("lexer") {
            good += 1;
        }
    }
    assert!(good > 120, "только {good} из 200 без ошибок tree-sitter");
}

#[test]
fn repo_corpus() {
    let mut n = 0;
    for p in repo_files(&["toml"]) {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        match check_lexer(Lang::Toml, &text) {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => panic!("{}: {e}", p.display()),
        }
    }
    assert!(n > 10, "{n}");
}

#[test]
#[ignore = "внешний корпус"]
fn ext_corpus() {
    let (mut n, mut bad) = (0, 0);
    for p in ext_files(&["toml"]) {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        match check_lexer(Lang::Toml, &text) {
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
    eprintln!("toml: {n} ok, {bad} bad");
    assert_eq!(bad, 0);
}
