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
use crate::syntax::lex::{Code, Cx, Feed, Lang, NoOut, Scanner, Src, St, Stack};

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

#[allow(clippy::too_many_lines, reason = "алфавиты языков")]
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
        Lang::Code(Code::Python) => &[
            "def f():\n",
            "class A:\n",
            "if a:\n",
            "else:\n",
            "    ",
            "        ",
            "\n",
            "\n\n",
            "x = 1",
            "(",
            ")",
            "[",
            "]",
            "{",
            "}",
            ",",
            ":",
            "\"",
            "'",
            "\"\"\"",
            "'''",
            "f\"",
            "f'",
            "r\"",
            "\\",
            "#",
            "@d\n",
            "lambda",
            " for ",
            "a",
            "1",
            "\t",
        ],
        Lang::Code(Code::Go) => &[
            "func f() {\n",
            "if a {\n",
            "}",
            "{",
            "(",
            ")",
            "[",
            "]",
            "\n",
            "\n\n",
            "a",
            "1",
            ",",
            "\"",
            "`",
            "'",
            "\\",
            "//",
            "/*",
            "*/",
            "switch x {\n",
            "case 1:\n",
            "const (\n",
            "x := ",
            ";",
        ],
        Lang::Code(Code::C | Code::Cpp) => &[
            "int f() {\n",
            "struct S {\n",
            "}",
            "};",
            "{",
            "(",
            ")",
            "[",
            "]",
            ";",
            ",",
            "\n",
            "\n\n",
            "a",
            "1",
            "\"",
            "'",
            "\\",
            "//",
            "/*",
            "*/",
            "#if A\n",
            "#else\n",
            "#endif\n",
            "#define X ",
            "\\\n",
            "= {",
            "R\"x(",
            ")x\"",
            "switch (a) {\n",
            "case 1:\n",
            "else ",
            " ",
        ],
        Lang::Code(Code::Js | Code::Ts | Code::Tsx) => &[
            "function f() {\n",
            "class A {\n",
            "}",
            "{",
            "(",
            ")",
            "[",
            "]",
            ";",
            ",",
            "\n",
            "\n\n",
            "a",
            "1",
            "\"",
            "'",
            "`",
            "${",
            "\\",
            "//",
            "/*",
            "*/",
            "/",
            "= ",
            "=> ",
            "<div>",
            "</div>",
            "<a b=\"c\"/>",
            "return ",
            "const ",
            " ",
        ],
        Lang::Code(Code::Lua) => &[
            "function f(a)\n",
            "local x = 1\n",
            "if a then\n",
            "else\n",
            "end",
            "end\n",
            "for i = 1, 2 do\n",
            "repeat\n",
            "until a\n",
            "while a do\n",
            "return 1\n",
            "{",
            "}",
            "(",
            ")",
            "[",
            "]",
            ",",
            "\n",
            "\"",
            "'",
            "\\",
            "--",
            "--[[",
            "]]",
            "[[",
            "[==[",
            "]==]",
            "a",
            "1",
            " ",
        ],
        Lang::Code(Code::Diff) => &[
            "diff --git a b\n",
            "@@ -1 +1 @@\n",
            "+a\n",
            "-b\n",
            " c\n",
            "--- a\n",
            "+++ b\n",
            "index 1..2\n",
            "\n",
            "x",
            "+",
            "-",
        ],
        Lang::Code(Code::Cmake) => &[
            "if(a)\n",
            "else()\n",
            "endif()\n",
            "foreach(x y)\n",
            "endforeach()\n",
            "function(f)\n",
            "endfunction()\n",
            "set(A 1)\n",
            "(",
            ")",
            "\"",
            "\\",
            "#",
            "#[[",
            "]]",
            "[[",
            "[=[",
            "]=]",
            "\n",
            " ",
            "a",
        ],
        Lang::Code(Code::Make) => &[
            "a: b\n",
            "\tcmd\n",
            "X = 1\n",
            "ifeq (a,b)\n",
            "else\n",
            "endif\n",
            "define X\n",
            "endef\n",
            "# c\n",
            "\\\n",
            "\n",
            "$(X)",
            ":",
            "=",
            " ",
            "a",
        ],
        Lang::Code(Code::Css) => &[
            "a {\n",
            "}",
            "{",
            ";",
            ":",
            "(",
            ")",
            "[",
            "]",
            ",",
            "\n",
            "\n\n",
            "@media x {\n",
            "b: c;\n",
            "\"",
            "'",
            "\\",
            "/*",
            "*/",
            "//",
            "url(",
            " ",
            "#a",
            ".b",
        ],
        Lang::Code(_) => &[
            "{",
            "}",
            "(",
            ")",
            "[",
            "]",
            ";",
            ",",
            "\"",
            "'",
            "\\",
            " ",
            "\n",
            "\n\n",
            "a",
            "1",
            "//",
            "/*",
            "*/",
            "fn ",
            "else ",
            "r#\"",
            "\"#",
            "#",
            "<",
            ">",
            "match x {",
            "=> ",
        ],
    }
}

/// Пакетные языки: трекер работает и без пакета грамматики.
fn code_lang() -> impl Strategy<Value = Lang> {
    prop_oneof![
        Just(Lang::Code(Code::Rust)),
        Just(Lang::Code(Code::C)),
        Just(Lang::Code(Code::Cpp)),
        Just(Lang::Code(Code::CSharp)),
        Just(Lang::Code(Code::Java)),
        Just(Lang::Code(Code::Go)),
        Just(Lang::Code(Code::Js)),
        Just(Lang::Code(Code::Ts)),
        Just(Lang::Code(Code::Tsx)),
        Just(Lang::Code(Code::Python)),
        Just(Lang::Code(Code::Css)),
        Just(Lang::Code(Code::Lua)),
        Just(Lang::Code(Code::Diff)),
        Just(Lang::Code(Code::Cmake)),
        Just(Lang::Code(Code::Make)),
    ]
}

/// Случайная цепочка токенов языка.
fn code_soup(lang: Lang, seed: usize, n: usize) -> String {
    let a = alphabet(lang);
    (0..n)
        .map(|i| a[(seed.wrapping_mul(31) + i * 17 + i * i) % a.len()])
        .collect()
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
        m.edit(
            &TextEdit {
                at: at as u64,
                old: del as u64,
                new: piece.len() as u64,
            },
            lang,
        );
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
    #![proptest_config(ProptestConfig { cases: crate::syntax::lex::testkit::cases(120), ..ProptestConfig::default() })]

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

    #[test]
    fn edits_code(lang in code_lang(), base in 0usize..100_000, n in 0usize..200,
                  edits in proptest::collection::vec((0usize..2000, 0usize..6, 0usize..32), 1..8)) {
        edits_case(lang, code_soup(lang, base, n), edits)?;
    }

    /// Настоящий код: правка → точки = проход с нуля, роли — те же.
    #[test]
    fn edits_tours(i in 0usize..6, edits in proptest::collection::vec((0usize..20_000, 0usize..6, 0usize..32), 1..6)) {
        let (lang, text) = [
            (Code::Java, include_str!("lex/code/samples/tour.java")),
            (Code::CSharp, include_str!("lex/code/samples/tour.cs")),
            (Code::Tsx, include_str!("lex/code/samples/tour.tsx")),
            (Code::Lua, include_str!("lex/code/samples/tour.lua")),
            (Code::Diff, include_str!("lex/code/samples/tour.diff")),
            (Code::Rust, include_str!("lex/code/brace.rs")),
        ][i];
        edits_case(Lang::Code(lang), text.to_owned(), edits)?;
    }

    /// Окно с любой точки = проход с начала — и на неверных документах (свои правила).
    #[test]
    fn windows_any_code(lang in code_lang(), seed in 0usize..1000,
                        cuts in proptest::collection::vec((0u64..3000, 0u64..3000), 1..5)) {
        let text = code_soup(lang, seed, 400);
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
    m.edit(
        &TextEdit {
            at: 7,
            old: 0,
            new: 1,
        },
        Lang::Json,
    );
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
    m.edit(
        &TextEdit {
            at: 3,
            old: 0,
            new: 1,
        },
        Lang::Json,
    );
    let mem = Mem(t.clone());
    let s = src(&mem);
    assert_eq!(
        advance(&mut m, &s, Lang::Json, u64::MAX, &|| false).unwrap(),
        End::Done
    );
}

/// Окно всего текста (роли, зоны, якоря).
fn full_window(lang: Lang, text: &[u8]) -> crate::syntax::win::Window {
    let mem = Mem(text.to_vec());
    let s = src(&mem);
    let m = pass(lang, text, 61);
    crate::syntax::win::window(
        &s,
        crate::enc::Enc::utf8(),
        lang,
        &m,
        0,
        text.len() as u64,
        &|| false,
    )
    .unwrap()
    .unwrap()
}

fn roles_of_window(text: &[u8], w: &crate::syntax::win::Window) -> Vec<u8> {
    let mut out = vec![0u8; text.len()];
    for s in &w.spans {
        out[s.start as usize..s.end as usize].fill(s.tag as u8);
    }
    for (i, b) in text.iter().enumerate() {
        if crate::syntax::lex::is_ws(*b) {
            out[i] = 9;
        }
    }
    out
}

/// «Без неточных цветов» (ADR-0024 §4): после любой правки роли до якоря не меняются;
/// правка в инертной зоне без запрещённых байт не меняет роли (со сдвигом).
fn guarantee_case(
    lang: Lang,
    base: String,
    at: usize,
    del: usize,
    ins: usize,
) -> Result<(), TestCaseError> {
    let a = alphabet(lang);
    let text = base.into_bytes();
    let w = full_window(lang, &text);
    let old = roles_of_window(&text, &w);
    let at = at % (text.len() + 1);
    let del = del.min(text.len() - at);
    let piece = a[ins % a.len()].as_bytes();
    let mut new_text = text.clone();
    new_text.splice(at..at + del, piece.iter().copied());
    let nw = full_window(lang, &new_text);
    let new = roles_of_window(&new_text, &nw);
    let q = w.guard(at as u64).unwrap_or(0) as usize;
    prop_assert_eq!(
        &old[..q],
        &new[..q],
        "якорь {} правка {} в {:?}",
        q,
        at,
        String::from_utf8_lossy(&text)
    );
    // Инертная правка: роли — со сдвигом.
    let inert = w.inert.iter().any(|z| {
        z.start <= at as u64
            && (at + del) as u64 <= z.end
            && (del == 0 || z.del)
            && !piece.iter().any(|b| z.deny.contains(b))
    });
    if inert {
        let mut shifted = old[..at].to_vec();
        // Вставленное — роль зоны (как у байта перед ним, если он в той же зоне).
        let fill = if at > 0 { old[at - 1] } else { 0 };
        for b in piece {
            shifted.push(if crate::syntax::lex::is_ws(*b) {
                9
            } else {
                fill
            });
        }
        shifted.extend_from_slice(&old[at + del..]);
        // Сравнение без вставленных байт (их роль — роль окружающего токена).
        prop_assert_eq!(&new[..at], &shifted[..at]);
        prop_assert_eq!(
            &new[at + piece.len()..],
            &shifted[at + piece.len()..],
            "инертная правка {} в {:?}",
            at,
            String::from_utf8_lossy(&text)
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: crate::syntax::lex::testkit::cases(300), ..ProptestConfig::default() })]

    #[test]
    fn guarantee_lexers(lang in prop_oneof![Just(Lang::Json), Just(Lang::Toml), Just(Lang::Xml), Just(Lang::Ini)],
                        seed in 0usize..100_000, at in 0usize..3000, del in 0usize..4, ins in 0usize..64) {
        let a = alphabet(lang);
        let n = 30 + seed % 200;
        let base: String = (0..n).map(|i| a[(seed / 7 + i * 13 + i * i * 3) % a.len()]).collect();
        guarantee_case(lang, base, at, del, ins)?;
    }

    #[test]
    fn guarantee_trees(lang in prop_oneof![Just(Lang::Yaml), Just(Lang::Md)],
                       seed in 0usize..100_000, at in 0usize..3000, del in 0usize..4, ins in 0usize..64) {
        let a = alphabet(lang);
        let n = 20 + seed % 120;
        let base: String = (0..n).map(|i| a[(seed / 7 + i * 13 + i * i * 3) % a.len()]).collect();
        guarantee_case(lang, base, at, del, ins)?;
    }
}

/// Места свёртки окна всего текста: `(строка заголовка, первая скрытая, первая видимая
/// после)` — номера строк с 0.
fn fold_rows(lang: Lang, text: &str) -> Vec<(usize, usize, usize)> {
    let w = full_window(lang, text.as_bytes());
    let row = |off: u64| text[..off as usize].matches('\n').count();
    w.folds
        .iter()
        .map(|f| (row(f.head), row(f.start), row(f.end)))
        .collect()
}

/// Свёртка по событиям структуры (ADR-0024 §5): закрывающая строка видна, пустые в
/// конце — тоже.
#[test]
fn folds_by_language() {
    let json = "{\n  \"a\": {\n    \"b\": [\n      1,\n      2\n    ]\n  },\n  \"c\": 1\n}\n";
    assert_eq!(
        fold_rows(Lang::Json, json),
        vec![(0, 1, 8), (1, 2, 6), (2, 3, 5)]
    );
    let toml = "[a]\nx = 1\n\n[b]\nl = [\n  1,\n]\n";
    assert_eq!(
        fold_rows(Lang::Toml, toml),
        vec![(0, 1, 2), (3, 4, 7), (4, 5, 6)]
    );
    let ini = "[s1]\na=1\nb=2\n\n[s2]\nc=3\n";
    assert_eq!(fold_rows(Lang::Ini, ini), vec![(0, 1, 3), (4, 5, 6)]);
    let xml = "<r>\n  <a x=\"1\">\n    t\n  </a>\n  <!--\n  c\n  -->\n</r>\n";
    assert_eq!(
        fold_rows(Lang::Xml, xml),
        vec![(0, 1, 7), (1, 2, 3), (4, 5, 6)]
    );
    let yaml = "a:\n  b: 1\n  c:\n    - x\n    - y\n\nd: 2\n";
    assert_eq!(fold_rows(Lang::Yaml, yaml), vec![(0, 1, 5), (2, 3, 5)]);
    let md = "# One\ntext\n## Two\n```\ncode\n```\nmore\n# Three\nend\n";
    assert_eq!(
        fold_rows(Lang::Md, md),
        vec![(0, 1, 7), (2, 3, 7), (3, 4, 5), (7, 8, 9)]
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: crate::syntax::lex::testkit::cases(200), ..ProptestConfig::default() })]

    /// Место свёртки с одним заголовком — одно и то же в любом окне (как в окне всего
    /// текста).
    #[test]
    fn folds_same_in_any_window(lang in prop_oneof![Just(Lang::Json), Just(Lang::Toml), Just(Lang::Xml), Just(Lang::Ini), Just(Lang::Yaml), Just(Lang::Md), code_lang()],
                                seed in 0usize..100_000, cuts in proptest::collection::vec((0u64..4000, 0u64..4000), 1..4)) {
        let a = alphabet(lang);
        let n = 40 + seed % 300;
        let text: String = (0..n).map(|i| a[(seed / 3 + i * 7 + i * i * 5) % a.len()]).collect();
        let t = text.as_bytes();
        let full = full_window(lang, t);
        let mem = Mem(t.to_vec());
        let s = src(&mem);
        let m = pass(lang, t, 61);
        for (x, y) in cuts {
            let len = t.len() as u64;
            let (x, y) = (x.min(len).min(y.min(len)), y.min(len).max(x.min(len)));
            let w = crate::syntax::win::window(&s, crate::enc::Enc::utf8(), lang, &m, x, y, &|| false)
                .unwrap()
                .unwrap();
            for f in &w.folds {
                let g = full.folds.iter().find(|g| g.head == f.head);
                prop_assert_eq!(g, Some(f), "окно {}..{} в {:?}", x, y, text);
            }
        }
    }
}

/// Свёртка кода — из событий трекера (ADR-0024 §5, §8): работает и без пакета грамматики.
#[test]
fn folds_of_code() {
    let rust = "fn a() {\n    x();\n}\n\n/* два\n   ряда */\nfn b() {}\n";
    assert_eq!(
        fold_rows(Lang::Code(Code::Rust), rust),
        vec![(0, 1, 2), (4, 5, 6)]
    );
    let py = "class A:\n    def f(self):\n        x = 1\n        y = 2\n\n    def g(self):\n        pass\n\nz = 1\n";
    assert_eq!(
        fold_rows(Lang::Code(Code::Python), py),
        vec![(0, 1, 7), (1, 2, 4), (5, 6, 7)]
    );
    let c = "#ifdef A\nint x;\n#endif\nstruct S {\n  int a;\n};\n";
    assert_eq!(
        fold_rows(Lang::Code(Code::C), c),
        vec![(0, 1, 2), (3, 4, 5)]
    );
    let lua = "function f()\n  if a then\n    b()\n  end\nend\n";
    assert_eq!(
        fold_rows(Lang::Code(Code::Lua), lua),
        vec![(0, 1, 4), (1, 2, 3)]
    );
    let diff = "diff --git a b\n--- a\n+++ b\n@@ -1 +1 @@\n-x\n+y\ndiff --git c d\n";
    assert_eq!(
        fold_rows(Lang::Code(Code::Diff), diff),
        vec![(0, 1, 6), (3, 4, 6)]
    );
}

/// Свёртка списков и цитат Markdown (решение 2026-11-03): весь список верхнего уровня (с
/// первого пункта; его пункт — тот же заголовок), пункт с содержимым больше строки, цитата
/// больше строки; пустые строки в конце видны.
#[test]
fn md_lists_and_quotes_fold() {
    let md = |s: &str| fold_rows(Lang::Md, s);
    // Вложенные маркированные: пункт в одну строку места не даёт.
    assert_eq!(
        md("- a\n  - b\n    c\n  - d\n- e\n\ntext\n"),
        vec![(0, 1, 5), (1, 2, 3)]
    );
    // Нумерованные, пустые строки внутри пункта; другой разделитель — новый список.
    assert_eq!(
        md("1. one\n\n   para\n2. two\n   more\n\n1) other\n   tail\n"),
        vec![(0, 1, 5), (3, 4, 5), (6, 7, 8)]
    );
    // Другой маркер — другой список.
    assert_eq!(md("- a\n- b\n* c\n* d\n"), vec![(0, 1, 2), (2, 3, 4)]);
    // Список, прерванный текстом с колонки 0 после пустой строки.
    assert_eq!(md("- a\n- b\n\nx\n- c\n  d\n"), vec![(0, 1, 2), (4, 5, 6)]);
    // Ленивое продолжение абзаца — в пункте.
    assert_eq!(md("- a\nlazy\n- b\n"), vec![(0, 1, 3)]);
    // Вложенные цитаты; `>` без текста закрывает внутреннюю, пустая строка — внешнюю.
    assert_eq!(
        md("> a\n> > b\n> > c\n>\n> d\n\ntext\n"),
        vec![(0, 1, 5), (1, 2, 3)]
    );
    // Ленивое продолжение — в обеих цитатах.
    assert_eq!(md("> > a\n> b\nc\n\nx\n"), vec![(0, 1, 3)]);
    // Цитата в одну строку — без места.
    assert_eq!(md("> a\n\n> b\n"), Vec::<(usize, usize, usize)>::new());
    // Цитата в пункте и список в цитате.
    assert_eq!(
        md("- item\n  > q1\n  > q2\n- next\n"),
        vec![(0, 1, 4), (1, 2, 3)]
    );
    assert_eq!(md("> - a\n>   b\n> - c\n"), vec![(0, 1, 3)]);
    assert_eq!(md("> x\n> - a\n>   b\n> - c\n"), vec![(0, 1, 4), (1, 2, 3)]);
    // Ограждение в пункте: строки кода — не пункты.
    assert_eq!(md("- a\n  ```\n  - not\n  ```\n- b\n"), vec![(0, 1, 5)]);
    // Разделы: заголовок закрывает список; список под заголовком.
    assert_eq!(
        md("# H\n- a\n  b\n# I\nend\n"),
        vec![(0, 1, 3), (1, 2, 3), (3, 4, 5)]
    );
    // Тематический разрыв — не пункт; порядковый не с 1 абзац не прерывает.
    assert_eq!(md("- - -\ntext\n"), Vec::<(usize, usize, usize)>::new());
    assert_eq!(md("text\n2. x\n"), Vec::<(usize, usize, usize)>::new());
    // CRLF и табуляция в отступе.
    assert_eq!(md("- a\r\n\tb\r\n- c\r\n"), vec![(0, 1, 3)]);
}

/// Уровни трекера Markdown всегда парные: закрытие не раньше открытия, в конце текста
/// закрыто всё (порядок разделов, ограждений, списков и цитат — стек).
fn md_balanced(text: &str) -> Result<(), String> {
    struct Bal {
        d: u64,
        bad: Option<String>,
    }
    impl crate::syntax::lex::Out for Bal {
        fn open(&mut self, _at: u64, depth: u64, _kind: bool) {
            self.d += 1;
            if depth != self.d && self.bad.is_none() {
                self.bad = Some(format!("открытие: глубина {depth}, ждали {}", self.d));
            }
        }
        fn close(&mut self, at: u64, depth: u64) {
            if self.d == 0 || depth + 1 != self.d {
                self.bad.get_or_insert(format!("закрытие на {at} без пары"));
            }
            self.d = self.d.saturating_sub(1);
        }
    }
    let mem = Mem(text.as_bytes().to_vec());
    let s = src(&mem);
    let mut out = Bal { d: 0, bad: None };
    let mut sc = Scanner::new(&s, Lang::Md, 0, St::default());
    let mut cx = Cx::new(Stack::default(), &mut out);
    cx.cap = false;
    sc.run_to(u64::MAX, &mut cx, &|| false).unwrap();
    sc.flush(&mut cx);
    let depth = cx.depth();
    drop(cx);
    if let Some(b) = out.bad {
        return Err(b);
    }
    if depth != 0 || out.d != 0 {
        return Err(format!("в конце открыто {depth}"));
    }
    Ok(())
}

const MD_NEST: &[&str] = &[
    "- ", "* ", "+ ", "1. ", "2) ", "> ", ">", "  ", "   ", "    ", "\t", "\n", "\n", "\n\n", "x",
    "```", "~~~", "# ", "## ", "---", "<div>", "\r\n",
];

proptest! {
    #![proptest_config(ProptestConfig { cases: crate::syntax::lex::testkit::cases(300), ..ProptestConfig::default() })]

    /// Списки и цитаты: уровни парные, место свёртки — одно и то же в любом окне.
    #[test]
    fn md_nest_folds_same_in_any_window(seed in 0usize..100_000,
                                        cuts in proptest::collection::vec((0u64..3000, 0u64..3000), 1..4)) {
        let text = md_nest_text(seed);
        prop_assert_eq!(md_balanced(&text), Ok(()), "{:?}", text);
        let t = text.as_bytes();
        let full = full_window(Lang::Md, t);
        let mem = Mem(t.to_vec());
        let s = src(&mem);
        let m = pass(Lang::Md, t, 37);
        for (x, y) in cuts {
            let len = t.len() as u64;
            let (x, y) = (x.min(len).min(y.min(len)), y.min(len).max(x.min(len)));
            let w = crate::syntax::win::window(&s, crate::enc::Enc::utf8(), Lang::Md, &m, x, y, &|| false)
                .unwrap()
                .unwrap();
            for f in &w.folds {
                let g = full.folds.iter().find(|g| g.head == f.head);
                prop_assert_eq!(g, Some(f), "окно {}..{} в {:?}", x, y, text);
            }
        }
    }
}

/// Текст свойства `md_nest_folds_same_in_any_window` по зерну.
fn md_nest_text(seed: usize) -> String {
    let a = MD_NEST;
    let n = 40 + seed % 300;
    (0..n)
        .map(|i| a[(seed / 3 + i * 7 + i * i * 5) % a.len()])
        .collect()
}

/// Закрытие за проходом (по сводкам точек) — на строке, начала которой проход не видел:
/// конец свёртки — по её настоящему началу, а не по последней известной строке (раздел
/// `# * # ` закрывает следующий заголовок за пустыми строками с CRLF).
#[test]
fn md_fold_close_past_scan_uses_real_line_start() {
    for (seed, x, y) in [(51393, 0, 183), (36543, 0, 543), (11652, 3, 556)] {
        let text = md_nest_text(seed);
        let t = text.as_bytes();
        let full = full_window(Lang::Md, t);
        let mem = Mem(t.to_vec());
        let s = src(&mem);
        let m = pass(Lang::Md, t, 37);
        let w =
            crate::syntax::win::window(&s, crate::enc::Enc::utf8(), Lang::Md, &m, x, y, &|| false)
                .unwrap()
                .unwrap();
        assert_ne!(w.folds.len(), 0);
        for f in &w.folds {
            let g = full.folds.iter().find(|g| g.head == f.head);
            assert_eq!(g, Some(f), "окно {x}..{y}, зерно {seed}");
        }
    }
}
