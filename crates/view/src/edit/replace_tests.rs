//! Приёмка S7-30: замена по кускам = `regex` по строке (литерал, regex, группы, CRLF,
//! 1251, UTF-16), совпадение через границу кусков и окон находится; через core — поиск,
//! «Заменить», «Заменить все» одной отменой, отмена задачи.

#![allow(clippy::many_single_char_names, reason = "тесты: короткие имена")]
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use encoding_rs::{Encoding, UTF_8, UTF_16LE, WINDOWS_1251};
use proptest::prelude::*;
use regex_automata::meta;
use regex_automata::util::syntax;
use urfin_core::{CancelToken, Command};

use super::*;
use crate::EditorCfg;
use crate::detect::Eol;
use crate::edit::input::tests::{cmd, ed, editor, eng, pump_until, text};
use crate::edit::open::tests::bytes_of;
use crate::edit::piece::tests::doc_of;
use crate::index::Sep;

/// Замена по модели: `meta::Regex` по строке (Unicode), группы — подстановкой.
fn model(s: &str, pat: &Pattern, with: &str) -> String {
    let (src, case, groups) = match pat {
        Pattern::Text { s, case, .. } => (regex_syntax::escape(s), *case, false),
        Pattern::Regex { s, case } => (s.clone(), *case, with.contains('$')),
        Pattern::Hex(_) => unreachable!(),
    };
    let re = meta::Regex::builder()
        .syntax(
            syntax::Config::new()
                .case_insensitive(!case)
                .multi_line(true)
                .crlf(true),
        )
        .build(&src)
        .unwrap();
    let mut out = String::new();
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let m = caps.get_match().unwrap();
        out.push_str(&s[last..m.start()]);
        if groups {
            caps.interpolate_string_into(s, with, &mut out);
        } else {
            out.push_str(with);
        }
        last = m.end();
    }
    out.push_str(&s[last..]);
    out
}

fn enc_of(i: usize) -> &'static Encoding {
    [UTF_8, WINDOWS_1251, UTF_16LE][i]
}

fn pat_of(i: usize) -> (Pattern, &'static str) {
    match i {
        0 => (
            Pattern::Text {
                s: "ab".into(),
                case: true,
                word: false,
            },
            "Z",
        ),
        1 => (
            Pattern::Text {
                s: "АБ".into(),
                case: false,
                word: false,
            },
            "ё",
        ),
        2 => (
            Pattern::Regex {
                s: "a(b+)".into(),
                case: true,
            },
            "<$1>",
        ),
        3 => (
            Pattern::Regex {
                s: "\r?\n".into(),
                case: true,
            },
            " ",
        ),
        4 => (
            Pattern::Regex {
                s: "(б+)x".into(),
                case: true,
            },
            "$1$1",
        ),
        _ => (
            Pattern::Regex {
                s: "^a|c$".into(),
                case: true,
            },
            "#",
        ),
    }
}

/// Правка модели: вставка строки или удаление символов (индексы — в символах).
#[derive(Clone, Debug)]
enum Ed {
    Ins(usize, String),
    Del(usize, usize),
}

fn alphabet() -> impl Strategy<Value = char> {
    prop_oneof![
        Just('a'),
        Just('b'),
        Just('c'),
        Just('x'),
        Just('\r'),
        Just('\n'),
        Just('А'),
        Just('б'),
    ]
}

fn text_of(n: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(alphabet(), 0..n).prop_map(|v| v.into_iter().collect())
}

/// Байтовое смещение символа `i` строки `s` в кодировке `e`.
fn off(s: &str, i: usize, e: &'static Encoding) -> u64 {
    let pre: String = s.chars().take(i).collect();
    bytes_of(&pre, e, false).len() as u64
}

/// Документ из кусков: исходник и правки по модели.
fn build(orig: &str, eds: &[Ed], e: &'static Encoding) -> (TextDoc, String) {
    let sep = Sep::of(&Enc::new(e), Eol::Lf);
    let mut doc = doc_of(&bytes_of(orig, e, false), sep);
    let mut m = orig.to_owned();
    for ed in eds {
        let n = m.chars().count();
        match ed {
            Ed::Ins(i, t) => {
                let i = i % (n + 1);
                doc.insert(off(&m, i, e), &bytes_of(t, e, false)).unwrap();
                let at = m.char_indices().nth(i).map_or(m.len(), |(b, _)| b);
                m.insert_str(at, t);
            }
            Ed::Del(i, k) => {
                if n == 0 {
                    continue;
                }
                let i = i % n;
                let j = (i + k).min(n);
                doc.remove(off(&m, i, e)..off(&m, j, e));
                let a = m.char_indices().nth(i).map_or(m.len(), |(b, _)| b);
                let b = m.char_indices().nth(j).map_or(m.len(), |(b, _)| b);
                m.replace_range(a..b, "");
            }
        }
    }
    (doc, m)
}

fn all_bytes(doc: &TextDoc) -> Vec<u8> {
    let s = doc.snap();
    let mut b = vec![0u8; s.len() as usize];
    assert_eq!(s.read_at(0, &mut b).unwrap(), b.len());
    b
}

fn run_replace(
    doc: &mut TextDoc,
    pat: &Pattern,
    with: &str,
    e: &'static Encoding,
    chunk: u64,
    max: usize,
) -> usize {
    let enc = Enc::new(e);
    let m = compile(pat, enc).unwrap();
    let repl = Repl::new(pat, with, enc).unwrap();
    let cancel = CancelToken::new();
    let scanned = AtomicU64::new(0);
    let o = Scan {
        chunk,
        cancel: &cancel,
        scanned: &scanned,
    };
    let dir = tempfile::tempdir().unwrap();
    let temp = TempAt {
        dir: dir.path().to_path_buf(),
        name: "t".into(),
    };
    let res = replace_all_max(&doc.snap(), &m, &repl, &o, 0, &temp, max).unwrap();
    let n = res.count();
    let (tree, _) = apply(doc, res, 0);
    doc.set_tree(tree);
    n
}

/// Приёмка S7-43: «Заменить все» не пишет в буфер правок `Add` из UI-потока. Замены — без
/// групп, с группами и больше порога памяти — задача кладёт в свой источник `Rewrite`; на
/// UI-потоке (поток помечен: любое чтение источника и подготовка замен на нём — паника)
/// строится только новое дерево, длина и выгрузка `Add` не меняются.
#[test]
fn replace_all_keeps_add_off_the_ui_thread() {
    let text = "ключ=1\nключ=22\nдругое\nключ=333\n".repeat(50);
    let long = format!("k[$1]{}", "x".repeat(8000));
    let cases = [
        (
            Pattern::Text {
                s: "ключ".into(),
                case: true,
                word: false,
            },
            "key",
        ),
        (
            Pattern::Regex {
                s: r"ключ=(\d+)".into(),
                case: true,
            },
            "k[$1]",
        ),
        // 150 замен по 8 КБ — больше `REPL_MEM`: источник замен — временный файл.
        (
            Pattern::Regex {
                s: r"ключ=(\d+)".into(),
                case: true,
            },
            long.as_str(),
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    let temp = TempAt {
        dir: dir.path().to_path_buf(),
        name: "t".into(),
    };
    let enc = Enc::new(UTF_8);
    // Задача — в своём потоке, как в приложении (Bulk lane).
    let mut jobs = Vec::new();
    for (pat, with) in &cases {
        let (doc, m) = build(&text, &[Ed::Ins(6, " вставка".into())], UTF_8);
        let snap = doc.snap();
        let res = std::thread::scope(|s| {
            s.spawn(|| {
                let m = compile(pat, enc).unwrap();
                let repl = Repl::new(pat, with, enc).unwrap();
                let cancel = CancelToken::new();
                let scanned = AtomicU64::new(0);
                let o = Scan {
                    chunk: 1 << 20,
                    cancel: &cancel,
                    scanned: &scanned,
                };
                replace_all(&snap, &m, &repl, &o, 0, &temp).unwrap()
            })
            .join()
            .unwrap()
        });
        jobs.push((doc, model(&m, pat, with), res));
    }
    assert!(
        matches!(&jobs[2].2, Replaced::Small { src: Some(s), .. } if s.len() > REPL_MEM as u64)
    );

    // UI-поток: только новое дерево.
    crate::source::mark_ui_thread();
    let mut done = Vec::new();
    for (mut doc, want, res) in jobs {
        let (len, spilled) = (doc.add().len(), doc.add().spilled());
        assert_eq!(res.count(), 150);
        let (tree, _) = apply(&mut doc, res, 0);
        doc.set_tree(tree);
        assert_eq!(doc.add().len(), len, "в `Add` ничего не дописано");
        assert_eq!(doc.add().spilled(), spilled);
        done.push((doc.snap(), want));
    }
    // Текст — как у модели (чтение снимка — снова вне UI-потока).
    std::thread::scope(|s| {
        s.spawn(|| {
            for (snap, want) in &done {
                let mut b = vec![0u8; snap.len() as usize];
                assert_eq!(snap.read_at(0, &mut b).unwrap(), b.len());
                assert_eq!(String::from_utf8(b).unwrap(), *want);
            }
        })
        .join()
        .unwrap();
    });
}

/// Курсор и текст потока — как у новых кусков: на всех местах и на окнах меньше текста.
#[test]
fn stream_caret_matches_pieces() {
    let text = "ab needle cd needle needle ef";
    let pat = Pattern::Text {
        s: "needle".into(),
        case: true,
        word: false,
    };
    let enc = Enc::new(UTF_8);
    let m = compile(&pat, enc).unwrap();
    let repl = Repl::new(&pat, "pin", enc).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let temp = TempAt {
        dir: dir.path().to_path_buf(),
        name: "t".into(),
    };
    let run = |caret, chunk, max| {
        let (mut doc, _) = build(text, &[], UTF_8);
        let cancel = CancelToken::new();
        let scanned = AtomicU64::new(0);
        let o = Scan {
            chunk,
            cancel: &cancel,
            scanned: &scanned,
        };
        let res = replace_all_max(&doc.snap(), &m, &repl, &o, caret, &temp, max).unwrap();
        let (tree, at) = apply(&mut doc, res, caret);
        doc.set_tree(tree);
        (all_bytes(&doc), at)
    };
    for caret in 0..=text.len() as u64 {
        let want = run(caret, 1 << 20, PIECES_MAX);
        for chunk in [1, 5, 7, 11, 1 << 20] {
            assert_eq!(run(caret, chunk, 1), want, "курсор {caret}, окно {chunk}");
        }
    }
}

/// Поток больше порога памяти: хвост записи доходит до файла целиком, в том числе
/// кусок длиннее блока; совпадения — на границах блока чтения.
#[test]
fn stream_through_file() {
    let line = b"aaaa needle bbbb
";
    let n = (super::super::write::MEM_MAX as usize + (1 << 20)) / line.len();
    let (mut doc, _) = build(std::str::from_utf8(&line.repeat(n)).unwrap(), &[], UTF_8);
    let pat = Pattern::Text {
        s: "needle".into(),
        case: true,
        word: false,
    };
    assert_eq!(run_replace(&mut doc, &pat, "pin", UTF_8, 1 << 20, 16), n);
    assert_eq!(
        all_bytes(&doc),
        b"aaaa pin bbbb
"
        .repeat(n)
    );
    // Без совпадений в хвосте: кусок после последнего — один блок чтения и больше.
    let long = "x".repeat(20 << 20);
    let (mut doc, _) = build(&format!("needle needle {long}"), &[], UTF_8);
    assert_eq!(run_replace(&mut doc, &pat, "pin", UTF_8, 1 << 20, 1), 2);
    assert_eq!(all_bytes(&doc), format!("pin pin {long}").as_bytes());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Приёмка: замена по кускам (куски и поток `Rewrite`) = модель по строке; окна
    /// поиска малы — совпадения через границы окон и кусков.
    #[test]
    fn replace_matches_model(
        ei in 0usize..3,
        pi in 0usize..6,
        orig in text_of(120),
        eds in proptest::collection::vec(
            prop_oneof![
                (0usize..200, text_of(12)).prop_map(|(i, t)| Ed::Ins(i, t)),
                (0usize..200, 1usize..6).prop_map(|(i, k)| Ed::Del(i, k)),
            ],
            0..8,
        ),
        chunk in 4u64..64,
        stream in any::<bool>(),
    ) {
        let e = enc_of(ei);
        let (pat, with) = pat_of(pi);
        let (mut doc, m) = build(&orig, &eds, e);
        prop_assert_eq!(all_bytes(&doc), bytes_of(&m, e, false));
        let want = model(&m, &pat, with);
        let max = if stream { 1 } else { PIECES_MAX };
        run_replace(&mut doc, &pat, with, e, chunk, max);
        let got = all_bytes(&doc);
        prop_assert!(
            got == bytes_of(&want, e, false),
            "{} {:?}: {:?} → {:?}, вышло {:?}",
            e.name(), pat, m, want, e.decode_without_bom_handling(&got).0
        );
    }
}

/// Совпадение через границу кусков (исходник + правка) находится и заменяется.
#[test]
fn match_across_pieces() {
    let (mut doc, m) = build(
        "xxa",
        &[Ed::Ins(3, "bbc".into()), Ed::Ins(0, "ab".into())],
        UTF_8,
    );
    assert_eq!(m, "abxxabbc");
    assert!(doc.tree().iter().count() >= 2);
    let (pat, with) = pat_of(2);
    assert_eq!(run_replace(&mut doc, &pat, with, UTF_8, 4, PIECES_MAX), 2);
    assert_eq!(all_bytes(&doc), b"<b>xx<bb>c");
}

/// Через core: поиск выделяет совпадение, «Заменить» — по одному, «Заменить все» — одна
/// отмена; Esc останавливает задачу.
#[test]
fn engine_find_and_replace() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(
        dir.path(),
        "f.txt",
        b"foo bar foo\r\nFOO foo",
        80,
        EditorCfg::default(),
    );
    c.dispatch(Command::new("editor.search").arg("text", "foo"));
    pump_until(&mut c, |c| {
        ed(c)
            .search
            .as_ref()
            .is_some_and(|s| !s.st.running && s.st.count.done)
    });
    assert_eq!(eng(&c).sel.stream(), Some(0..3));
    // Регистр по умолчанию не учитывается (`case` = false): 4 совпадения.
    assert_eq!(ed(&c).search.as_ref().unwrap().st.count.n, 4);
    c.dispatch(
        Command::new("editor.search")
            .arg("text", "foo")
            .arg("case", true),
    );
    pump_until(&mut c, |c| {
        ed(c).search.as_ref().is_some_and(|s| !s.st.running)
    });
    assert_eq!(eng(&c).sel.stream(), Some(8..11), "следующее от выделения");
    c.dispatch(
        Command::new("editor.replace")
            .arg("text", "foo")
            .arg("case", true)
            .arg("with", "Q"),
    );
    pump_until(&mut c, |c| {
        ed(c).search.as_ref().is_some_and(|s| !s.st.running)
    });
    assert_eq!(text(&c), b"foo bar Q\r\nFOO foo");
    assert_eq!(eng(&c).sel.stream(), Some(15..18), "дальше — следующее");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"foo bar foo\r\nFOO foo");
    c.dispatch(
        Command::new("editor.replace")
            .arg("text", "o+")
            .arg("kind", "regex")
            .arg("with", "[$0]")
            .arg("all", true),
    );
    pump_until(&mut c, |c| ed(c).replace.is_none() && !eng(c).busy);
    assert_eq!(text(&c), b"f[oo] bar f[oo]\r\nF[OO] f[oo]");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"foo bar foo\r\nFOO foo", "одна отмена");
    cmd(&mut c, "editor.redo");
    assert_eq!(text(&c), b"f[oo] bar f[oo]\r\nF[OO] f[oo]");
    // Esc во время задачи: документ снова правится.
    c.dispatch(
        Command::new("editor.replace")
            .arg("text", "o")
            .arg("with", "0")
            .arg("all", true),
    );
    cmd(&mut c, "editor.search_cancel");
    assert!(ed(&c).replace.is_none() && !eng(&c).busy);
    c.pump(Duration::from_millis(50));
    assert_eq!(
        text(&c),
        b"f[oo] bar f[oo]\r\nF[OO] f[oo]",
        "отменённая задача не правит"
    );
}
