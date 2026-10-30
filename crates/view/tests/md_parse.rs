//! Ребро view → pulldown-cmark (S7-00b) и модель документа S7-22 (S7 §3.10): корпус
//! `CommonMark` + GFM и README с HTML — блоки и смещения; proptest — `src` блоков по порядку
//! и внутри файла, проекция ↔ исходник обратимы на границах блоков; тяжёлый замер
//! разбора 1 и 16 МБ (`#[ignore]`, release):
//! `cargo nextest run -p urfin-view --release --test md_parse --run-ignored only --no-capture`.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "тесты: доли и замеры"
)]

use std::fmt::Write as _;
use std::time::Instant;

use proptest::prelude::*;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use urfin_core::CancelToken;
use urfin_view::Enc;
use urfin_view::md::job::{Decoded, load};
use urfin_view::md::{
    BlockKind, ImageIssue, ImageState, MdDoc, RealFs, Resolver, RunStyle, parse, parse_with,
};
use urfin_view::source::OwnedSource;

#[test]
fn gfm_events_carry_source_offsets() {
    let src = "# Урфин\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] ~~готово~~\n";
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES;
    let events: Vec<_> = Parser::new_ext(src, opts).into_offset_iter().collect();

    let heading = events
        .iter()
        .find(|(e, _)| matches!(e, Event::Start(Tag::Heading { .. })))
        .expect("заголовок");
    assert_eq!(&src[heading.1.clone()], "# Урфин\n");
    let has = |f: fn(&Event) -> bool| events.iter().any(|(e, _)| f(e));
    assert!(has(|e| matches!(e, Event::End(TagEnd::Table))));
    assert!(has(|e| matches!(e, Event::TaskListMarker(true))));
    assert!(has(|e| matches!(e, Event::Start(Tag::Strikethrough))));
}

/// Блоки документа строками: отступ по глубине, вид, смещения, текст и стили.
fn dump(doc: &MdDoc, src: &str) -> String {
    let mut out = String::new();
    for (i, b) in doc.blocks.iter().enumerate() {
        let pad = "  ".repeat(b.depth as usize);
        let s = &src[b.src.start as usize..b.src.end as usize];
        let head: String = s.chars().take(24).collect();
        let n = i as u32;
        let kind = match &b.kind {
            BlockKind::Code { .. } => format!("Code({:?})", doc.code_lang(n)),
            BlockKind::Table(_) => format!("Table({:?})", doc.table_align(n)),
            k => format!("{k:?}"),
        };
        let _ = write!(
            out,
            "{i:>2} {pad}{kind} src={}..{} {:?}",
            b.src.start, b.src.end, head
        );
        if b.align != urfin_view::md::Align::Left {
            let _ = write!(out, " align={:?}", b.align);
        }
        let _ = writeln!(out);
        for r in doc.block_runs(i as u32) {
            let t = doc.run_text(r);
            let _ = write!(out, "   {pad}| {t:?}");
            if r.style != RunStyle::default() {
                let _ = write!(out, " style={:#x}", r.style.0);
            }
            if let Some(l) = r.link() {
                let _ = write!(out, " link={:?}", doc.links[l as usize].target);
            }
            if let Some(m) = r.image() {
                let im = &doc.images[m as usize];
                let _ = write!(out, " image={:?} {:?}x{:?}", im.src, im.width, im.height);
            }
            let _ = writeln!(out, " @{}", r.src);
        }
    }
    out
}

/// Инварианты модели: порядок и вложенность блоков, покрытие текста отрезками,
/// обратимость на границах блоков.
fn check(doc: &MdDoc, len: u64) {
    let n = doc.blocks.len();
    let mut prev = 0;
    let mut prev_text = 0;
    for (i, b) in doc.blocks.iter().enumerate() {
        assert!(b.src.start <= b.src.end && b.src.end <= len, "{i}: {b:?}");
        assert!(b.src.start >= prev, "{i}: порядок {prev} > {:?}", b.src);
        prev = b.src.start;
        assert!(b.text.start >= prev_text && b.text.end as usize <= doc.text.len());
        prev_text = b.text.start;
        assert!(
            b.end as usize > i && b.end as usize <= n,
            "{i}: end {}",
            b.end
        );
        if let Some(p) = b.parent {
            let pb = &doc.blocks[p as usize];
            assert!((p as usize) < i && i < pb.end as usize, "{i}: родитель {p}");
            assert!(
                pb.src.start <= b.src.start && b.src.end <= pb.src.end,
                "{i}: {:?} вне родителя {:?}",
                b.src,
                pb.src
            );
            assert_eq!(b.depth, pb.depth + 1);
        } else {
            assert_eq!(b.depth, 0);
        }
        let runs = doc.block_runs(i as u32);
        let mut at = b.text.start;
        for r in runs {
            assert_eq!(r.text.start, at, "{i}: дыра в отрезках");
            at = r.text.end;
        }
        assert_eq!(at, b.text.end, "{i}: отрезки не покрывают текст");
        // Граница блока: исходник → проекция → исходник.
        assert_eq!(doc.src_to_text(b.src.start), b.text.start, "{i}: {b:?}");
        if !b.text.is_empty() {
            assert_eq!(doc.text_to_src(b.text.start), b.src.start, "{i}");
            assert_eq!(doc.block_at_text(b.text.start), Some(i as u32));
        }
    }
}

const README: &str = r#"---
title: Урфин
---

<p align="center">
  <img src="docs/logo.png" width="120" alt="Урфин">
  <br>
  <b>Двухпанельный</b> файловый менеджер
</p>

<div align="center">

[![CI](https://github.com/x/y/actions/workflows/ci.yml/badge.svg)](https://github.com/x/y/actions)
![Скриншот](docs/screen.png)

</div>

# Возможности

* Две панели, **Far**-клавиши: <kbd>F5</kbd> копия, <kbd>Ctrl</kbd>+<kbd>R</kbd>
* Формулы H<sub>2</sub>O и E=mc<sup>2</sup>
* <span>неизвестный</span> тег и <script>alert(1)</script>

<details>
<summary>Сборка из исходников</summary>

```powershell
cargo build --release
```

1. Установите Rust
2. Запустите `cargo run`
   - вложенный пункт

</details>

<details open><summary>Раскрыто сразу</summary>Текст внутри</details>

<!-- скрытый комментарий -->

| Клавиша | Действие |
|:--------|---------:|
| F3      | Просмотр |
| F4      | Правка   |

> **Примечание.** Цитата
> > вложенная

Сноска[^1] и [ссылка на раздел](#возможности).

[^1]: Текст сноски.

- [ ] задача
- [x] сделано

***

    код с отступом
"#;

const GFM: &str = "Setext\n======\n\nВторой\n------\n\nабзац  \nс жёстким\\\nпереносом и мягким\nпереносом.\n\n*курсив* _тоже_ **жирный** ~~зачёркнутый~~ `код` [ref][r] <https://auto.link> www.not-auto.org\n\n[r]: https://example.com \"Заголовок\"\n\n3. три\n4. четыре\n\n10) десять\n\n- a\n\n  второй абзац пункта\n- b\n\n> - в цитате\n>   1. глубже\n\n~~~\nfenced ~~~\n~~~\n\n| a |\n|---|\n| `x\\|y` |\n\n<table><tr><td>HTML</td></tr></table>\n\n\\# не заголовок &amp; &copy;\n";

#[test]
fn readme_with_html_blocks_and_offsets() {
    let doc = parse(README);
    check(&doc, README.len() as u64);
    insta::assert_snapshot!("readme_html", dump(&doc, README));
}

#[test]
fn commonmark_gfm_corpus() {
    let doc = parse(GFM);
    check(&doc, GFM.len() as u64);
    insta::assert_snapshot!("gfm_corpus", dump(&doc, GFM));
}

/// Куски Markdown для случайных документов.
const PIECES: &[&str] = &[
    "# Заголовок",
    "## H2 *em*",
    "Абзац **жирный** и `код`.",
    "текст  \nперенос",
    "- пункт\n- [x] задача\n  - вложенный",
    "1. раз\n2. два",
    "> цитата\n> > глубже",
    "| a | b |\n|:-:|--:|\n| 1 | 2 |",
    "```rust\nfn x() {}\n```",
    "    отступ",
    "---",
    "<p align=\"center\"><img src=\"a.png\" width=\"10\"><br>текст</p>",
    "<details>\n<summary>S</summary>",
    "</details>",
    "<details open>",
    "<div align=\"right\">",
    "</div>",
    "x <kbd>K</kbd> H<sub>2</sub> x<sup>2</sup> <span>y</span>",
    "<!-- комментарий -->",
    "<script>bad()</script>",
    "Сноска[^a].",
    "[^a]: определение",
    "[ссылка](#заголовок) ![картинка](https://x/y.png)",
    "[![b](b.svg)](https://ci)",
    "ёжик \t таб",
    "</kbd> </p>",
    "&amp; &#x41; \\*",
];

fn doc_from(parts: &[(usize, bool)], tail: &str) -> String {
    let mut s = String::new();
    for (i, blank) in parts {
        s.push_str(PIECES[i % PIECES.len()]);
        s.push_str(if *blank { "\n\n" } else { "\n" });
    }
    s.push_str(tail);
    s
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn blocks_ordered_inside_and_reversible(
        parts in prop::collection::vec((0usize..64, any::<bool>()), 0..40),
        tail in "[ -~а-я\n<>*_`#|\\[\\]()!-]{0,80}",
    ) {
        let src = doc_from(&parts, &tail);
        let doc = parse(&src);
        check(&doc, src.len() as u64);
    }
}

#[test]
fn network_images_resolve_without_network() {
    let src = "![a](https://example.com/a.png)\n\n<img src=\"http://example.com/b.png\">\n";
    let mut doc = parse(src);
    let r = Resolver {
        dir: Some(std::env::temp_dir()),
        image_max: 20 << 20,
        fs: &RealFs,
    };
    urfin_view::md::resolve(&mut doc, &r);
    assert!(
        doc.images
            .iter()
            .all(|i| i.state == ImageState::Blocked(ImageIssue::Network))
    );
}

/// Плотный синтетический документ ~`len` байт: стиль каждые ~20 байт, блок каждые ~40
/// (худший случай для памяти; бюджет не проверяется — только печать).
fn dense(len: usize) -> String {
    let mut s = String::with_capacity(len + 4096);
    let mut i = 0u32;
    while s.len() < len {
        let _ = write!(
            s,
            "## Раздел {i}\n\nАбзац **{i}** с *курсивом*, `кодом` и [ссылкой](#раздел-{i}). \
             Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor.\n\
             Вторая строка абзаца с ~~зачёркнутым~~ текстом и <kbd>Ctrl</kbd>.\n\n\
             - пункт {i}\n- [x] задача\n  - вложенный\n\n\
             | a | b |\n|---|--:|\n| {i} | {} |\n\n\
             ```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n\n> цитата {i}\n\n",
            i * 2
        );
        i += 1;
    }
    s
}

/// Документ ~`len` байт из корпуса README + GFM и прозы (как README и CHANGELOG).
fn corpus(len: usize) -> String {
    const PROSE: &str = "Урфин показывает Markdown как документ: заголовки, списки, таблицы и \
        картинки из локальных файлов. Разбор идёт в фоне, первый экран большого файла строится \
        из начала, остальное дописывается. Поиск F7 работает по видимому тексту, а переход к \
        строке и Hex-поиск — через режим «Текст».\n\n";
    let mut s = String::with_capacity(len + 4096);
    while s.len() < len {
        s.push_str(README);
        s.push_str("\n\n");
        s.push_str(GFM);
        for _ in 0..6 {
            s.push_str(PROSE);
        }
    }
    s
}

/// Лучшее из трёх (после прогрева) декодирование + разбор, мс; документ.
fn measure(text: &str) -> (f64, MdDoc) {
    let cancel = CancelToken::new();
    let mut best = f64::MAX;
    let mut doc = MdDoc::default();
    for _ in 0..4 {
        let t = Instant::now();
        let dec = Decoded::new(text.as_bytes(), Enc::utf8());
        doc = parse_with(&dec.text, &cancel).expect("разбор");
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    (best, doc)
}

/// S7 §3.10: разбор 1 МБ ≤ 20 мс, 16 МБ ≤ 300 мс (release, warm), память ≤ 3× текста.
#[test]
#[ignore = "тяжёлый: 1 и 16 МБ, release"]
fn parse_1mb_and_16mb_budget() {
    for (mb, budget_ms) in [(1usize, 20.0f64), (16, 300.0)] {
        let text = corpus(mb << 20);
        let (best, doc) = measure(&text);
        let mem = doc.heap_bytes() as f64 / text.len() as f64;
        // Первый экран большого файла: начало 256 КиБ.
        let src = OwnedSource::new(text.as_bytes().to_vec());
        let r = Resolver {
            dir: None,
            image_max: 20 << 20,
            fs: &RealFs,
        };
        let t = Instant::now();
        let mut first = None;
        load(&src, Enc::utf8(), &r, &CancelToken::new(), |d| {
            if first.is_none() {
                first = Some((t.elapsed().as_secs_f64() * 1000.0, d.complete));
            }
        })
        .expect("load");
        let (dense_ms, dense_doc) = measure(&dense(mb << 20));
        println!(
            "md {mb} МБ: разбор {best:.1} мс, блоков {}, отрезков {}, память {mem:.2}× текста, \
             первый экран {:?}; плотный: {dense_ms:.1} мс, память {:.2}×",
            doc.blocks.len(),
            doc.runs.len(),
            first,
            dense_doc.heap_bytes() as f64 / (mb << 20) as f64,
        );
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b.kind, BlockKind::Table(_)))
        );
        if !cfg!(debug_assertions) {
            assert!(best <= budget_ms, "{mb} МБ: {best:.1} мс > {budget_ms} мс");
            assert!(mem <= 3.0, "{mb} МБ: память {mem:.2}× текста");
        }
    }
}
