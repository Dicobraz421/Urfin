//! Приёмка S7-21: грамматики в exe и их запросы, роли по макетам, отмена, кодировки.

use std::sync::Arc;

use super::job::load;
use super::*;
use crate::enc::Enc;
use crate::source::{ByteSource, SourceKind};

/// Роли по тексту: `(фрагмент, роль)` для каждого отрезка.
fn roles(lang: &str, text: &str) -> Vec<(String, Tag)> {
    let g = grammar(lang).expect("grammar");
    highlight(&g, text, &|| false)
        .expect("highlight")
        .iter()
        .map(|s| (text[s.start as usize..s.end as usize].to_owned(), s.tag))
        .collect()
}

/// Отрезок с этим текстом (без перевода строки в конце: узел строки его включает).
fn has(list: &[(String, Tag)], frag: &str, tag: Tag) -> bool {
    list.iter()
        .any(|(f, t)| f.trim_end_matches('\n') == frag && *t == tag)
}

#[test]
fn capture_names_to_roles() {
    assert_eq!(tag_of("keyword"), Some(Tag::Keyword));
    assert_eq!(tag_of("keyword.function"), Some(Tag::Keyword));
    assert_eq!(tag_of("type.builtin"), Some(Tag::Keyword));
    assert_eq!(tag_of("function.method"), Some(Tag::Keyword));
    assert_eq!(tag_of("string.special.key"), Some(Tag::String));
    assert_eq!(tag_of("escape"), Some(Tag::String));
    assert_eq!(tag_of("boolean"), Some(Tag::Number));
    assert_eq!(tag_of("constant.builtin"), Some(Tag::Number));
    assert_eq!(tag_of("constant"), Some(Tag::Plain));
    assert_eq!(tag_of("comment.documentation"), Some(Tag::Comment));
    assert_eq!(tag_of("markup.heading.1"), Some(Tag::Keyword));
    assert_eq!(tag_of("property"), Some(Tag::Plain));
    assert_eq!(tag_of("none"), Some(Tag::Plain));
    assert_eq!(tag_of("_name"), None);
    assert_eq!(tag_of("spell"), None);
}

#[test]
fn builtin_grammars_load() {
    for name in builtin::NAMES {
        assert!(available(name), "{name}");
        let g = grammar(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(g.name, *name);
    }
    assert!(grammar("klingon").is_none());
}

/// `screen-quickview.png`: заголовок таблицы — keyword, ключ — как текст, строка —
/// string, `false` — number, комментарий — comment.
#[test]
fn toml_as_on_mockup() {
    let r = roles(
        "toml",
        "[workspace]\nresolver = \"3\"\n\n[profile.dev.package.\"*\"]\ndebug = false\nlto = 1 # UI\n[[bin]]\n",
    );
    assert!(has(&r, "[workspace]", Tag::Keyword), "{r:?}");
    assert!(has(&r, "\"3\"", Tag::String), "{r:?}");
    assert!(
        has(&r, "[profile.dev.package.\"*\"]", Tag::Keyword),
        "{r:?}"
    );
    assert!(has(&r, "false", Tag::Number), "{r:?}");
    assert!(has(&r, "1", Tag::Number), "{r:?}");
    assert!(has(&r, "# UI", Tag::Comment), "{r:?}");
    assert!(has(&r, "[[bin]]", Tag::Keyword), "{r:?}");
    assert!(
        !r.iter()
            .any(|(f, _)| f.contains("resolver") || f.contains("debug"))
    );
}

#[test]
fn json_keys_plain_values_colored() {
    let r = roles(
        "json",
        "{\"name\": \"urfin\", \"n\": 42, \"ok\": true, \"x\": null}",
    );
    assert!(has(&r, "\"urfin\"", Tag::String), "{r:?}");
    assert!(has(&r, "42", Tag::Number), "{r:?}");
    assert!(has(&r, "true", Tag::Number), "{r:?}");
    assert!(has(&r, "null", Tag::Number), "{r:?}");
    assert!(!r.iter().any(|(f, _)| f.contains("name")), "{r:?}");
}

#[test]
fn yaml_ini_xml() {
    let r = roles("yaml", "# c\nname: urfin\nport: 8080\nlist:\n  - \"a\"\n");
    assert!(has(&r, "# c", Tag::Comment), "{r:?}");
    assert!(has(&r, "name", Tag::Keyword), "{r:?}");
    assert!(has(&r, "urfin", Tag::String), "{r:?}");
    assert!(has(&r, "8080", Tag::Number), "{r:?}");
    let r = roles("ini", "; c\n[core]\nkey = value\n");
    assert!(has(&r, "; c", Tag::Comment), "{r:?}");
    assert!(
        r.iter()
            .any(|(f, t)| f.contains("core") && *t == Tag::Keyword),
        "{r:?}"
    );
    let r = roles(
        "xml",
        "<?xml version=\"1.0\"?>\n<a x=\"1\"><!-- c --><b/></a>\n",
    );
    assert!(has(&r, "<!-- c -->", Tag::Comment), "{r:?}");
    assert!(has(&r, "\"1\"", Tag::String), "{r:?}");
    assert!(
        r.iter()
            .any(|(f, t)| f.starts_with("<a") && *t == Tag::Keyword),
        "{r:?}"
    );
}

/// Markdown-исходник: заголовок и маркеры — keyword, код (блок и строчный) — string.
#[test]
fn markdown_blocks_and_inline() {
    let r = roles(
        "markdown",
        "# Title\n\nText with `code` and [link](x.md).\n\n- item\n\n```toml\na = 1\n```\n",
    );
    assert!(has(&r, "# Title", Tag::Keyword), "{r:?}");
    assert!(has(&r, "`code`", Tag::String), "{r:?}");
    assert!(has(&r, "link", Tag::Keyword), "{r:?}");
    assert!(has(&r, "x.md", Tag::String), "{r:?}");
    assert!(
        r.iter()
            .any(|(f, t)| f.starts_with('-') && *t == Tag::Keyword),
        "{r:?}"
    );
    assert!(has(&r, "a = 1", Tag::String), "{r:?}");
}

#[test]
fn spans_sorted_disjoint_and_window() {
    let g = grammar("json").unwrap();
    let text = "[1, \"a\", 2, \"b\", true]";
    let s = highlight(&g, text, &|| false).unwrap();
    assert!(s.windows(2).all(|w| w[0].end <= w[1].start));
    let w = spans_in(&s, 4, 10);
    assert_ne!(w, []);
    assert!(
        w.iter()
            .all(|x| u64::from(x.end) > 4 && u64::from(x.start) < 10)
    );
    assert_eq!(spans_in(&s, 100, 200), []);
}

#[test]
fn cancel_stops_parse() {
    let g = grammar("json").unwrap();
    let text = "[1,2,3]".repeat(10_000);
    assert_eq!(highlight(&g, &text, &|| true), Err(Cancelled));
}

/// Пакеты: имя только из `[a-z0-9_]`, папки нет — пакета нет.
#[test]
fn packs_need_valid_names() {
    assert!(!pack::has("../evil"));
    assert!(!pack::has("Rust"));
    assert!(!pack::has(""));
    assert!(pack::load("no_such_lang").is_none());
}

#[derive(Debug)]
struct Mem(Vec<u8>);

impl ByteSource for Mem {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn is_final(&self) -> bool {
        true
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let at = (off as usize).min(self.0.len());
        let n = buf.len().min(self.0.len() - at);
        buf[..n].copy_from_slice(&self.0[at..at + n]);
        Ok(n)
    }
    fn mapped(&self) -> Option<&[u8]> {
        Some(&self.0)
    }
    fn kind(&self) -> SourceKind {
        SourceKind::Owned
    }
    fn refresh_len(&self) -> std::io::Result<u64> {
        Ok(self.len())
    }
}

/// Отрезки — по байтам источника: windows-1251 (кириллица — 1 байт) и UTF-8 с BOM.
#[test]
fn spans_map_to_source_bytes() {
    let (cp, _, _) = encoding_rs::WINDOWS_1251.encode("a = \"Привет\" # ок\n");
    let src = Mem(cp.into_owned());
    let mut got = Vec::new();
    load(
        &src,
        Enc::new(encoding_rs::WINDOWS_1251),
        "toml",
        &urfin_core::CancelToken::default(),
        |s, done| got.push((s, done)),
    )
    .unwrap();
    let (spans, done) = got.pop().unwrap();
    assert!(done);
    let s: Vec<(u32, u32, Tag)> = spans.iter().map(|s| (s.start, s.end, s.tag)).collect();
    // `"Привет"` — байты 4..12, `# ок` — 13..17.
    assert!(s.contains(&(4, 12, Tag::String)), "{s:?}");
    assert!(s.contains(&(13, 17, Tag::Comment)), "{s:?}");

    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice("x = \"ё\"\n".as_bytes());
    let src = Mem(bom);
    let mut enc = Enc::utf8();
    enc.bom_len = 3;
    let mut last: Option<Arc<[Span]>> = None;
    load(
        &src,
        enc,
        "toml",
        &urfin_core::CancelToken::default(),
        |s, _| {
            last = Some(s);
        },
    )
    .unwrap();
    let s = last.unwrap();
    assert!(
        s.iter()
            .any(|x| (x.start, x.end, x.tag) == (7, 11, Tag::String)),
        "{s:?}"
    );
}

/// Блок кода Markdown-документа: подсветка по языку строки ```` ``` ```` (S7-21),
/// неизвестный язык — без неё.
#[test]
fn markdown_code_blocks_by_fence() {
    let mut doc = crate::md::parse("# T\n\n```toml\na = \"x\"\n```\n\n```text\nb = 1\n```\n");
    crate::md::job::highlight_code(&mut doc, &urfin_core::CancelToken::new()).unwrap();
    let got: Vec<(&str, Tag)> = doc
        .code_hl
        .iter()
        .map(|s| (&doc.text[s.start as usize..s.end as usize], s.tag))
        .collect();
    assert_eq!(got, vec![("\"x\"", Tag::String)]);
}
