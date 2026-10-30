//! Разбор: события pulldown-cmark (`into_offset_iter`) → [`MdDoc`]. Смещения — в разобранной
//! строке; перевод в байты исходника (BOM, кодировка) — [`super::job`].

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use urfin_core::CancelToken;

use super::html::{self, HtmlTag, Tok};
use super::{
    Align, Anchor, Block, BlockKind, ImageRef, ImageState, LinkRef, LinkTarget, Mark, MdDoc, Run,
    RunStyle, slug,
};

/// Разбор отменён токеном.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("markdown parse cancelled")]
    Cancelled,
}

/// Расширения: GFM (таблицы, задачи, зачёркнутый, сноски) и front matter.
pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

/// Разбор целиком (без отмены). Пути картинок и ссылок — `Pending` / `None` до
/// [`super::resolve`].
pub fn parse(text: &str) -> MdDoc {
    parse_with(text, &CancelToken::new()).unwrap_or_default()
}

/// Разбор с отменой: токен проверяется каждые 4096 событий.
pub fn parse_with(text: &str, cancel: &CancelToken) -> Result<MdDoc, ParseError> {
    let mut b = Builder::new(text);
    for (n, (ev, r)) in Parser::new_ext(text, options())
        .into_offset_iter()
        .enumerate()
    {
        if n % 4096 == 4095 && cancel.is_cancelled() {
            return Err(ParseError::Cancelled);
        }
        b.event(ev, r);
    }
    Ok(b.finish())
}

/// Открытый контейнер.
#[derive(Debug)]
struct Open {
    block: u32,
    /// Открыт тегом `<details>` (закрывается `</details>` или концом родителя).
    html: bool,
    /// `Item` без своего текста и детей: первый абзац — его текст.
    fresh: bool,
}

/// Лист, принимающий строчное содержимое.
#[derive(Copy, Clone, Debug)]
struct Leaf {
    block: u32,
    /// Без `Start` события (текст тесного пункта, текст после вложенного блока): конец —
    /// по последнему событию, закрывается первым блочным событием.
    implicit: bool,
    /// Из HTML-блока: закрывается тегом или концом блока.
    html: bool,
    code: bool,
}

/// `align=` у `<p>` / `<div>`: действует на блоки до закрывающего тега.
#[derive(Debug)]
struct Scope {
    align: Align,
    tag: &'static str,
    /// Глубина контейнеров при открытии: конец контейнера снимает внутренние.
    level: usize,
}

/// Счётчики стилей (вложенность).
#[derive(Debug, Default)]
struct Styles {
    bold: u16,
    italic: u16,
    strike: u16,
    sub: u16,
    sup: u16,
    /// HTML: `<kbd>`, `<sub>`, `<sup>` — сбрасываются в конце листа.
    kbd: u16,
    hsub: u16,
    hsup: u16,
}

impl Styles {
    fn style(&self) -> RunStyle {
        let mut s = 0;
        let mut on = |n: u16, f: u16| {
            if n > 0 {
                s |= f;
            }
        };
        on(self.bold, RunStyle::BOLD);
        on(self.italic, RunStyle::ITALIC);
        on(self.strike, RunStyle::STRIKE);
        on(self.sub + self.hsub, RunStyle::SUB);
        on(self.sup + self.hsup, RunStyle::SUP);
        on(self.kbd, RunStyle::KBD);
        RunStyle(s)
    }
}

/// Картинка, чей alt собирается.
#[derive(Debug)]
struct ImgFrame {
    idx: u32,
    alt: String,
    at: usize,
}

/// HTML-блок, собранный из событий `Html`.
#[derive(Debug, Default)]
struct HtmlAcc {
    text: String,
    /// (смещение в `text`, смещение в исходнике).
    segs: Vec<(usize, usize)>,
    range: Range<usize>,
}

impl HtmlAcc {
    fn src(&self, o: usize) -> usize {
        let i = self.segs.partition_point(|s| s.0 <= o);
        if i == 0 {
            return self.range.start;
        }
        let (a, s) = self.segs[i - 1];
        (s + (o - a)).min(self.range.end)
    }
}

struct Builder<'s> {
    src: &'s str,
    doc: MdDoc,
    open: Vec<Open>,
    leaf: Option<Leaf>,
    lists: Vec<Option<u64>>,
    styles: Styles,
    links: Vec<u32>,
    image: Option<ImgFrame>,
    image_depth: u32,
    /// Номер ячейки в строке таблицы.
    cell: Option<u32>,
    scopes: Vec<Scope>,
    html: Option<HtmlAcc>,
    footnotes: FxHashMap<String, u32>,
    slugs: FxHashMap<String, u32>,
    /// Конец последнего строчного события (конец неявного листа).
    last_end: usize,
}

impl<'s> Builder<'s> {
    fn new(src: &'s str) -> Builder<'s> {
        let mut doc = MdDoc::default();
        doc.text.reserve(src.len() + src.len() / 16);
        Builder {
            src,
            doc,
            open: Vec::new(),
            leaf: None,
            lists: Vec::new(),
            styles: Styles::default(),
            links: Vec::new(),
            image: None,
            image_depth: 0,
            cell: None,
            scopes: Vec::new(),
            html: None,
            footnotes: FxHashMap::default(),
            slugs: FxHashMap::default(),
            last_end: 0,
        }
    }

    fn proj_len(&self) -> u32 {
        u32::try_from(self.doc.text.len()).unwrap_or(u32::MAX)
    }

    fn runs_len(&self) -> u32 {
        u32::try_from(self.doc.runs.len()).unwrap_or(u32::MAX)
    }

    // ------------------------------------------------------------ блоки

    fn new_block(&mut self, kind: BlockKind, src: Range<usize>) -> u32 {
        let idx = u32::try_from(self.doc.blocks.len()).unwrap_or(u32::MAX);
        let parent = self.open.last_mut().map(|o| {
            o.fresh = false;
            o.block
        });
        let p = self.proj_len();
        let r = self.runs_len();
        self.doc.blocks.push(Block {
            kind,
            src: src.start as u64..src.end as u64,
            text: p..p,
            runs: r..r,
            parent,
            depth: u8::try_from(self.open.len()).unwrap_or(u8::MAX),
            end: idx + 1,
            align: self.scopes.last().map_or(Align::Left, |s| s.align),
        });
        idx
    }

    fn open_container(&mut self, kind: BlockKind, src: Range<usize>, html: bool) {
        let fresh = matches!(kind, BlockKind::Item { .. });
        let block = self.new_block(kind, src);
        self.open.push(Open { block, html, fresh });
    }

    /// Закрывает верхний контейнер; `end` — конец в исходнике (у `<details>`).
    fn close_container(&mut self, end: Option<usize>) {
        let Some(o) = self.open.pop() else {
            return;
        };
        let n = u32::try_from(self.doc.blocks.len()).unwrap_or(u32::MAX);
        // Дети — внутри родителя (у `<details>` без `</details>` конец — по детям).
        let kids = self.doc.blocks[o.block as usize + 1..]
            .iter()
            .map(|c| c.src.end)
            .max()
            .unwrap_or(0);
        let b = &mut self.doc.blocks[o.block as usize];
        b.end = n;
        if let Some(e) = end {
            b.src.end = (e as u64).max(b.src.start);
        }
        b.src.end = b.src.end.max(kids);
        let level = self.open.len();
        self.scopes.retain(|s| s.level <= level);
    }

    fn start_leaf(&mut self, kind: BlockKind, src: Range<usize>, html: bool) -> u32 {
        let code = matches!(kind, BlockKind::Code { .. });
        let block = self.new_block(kind, src);
        self.leaf = Some(Leaf {
            block,
            implicit: false,
            html,
            code,
        });
        block
    }

    /// Строчное содержимое без открытого листа: текст свежего пункта или неявный абзац.
    fn ensure_leaf(&mut self, at: usize) {
        if self.leaf.is_some() {
            return;
        }
        if let Some(o) = self.open.last_mut()
            && o.fresh
        {
            o.fresh = false;
            self.leaf = Some(Leaf {
                block: o.block,
                implicit: true,
                html: false,
                code: false,
            });
            return;
        }
        let block = self.new_block(BlockKind::Para, at..at);
        self.leaf = Some(Leaf {
            block,
            implicit: true,
            html: false,
            code: false,
        });
    }

    /// Блочное событие: неявный лист закрывается.
    fn block_start(&mut self) {
        if self.leaf.is_some_and(|l| l.implicit || l.html) {
            self.finish_leaf();
        }
    }

    fn finish_leaf(&mut self) {
        let Some(leaf) = self.leaf.take() else {
            return;
        };
        self.styles.kbd = 0;
        self.styles.hsub = 0;
        self.styles.hsup = 0;
        self.cell = None;
        let bi = leaf.block as usize;
        let start = self.doc.blocks[bi].text.start as usize;
        // Хвост: у кода — переводы строк, у прочих — пробелы и переводы.
        let keep = |c: char| {
            if leaf.code {
                c != '\n'
            } else {
                !c.is_ascii_whitespace()
            }
        };
        let trimmed = self.doc.text[start..].trim_end_matches(|c| !keep(c)).len() + start;
        self.doc.text.truncate(trimmed);
        let t = self.proj_len();
        let r0 = self.doc.blocks[bi].runs.start as usize;
        self.clip_runs(r0);
        let r = self.runs_len();
        let item = matches!(self.doc.blocks[bi].kind, BlockKind::Item { .. });
        {
            let b = &mut self.doc.blocks[bi];
            b.text.end = t;
            b.runs.end = r;
            if (leaf.implicit || leaf.html) && !item {
                b.src.end = (self.last_end as u64).max(b.src.start);
            }
        }
        let runs = &self.doc.runs[r0..];
        if self.doc.blocks[bi].kind == BlockKind::Para
            && runs.len() == 1
            && let Some(img) = runs[0].image()
        {
            self.doc.blocks[bi].kind = BlockKind::Image(img);
        }
        if let BlockKind::Heading(_) = self.doc.blocks[bi].kind {
            let s = slug(&self.doc.text[start..t as usize], &mut self.slugs);
            self.doc.anchors.push(Anchor {
                slug: s,
                block: leaf.block,
            });
        }
        if t as usize > start || matches!(self.doc.blocks[bi].kind, BlockKind::Image(_)) {
            self.doc.text.push('\n');
        }
    }

    // ------------------------------------------------------------ текст

    fn push_run(&mut self, t: &str, style: RunStyle, image: Option<u32>, at: usize) {
        let Some(leaf) = self.leaf else {
            return;
        };
        let link = self.links.last().copied();
        let start = self.proj_len();
        let row = matches!(
            self.doc.blocks[leaf.block as usize].kind,
            BlockKind::Row { .. }
        ) && self.cell.is_some();
        if row && t.contains('\t') {
            self.doc.text.push_str(&t.replace('\t', " "));
        } else {
            self.doc.text.push_str(t);
        }
        let end = self.proj_len();
        let r0 = self.doc.blocks[leaf.block as usize].runs.start as usize;
        if image.is_none()
            && self.doc.runs.len() > r0
            && let Some(last) = self.doc.runs.last_mut()
            && last.text.end == start
            // Склеиваем, только если исходник идёт подряд: карта остаётся точной.
            && u64::from(last.src) + u64::from(start - last.text.start) == at as u64
            && last.style == style
            && last.link() == link
            && last.image().is_none()
        {
            last.text.end = end;
            return;
        }
        self.doc.runs.push(Run::new(
            start..end,
            u32::try_from(at).unwrap_or(u32::MAX),
            style,
            link,
            image,
        ));
    }

    fn inline(&mut self, t: &str, style: RunStyle, r: &Range<usize>) {
        if self.image_depth > 0 {
            if let Some(f) = &mut self.image {
                f.alt.push_str(t);
            }
            return;
        }
        self.ensure_leaf(r.start);
        self.push_run(t, style, None, r.start);
        self.last_end = r.end;
    }

    fn text(&mut self, t: &str, r: &Range<usize>) {
        if self.leaf.is_some_and(|l| l.code) {
            self.push_run(t, RunStyle::default(), None, r.start);
            self.last_end = r.end;
            return;
        }
        let s = self.styles.style();
        self.inline(t, s, r);
    }

    // ------------------------------------------------------------ события

    fn event(&mut self, ev: Event<'_>, r: Range<usize>) {
        match ev {
            Event::Start(tag) => self.start(tag, &r),
            Event::End(tag) => self.end(tag, &r),
            Event::Text(t) => self.text(&t, &r),
            Event::Code(t) | Event::InlineMath(t) | Event::DisplayMath(t) => {
                let s = RunStyle(self.styles.style().0 | RunStyle::CODE);
                self.inline(&t, s, &r);
            }
            Event::Html(t) => {
                if let Some(acc) = &mut self.html {
                    acc.segs.push((acc.text.len(), r.start));
                    acc.text.push_str(&t);
                    acc.range.end = acc.range.end.max(r.end);
                }
            }
            Event::InlineHtml(t) => self.inline_html(&t, &r),
            Event::FootnoteReference(label) => self.footnote_ref(&label, &r),
            Event::SoftBreak => {
                let s = self.styles.style();
                self.inline(" ", s, &r);
            }
            Event::HardBreak => {
                let s = self.styles.style();
                self.inline("\n", s, &r);
            }
            Event::Rule => {
                self.block_start();
                let b = self.new_block(BlockKind::Rule, r.clone());
                self.doc.blocks[b as usize].end = b + 1;
            }
            Event::TaskListMarker(done) => {
                let item = self.open.iter().rev().find_map(|o| {
                    matches!(
                        self.doc.blocks[o.block as usize].kind,
                        BlockKind::Item { .. }
                    )
                    .then_some(o.block)
                });
                if let Some(i) = item {
                    self.doc.blocks[i as usize].kind = BlockKind::Item {
                        mark: Mark::Task(done),
                    };
                }
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "таблица тегов pulldown-cmark")]
    fn start(&mut self, tag: Tag<'_>, r: &Range<usize>) {
        match tag {
            Tag::Paragraph => {
                self.block_start();
                if let Some(o) = self.open.last_mut()
                    && o.fresh
                {
                    // Первый абзац пункта — текст пункта.
                    o.fresh = false;
                    self.leaf = Some(Leaf {
                        block: o.block,
                        implicit: false,
                        html: false,
                        code: false,
                    });
                } else {
                    self.start_leaf(BlockKind::Para, r.clone(), false);
                }
            }
            Tag::Heading { level, .. } => {
                self.block_start();
                self.start_leaf(BlockKind::Heading(level as u8), r.clone(), false);
            }
            Tag::BlockQuote(_) => {
                self.block_start();
                self.open_container(BlockKind::Quote, r.clone(), false);
            }
            Tag::CodeBlock(kind) => {
                self.block_start();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(|l| self.lang_id(l)),
                    CodeBlockKind::Indented => None,
                };
                self.start_leaf(BlockKind::Code { lang }, r.clone(), false);
            }
            Tag::MetadataBlock(_) => {
                self.block_start();
                let yaml = self.lang_id("yaml");
                self.start_leaf(BlockKind::Code { lang: Some(yaml) }, r.clone(), false);
            }
            Tag::HtmlBlock => {
                self.block_start();
                self.html = Some(HtmlAcc {
                    range: r.clone(),
                    ..HtmlAcc::default()
                });
            }
            Tag::List(start) => {
                self.block_start();
                self.lists.push(start);
            }
            Tag::Item => {
                self.block_start();
                let mark = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = Mark::Num(u32::try_from(*n).unwrap_or(u32::MAX));
                        *n += 1;
                        m
                    }
                    _ => Mark::Bullet,
                };
                self.open_container(BlockKind::Item { mark }, r.clone(), false);
            }
            Tag::FootnoteDefinition(label) => {
                self.block_start();
                let n = self.footnote_no(&label);
                self.open_container(BlockKind::FootnoteDef { n }, r.clone(), false);
            }
            Tag::Table(al) => {
                self.block_start();
                let align = al
                    .iter()
                    .map(|a| match a {
                        pulldown_cmark::Alignment::Center => Align::Center,
                        pulldown_cmark::Alignment::Right => Align::Right,
                        _ => Align::Left,
                    })
                    .collect::<SmallVec<_>>();
                let t = u32::try_from(self.doc.tables.len()).unwrap_or(u32::MAX);
                self.doc.tables.push(align);
                self.open_container(BlockKind::Table(t), r.clone(), false);
            }
            Tag::TableHead => {
                self.block_start();
                self.start_leaf(BlockKind::Row { head: true }, r.clone(), false);
            }
            Tag::TableRow => {
                self.block_start();
                self.start_leaf(BlockKind::Row { head: false }, r.clone(), false);
            }
            Tag::TableCell => {
                if let Some(c) = self.cell {
                    // Разделитель ячеек — не таб внутри ячейки (тот → пробел).
                    self.cell = None;
                    self.push_run("\t", RunStyle::default(), None, r.start);
                    self.cell = Some(c + 1);
                } else {
                    self.cell = Some(0);
                }
            }
            Tag::Emphasis => self.styles.italic += 1,
            Tag::Strong => self.styles.bold += 1,
            Tag::Strikethrough => self.styles.strike += 1,
            Tag::Superscript => self.styles.sup += 1,
            Tag::Subscript => self.styles.sub += 1,
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            } => {
                let dest = if link_type == LinkType::Email && !dest_url.starts_with("mailto:") {
                    format!("mailto:{dest_url}")
                } else {
                    dest_url.to_string()
                };
                let idx = self.add_link(dest, title.to_string());
                self.links.push(idx);
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                if self.image_depth == 0 {
                    let idx = self.add_image(ImageRef {
                        src: dest_url.to_string(),
                        alt: String::new(),
                        title: title.to_string(),
                        width: None,
                        height: None,
                        state: ImageState::Pending,
                    });
                    self.image = Some(ImgFrame {
                        idx,
                        alt: String::new(),
                        at: r.start,
                    });
                }
                self.image_depth += 1;
            }
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {}
        }
    }

    fn end(&mut self, tag: TagEnd, r: &Range<usize>) {
        match tag {
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::CodeBlock
            | TagEnd::MetadataBlock(_)
            | TagEnd::TableHead
            | TagEnd::TableRow => {
                self.last_end = self.last_end.max(r.end);
                self.finish_leaf();
            }
            TagEnd::BlockQuote(_) | TagEnd::Item | TagEnd::FootnoteDefinition | TagEnd::Table => {
                self.block_start();
                // `<details>` внутри контейнера, не закрытые до его конца.
                while self.open.last().is_some_and(|o| o.html) {
                    self.close_container(None);
                }
                self.close_container(None);
            }
            TagEnd::List(_) => {
                self.block_start();
                self.lists.pop();
            }
            TagEnd::HtmlBlock => {
                if let Some(acc) = self.html.take() {
                    self.html_block(&acc);
                }
            }
            TagEnd::Emphasis => self.styles.italic = self.styles.italic.saturating_sub(1),
            TagEnd::Strong => self.styles.bold = self.styles.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.styles.strike = self.styles.strike.saturating_sub(1),
            TagEnd::Superscript => self.styles.sup = self.styles.sup.saturating_sub(1),
            TagEnd::Subscript => self.styles.sub = self.styles.sub.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                self.image_depth = self.image_depth.saturating_sub(1);
                if self.image_depth == 0
                    && let Some(f) = self.image.take()
                {
                    self.doc.images[f.idx as usize].alt.clone_from(&f.alt);
                    self.ensure_leaf(f.at);
                    let s = self.styles.style();
                    self.push_run(&f.alt, s, Some(f.idx), f.at);
                    self.last_end = r.end;
                }
            }
            TagEnd::TableCell
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => {}
        }
    }

    /// Язык блока кода — индекс в `langs` (без повторов).
    fn lang_id(&mut self, l: &str) -> u32 {
        let pos = self.doc.langs.iter().position(|x| x == l);
        let i = pos.unwrap_or_else(|| {
            self.doc.langs.push(l.into());
            self.doc.langs.len() - 1
        });
        u32::try_from(i).unwrap_or(u32::MAX)
    }

    fn footnote_no(&mut self, label: &str) -> u32 {
        let n = u32::try_from(self.footnotes.len() + 1).unwrap_or(u32::MAX);
        *self.footnotes.entry(label.to_lowercase()).or_insert(n)
    }

    fn footnote_ref(&mut self, label: &str, r: &Range<usize>) {
        let n = self.footnote_no(label);
        let idx = self.add_link_target(
            format!("#fn-{label}"),
            String::new(),
            LinkTarget::Footnote { n, block: None },
        );
        self.links.push(idx);
        let s = RunStyle(self.styles.style().0 | RunStyle::SUP | RunStyle::FOOTNOTE);
        self.inline(&n.to_string(), s, r);
        self.links.pop();
    }

    fn add_link(&mut self, dest: String, title: String) -> u32 {
        let target = classify(&dest);
        self.add_link_target(dest, title, target)
    }

    fn add_link_target(&mut self, dest: String, title: String, target: LinkTarget) -> u32 {
        let idx = u32::try_from(self.doc.links.len()).unwrap_or(u32::MAX);
        self.doc.links.push(LinkRef {
            dest,
            title,
            target,
        });
        idx
    }

    fn add_image(&mut self, img: ImageRef) -> u32 {
        let idx = u32::try_from(self.doc.images.len()).unwrap_or(u32::MAX);
        self.doc.images.push(img);
        idx
    }

    // ------------------------------------------------------------ HTML

    fn img_tag(&mut self, g: &HtmlTag<'_>) -> u32 {
        self.add_image(ImageRef {
            src: g.attr("src").unwrap_or_default().to_owned(),
            alt: g.attr("alt").unwrap_or_default().to_owned(),
            title: g.attr("title").unwrap_or_default().to_owned(),
            width: g.attr("width").and_then(html::px),
            height: g.attr("height").and_then(html::px),
            state: ImageState::Pending,
        })
    }

    /// Убирает пробелы в конце текста листа (с отрезками).
    fn trim_tail_space(&mut self) {
        let Some(leaf) = self.leaf else {
            return;
        };
        let start = self.doc.blocks[leaf.block as usize].text.start as usize;
        let keep = self.doc.text[start..].trim_end_matches(' ').len() + start;
        self.doc.text.truncate(keep);
        let r0 = self.doc.blocks[leaf.block as usize].runs.start as usize;
        self.clip_runs(r0);
    }

    /// Отрезки листа (с `r0`) — по обрезанному концу проекции.
    fn clip_runs(&mut self, r0: usize) {
        let t = self.proj_len();
        while self.doc.runs.len() > r0 {
            let last = self.doc.runs.last_mut().expect("непусто");
            if last.text.start >= t && last.image().is_none() {
                self.doc.runs.pop();
                continue;
            }
            last.text.end = last.text.end.min(t);
            break;
        }
    }

    /// Тег вне набора — текстом, моноширинно.
    fn raw_tag(&mut self, raw: &str, at: usize, html_leaf: bool) {
        self.ensure_html_leaf(at, html_leaf);
        let s = RunStyle(self.styles.style().0 | RunStyle::CODE | RunStyle::RAW);
        self.push_run(raw, s, None, at);
    }

    /// Лист для содержимого HTML: в HTML-блоке — свой абзац, в строке — текущий.
    fn ensure_html_leaf(&mut self, at: usize, html_leaf: bool) {
        if !html_leaf {
            self.ensure_leaf(at);
        } else if self.leaf.is_none() {
            let block = self.new_block(BlockKind::Para, at..at);
            self.leaf = Some(Leaf {
                block,
                implicit: false,
                html: true,
                code: false,
            });
        }
    }

    /// Строчные теги набора (`<br>`, `<img>`, `<kbd>`, `<sub>`, `<sup>`); `false` — не
    /// из них или непарный закрывающий.
    fn inline_tag(&mut self, g: &HtmlTag<'_>, at: usize, html_leaf: bool) -> bool {
        let st = &mut self.styles;
        match (g.name.as_str(), g.close) {
            ("br", _) => {
                self.ensure_html_leaf(at, html_leaf);
                if html_leaf {
                    // Пробел перед `<br>` в HTML не виден.
                    self.trim_tail_space();
                }
                let s = self.styles.style();
                self.push_run("\n", s, None, at);
            }
            ("img", false) => {
                let idx = self.img_tag(g);
                let alt = self.doc.images[idx as usize].alt.clone();
                self.ensure_html_leaf(at, html_leaf);
                let s = self.styles.style();
                self.push_run(&alt, s, Some(idx), at);
            }
            ("kbd", false) => st.kbd += 1,
            ("sub", false) => st.hsub += 1,
            ("sup", false) => st.hsup += 1,
            ("kbd", true) if st.kbd > 0 => st.kbd -= 1,
            ("sub", true) if st.hsub > 0 => st.hsub -= 1,
            ("sup", true) if st.hsup > 0 => st.hsup -= 1,
            _ => return false,
        }
        true
    }

    /// `InlineHtml` в абзаце: тег набора, комментарий (скрыт) или текстом.
    fn inline_html(&mut self, t: &str, r: &Range<usize>) {
        if self.image_depth > 0 {
            return;
        }
        for tok in html::tokenize(t) {
            match tok {
                Tok::Comment => {}
                Tok::Text(s, off) => {
                    let st = RunStyle(self.styles.style().0 | RunStyle::CODE | RunStyle::RAW);
                    self.inline(s, st, &(r.start + off..r.start + off + s.len()));
                }
                Tok::Tag(g) => {
                    let at = r.start + g.off;
                    if !self.inline_tag(&g, at, false) {
                        self.raw_tag(g.raw, at, false);
                    }
                }
            }
        }
        self.last_end = r.end;
    }

    /// Конец HTML-блока: без тегов набора и комментариев — `RawHtml`; иначе теги набора
    /// работают, текст — как есть (пробелы схлопнуты), прочие теги — текстом.
    fn html_block(&mut self, acc: &HtmlAcc) {
        let toks = html::tokenize(&acc.text);
        let ours = toks.iter().any(|t| match t {
            Tok::Comment => true,
            Tok::Tag(g) => g.known(),
            Tok::Text(..) => false,
        });
        if !ours {
            let raw = acc.text.trim_end();
            if raw.is_empty() {
                return;
            }
            self.start_leaf(BlockKind::RawHtml, acc.range.clone(), true);
            self.push_run(
                raw,
                RunStyle(RunStyle::CODE | RunStyle::RAW),
                None,
                acc.range.start,
            );
            self.last_end = acc.range.end;
            self.finish_leaf();
            return;
        }
        for tok in &toks {
            match tok {
                Tok::Comment => {}
                Tok::Text(s, off) => {
                    let at = acc.src(*off);
                    self.html_text(s, at);
                    self.last_end = acc.src(off + s.len());
                }
                Tok::Tag(g) => {
                    let at = acc.src(g.off);
                    let end = acc.src(g.off + g.raw.len());
                    self.html_tag(g, at, end);
                    self.last_end = end;
                }
            }
        }
        if self.leaf.is_some_and(|l| l.html) {
            self.finish_leaf();
        }
    }

    /// Текст HTML-блока: сущности раскрыты, пробелы схлопнуты.
    fn html_text(&mut self, s: &str, at: usize) {
        let s = html::entities(s);
        let mut out = String::with_capacity(s.len());
        let mut ws = false;
        for c in s.chars() {
            if c.is_ascii_whitespace() {
                ws = true;
                continue;
            }
            if ws {
                out.push(' ');
                ws = false;
            }
            out.push(c);
        }
        if ws {
            out.push(' ');
        }
        // В начале листа (или после переноса) пробел не нужен.
        let fresh = match self.leaf {
            None => true,
            Some(l) => {
                let b = &self.doc.blocks[l.block as usize];
                self.doc.text.len() == b.text.start as usize || self.doc.text.ends_with('\n')
            }
        };
        let t = if fresh { out.trim_start() } else { &out };
        if t.is_empty() {
            return;
        }
        self.ensure_html_leaf(at, true);
        let st = self.styles.style();
        self.push_run(t, st, None, at);
    }

    fn html_tag(&mut self, g: &HtmlTag<'_>, at: usize, end: usize) {
        let html_leaf = self.leaf.is_none_or(|l| l.html);
        match (g.name.as_str(), g.close) {
            ("p" | "div", false) => {
                self.block_start();
                let align = match g.attr("align").map(str::to_ascii_lowercase).as_deref() {
                    Some("center") => Align::Center,
                    Some("right") => Align::Right,
                    _ => Align::Left,
                };
                let tag = if g.name == "p" { "p" } else { "div" };
                self.scopes.push(Scope {
                    align,
                    tag,
                    level: self.open.len(),
                });
            }
            ("p" | "div", true) => {
                if self.scopes.last().is_some_and(|s| s.tag == g.name) {
                    self.block_start();
                    self.scopes.pop();
                } else {
                    self.raw_tag(g.raw, at, html_leaf);
                }
            }
            ("details", false) => {
                self.block_start();
                let open = g.attr("open").is_some();
                self.open_container(BlockKind::Details { open }, at..end, true);
            }
            ("details", true) => {
                // Закрываем ближайший `<details>`, если над ним только `<details>`.
                let k = self.open.iter().rposition(|o| !o.html).map_or(0, |i| i + 1);
                if self.open.len() > k {
                    self.block_start();
                    if self.leaf.is_some() {
                        self.finish_leaf();
                    }
                    self.close_container(Some(end));
                } else {
                    self.raw_tag(g.raw, at, html_leaf);
                }
            }
            ("summary", false)
                if self.open.last().is_some_and(|o| {
                    o.html
                        && matches!(
                            self.doc.blocks[o.block as usize].kind,
                            BlockKind::Details { .. }
                        )
                }) =>
            {
                self.block_start();
                self.start_leaf(BlockKind::Summary, at..end, true);
            }
            ("summary", true)
                if self.leaf.is_some_and(|l| {
                    self.doc.blocks[l.block as usize].kind == BlockKind::Summary
                }) =>
            {
                self.last_end = end;
                self.finish_leaf();
            }
            _ => {
                if !self.inline_tag(g, at, html_leaf) {
                    self.raw_tag(g.raw, at, html_leaf);
                }
            }
        }
    }

    // ------------------------------------------------------------ конец

    fn finish(mut self) -> MdDoc {
        if let Some(acc) = self.html.take() {
            self.html_block(&acc);
        }
        self.finish_leaf();
        while !self.open.is_empty() {
            self.close_container(None);
        }
        // Якоря и сноски — когда известны все заголовки и определения.
        let mut doc = self.doc;
        let mut defs = FxHashMap::default();
        for (i, b) in doc.blocks.iter().enumerate() {
            if let BlockKind::FootnoteDef { n } = b.kind {
                defs.entry(n).or_insert(i as u32);
            }
        }
        // Якорь → блок: точное совпадение, затем без учёта регистра (как `MdDoc::anchor`).
        let mut slugs: FxHashMap<&str, u32> = FxHashMap::default();
        for a in doc.anchors.iter().rev() {
            slugs.insert(a.slug.as_str(), a.block);
        }
        let found: Vec<Option<u32>> = doc
            .links
            .iter()
            .map(|l| match &l.target {
                LinkTarget::Anchor { slug, .. } => slugs
                    .get(slug.as_str())
                    .or_else(|| slugs.get(slug.to_lowercase().as_str()))
                    .copied(),
                _ => None,
            })
            .collect();
        for (l, f) in doc.links.iter_mut().zip(found) {
            match &mut l.target {
                LinkTarget::Anchor { block, .. } => *block = f,
                LinkTarget::Footnote { n, block } => *block = defs.get(n).copied(),
                _ => {}
            }
        }
        doc.complete = true;
        doc.src_len = self.src.len() as u64;
        doc.text.shrink_to_fit();
        doc.blocks.shrink_to_fit();
        doc.runs.shrink_to_fit();
        doc
    }
}

/// Вид ссылки по цели (без I/O): якорь, внешняя, прочая схема или локальный путь.
pub(crate) fn classify(dest: &str) -> LinkTarget {
    let d = dest.trim();
    if let Some(a) = d.strip_prefix('#') {
        return LinkTarget::Anchor {
            slug: super::resolve::percent_decode(a),
            block: None,
        };
    }
    let low = d.get(..d.len().min(10)).unwrap_or(d).to_ascii_lowercase();
    if low.starts_with("http://") || low.starts_with("https://") || low.starts_with("mailto:") {
        return LinkTarget::External(d.to_owned());
    }
    if d.starts_with("//") || d.starts_with("\\\\") {
        return LinkTarget::Other;
    }
    if super::resolve::scheme(d).is_some_and(|s| s != "file") {
        return LinkTarget::Other;
    }
    let (path, anchor) = match d.split_once('#') {
        Some((p, a)) => (p, Some(super::resolve::percent_decode(a))),
        None => (d, None),
    };
    let path = path.split('?').next().unwrap_or(path);
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let markdown =
        crate::lang::detect(&super::resolve::percent_decode(name), &[]) == Some("markdown");
    LinkTarget::Path {
        path: None,
        anchor,
        markdown,
    }
}
