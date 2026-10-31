//! Markdown-документ во вьювере: модель и разбор (S7 §3.10, решение 2026-10-30,
//! карточка S7-22). Рисование — UI (S7-22a).
//!
//! [`parse`] разбирает текст pulldown-cmark с GFM (таблицы, задачи, зачёркнутый, сноски,
//! front matter) через `into_offset_iter` и строит [`MdDoc`]:
//! - блоки в порядке документа (прямой обход): у каждого — смещения в исходнике, свой
//!   текст в проекции, свои отрезки (`Run`), родитель и глубина;
//! - проекция [`MdDoc::text`] — видимый текст одной UTF-8 строкой: текст блока, затем
//!   `\n` (у блоков с текстом), `<br>` и жёсткий перенос — `\n`, ячейки — `\t`;
//! - отрезки покрывают текст блока без пропусков, у каждого — начало в исходнике: карта
//!   «проекция → блок → исходник» ([`MdDoc::text_to_src`], [`MdDoc::src_to_text`]);
//! - ссылки и картинки — таблицы [`MdDoc::links`], [`MdDoc::images`]; пути разрешает
//!   [`resolve::resolve`] (I/O — только в задаче разбора, не в UI-потоке);
//! - якоря заголовков — slug как у GitHub ([`slug`]).
//!
//! Сырой HTML — небольшой набор (решение §13, вопрос 3) свой токенайзер [`html`]:
//! `<br>`, `<img>`, `<details>` / `<summary>`, `align` у `<p>` / `<div>`, `<kbd>`,
//! `<sub>`, `<sup>`. Комментарии `<!-- … -->` скрыты, как на GitHub (решение 2026-10-30).
//! Прочие теги — текстом, моноширинно (`RawHtml`, run `CODE | RAW`).
//!
//! Задача разбора на `Lane::Interactive` и режим документа у `ViewerDoc` — [`job`] и
//! `viewer::md_view`.

pub mod html;
pub mod job;
pub mod resolve;

mod build;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod view_tests;

use std::num::NonZeroU32;
use std::ops::Range;
use std::path::PathBuf;

use compact_str::CompactString;
use smallvec::SmallVec;

pub use build::{ParseError, parse, parse_with};
pub use job::{MdMsg, ProjSource};
pub use resolve::{FileMeta, MdFs, RealFs, Resolver, resolve};

/// Документ — для файлов не больше (как «Разметка», S7 §3.4).
pub const DOC_MAX: u64 = 16 << 20;
/// Файл больше — сначала разбирается начало ([`FIRST_CHUNK`]), потом весь.
pub const SPLIT_MIN: u64 = 1 << 20;
/// Начало большого файла для первого экрана.
pub const FIRST_CHUNK: usize = 256 << 10;
/// Картинки больше — плашкой (`viewer.md_image_max_mb` по умолчанию).
pub const IMAGE_MAX_MB: u32 = 20;

/// Выравнивание блока (`align=` у `<p>` / `<div>`) или колонки таблицы.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Маркер пункта списка.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// `•`
    Bullet,
    /// `N.`
    Num(u32),
    /// `[ ]` / `[x]`
    Task(bool),
}

/// Вид блока. Контейнеры (`Quote`, `Item`, `Table`, `FootnoteDef`, `Details`) держат
/// детей — блоки после них с `parent` = их индекс и `depth + 1`; `Item` ещё и свой текст
/// (первый абзац пункта).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// H1–H6.
    Heading(u8),
    Para,
    Item {
        mark: Mark,
    },
    Quote,
    /// Таблица: выравнивание колонок — [`MdDoc::table_align`] (индекс в
    /// [`MdDoc::tables`]); строки — дети `Row`.
    Table(u32),
    /// Строка таблицы: ячейки в тексте через `\t` (таб внутри ячейки — пробел).
    Row {
        head: bool,
    },
    /// Блок кода (fenced, с отступом, front matter — `yaml`); язык — [`MdDoc::code_lang`]
    /// (индекс в [`MdDoc::langs`]).
    Code {
        lang: Option<u32>,
    },
    Rule,
    /// Абзац из одной картинки или `<img>` в HTML-блоке; индекс в [`MdDoc::images`].
    Image(u32),
    /// Определение сноски `[^x]:`; `n` — номер (по первой ссылке, как на GitHub).
    FootnoteDef {
        n: u32,
    },
    /// `<details>`: по умолчанию свёрнут, `open` — раскрыт сразу. Подпись — первый ребёнок
    /// `Summary` ([`MdDoc::summary_of`]); без него — «Подробнее» (UI).
    Details {
        open: bool,
    },
    /// `<summary>` — подпись `Details`.
    Summary,
    /// HTML-блок без тегов набора — текстом, моноширинно, `secondary`.
    RawHtml,
}

impl BlockKind {
    /// Контейнер: у него бывают дети.
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            BlockKind::Quote
                | BlockKind::Item { .. }
                | BlockKind::Table(_)
                | BlockKind::FootnoteDef { .. }
                | BlockKind::Details { .. }
        )
    }
}

/// Блок документа.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// Байты исходника (с учётом BOM и кодировки), у контейнера — вместе с детьми.
    pub src: Range<u64>,
    /// Свой текст в проекции (без `\n` после него); у контейнера без текста — пустой.
    pub text: Range<u32>,
    /// Свои отрезки — индексы в [`MdDoc::runs`]; покрывают `text` без пропусков.
    pub runs: Range<u32>,
    pub parent: Option<u32>,
    /// Число контейнеров-предков.
    pub depth: u8,
    /// Индекс за последним потомком (у листа — `self + 1`).
    pub end: u32,
    pub align: Align,
}

/// Стиль отрезка — набор флагов.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RunStyle(pub u16);

impl RunStyle {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 1 << 1;
    pub const STRIKE: u16 = 1 << 2;
    /// Код в строке (моноширинно на подложке).
    pub const CODE: u16 = 1 << 3;
    /// `<kbd>` — клавиша.
    pub const KBD: u16 = 1 << 4;
    pub const SUB: u16 = 1 << 5;
    pub const SUP: u16 = 1 << 6;
    /// Тег вне набора — текстом (`CODE | RAW`, цвет `secondary`).
    pub const RAW: u16 = 1 << 7;
    /// Метка сноски (`SUP | FOOTNOTE`, ссылка — `LinkTarget::Footnote`).
    pub const FOOTNOTE: u16 = 1 << 8;

    pub fn has(self, f: u16) -> bool {
        self.0 & f == f
    }
}

/// Отрезок текста блока одного стиля (24 байта: их сотни тысяч в 16 МБ).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// В проекции.
    pub text: Range<u32>,
    /// Начало в исходнике (байт; документ ≤ 16 МБ).
    pub src: u32,
    pub style: RunStyle,
    link: Option<NonZeroU32>,
    image: Option<NonZeroU32>,
}

fn pack(i: Option<u32>) -> Option<NonZeroU32> {
    i.and_then(|i| NonZeroU32::new(i.wrapping_add(1)))
}

impl Run {
    pub fn new(
        text: Range<u32>,
        src: u32,
        style: RunStyle,
        link: Option<u32>,
        image: Option<u32>,
    ) -> Run {
        Run {
            text,
            src,
            style,
            link: pack(link),
            image: pack(image),
        }
    }

    /// Ссылка — индекс в [`MdDoc::links`].
    pub fn link(&self) -> Option<u32> {
        self.link.map(|n| n.get() - 1)
    }

    /// Картинка (текст отрезка — её alt) — индекс в [`MdDoc::images`].
    pub fn image(&self) -> Option<u32> {
        self.image.map(|n| n.get() - 1)
    }
}

/// Почему картинка не показана — причина на плашке.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImageIssue {
    /// `http(s)://`, `//host`, UNC — «из сети не загружается».
    Network,
    /// «файл не найден».
    NotFound,
    /// «формат не поддерживается».
    Unsupported,
    /// «больше N МБ» (`viewer.md_image_max_mb`).
    TooLarge,
    /// Документ не на локальном диске (архив, SFTP): относительные пути не читаются.
    NotLocal,
}

/// Состояние картинки после [`resolve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageState {
    /// Ещё не разрешена (сразу после [`parse`]).
    Pending,
    /// Локальный файл: грузит и декодирует GPUI в фоне (`gpui::img` с путём).
    File(PathBuf),
    /// `data:` URI (без сети): байты — в `ImageRef::src`.
    Data,
    /// Плашка: значок, alt, причина.
    Blocked(ImageIssue),
}

/// Картинка: `![alt](src "title")` или `<img src width height alt>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef {
    /// Как в исходнике.
    pub src: String,
    pub alt: String,
    pub title: String,
    /// Размер в px из `<img width height>`: задан один — пропорции сохраняются (UI), не
    /// шире колонки.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub state: ImageState,
}

/// Куда ведёт ссылка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// `#якорь` — прокрутка к заголовку; `block` — `None`, если такого нет.
    Anchor { slug: String, block: Option<u32> },
    /// Метка сноски → её определение.
    Footnote { n: u32, block: Option<u32> },
    /// `http(s)`, `mailto` — Ctrl+клик открывает программой Windows (решение §13, вопрос 2).
    External(String),
    /// Локальный путь. `markdown` — открыть в этом же вьювере (якорь — `anchor`), иначе —
    /// курсор панели на файл (`viewer.reveal`). `path` — `None` до [`resolve`] и у
    /// документа не на локальном диске.
    Path {
        path: Option<PathBuf>,
        anchor: Option<String>,
        markdown: bool,
    },
    /// Прочие схемы — не открываются, цель видна в подсказке.
    Other,
}

impl LinkTarget {
    /// Команда клика по локальной ссылке (S7 §3.10, S7-14): `.md` — в этом же экране
    /// вьювера (`viewer.open placement=here`, якорь; назад — `viewer.back`), другой файл
    /// или папка — курсор панели на него (`viewer.reveal`). `None` — не локальный путь.
    pub fn command(&self) -> Option<urfin_core::Command> {
        use urfin_core::{Command, Location};
        let LinkTarget::Path {
            path: Some(p),
            anchor,
            markdown,
        } = self
        else {
            return None;
        };
        if !markdown {
            return Some(Command::new("viewer.reveal").arg("path", Location::local(p)));
        }
        let mut c = Command::new("viewer.open")
            .arg("path", Location::local(p))
            .arg("placement", "here");
        if let Some(a) = anchor.as_deref().filter(|a| !a.is_empty()) {
            c = c.arg("anchor", a);
        }
        Some(c)
    }
}

/// Ссылка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRef {
    /// Цель как в исходнике (у e-mail автоссылки — с `mailto:`): подсказка.
    pub dest: String,
    pub title: String,
    pub target: LinkTarget,
}

/// Якорь заголовка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub slug: String,
    pub block: u32,
}

/// Разобранный документ.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MdDoc {
    pub blocks: Vec<Block>,
    pub runs: Vec<Run>,
    /// Проекция — видимый текст (поиск F7, выделение, копирование).
    pub text: String,
    pub images: Vec<ImageRef>,
    pub links: Vec<LinkRef>,
    /// Якоря заголовков в порядке документа.
    pub anchors: Vec<Anchor>,
    /// Выравнивание колонок таблиц (`BlockKind::Table`).
    pub tables: Vec<SmallVec<[Align; 8]>>,
    /// Языки блоков кода без повторов (`BlockKind::Code`).
    pub langs: Vec<CompactString>,
    /// `false` — разобрано только начало большого файла ([`FIRST_CHUNK`]), весь документ
    /// придёт следующим сообщением.
    pub complete: bool,
    /// Разобрано байт исходника.
    pub src_len: u64,
}

impl MdDoc {
    pub fn block_text(&self, b: u32) -> &str {
        let r = &self.blocks[b as usize].text;
        &self.text[r.start as usize..r.end as usize]
    }

    pub fn block_runs(&self, b: u32) -> &[Run] {
        let r = &self.blocks[b as usize].runs;
        &self.runs[r.start as usize..r.end as usize]
    }

    /// Выравнивание колонок таблицы `b` (пусто — не таблица).
    pub fn table_align(&self, b: u32) -> &[Align] {
        match self.blocks.get(b as usize).map(|b| &b.kind) {
            Some(BlockKind::Table(t)) => self.tables.get(*t as usize).map_or(&[], |a| a),
            _ => &[],
        }
    }

    /// Язык блока кода `b`.
    pub fn code_lang(&self, b: u32) -> Option<&str> {
        match self.blocks.get(b as usize).map(|b| &b.kind) {
            Some(BlockKind::Code { lang: Some(l) }) => {
                self.langs.get(*l as usize).map(CompactString::as_str)
            }
            _ => None,
        }
    }

    pub fn run_text(&self, r: &Run) -> &str {
        &self.text[r.text.start as usize..r.text.end as usize]
    }

    /// Последний блок, начинающийся не позже `off` (байт исходника): для позиции
    /// «Текст → документ» — блок, содержащий верх экрана Текста.
    pub fn block_at_src(&self, off: u64) -> Option<u32> {
        let i = self.blocks.partition_point(|b| b.src.start <= off);
        if i == 0 {
            return (!self.blocks.is_empty()).then_some(0);
        }
        Some((i - 1) as u32)
    }

    /// Блок, чей текст содержит позицию проекции `p` (на `\n` после текста — этот блок).
    pub fn block_at_text(&self, p: u32) -> Option<u32> {
        let i = self.blocks.partition_point(|b| b.text.start <= p);
        if i == 0 {
            return (!self.blocks.is_empty()).then_some(0);
        }
        Some((i - 1) as u32)
    }

    /// Байт исходника для позиции проекции: у начала текста блока — начало блока, внутри
    /// отрезка — его начало плюс сдвиг (точно для текста без экранирования).
    pub fn text_to_src(&self, p: u32) -> u64 {
        let Some(bi) = self.block_at_text(p) else {
            return 0;
        };
        let b = &self.blocks[bi as usize];
        if p <= b.text.start {
            return b.src.start;
        }
        let runs = &self.runs[b.runs.start as usize..b.runs.end as usize];
        let k = runs.partition_point(|r| r.text.start <= p);
        if k == 0 {
            return b.src.start;
        }
        let r = &runs[k - 1];
        let off = u64::from(r.src) + u64::from(p.min(r.text.end) - r.text.start);
        off.clamp(b.src.start, b.src.end.max(b.src.start))
    }

    /// Позиция проекции для байта исходника: блок по [`Self::block_at_src`], в нём —
    /// отрезок, начавшийся не позже `off`. Начало блока → начало его текста.
    pub fn src_to_text(&self, off: u64) -> u32 {
        let Some(bi) = self.block_at_src(off) else {
            return 0;
        };
        let b = &self.blocks[bi as usize];
        let runs = &self.runs[b.runs.start as usize..b.runs.end as usize];
        let k = runs.partition_point(|r| u64::from(r.src) <= off);
        if k == 0 {
            return b.text.start;
        }
        let r = &runs[k - 1];
        let d = u32::try_from(off - u64::from(r.src)).unwrap_or(u32::MAX);
        r.text.start.saturating_add(d).min(r.text.end)
    }

    /// Блок заголовка по якорю (slug как у GitHub; без учёта регистра — запасной путь).
    pub fn anchor(&self, slug: &str) -> Option<u32> {
        let slug = slug.strip_prefix('#').unwrap_or(slug);
        self.anchors
            .iter()
            .find(|a| a.slug == slug)
            .or_else(|| {
                let low = slug.to_lowercase();
                self.anchors.iter().find(|a| a.slug == low)
            })
            .map(|a| a.block)
    }

    /// Подпись `Details`: первый ребёнок, если он `Summary`.
    pub fn summary_of(&self, details: u32) -> Option<u32> {
        let i = details + 1;
        let b = self.blocks.get(i as usize)?;
        (b.parent == Some(details) && b.kind == BlockKind::Summary).then_some(i)
    }

    /// Предки блока от ближайшего.
    pub fn ancestors(&self, b: u32) -> impl Iterator<Item = u32> + '_ {
        std::iter::successors(self.blocks.get(b as usize).and_then(|x| x.parent), |p| {
            self.blocks[*p as usize].parent
        })
    }

    /// Картинок с плашкой (середина статуса: «N картинок не показано»).
    pub fn hidden_images(&self) -> usize {
        self.images
            .iter()
            .filter(|i| matches!(i.state, ImageState::Blocked(_)))
            .count()
    }

    /// Память документа в куче (бюджет ≤ 3× текста, S7 §3.10).
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        let strs = |s: &String| s.capacity();
        self.blocks.capacity() * size_of::<Block>()
            + self.runs.capacity() * size_of::<Run>()
            + self.text.capacity()
            + self.images.capacity() * size_of::<ImageRef>()
            + self
                .images
                .iter()
                .map(|i| strs(&i.src) + strs(&i.alt) + strs(&i.title))
                .sum::<usize>()
            + self.links.capacity() * size_of::<LinkRef>()
            + self
                .links
                .iter()
                .map(|l| strs(&l.dest) + strs(&l.title))
                .sum::<usize>()
            + self.anchors.capacity() * size_of::<Anchor>()
            + self.anchors.iter().map(|a| strs(&a.slug)).sum::<usize>()
            + self.tables.capacity() * size_of::<SmallVec<[Align; 8]>>()
            + self.langs.capacity() * size_of::<CompactString>()
    }
}

/// Slug заголовка как у GitHub (github-slugger): нижний регистр, буквы, цифры, `_` и `-`
/// остаются, пробел → `-`, прочее выбрасывается. Повтор — суффикс `-1`, `-2`… (`seen`).
pub fn slug<S: std::hash::BuildHasher>(
    text: &str,
    seen: &mut std::collections::HashMap<String, u32, S>,
) -> String {
    let mut base = String::with_capacity(text.len());
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            base.push(c);
        } else if c == ' ' {
            base.push('-');
        }
    }
    let mut out = base.clone();
    if let Some(&n0) = seen.get(&base) {
        let mut n = n0;
        loop {
            n += 1;
            out = format!("{base}-{n}");
            if !seen.contains_key(&out) {
                break;
            }
        }
        seen.insert(base, n);
    }
    seen.entry(out.clone()).or_insert(0);
    out
}
