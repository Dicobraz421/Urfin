//! Дифференциальные тесты кода (ADR-0024 §8): окно по кускам = полный разбор tree-sitter,
//! окно с любой точки = проход с начала, точки после правок = проход с нуля. Сравнение с
//! tree-sitter требует пакетов: `HL_PACK=<папка DLL>`, внешний корпус — `HL_CORPUS`.

#![allow(clippy::many_single_char_names, reason = "тесты: текст, точки, окно")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

use super::super::Lang;
use super::super::testkit::{ext_files, first_diff, marks_of, repo_files, ts_roles, win_roles};
use super::{Code, TEST_CUT_MIN};
use crate::syntax::grammar;

/// Итог файла.
enum Res {
    /// Сравнён, границ столько.
    Same(usize),
    /// В дереве ошибка: истины нет.
    Skip,
    Diff(String),
}

/// Границы — каждый кандидат; окно всего файла по кускам = полный разбор.
fn check(lang: Lang, text: &str, min: u64) -> Res {
    TEST_CUT_MIN.with(|c| c.set(min));
    let Some(want) = ts_roles(lang, text) else {
        return Res::Skip;
    };
    let t = text.as_bytes();
    let m = marks_of(lang, t);
    let got = win_roles(lang, t, &m, 0, t.len() as u64);
    match first_diff(t, &want, &got, 0, t.len()) {
        Some(d) => Res::Diff(d),
        None => Res::Same(cuts(lang, t)),
    }
}

/// Сколько границ в тексте.
fn cuts(lang: Lang, t: &[u8]) -> usize {
    use super::super::testkit::Mem;
    use super::super::{Cx, Feed, Out, Scanner, Src, St, Stack};
    struct Rec(usize);
    impl Out for Rec {
        fn cut(&mut self, _at: u64) {
            self.0 += 1;
        }
    }
    let mem = Mem(t.to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    let mut rec = Rec(0);
    let mut sc = Scanner::new(&src, lang, 0, St::default());
    let mut cx = Cx::new(Stack::default(), &mut rec);
    cx.cap = false;
    sc.run_to(t.len() as u64, &mut cx, &|| false).expect("run");
    drop(cx);
    rec.0
}

/// Прогон по файлам: сводка и первые расхождения.
fn corpus(lang: Lang, files: &[PathBuf], max: u64) -> (usize, usize, usize, Vec<String>) {
    let (same, skip, cut) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let bad: Vec<String> = files
        .par_iter()
        .filter_map(|p| {
            let meta = std::fs::metadata(p).ok()?;
            if meta.len() > max {
                return None;
            }
            let text = std::fs::read_to_string(p).ok()?;
            match check(lang, &text, 0) {
                Res::Same(n) => {
                    same.fetch_add(1, Ordering::Relaxed);
                    cut.fetch_add(n, Ordering::Relaxed);
                    None
                }
                Res::Skip => {
                    skip.fetch_add(1, Ordering::Relaxed);
                    None
                }
                Res::Diff(d) => Some(format!("{}: {d}", p.display())),
            }
        })
        .collect();
    (same.into_inner(), skip.into_inner(), cut.into_inner(), bad)
}

fn run_corpus(code: Code, ext: &[&str]) {
    let lang = Lang::Code(code);
    if grammar(lang.name()).is_none() {
        eprintln!("нет грамматики {}: HL_PACK", lang.name());
        return;
    }
    // `HL_EXT` — другие расширения (заголовки `.h` как C++).
    let over = std::env::var("HL_EXT").unwrap_or_default();
    let ext: Vec<&str> = if over.is_empty() {
        ext.to_vec()
    } else {
        over.split(',').collect()
    };
    let mut files = repo_files(&ext);
    files.extend(ext_files(&ext));
    let max = std::env::var("HL_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512 << 10);
    let (same, skip, cut, bad) = corpus(lang, &files, max);
    eprintln!(
        "{}: файлов {} совпало {same} с ошибками {skip} расхождений {} границ {cut}",
        lang.name(),
        files.len(),
        bad.len()
    );
    for b in bad.iter().take(12) {
        eprintln!("{b}");
    }
    assert!(bad.is_empty(), "расхождений: {}", bad.len());
}

/// Один файл (`HL_ONE`, `HL_LANG`): первое расхождение, `HL_TRACE=1` — куски.
#[test]
fn code_one() {
    let (Ok(p), Ok(l)) = (std::env::var("HL_ONE"), std::env::var("HL_LANG")) else {
        return;
    };
    let lang = [
        Code::Rust,
        Code::C,
        Code::Cpp,
        Code::CSharp,
        Code::Java,
        Code::Go,
        Code::Js,
        Code::Ts,
        Code::Tsx,
        Code::Python,
        Code::Css,
        Code::Lua,
        Code::Diff,
    ]
    .into_iter()
    .find(|c| c.name() == l)
    .map(Lang::Code)
    .expect("lang");
    let text = std::fs::read_to_string(p).expect("read");
    match check(lang, &text, 0) {
        Res::Same(n) => eprintln!("совпало, границ {n}"),
        Res::Skip => eprintln!("в дереве ошибка"),
        Res::Diff(d) => {
            if let Some(at) = d
                .strip_prefix("byte ")
                .and_then(|r| r.split(' ').next())
                .and_then(|n| n.parse::<usize>().ok())
            {
                let g = grammar(lang.name()).expect("grammar");
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&g.lang).expect("lang");
                let t = parser.parse(&text, None).expect("parse");
                let mut n = t.root_node().descendant_for_byte_range(at, at + 1);
                while let Some(x) = n {
                    eprintln!("{} {}..{}", x.kind(), x.start_byte(), x.end_byte());
                    n = x.parent();
                }
            }
            panic!("{d}")
        }
    }
}

/// Дерево текста (`HL_TREE` — файл, `HL_LANG`): отладка.
#[test]
fn code_tree() {
    let (Ok(p), Ok(l)) = (std::env::var("HL_TREE"), std::env::var("HL_LANG")) else {
        return;
    };
    let g = grammar(&l).expect("grammar");
    let text = std::fs::read_to_string(p).expect("read");
    for part in text.split(
        "
%%
",
    ) {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&g.lang).expect("lang");
        let t = parser.parse(part, None).expect("parse");
        eprintln!(
            "{part}
=> {}",
            t.root_node().to_sexp()
        );
    }
}

/// Обзоры языков: без ошибок в дереве, окно = полный разбор при границе на каждом
/// кандидате; элементы размножены и перемешаны, чтобы границы попадали на разные места.
fn tour(code: Code, text: &str) {
    let lang = Lang::Code(code);
    if grammar(lang.name()).is_none() {
        return;
    }
    match check(lang, text, 0) {
        Res::Same(n) => assert!(n > 20, "{}: границ {n}", lang.name()),
        Res::Skip => panic!("{}: в дереве обзора ошибка", lang.name()),
        Res::Diff(d) => panic!("{}: {d}", lang.name()),
    }
}

#[test]
fn lua_tour() {
    tour(Code::Lua, include_str!("samples/tour.lua"));
}

#[test]
fn diff_tour() {
    tour(Code::Diff, include_str!("samples/tour.diff"));
}

#[test]
fn java_tour() {
    tour(Code::Java, include_str!("samples/tour.java"));
}

#[test]
fn csharp_tour() {
    tour(Code::CSharp, include_str!("samples/tour.cs"));
}

#[test]
fn tsx_tour() {
    tour(Code::Tsx, include_str!("samples/tour.tsx"));
}

/// Кодировки: те же роли в UTF-16 и в однобайтовой, что в UTF-8 (смещения — по источнику).
#[test]
fn encodings() {
    use super::super::testkit::Mem;
    use super::super::{Feed, Src};
    use crate::enc::Enc;
    use crate::syntax::marks::{Marks, advance};
    use crate::syntax::win::window;
    let lang = Lang::Code(Code::Rust);
    if grammar(lang.name()).is_none() {
        return;
    }
    TEST_CUT_MIN.with(|c| c.set(0));
    let text = "// c\nfn a(x: u32) -> u32 {\n    let s = \"строка\";\n    x + 1\n}\n\nfn b() {\n    a(1);\n}\n";
    let m8 = marks_of(lang, text.as_bytes());
    let roles = win_roles(lang, text.as_bytes(), &m8, 0, text.len() as u64);
    let raw: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let enc = Enc::new(encoding_rs::UTF_16LE);
    let mem = Mem(raw.clone());
    let src = Src {
        text: &mem,
        feed: Feed::Utf16Le,
        start: 0,
    };
    let mut m = Marks::new(0, raw.len() as u64);
    advance(&mut m, &src, lang, u64::MAX, &|| false).expect("pass");
    let w = window(&src, enc, lang, &m, 0, raw.len() as u64, &|| false)
        .expect("window")
        .expect("marks");
    // Байт UTF-16 → символ текста.
    let mut char_at = Vec::new();
    let mut off8 = 0;
    for c in text.chars() {
        for _ in 0..c.len_utf16() * 2 {
            char_at.push(off8);
        }
        off8 += c.len_utf8();
    }
    let mut got = vec![0u8; text.len()];
    for s in &w.spans {
        for b in s.start..s.end {
            let o = char_at[b as usize];
            got[o] = s.tag as u8;
        }
    }
    for (i, r) in roles.iter().enumerate() {
        if *r != super::super::testkit::WS && text.is_char_boundary(i) {
            assert_eq!(got[i], *r, "байт {i}");
        }
    }
}

#[test]
fn rust_corpus() {
    run_corpus(Code::Rust, &["rs"]);
}

#[test]
fn go_corpus() {
    run_corpus(Code::Go, &["go"]);
}

#[test]
fn java_corpus() {
    run_corpus(Code::Java, &["java"]);
}

#[test]
fn c_corpus() {
    run_corpus(Code::C, &["c", "h"]);
}

#[test]
fn cpp_corpus() {
    run_corpus(Code::Cpp, &["cc", "cpp", "cxx", "hpp", "hh"]);
}

#[test]
fn csharp_corpus() {
    run_corpus(Code::CSharp, &["cs"]);
}

#[test]
fn js_corpus() {
    run_corpus(Code::Js, &["js", "mjs", "cjs", "jsx"]);
}

#[test]
fn ts_corpus() {
    run_corpus(Code::Ts, &["ts", "mts", "cts"]);
}

#[test]
fn python_corpus() {
    run_corpus(Code::Python, &["py", "pyi", "pyw"]);
}

#[test]
fn css_corpus() {
    run_corpus(Code::Css, &["css"]);
}

#[test]
fn lua_corpus() {
    run_corpus(Code::Lua, &["lua"]);
}

#[test]
fn diff_corpus() {
    run_corpus(Code::Diff, &["diff", "patch"]);
}

#[test]
fn tsx_corpus() {
    run_corpus(Code::Tsx, &["tsx"]);
}
