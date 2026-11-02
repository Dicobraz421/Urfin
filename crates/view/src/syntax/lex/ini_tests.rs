//! INI: лексер = tree-sitter + `highlights.scm` (ADR-0024 §6).

use proptest::prelude::*;

use super::super::Lang;
use super::super::testkit::{check_lexer, check_windows};

fn ok(text: &str) {
    match check_lexer(Lang::Ini, text) {
        Ok(true) => {}
        Ok(false) => panic!("tree-sitter: ошибка в образце\n{text}"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn samples() {
    ok("; top\n[sec]\nkey = value ; c\n# c2\n  [ sp ] \nk2=v2\n");
    ok("a=1\n\n[s1]\nx = [y] ; z\n[s2]\n");
}

fn document() -> BoxedStrategy<String> {
    let line = prop_oneof![
        3 => ("[a-zA-Z][a-zA-Z0-9_.]{0,6}", "[ ]{0,2}", "[a-zA-Zа-я0-9 ;#=\\[\\]]{0,10}")
            .prop_map(|(k, s, v)| format!("{k}{s}={s}{v}\n")),
        1 => "[;#][a-zA-Z0-9 =\\[\\]]{0,10}".prop_map(|c| format!("{c}\n")),
        1 => Just("\n".to_owned()),
    ];
    let section = (
        "[a-zA-Z0-9 ._-]{1,8}",
        proptest::collection::vec(line.clone(), 0..5),
    )
        .prop_map(|(n, lines)| format!("[{n}]\n{}", lines.concat()));
    (
        proptest::collection::vec(line, 0..4),
        proptest::collection::vec(section, 0..4),
    )
        .prop_map(|(a, b)| a.concat() + &b.concat())
        .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn generated_documents(doc in document()) {
        let r = check_lexer(Lang::Ini, &doc);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn windows_match_full_pass(doc in document(), cuts in proptest::collection::vec((0u64..800, 0u64..800), 1..6)) {
        let r = check_windows(Lang::Ini, doc.as_bytes(), &cuts);
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}
