//! Точки: проход, правки (сходимость = проход с нуля), стек глубже 64 уровней и глубже
//! памяти прохода (ADR-0024 §2, §4, §6).

#![allow(
    clippy::many_single_char_names,
    clippy::format_push_string,
    reason = "тесты: текст, точки, окно"
)]

use proptest::prelude::*;

use super::*;
use crate::syntax::Tag;
use crate::syntax::lex::testkit::{Mem, first_diff, win_roles};
use crate::syntax::lex::{Cx, Feed, Lang, NoOut, Scanner, Src, St, Stack};

fn src(mem: &Mem) -> Src<'_> {
    Src {
        text: mem,
        feed: Feed::Bytes,
        start: 0,
    }
}

fn pass(lang: Lang, text: &[u8], step: u64) -> Marks {
    let mem = Mem(text.to_vec());
    let s = src(&mem);
    let mut m = Marks::new(0, mem.0.len() as u64);
    m.step = step;
    assert_eq!(
        advance(&mut m, &s, lang, u64::MAX, &|| false).unwrap(),
        End::Done
    );
    m
}

/// Роли одним проходом с начала, без точек (весь стек в памяти).
fn reference(lang: Lang, text: &[u8]) -> Vec<u8> {
    let m = pass(lang, text, u64::MAX);
    assert_eq!(m.v.len(), 1);
    win_roles(lang, text, &m, 0, text.len() as u64)
}

/// Состояние прохода с начала на позиции `pos`.
fn state_at(lang: Lang, text: &[u8], pos: u64) -> Option<(St, u64)> {
    let mem = Mem(text.to_vec());
    let s = src(&mem);
    let mut out = NoOut;
    let mut sc = Scanner::new(&s, lang, 0, St::default());
    let mut cx = Cx::new(Stack::default(), &mut out);
    cx.cap = false;
    sc.run_to(pos, &mut cx, &|| false).unwrap();
    (sc.at == pos && sc.lex.can_mark()).then(|| (sc.state(&cx), cx.depth()))
}

fn json_tree(d: u32) -> String {
    if d == 0 {
        return "\"leaf\"".to_owned();
    }
    format!(
        "[{{\"k\": {}, \"s\": \"x\"}}, \"y\", {{\"z\": [1, {{\"w\": \"q\"}}]}}]",
        json_tree(d - 1)
    )
}

/// Вложенность 300 (глубже 64 уровней точки): окна с любых мест = проход с начала.
#[test]
fn deep_nesting_windows() {
    let text = format!("{{\"a\": {}, \"b\": \"tail\"}}", json_tree(300));
    let t = text.as_bytes();
    let want = reference(Lang::Json, t);
    let m = pass(Lang::Json, t, 97);
    assert!(m.v.len() > 50);
    assert!(m.v.iter().any(|k| k.depth > 200));
    let n = t.len() as u64;
    for a in (0..n).step_by(311) {
        let b = (a + 1500).min(n);
        let got = win_roles(Lang::Json, t, &m, a, b);
        if let Some(d) = first_diff(t, &want, &got, a as usize, b as usize) {
            panic!("окно {a}..{b}: {d}");
        }
    }
}

/// Вложенность глубже памяти прохода: нижние уровни забываются и восстанавливаются по
/// точкам; роли после закрытия всех уровней верны.
#[test]
fn deeper_than_known_max() {
    let n = 70_000;
    let text = format!(
        "{{\"a\":{}{{\"k\":\"v\",\"q\":[1]}}{},\"b\":\"s\",\"c\":{{\"d\":\"e\"}}}}",
        "[".repeat(n),
        "]".repeat(n)
    );
    let t = text.as_bytes();
    let want = reference(Lang::Json, t);
    let m = pass(Lang::Json, t, 4096);
    let got = win_roles(Lang::Json, t, &m, 0, t.len() as u64);
    assert_eq!(first_diff(t, &want, &got, 0, t.len()), None);
    // Ключ `"b"` — текст, `"s"` — строка.
    assert_eq!(got[text.rfind("\"s\"").unwrap()], Tag::String as u8);
    assert_eq!(got[text.rfind("\"b\"").unwrap()], 0);
    let a = t.len() as u64 - 40;
    let w = win_roles(Lang::Json, t, &m, a, t.len() as u64);
    assert_eq!(first_diff(t, &want, &w, a as usize, t.len()), None);
}

fn alphabet(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Json => &[
            "{", "}", "[", "]", ",", ":", "\"", "\\", " ", "\n", "a", "1", "true", "/*", "*/", "//",
        ],
        Lang::Toml => &[
            "[", "]", "{", "}", ",", "=", "\"", "'", "\"\"\"", "'''", "#", " ", "\n", "k", "1",
            "\\",
        ],
        Lang::Ini => &["[", "]", ";", "#", "=", " ", "\n", "k", "v"],
        Lang::Xml => &[
            "<",
            ">",
            "/",
            "!--",
            "-->",
            "<![CDATA[",
            "]]>",
            "&amp;",
            "\"",
            "'",
            "a",
            " ",
            "\n",
            "<?",
            "?>",
            "=",
            "<!DOCTYPE r [",
            "]>",
        ],
        Lang::Yaml => &[
            "- ", "k: ", "  ", "\n", "\"", "'", "[", "]", "{", "}", "|", ">", "#", "---", "x", ",",
        ],
        Lang::Md => &[
            "# ", "- ", "1. ", "```", "\n", "\n\n", "  ", "<div>", "<!--", "-->", "x", "`", "> ",
            "---", "+++",
        ],
    }
}

fn soup(lang: Lang) -> impl Strategy<Value = String> {
    let a = alphabet(lang);
    proptest::collection::vec(0..a.len(), 0..200)
        .prop_map(move |v| v.into_iter().map(|i| a[i]).collect())
}

/// Правки: точки после правок и прохода = проход с нуля (состояния на точках и роли).
fn edits_case(
    lang: Lang,
    base: String,
    edits: Vec<(usize, usize, usize)>,
) -> Result<(), TestCaseError> {
    let a = alphabet(lang);
    let mut text = base.into_bytes();
    let mut m = pass(lang, &text, 37);
    for (pos, del, ins) in edits {
        let at = pos % (text.len() + 1);
        let del = del.min(text.len() - at);
        let piece = a[ins % a.len()].as_bytes();
        text.splice(at..at + del, piece.iter().copied());
        m.edit(&TextEdit {
            at: at as u64,
            old: del as u64,
            new: piece.len() as u64,
        });
        let mem = Mem(text.clone());
        let s = src(&mem);
        advance(&mut m, &s, lang, u64::MAX, &|| false).unwrap();
        prop_assert!(m.complete());
        prop_assert_eq!(m.len, text.len() as u64);
        for k in &m.v {
            let st = state_at(lang, &text, k.at);
            prop_assert_eq!(
                st,
                Some((k.st, k.depth)),
                "точка {} в {:?}",
                k.at,
                String::from_utf8_lossy(&text)
            );
        }
        if !lang.by_tree() {
            let want = reference(lang, &text);
            let got = win_roles(lang, &text, &m, 0, text.len() as u64);
            let d = first_diff(&text, &want, &got, 0, text.len());
            prop_assert!(d.is_none(), "{:?}", d);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 120, ..ProptestConfig::default() })]

    #[test]
    fn edits_json(base in soup(Lang::Json), edits in proptest::collection::vec((0usize..2000, 0usize..6, 0usize..32), 1..8)) {
        edits_case(Lang::Json, base, edits)?;
    }

    #[test]
    fn edits_toml(base in soup(Lang::Toml), edits in proptest::collection::vec((0usize..2000, 0usize..6, 0usize..32), 1..8)) {
        edits_case(Lang::Toml, base, edits)?;
    }

    #[test]
    fn edits_xml(base in soup(Lang::Xml), edits in proptest::collection::vec((0usize..2000, 0usize..6, 0usize..32), 1..8)) {
        edits_case(Lang::Xml, base, edits)?;
    }

    #[test]
    fn edits_ini(base in soup(Lang::Ini), edits in proptest::collection::vec((0usize..2000, 0usize..6, 0usize..32), 1..8)) {
        edits_case(Lang::Ini, base, edits)?;
    }

    #[test]
    fn edits_yaml(base in soup(Lang::Yaml), edits in proptest::collection::vec((0usize..2000, 0usize..6, 0usize..32), 1..8)) {
        edits_case(Lang::Yaml, base, edits)?;
    }

    #[test]
    fn edits_md(base in soup(Lang::Md), edits in proptest::collection::vec((0usize..2000, 0usize..6, 0usize..32), 1..8)) {
        edits_case(Lang::Md, base, edits)?;
    }

    /// Окно с любой точки = проход с начала — и на неверных документах (свои правила).
    #[test]
    fn windows_any_text(lang in prop_oneof![Just(Lang::Json), Just(Lang::Toml), Just(Lang::Xml), Just(Lang::Ini)],
                        seed in 0usize..1000, cuts in proptest::collection::vec((0u64..3000, 0u64..3000), 1..5)) {
        let a = alphabet(lang);
        let text: String = (0..400).map(|i| a[(seed * 31 + i * 17 + i * i) % a.len()]).collect();
        let t = text.as_bytes();
        let want = reference(lang, t);
        let m = pass(lang, t, 53);
        for (x, y) in cuts {
            let n = t.len() as u64;
            let (x, y) = (x.min(n).min(y.min(n)), y.min(n).max(x.min(n)));
            let got = win_roles(lang, t, &m, x, y);
            let d = first_diff(t, &want, &got, x as usize, y as usize);
            prop_assert!(d.is_none(), "{:?}", d);
        }
    }
}

/// Сходимость: правка внутри строки — пересчёт одного куска, точки дальше — прежние.
#[test]
fn converges_after_local_edit() {
    let text = "{\"a\": \"xxxxxxxx\"}\n".repeat(2000);
    let mut t = text.into_bytes();
    let mut m = pass(Lang::Json, &t, 256);
    let before = m.v.len();
    t.insert(7, b'y');
    m.edit(&TextEdit {
        at: 7,
        old: 0,
        new: 1,
    });
    assert!(m.dirty.is_some());
    let mem = Mem(t.clone());
    let s = src(&mem);
    assert_eq!(
        advance(&mut m, &s, Lang::Json, u64::MAX, &|| false).unwrap(),
        End::Converged
    );
    assert!(m.complete());
    assert!(m.v.len() <= before + 1, "{} {before}", m.v.len());
    // Открытая кавычка: состояние дальше другое — до конца.
    t.insert(3, b'"');
    m.edit(&TextEdit {
        at: 3,
        old: 0,
        new: 1,
    });
    let mem = Mem(t.clone());
    let s = src(&mem);
    assert_eq!(
        advance(&mut m, &s, Lang::Json, u64::MAX, &|| false).unwrap(),
        End::Done
    );
}
