//! Тесты модели Markdown (S7-22): токенайзер HTML, slug, пути, декодирование, разбор.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

use super::html::{Tok, entities, px, tokenize};
use super::job::{Decoded, cut_point};
use super::resolve::percent_decode;
use super::*;
use crate::enc::Enc;

#[test]
fn tokenizer_tags_attrs_comments() {
    let t = tokenize(
        r#"<p align="center"><IMG SRC=a.png width='120px' alt="a &amp; b"/>x<!-- c -->y</p><br>"#,
    );
    let tags: Vec<_> = t
        .iter()
        .filter_map(|t| match t {
            Tok::Tag(g) => Some((g.name.as_str(), g.close, g.self_close)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tags,
        [
            ("p", false, false),
            ("img", false, true),
            ("p", true, false),
            ("br", false, false)
        ]
    );
    let Tok::Tag(img) = &t[1] else { panic!() };
    assert_eq!(img.attr("src"), Some("a.png"));
    assert_eq!(img.attr("width").and_then(px), Some(120));
    assert_eq!(img.attr("alt"), Some("a & b"));
    assert!(t.contains(&Tok::Comment));
    // Битый тег — текст.
    let t = tokenize("a < b <3 <x");
    assert!(t.iter().all(|t| matches!(t, Tok::Text(..))), "{t:?}");
}

#[test]
fn entities_and_percent() {
    assert_eq!(
        entities("&lt;a&gt; &#x41;&#66; &nbsp;&bogus; &"),
        "<a> AB \u{a0}&bogus; &"
    );
    assert_eq!(percent_decode("My%20Pic%D0%AF.png%2"), "My PicЯ.png%2");
    assert_eq!(px("50%"), None);
    assert_eq!(px(" 64 "), Some(64));
}

#[test]
fn slugs_like_github() {
    let mut seen = FxHashMap::default();
    assert_eq!(slug("Hello, World!", &mut seen), "hello-world");
    assert_eq!(slug("Hello World", &mut seen), "hello-world-1");
    assert_eq!(slug("hello-world", &mut seen), "hello-world-2");
    assert_eq!(slug("Привет, мир — 2", &mut seen), "привет-мир--2");
    assert_eq!(slug("snake_case `code`", &mut seen), "snake_case-code");
}

#[test]
fn cut_at_blank_line() {
    let t = "a\n\nb\nc\n\nd";
    assert_eq!(cut_point(t, 100), t.len());
    assert_eq!(cut_point(t, 8), 8);
    assert_eq!(cut_point(t, 5), 3);
    assert_eq!(cut_point("абв\nгд", 9), 7);
    assert_eq!(cut_point("абвгд", 5), 4);
}

#[test]
fn decoded_maps_to_source_bytes() {
    // UTF-8 с BOM: сдвиг на 3.
    let d = Decoded::new(
        b"\xEF\xBB\xBF# x\n",
        Enc {
            bom_len: 3,
            ..Enc::utf8()
        },
    );
    assert_eq!(d.text, "# x\n");
    assert_eq!(d.to_src(2), 5);
    // 1251: «Я» — 1 байт источника, 2 байта UTF-8.
    let d = Decoded::new(b"\xDF\xDF x", Enc::new(encoding_rs::WINDOWS_1251));
    assert_eq!(d.text, "ЯЯ x");
    assert_eq!(d.to_src(4), 2);
    assert_eq!(d.to_src(5), 3);
    let mut doc = parse(&d.text);
    d.remap(&mut doc);
    assert_eq!(doc.blocks[0].src, 0..4);
    assert_eq!(doc.src_len, 4);
}

fn kinds(doc: &MdDoc) -> Vec<(u8, BlockKind)> {
    doc.blocks
        .iter()
        .map(|b| (b.depth, b.kind.clone()))
        .collect()
}

#[test]
fn blocks_text_and_runs() {
    let src = "# Заголовок *раз*\n\nАбзац **жирный** и `код`  \nперенос.\n\n- [x] готово\n- нет\n  - вложенный\n\n> цитата\n\n---\n\n```rust\nfn main() {}\n```\n";
    let doc = parse(src);
    let item = |mark| BlockKind::Item { mark };
    assert_eq!(
        kinds(&doc),
        [
            (0, BlockKind::Heading(1)),
            (0, BlockKind::Para),
            (0, item(Mark::Task(true))),
            (0, item(Mark::Bullet)),
            (1, item(Mark::Bullet)),
            (0, BlockKind::Quote),
            (1, BlockKind::Para),
            (0, BlockKind::Rule),
            (0, BlockKind::Code { lang: Some(0) }),
        ]
    );
    assert_eq!(doc.code_lang(8), Some("rust"));
    assert_eq!(doc.block_text(0), "Заголовок раз");
    assert_eq!(doc.block_text(1), "Абзац жирный и код\nперенос.");
    assert_eq!(doc.block_text(2), "готово");
    assert_eq!(doc.block_text(8), "fn main() {}");
    let styles: Vec<_> = doc.block_runs(1).iter().map(|r| r.style.0).collect();
    assert_eq!(
        styles,
        [0, RunStyle::BOLD, 0, RunStyle::CODE, 0, 0],
        "{:?}",
        doc.block_runs(1)
    );
    assert_eq!(doc.blocks[4].parent, Some(3));
    assert_eq!(doc.blocks[3].end, 5);
    assert_eq!(doc.anchors[0].slug, "заголовок-раз");
    for (i, b) in doc.blocks.iter().enumerate() {
        assert_eq!(doc.src_to_text(b.src.start), b.text.start, "блок {i}");
    }
}

#[test]
fn table_rows_and_cells() {
    let doc = parse("| a | b\tc | `d` |\n|:-:|--:|---|\n| 1 | 2 | 3 |\n");
    assert_eq!(doc.blocks[0].kind, BlockKind::Table(0));
    assert_eq!(
        doc.table_align(0),
        [Align::Center, Align::Right, Align::Left]
    );
    assert_eq!(doc.blocks[1].kind, BlockKind::Row { head: true });
    assert_eq!(doc.block_text(1), "a\tb c\td");
    assert_eq!(doc.block_text(2), "1\t2\t3");
    assert_eq!(doc.blocks[2].depth, 1);
}

#[test]
fn html_set_details_align_kbd() {
    let src = "<p align=\"center\">\n  <img src=\"logo.png\" width=\"120\" alt=\"Logo\">\n  <br>Текст\n</p>\n\n<details open>\n<summary>Подробнее <kbd>Ctrl</kbd></summary>\n\nВнутри **md**\n\n</details>\n\nH<sub>2</sub>O и x<sup>2</sup> <kbd>F7</kbd> <span>x</span>\n\n<!-- скрыт -->\n\n<script>alert(1)</script>\n";
    let doc = parse(src);
    assert_eq!(
        kinds(&doc),
        [
            (0, BlockKind::Para),
            (0, BlockKind::Details { open: true }),
            (1, BlockKind::Summary),
            (1, BlockKind::Para),
            (0, BlockKind::Para),
            (0, BlockKind::RawHtml),
        ],
        "{doc:#?}"
    );
    assert_eq!(doc.blocks[0].align, Align::Center);
    assert_eq!(doc.block_text(0), "Logo\nТекст");
    let img = &doc.images[0];
    assert_eq!(
        (img.src.as_str(), img.width, img.height),
        ("logo.png", Some(120), None)
    );
    assert_eq!(doc.block_runs(0)[0].image(), Some(0));
    assert_eq!(doc.block_text(2), "Подробнее Ctrl");
    assert!(doc.block_runs(2).iter().any(|r| r.style.has(RunStyle::KBD)));
    assert_eq!(doc.summary_of(1), Some(2));
    assert_eq!(doc.blocks[1].end, 4);
    assert_eq!(doc.block_text(4), "H2O и x2 F7 <span>x</span>");
    let st: Vec<_> = doc
        .block_runs(4)
        .iter()
        .map(|r| (doc.run_text(r), r.style.0))
        .collect();
    assert!(st.contains(&("2", RunStyle::SUB)), "{st:?}");
    assert!(st.contains(&("2", RunStyle::SUP)), "{st:?}");
    assert!(st.contains(&("F7", RunStyle::KBD)), "{st:?}");
    assert!(
        st.contains(&("<span>", RunStyle::CODE | RunStyle::RAW)),
        "{st:?}"
    );
    assert_eq!(doc.block_text(5), "<script>alert(1)</script>");
    // Комментарий скрыт.
    assert!(!doc.text.contains("скрыт"));
}

#[test]
fn div_align_wraps_markdown() {
    let doc = parse("<div align=\"right\">\n\n# Title\n\ntext\n\n</div>\n\nafter\n");
    let al: Vec<_> = doc.blocks.iter().map(|b| b.align).collect();
    assert_eq!(al, [Align::Right, Align::Right, Align::Left]);
}

#[test]
fn details_without_close_and_stray_tags() {
    let doc = parse("<details>\n\ntext\n");
    assert_eq!(doc.blocks[0].kind, BlockKind::Details { open: false });
    assert_eq!(doc.summary_of(0), None);
    assert!(doc.blocks[0].src.end >= doc.blocks[1].src.end);
    let doc = parse("a </kbd> b\n\n</details>\n");
    assert_eq!(doc.block_text(0), "a </kbd> b");
    assert_eq!(doc.block_text(1), "</details>");
}

#[test]
fn links_images_footnotes_anchors() {
    let src = "# Установка\n\n[к разделу](#установка) [сайт](https://x.org) <a@b.c> [док](docs/A%20B.md#part) [ftp](ftp://h/x) [![badge](b.svg)](https://ci)\n\nСноска[^n].\n\n[^n]: Текст сноски.\n";
    let doc = parse(src);
    let t: Vec<_> = doc.links.iter().map(|l| &l.target).collect();
    assert_eq!(
        t[0],
        &LinkTarget::Anchor {
            slug: "установка".into(),
            block: Some(0)
        }
    );
    assert_eq!(t[1], &LinkTarget::External("https://x.org".into()));
    assert_eq!(t[2], &LinkTarget::External("mailto:a@b.c".into()));
    assert_eq!(
        t[3],
        &LinkTarget::Path {
            path: None,
            anchor: Some("part".into()),
            markdown: true
        }
    );
    assert_eq!(t[4], &LinkTarget::Other);
    let badge = doc
        .runs
        .iter()
        .find(|r| r.image().is_some())
        .expect("картинка в ссылке");
    assert_eq!(badge.link(), Some(5));
    let fref = doc
        .runs
        .iter()
        .find(|r| r.style.has(RunStyle::FOOTNOTE))
        .expect("метка сноски");
    let def = doc
        .blocks
        .iter()
        .position(|b| b.kind == BlockKind::FootnoteDef { n: 1 })
        .expect("определение сноски");
    assert_eq!(
        doc.links[fref.link().expect("ссылка") as usize].target,
        LinkTarget::Footnote {
            n: 1,
            block: Some(u32::try_from(def).unwrap())
        }
    );
}

#[test]
fn front_matter_is_yaml_code() {
    let doc = parse("---\ntitle: x\n---\n\n# H\n");
    assert_eq!(doc.code_lang(0), Some("yaml"));
    assert_eq!(doc.block_text(0), "title: x");
}

#[test]
fn single_image_paragraph_is_image_block() {
    let doc = parse("![alt text](pic.png)\n\ntext ![a](b.png)\n");
    assert_eq!(doc.blocks[0].kind, BlockKind::Image(0));
    assert_eq!(doc.block_text(0), "alt text");
    assert_eq!(doc.blocks[1].kind, BlockKind::Para);
}

/// ФС-подмена: считает обращения.
struct CountFs {
    calls: Cell<usize>,
    files: Vec<(PathBuf, FileMeta)>,
}

impl MdFs for CountFs {
    fn meta(&self, p: &Path) -> Option<FileMeta> {
        self.calls.set(self.calls.get() + 1);
        self.files.iter().find(|(f, _)| f == p).map(|(_, m)| *m)
    }
}

#[test]
fn network_images_make_no_fs_or_net_calls() {
    let mut doc = parse(
        "![a](https://x.org/a.png) ![b](http://x/b.png) ![c](//cdn/c.png) <img src=\"\\\\srv\\share\\d.png\"> ![e](file://host/e.png)\n",
    );
    let fs = CountFs {
        calls: Cell::new(0),
        files: Vec::new(),
    };
    let r = Resolver {
        dir: Some(PathBuf::from(r"C:\repo\docs")),
        image_max: 20 << 20,
        fs: &fs,
    };
    resolve(&mut doc, &r);
    assert_eq!(fs.calls.get(), 0, "сеть — без обращений");
    assert!(
        doc.images
            .iter()
            .all(|i| i.state == ImageState::Blocked(ImageIssue::Network)),
        "{:?}",
        doc.images
    );
    assert_eq!(doc.hidden_images(), 5);
}

#[test]
fn local_paths_resolve() {
    let mut doc = parse(
        "![a](img/a%20b.png) ![b](/assets/b.svg) ![c](missing.png) ![d](big.png) ![e](doc.pdf) ![f](C:/x/f.gif) ![g](data:image/png;base64,AA) ![h](data:text/html,x) [l](../README.md) [m](sub/)\n",
    );
    let m = |len| FileMeta { len, dir: false };
    let fs = CountFs {
        calls: Cell::new(0),
        files: vec![
            (PathBuf::from(r"C:\repo\docs\img\a b.png"), m(10)),
            (
                PathBuf::from(r"C:\repo\.git"),
                FileMeta { len: 0, dir: true },
            ),
            (PathBuf::from(r"C:\repo\assets\b.svg"), m(10)),
            (PathBuf::from(r"C:\repo\docs\big.png"), m(30 << 20)),
            (PathBuf::from(r"C:\repo\docs\doc.pdf"), m(1)),
            (PathBuf::from(r"C:\x\f.gif"), m(1)),
        ],
    };
    let r = Resolver {
        dir: Some(PathBuf::from(r"C:\repo\docs")),
        image_max: 20 << 20,
        fs: &fs,
    };
    resolve(&mut doc, &r);
    let st: Vec<_> = doc.images.iter().map(|i| i.state.clone()).collect();
    assert_eq!(
        st,
        [
            ImageState::File(PathBuf::from(r"C:\repo\docs\img\a b.png")),
            ImageState::File(PathBuf::from(r"C:\repo\assets\b.svg")),
            ImageState::Blocked(ImageIssue::NotFound),
            ImageState::Blocked(ImageIssue::TooLarge),
            ImageState::Blocked(ImageIssue::Unsupported),
            ImageState::File(PathBuf::from(r"C:\x\f.gif")),
            ImageState::Data,
            ImageState::Blocked(ImageIssue::Unsupported),
        ]
    );
    assert_eq!(
        doc.links[0].target,
        LinkTarget::Path {
            path: Some(PathBuf::from(r"C:\repo\README.md")),
            anchor: None,
            markdown: true
        }
    );
    // Документ не на локальном диске: относительные — NotLocal.
    let mut doc = parse("![a](a.png)\n");
    let r = Resolver {
        dir: None,
        image_max: 1,
        fs: &fs,
    };
    resolve(&mut doc, &r);
    assert_eq!(
        doc.images[0].state,
        ImageState::Blocked(ImageIssue::NotLocal)
    );
}

#[test]
fn cancel_stops_parse() {
    let t = urfin_core::CancelToken::new();
    t.cancel();
    let big = "w

"
    .repeat(20_000);
    assert_eq!(parse_with(&big, &t), Err(ParseError::Cancelled));
}

#[test]
fn load_sends_head_then_whole() {
    let mut text = String::new();
    while text.len() < (1 << 20) + 10 {
        text.push_str(
            "# H

абзац текста

",
        );
    }
    let src = crate::source::OwnedSource::new(text.clone().into_bytes());
    let fs = CountFs {
        calls: Cell::new(0),
        files: Vec::new(),
    };
    let r = Resolver {
        dir: None,
        image_max: 1,
        fs: &fs,
    };
    let mut got = Vec::new();
    job::load(
        &src,
        Enc::utf8(),
        &r,
        &urfin_core::CancelToken::new(),
        |d| {
            got.push((d.complete, d.src_len));
        },
    )
    .unwrap();
    assert_eq!(got.len(), 2);
    assert!(!got[0].0 && got[0].1 <= FIRST_CHUNK as u64);
    assert_eq!(got[1], (true, text.len() as u64));
}

#[test]
fn compact_structs() {
    // Бюджет памяти ≤ 3× текста (S7 §3.10): блоки и отрезки — компактные.
    assert!(std::mem::size_of::<Run>() <= 24);
    assert!(std::mem::size_of::<Block>() <= 64);
}
