//! Дифференциальные тесты подсветки (ADR-0024 §6): роли окна против tree-sitter +
//! `highlights.scm`, окно с любой точки против прохода с начала, точки после правок
//! против прохода с нуля.

use std::io;

use super::{Cx, Feed, Lang, Out, Scanner, Src, St, Stack, Text};
use crate::enc::Enc;
use crate::syntax::marks::{Marks, advance};
use crate::syntax::win::window;
use crate::syntax::{Tag, grammar, highlight};

/// Текст в памяти.
#[derive(Clone, Debug)]
pub(crate) struct Mem(pub Vec<u8>);

impl Text for Mem {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let off = usize::try_from(off).unwrap_or(usize::MAX).min(self.0.len());
        let n = buf.len().min(self.0.len() - off);
        buf[..n].copy_from_slice(&self.0[off..off + n]);
        Ok(n)
    }
}

/// Роль непробельного байта; пробел, табуляция, перевод строки — `9` (не сравниваются).
pub(crate) const WS: u8 = 9;

fn mask_ws(text: &[u8], roles: &mut [u8]) {
    for (i, b) in text.iter().enumerate() {
        if super::is_ws(*b) {
            roles[i] = WS;
        }
    }
}

/// Роли tree-sitter (полный разбор); `None` — в дереве ошибка.
pub(crate) fn ts_roles(lang: Lang, text: &str) -> Option<Vec<u8>> {
    let g = grammar(lang.name()).expect("grammar");
    let mut p = tree_sitter::Parser::new();
    p.set_language(&g.lang).ok()?;
    let t = p.parse(text, None)?;
    if t.root_node().has_error() {
        return None;
    }
    let mut out = vec![0u8; text.len()];
    for s in highlight(&g, text, &|| false).ok()? {
        out[s.start as usize..s.end as usize].fill(s.tag as u8);
    }
    mask_ws(text.as_bytes(), &mut out);
    Some(out)
}

/// Точки по всему тексту.
pub(crate) fn marks_of(lang: Lang, text: &[u8]) -> Marks {
    let mem = Mem(text.to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    let mut m = Marks::new(0, mem.len());
    advance(&mut m, &src, lang, u64::MAX, &|| false).expect("pass");
    m
}

/// Роли окна `[a, b)` (вне окна — 0).
pub(crate) fn win_roles(lang: Lang, text: &[u8], marks: &Marks, a: u64, b: u64) -> Vec<u8> {
    let mem = Mem(text.to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    let w = window(&src, Enc::utf8(), lang, marks, a, b, &|| false)
        .expect("window")
        .expect("marks reach");
    let mut out = vec![0u8; text.len()];
    for s in &w.spans {
        assert!(
            s.start >= a && s.end <= b && s.start < s.end,
            "span {s:?} out of {a}..{b}"
        );
        out[s.start as usize..s.end as usize].fill(s.tag as u8);
    }
    mask_ws(text, &mut out);
    out
}

/// Роли лексера по всему тексту.
pub(crate) fn lex_roles(lang: Lang, text: &[u8]) -> Vec<u8> {
    let m = marks_of(lang, text);
    win_roles(lang, text, &m, 0, text.len() as u64)
}

/// Первое расхождение ролей: описание с контекстом.
pub(crate) fn first_diff(
    text: &[u8],
    want: &[u8],
    got: &[u8],
    a: usize,
    b: usize,
) -> Option<String> {
    let i = (a..b).find(|&i| want[i] != got[i])?;
    let ls = text[..i]
        .iter()
        .rposition(|&c| c == b'\n')
        .map_or(0, |x| x + 1);
    let le = text[i..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(text.len(), |x| i + x);
    Some(format!(
        "byte {i} (line {:?}): want {:?} got {:?}",
        String::from_utf8_lossy(&text[ls..le]),
        tag_name(want[i]),
        tag_name(got[i])
    ))
}

fn tag_name(t: u8) -> &'static str {
    match t {
        x if x == Tag::Keyword as u8 => "keyword",
        x if x == Tag::String as u8 => "string",
        x if x == Tag::Number as u8 => "number",
        x if x == Tag::Comment as u8 => "comment",
        WS => "ws",
        _ => "plain",
    }
}

/// Ошибки самой грамматики (ADR-0024 §6), где верна спецификация, а не tree-sitter:
/// сканер `tree-sitter-xml` не закрывает CDATA на `]]]>` и пустую CDATA `<![CDATA[]]>`
/// (тянет до следующего `]]>`); решение 2026-11-03 — не повторять.
pub(crate) fn oracle_bug(lang: Lang, text: &str) -> bool {
    lang == Lang::Xml && (text.contains("]]]>") || text.contains("<![CDATA[]]>"))
}

/// Под абзацем из определений ссылок нет подчёркивания: грамматика делает из него заголовок
/// Setext или пункт, а `CommonMark` — текст абзаца или разрыв, и окно красит по спецификации
/// (`Out::no_setext`): сравнивать окно с таким разбором незачем.
pub(crate) fn md_setext_agrees(text: &str) -> bool {
    #[derive(Default)]
    struct Rec(bool);
    impl Out for Rec {
        fn no_setext(&mut self, _at: u64) {
            self.0 = true;
        }
    }
    let mem = Mem(text.as_bytes().to_vec());
    let src = Src {
        text: &mem,
        feed: Feed::Bytes,
        start: 0,
    };
    let mut rec = Rec::default();
    let mut sc = Scanner::new(&src, Lang::Md, 0, St::default());
    let mut cx = Cx::new(Stack::default(), &mut rec);
    cx.cap = false;
    sc.run_to(mem.len(), &mut cx, &|| false).expect("run");
    sc.flush(&mut cx);
    drop(cx);
    !rec.0
}

/// Строки (непустые) внутри HTML-блоков по tree-sitter и по pulldown-cmark совпали.
/// Грамматика `tree-sitter-md` местами расходится с `CommonMark` (`<textarea`, `<!x`
/// строчной, `<?>`, конец типа 4 не отдельным `>`), и верна спецификация, как и в
/// [`oracle_bug`]: сравнивать окно с таким разбором незачем. Окно и проход с начала
/// сравниваются и на таких текстах (`md::zone_tests`).
pub(crate) fn md_html_agrees(text: &str) -> bool {
    let g = grammar(Lang::Md.name()).expect("grammar");
    let mut p = tree_sitter::Parser::new();
    p.set_language(&g.lang).expect("language");
    let tree = p.parse(text, None).expect("parse");
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line = |o: usize| starts.partition_point(|&s| s <= o) - 1;
    let (mut ts, mut cm) = (vec![false; starts.len()], vec![false; starts.len()]);
    let mut stack = vec![tree.root_node()];
    while let Some(x) = stack.pop() {
        if x.kind() == "html_block" && x.end_byte() > x.start_byte() {
            ts[line(x.start_byte())..=line(x.end_byte() - 1)].fill(true);
        }
        let mut walk = x.walk();
        stack.extend(x.children(&mut walk));
    }
    for (ev, r) in pulldown_cmark::Parser::new(text).into_offset_iter() {
        if matches!(
            ev,
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock)
        ) {
            cm[line(r.start)..=line(r.end - 1)].fill(true);
        }
    }
    let blank = |l: usize| {
        let end = starts.get(l + 1).copied().unwrap_or(text.len());
        text[starts[l]..end].trim().is_empty()
    };
    (0..starts.len()).all(|l| ts[l] == cm[l] || blank(l))
}

/// Лексер = tree-sitter (если документ без ошибок): `Err` — расхождение.
pub(crate) fn check_lexer(lang: Lang, text: &str) -> Result<bool, String> {
    if oracle_bug(lang, text) {
        return Ok(false);
    }
    let Some(want) = ts_roles(lang, text) else {
        return Ok(false);
    };
    let got = lex_roles(lang, text.as_bytes());
    match first_diff(text.as_bytes(), &want, &got, 0, text.len()) {
        Some(d) => Err(format!("{}: {d}\n--- text ---\n{text}", lang.name())),
        None => Ok(true),
    }
}

/// Окна с разных мест = проход с начала (роли).
pub(crate) fn check_windows(lang: Lang, text: &[u8], cuts: &[(u64, u64)]) -> Result<(), String> {
    let m = marks_of(lang, text);
    let full = win_roles(lang, text, &m, 0, text.len() as u64);
    for &(a, b) in cuts {
        let (a, b) = (a.min(b), b.max(a).min(text.len() as u64));
        let w = win_roles(lang, text, &m, a, b);
        if let Some(d) = first_diff(text, &full, &w, a as usize, b as usize) {
            return Err(format!("{} window {a}..{b}: {d}", lang.name()));
        }
    }
    Ok(())
}

/// Корпус: файлы репозитория с расширениями `ext` (тесты идут из `crates/view`).
pub(crate) fn repo_files(ext: &[&str]) -> Vec<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if p.is_dir() {
                if !matches!(name.as_ref(), "target" | ".git" | "node_modules") {
                    stack.push(p);
                }
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| ext.contains(&x.to_ascii_lowercase().as_str()))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Внешний корпус (`HL_CORPUS`): для ручного прогона на больших наборах.
pub(crate) fn ext_files(ext: &[&str]) -> Vec<std::path::PathBuf> {
    let Ok(root) = std::env::var("HL_CORPUS") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Несколько папок — через `;`.
    let mut stack: Vec<_> = root.split(';').map(std::path::PathBuf::from).collect();
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("target") && name != ".git" {
                    stack.push(p);
                }
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| ext.contains(&x.to_ascii_lowercase().as_str()))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Случаев proptest: по умолчанию `n`, `HL_CASES` — больше (ручной прогон).
pub(crate) fn cases(n: u32) -> u32 {
    std::env::var("HL_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(n)
}
