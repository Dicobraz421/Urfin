//! Начала и концы HTML-блока трекера = `CommonMark` (эталон — pulldown-cmark, им же
//! разбирается документ).

use pulldown_cmark::{Event, Parser, Tag};

use super::{html_closes, html_kind};

/// Начала: типы 1–5, почти они и типы 6–7.
pub(crate) const STARTS: &[&str] = &[
    "<!--",
    "<!-- c",
    "<!-- c -->",
    "<!-->",
    "<!--->",
    "<!-",
    "<?",
    "<?>",
    "<?x",
    "<? x ?>",
    "<!X",
    "<!X>",
    "<!x y>",
    "<!",
    "<!>",
    "<!1",
    "<![CDATA[",
    "<![CDATA[x",
    "<![CDATA[x]]>",
    "<![CDATA[]]>",
    "<![cdata[",
    "<![CDATA",
    "<script",
    "<script>",
    "<script a=1>",
    "<SCRIPT>",
    "<script>x</script>",
    "<script/>",
    "<scripts>",
    "<pre",
    "<pre>",
    "<PRE>",
    "<pre>x</pre>",
    "<pre/>",
    "<prex",
    "<style",
    "<style>",
    "<style>x</style>",
    "<textarea",
    "<textarea>",
    "<textarea>x</textarea>",
    "<div",
    "<div>",
    "</div>",
    "<a>",
    "</a>",
    "</script>",
    "</PRE>",
    "</style>",
    "</textarea>",
    "<a",
    "<",
];

/// Открывает ли строка HTML-блок и закрыт ли он на ней же.
fn cm(line: &str) -> Option<bool> {
    let doc = format!("{line}\nb\n");
    let ev: Vec<_> = Parser::new(&doc).collect();
    matches!(ev.first(), Some(Event::Start(Tag::HtmlBlock)))
        .then(|| ev.iter().any(|e| matches!(e, Event::Start(Tag::Paragraph))))
}

#[test]
fn html_kind_and_close_as_commonmark() {
    for s in STARTS {
        let k = html_kind(s.as_bytes());
        let want = cm(s);
        assert_eq!(k != 0, want.is_some(), "{s:?}: тип {k}");
        if let (1..=5, Some(closed)) = (k, want) {
            assert_eq!(html_closes(k, s.as_bytes()), closed, "{s:?}: тип {k}");
        }
    }
}

/// Концы типа 1 — любой из четырёх закрывающих тегов без учёта регистра (спецификация и
/// cmark; pulldown-cmark ждёт только свой тег, tree-sitter не знает `textarea`).
#[test]
fn html_type1_ends_any_tag() {
    for e in ["</script>", "</pre>", "</style>", "</textarea>", "x</PRE>y"] {
        assert!(html_closes(1, e.as_bytes()), "{e}");
    }
    assert!(!html_closes(1, b"</scripts>"));
    assert!(!html_closes(1, b"<pre>"));
}

/// Конец типов 2–5 — на самой строке начала, с перекрытием начала (`<!-->`, `<?>`).
#[test]
fn html_end_overlaps_start() {
    for (k, s) in [
        (2, "<!-->"),
        (2, "<!--->"),
        (3, "<?>"),
        (4, "<!X>"),
        (5, "<![CDATA[]]>"),
    ] {
        assert_eq!(html_kind(s.as_bytes()), k, "{s}");
        assert!(html_closes(k, s.as_bytes()), "{s}");
    }
}
