//! XML: лексер = tree-sitter + `highlights.scm` (ADR-0024 §6).

use proptest::prelude::*;

use super::super::Lang;
use super::super::testkit::{check_lexer, check_windows, ext_files};

/// Образец: если tree-sitter разобрал без ошибок — роли равны.
fn ok(text: &str) -> bool {
    match check_lexer(Lang::Xml, text) {
        Ok(v) => v,
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn samples() {
    let mut n = 0;
    n += usize::from(ok(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE r [ <!ENTITY e \"v&amp;\"> <!-- c --> ]>\n<?xml-stylesheet href=\"a\"?>\n<r a=\"x&amp;y\" b='z'>t &lt; <![CDATA[ x ]]><!-- c --><e/><?pi x?></r>\n",
    ));
    n += usize::from(ok(
        "<root>\n  <a x=\"1\"\n     y='2'>text &#123; &#x1F;</a>\n  <b/>\n  <!--\n  multi\n  -->\n</root>\n",
    ));
    n += usize::from(ok(
        "<!DOCTYPE html SYSTEM \"http://x?a=1&b\">\n<html><имя атр=\"з\">т</имя></html>",
    ));
    n += usize::from(ok(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<!-- c -->\n<a:b xmlns:a=\"u\"><a:c/></a:b>\n<!-- after -->\n",
    ));
    assert!(n >= 2, "{n}");
}

fn name() -> BoxedStrategy<String> {
    "[a-zA-Z_][a-zA-Z0-9_.-]{0,5}".boxed()
}

fn attr() -> BoxedStrategy<String> {
    (
        name(),
        prop_oneof![
            "\"([a-zA-Z0-9 >'/=]|&amp;|&#[0-9]{2};|&lt;){0,8}\"",
            "'([a-zA-Z0-9 >\"/=]|&gt;){0,8}'",
        ],
    )
        .prop_map(|(n, v)| format!(" {n}={v}"))
        .boxed()
}

fn content(depth: u32) -> BoxedStrategy<String> {
    let leaf = prop_oneof![
        "[a-zA-Zа-я0-9 \\n>'\"=/]{1,10}",
        Just("&amp;".to_owned()),
        Just("&#x1F600;".to_owned()),
        "<!--[a-z \\n]{0,10}-->",
        "<!\\[CDATA\\[[a-z <>&\\n]{0,10}\\]\\]>",
        "<\\?[a-wyz][a-z]{0,4} [a-z ]{0,6}\\?>",
    ];
    if depth == 0 {
        return leaf.boxed();
    }
    prop_oneof![
        2 => leaf,
        1 => element(depth - 1),
    ]
    .boxed()
}

fn element(depth: u32) -> BoxedStrategy<String> {
    (
        name(),
        proptest::collection::vec(attr(), 0..3),
        proptest::option::of(proptest::collection::vec(content(depth), 0..4)),
        prop_oneof![Just(""), Just("\n"), Just(" ")],
    )
        .prop_map(|(n, attrs, body, ws)| match body {
            None => format!("<{n}{}{ws}/>", attrs.concat()),
            Some(b) => format!("<{n}{}{ws}>{}</{n}{ws}>", attrs.concat(), b.concat()),
        })
        .boxed()
}

fn document() -> BoxedStrategy<String> {
    (
        proptest::option::of(Just("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n".to_owned())),
        proptest::option::of(prop_oneof![
            Just("<!DOCTYPE r>\n".to_owned()),
            Just("<!DOCTYPE r SYSTEM \"r.dtd\">\n".to_owned()),
            Just("<!DOCTYPE r [\n  <!ELEMENT r (#PCDATA)>\n  <!ATTLIST r a CDATA \"x&amp;\">\n  <!ENTITY e \"v&#65;\">\n  <!-- c -->\n]>\n".to_owned()),
        ]),
        proptest::option::of(Just("<!-- pre -->\n".to_owned())),
        element(3),
    )
        .prop_map(|(d, t, c, e)| {
            format!("{}{}{}{e}\n", d.unwrap_or_default(), t.unwrap_or_default(), c.unwrap_or_default())
        })
        .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn generated_documents(doc in document()) {
        let r = check_lexer(Lang::Xml, &doc);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn windows_match_full_pass(doc in document(), cuts in proptest::collection::vec((0u64..2000, 0u64..2000), 1..6)) {
        let r = check_windows(Lang::Xml, doc.as_bytes(), &cuts);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}

#[test]
fn generator_is_valid_mostly() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut good = 0;
    for _ in 0..200 {
        let doc = document().new_tree(&mut runner).expect("doc").current();
        if check_lexer(Lang::Xml, &doc).expect("lexer") {
            good += 1;
        }
    }
    assert!(good > 120, "только {good} из 200 без ошибок tree-sitter");
}

#[test]
#[ignore = "внешний корпус"]
fn ext_corpus() {
    let (mut n, mut bad) = (0, 0);
    for p in ext_files(&["xml", "svg", "xsd", "plist", "xaml", "csproj", "props"]) {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if text.len() > 4 << 20 {
            continue;
        }
        match check_lexer(Lang::Xml, &text) {
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
    eprintln!("xml: {n} ok, {bad} bad");
    assert_eq!(bad, 0);
}
