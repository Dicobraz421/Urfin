//! Markdown-документ во вьювере (S7-22a; S7 §3.10, §6; DESIGN «Markdown-документ»,
//! `screen-viewer-md.png`): `gpui::list` блоков вместо `canvas` строк.
//!
//! Модель — `urfin_view::md::MdDoc` (разбор в фоне, пути картинок уже разрешены); здесь
//! только раскладка и ввод:
//! - элемент списка — [`Unit`]: лист-блок (заголовок, абзац, пункт, картинка, код, черта,
//!   подпись `<details>`); таблица и блок кода режутся по [`CHUNK`] строк. Контейнеры
//!   (цитата, пункт, сноска, `<details>`) — отступы и черта у потомков;
//! - текст — `StyledText` с `TextRun` по стилям отрезков и роли темы; выделение, совпадения
//!   F7 и наведённая ссылка — тоже `TextRun`. Каждый текст кадра пишется в реестр
//!   [`HitText`]: по нему мышь находит позицию проекции (выделение, ссылки);
//! - картинки — `gpui::img` с путём: файл читает и декодирует GPUI в фоне, UI-поток его не
//!   трогает; до загрузки — скелетон 24, не показанные — плашка с причиной;
//! - позиция — `InboxMsg::MdTop` (верхний блок и доля его высоты); прокрутку handler'а
//!   (клавиши, совпадение, Документ ↔ Текст) список догоняет сам ([`DocView::sync`]).

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, Font, FontStyle, FontWeight, Hsla, Image,
    ImageCacheError, ImageFormat, ImgResourceLoader, ListAlignment, ListOffset, ListState,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage,
    Resource, ScrollWheelEvent, SharedString, StrikethroughStyle, StyledText, TextLayout, TextRun,
    UnderlineStyle, WeakEntity, Window, anchored, canvas, deferred, div, img, list, point, px,
    size,
};
use rustc_hash::{FxHashMap, FxHashSet};
use urfin_core::{AppState, Command};
use urfin_i18n::FluentArgs;
use urfin_view::md::{Align, BlockKind, ImageIssue, ImageState, LinkTarget, Mark, MdDoc, RunStyle};
use urfin_view::{DocId, InboxMsg, MdState, ViewState, ViewerDoc};

use super::super::CoreHost;
use super::super::theme::Theme;
use super::{Spot, Viewer, rows};
use crate::tokens::Role;

/// Колонка во вьювере (с полями), поля, отступ сверху.
const COL_W: f32 = 820.0;
const PAD_X: f32 = 32.0;
/// Поля в области панели (быстрый просмотр): вся ширина (DESIGN «Markdown-документ»).
const PAD_X_PANEL: f32 = 28.0;
const PAD_TOP: f32 = 20.0;
/// Отступ уровня (список, `<details>`, сноска), колонка маркера.
const INDENT: f32 = 22.0;
const MARK_W: f32 = 14.0;
/// Строк таблицы и кода в одном элементе списка.
pub(super) const CHUNK: usize = 64;
/// Полоса прокрутки.
const W_SCROLL: f32 = 14.0;
const THUMB_MIN: f32 = 24.0;
/// Шрифт документа по умолчанию (`viewer.md_font_size` = 0).
pub(super) const FONT: f32 = 14.0;
/// Подсветка совпадений — по проекции видимых блоков не больше (как в тексте).
const HL_MAX: u32 = 64 << 10;
const HL_COUNT: usize = 4096;
const ICONS: &str = "Segoe Fluent Icons";
/// Значки: картинка, шеврон свёрнут / раскрыт, галочка.
const ICON_PHOTO: &str = "\u{E91B}";
const ICON_CLOSED: &str = "\u{E76C}";
const ICON_OPEN: &str = "\u{E70D}";
const ICON_CHECK: &str = "\u{E73E}";

thread_local! {
    /// Картинок из файлов, ещё не загруженных GPUI в прошлом кадре (сцены `--screens`).
    static PENDING: Cell<usize> = const { Cell::new(0) };
}

/// Картинки прошлого кадра ещё грузятся.
pub(super) fn images_pending() -> bool {
    PENDING.with(Cell::get) > 0
}

/// Элемент списка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Unit {
    pub block: u32,
    /// Часть блока и их число (таблица, код длиннее [`CHUNK`] строк).
    pub part: u32,
    pub parts: u32,
    /// Код — отрезок проекции; таблица — индексы строк-блоков.
    pub sub: Range<u32>,
}

impl Unit {
    fn whole(block: u32) -> Unit {
        Unit {
            block,
            part: 0,
            parts: 1,
            sub: 0..0,
        }
    }
}

fn is_open(doc: &MdDoc, toggled: &FxHashSet<u64>, b: u32) -> bool {
    match doc.blocks.get(b as usize).map(|x| (&x.kind, x.src.start)) {
        Some((BlockKind::Details { open }, key)) => *open != toggled.contains(&key),
        _ => true,
    }
}

/// Элементы списка документа: свёрнутые `<details>` — только подпись.
pub(super) fn units(doc: &MdDoc, toggled: &FxHashSet<u64>) -> Vec<Unit> {
    let n = u32::try_from(doc.blocks.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(doc.blocks.len());
    let mut i = 0u32;
    while i < n {
        let b = &doc.blocks[i as usize];
        match &b.kind {
            BlockKind::Table(_) => {
                let rows: Vec<u32> = (i + 1..b.end)
                    .filter(|&r| matches!(doc.blocks[r as usize].kind, BlockKind::Row { .. }))
                    .collect();
                let chunks = rows.chunks(CHUNK).collect::<Vec<_>>();
                let parts = u32::try_from(chunks.len().max(1)).unwrap_or(1);
                if chunks.is_empty() {
                    out.push(Unit::whole(i));
                }
                for (k, c) in chunks.iter().enumerate() {
                    out.push(Unit {
                        block: i,
                        part: u32::try_from(k).unwrap_or(0),
                        parts,
                        sub: c[0]..c[c.len() - 1] + 1,
                    });
                }
                i = b.end.max(i + 1);
                continue;
            }
            BlockKind::Code { .. } => {
                let r = &b.text;
                let text = doc.block_text(i);
                let mut starts = vec![r.start];
                let mut lines = 0usize;
                for (k, _) in text.match_indices('\n') {
                    lines += 1;
                    if lines.is_multiple_of(CHUNK) {
                        starts.push(r.start + u32::try_from(k + 1).unwrap_or(0));
                    }
                }
                if starts.len() > 1 && *starts.last().unwrap_or(&0) >= r.end {
                    starts.pop();
                }
                let parts = u32::try_from(starts.len()).unwrap_or(1);
                for (k, s) in starts.iter().enumerate() {
                    let e = starts.get(k + 1).map_or(r.end, |n| n - 1);
                    out.push(Unit {
                        block: i,
                        part: u32::try_from(k).unwrap_or(0),
                        parts,
                        sub: *s..e.max(*s),
                    });
                }
            }
            BlockKind::Details { .. } => {
                let summary = doc.summary_of(i);
                if summary.is_none() {
                    out.push(Unit::whole(i));
                }
                if !is_open(doc, toggled, i) {
                    if let Some(s) = summary {
                        out.push(Unit::whole(s));
                    }
                    i = b.end.max(i + 1);
                    continue;
                }
            }
            BlockKind::Quote | BlockKind::FootnoteDef { .. } if b.end > i + 1 => {}
            BlockKind::Row { .. } => {}
            _ => out.push(Unit::whole(i)),
        }
        i += 1;
    }
    out
}

/// Первый элемент списка блока `b` (или следующего за ним).
fn unit_of(units: &[Unit], b: u32) -> usize {
    units
        .partition_point(|u| u.block < b)
        .min(units.len().saturating_sub(1))
}

/// Кусок текста на экране: байты подписи (`d`, `len`) и начало в проекции (`p`); `pad` —
/// поле вокруг кода (в проекции его нет).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seg {
    d: u32,
    len: u32,
    p: u32,
    pad: bool,
}

/// Позиция проекции по байту подписи.
pub(super) fn disp_to_proj(segs: &[Seg], i: u32) -> Option<u32> {
    let k = segs.partition_point(|s| s.d <= i);
    let s = if k == 0 { segs.first()? } else { &segs[k - 1] };
    if s.pad || i < s.d {
        return Some(s.p);
    }
    Some(s.p + (i - s.d).min(s.len))
}

/// Текст кадра для мыши: раскладка, куски, блок; `ready` — раскладка уже размещена.
struct HitText {
    layout: TextLayout,
    segs: Rc<[Seg]>,
    block: u32,
    ready: Rc<Cell<bool>>,
}

/// Позиция мыши в проекции: позиция, блок, точно над текстом.
#[derive(Clone, Copy, Debug)]
struct Hit {
    p: u32,
    block: u32,
    exact: bool,
}

fn hit_at(reg: &[HitText], at: Point<Pixels>) -> Option<Hit> {
    let (x, y) = (f32::from(at.x), f32::from(at.y));
    let mut best: Option<(f32, &HitText, Bounds<Pixels>)> = None;
    for h in reg.iter().filter(|h| h.ready.get()) {
        let b = h.layout.bounds();
        let (top, bot) = (f32::from(b.top()), f32::from(b.bottom()));
        let (l, r) = (f32::from(b.left()), f32::from(b.right()));
        let dy = if y < top {
            top - y
        } else if y > bot {
            y - bot
        } else {
            0.0
        };
        let dx = if x < l {
            l - x
        } else if x > r {
            x - r
        } else {
            0.0
        };
        let dist = dy * 10_000.0 + dx;
        if best.as_ref().is_none_or(|(d, ..)| dist < *d) {
            best = Some((dist, h, b));
        }
    }
    let (dist, h, _) = best?;
    let (ix, exact) = match h.layout.index_for_position(at) {
        Ok(i) => (i, dist == 0.0),
        Err(i) => (i, false),
    };
    let p = disp_to_proj(&h.segs, u32::try_from(ix).unwrap_or(u32::MAX))?;
    Some(Hit {
        p,
        block: h.block,
        exact,
    })
}

/// Ссылка в позиции `p` блока `b`.
fn link_at(doc: &MdDoc, b: u32, p: u32) -> Option<u32> {
    doc.block_runs(b)
        .iter()
        .find(|r| r.text.start <= p && p < r.text.end)
        .and_then(urfin_view::md::Run::link)
}

/// Картинки документа: размер после загрузки, ошибки декодера, `data:`.
#[derive(Default)]
struct Imgs {
    sizes: FxHashMap<u32, (f32, f32)>,
    failed: FxHashSet<u32>,
    data: FxHashMap<u32, Option<Arc<Image>>>,
}

/// Цвета кадра.
#[derive(Clone, Copy)]
struct Paint {
    text: Hsla,
    secondary: Hsla,
    accent: Hsla,
    accent_fill: Hsla,
    panel: Hsla,
    header: Hsla,
    border: Hsla,
    sel: Hsla,
    hit: Hsla,
    hover: Hsla,
    skeleton: Hsla,
    overlay: Hsla,
}

impl Paint {
    fn of(t: &Theme) -> Paint {
        Paint {
            text: t.c(Role::TextPrimary),
            secondary: t.c(Role::TextSecondary),
            accent: t.c(Role::AccentText),
            accent_fill: t.c(Role::AccentFill),
            panel: t.c(Role::LayerPanel),
            header: t.c(Role::Header),
            border: t.c(Role::StrokeDivider),
            sel: t.c(Role::FillCursor),
            hit: t.c(Role::Highlight),
            hover: t.c(Role::CurrentLine),
            skeleton: t.c(Role::FillSubtleHover),
            overlay: t.c(Role::LayerOverlay),
        }
    }
}

/// Строки интерфейса кадра.
struct Words {
    details: SharedString,
    img: String,
    remote: String,
    missing: String,
    format: String,
    too_big: String,
}

impl Words {
    fn new(max_mb: u32) -> Words {
        let i18n = urfin_i18n::current();
        let mut a = FluentArgs::new();
        a.set("mb", u64::from(max_mb));
        Words {
            details: SharedString::from(i18n.get("ui-md-details")),
            img: i18n.get("ui-md-img"),
            remote: i18n.get("ui-md-img-remote"),
            missing: i18n.get("ui-md-img-missing"),
            format: i18n.get("ui-md-img-format"),
            too_big: i18n.fmt("ui-md-img-too-big", &a),
        }
    }

    /// Причина на плашке (макет: «картинка из сети не загружается»).
    fn reason(&self, issue: ImageIssue) -> String {
        match issue {
            ImageIssue::Network => format!("{} {}", self.img, self.remote),
            ImageIssue::NotFound | ImageIssue::NotLocal => self.missing.clone(),
            ImageIssue::Unsupported => self.format.clone(),
            ImageIssue::TooLarge => self.too_big.clone(),
        }
    }
}

/// Всё, что нужно элементам списка в кадре.
struct Ctx {
    doc: Arc<MdDoc>,
    units: Rc<[Unit]>,
    p: Paint,
    /// Шрифт документа и масштаб к 14.
    f: f32,
    k: f32,
    /// Колонка: отступ слева и ширина текста.
    left: f32,
    width: f32,
    ui: Font,
    mono: Font,
    sel: Option<Range<u32>>,
    cur: Option<Range<u32>>,
    hits: Rc<[(u32, u32)]>,
    hover: Option<u32>,
    toggled: FxHashSet<u64>,
    reg: Rc<RefCell<Vec<HitText>>>,
    imgs: Rc<RefCell<Imgs>>,
    host: WeakEntity<CoreHost>,
    doc_id: DocId,
    /// Где документ: окно или область панели (события — своему `DocView`).
    spot: Spot,
    words: Words,
}

/// Шрифт, цвет и фон по умолчанию для текста блока.
#[derive(Clone)]
struct Base {
    font: Font,
    color: Hsla,
}

/// Подпись одного текста: строка, отрезки и куски для мыши.
#[derive(Default)]
pub(super) struct TextOut {
    pub text: String,
    pub runs: Vec<TextRun>,
    pub segs: Vec<Seg>,
}

impl TextOut {
    fn push(&mut self, s: &str, run: TextRun, p: u32, pad: bool) {
        if s.is_empty() {
            return;
        }
        let d = u32::try_from(self.text.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(s.len()).unwrap_or(u32::MAX);
        self.text.push_str(s);
        self.runs.push(TextRun {
            len: s.len(),
            ..run
        });
        self.segs.push(Seg { d, len, p, pad });
    }
}

/// Подсветка отрезка: текущее совпадение, выделение, совпадение.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark2 {
    Cur,
    Sel,
    Hit,
}

/// Отрезок `[a, z)` по границам подсветок: куски и их подсветка.
pub(super) fn split_marks(
    a: u32,
    z: u32,
    cur: Option<&Range<u32>>,
    sel: Option<&Range<u32>>,
    hits: &[(u32, u32)],
) -> Vec<(Range<u32>, Option<Mark2>)> {
    let mut cuts = vec![a, z];
    let mut add = |s: u32, e: u32| {
        for x in [s, e] {
            if x > a && x < z {
                cuts.push(x);
            }
        }
    };
    if let Some(r) = cur {
        add(r.start, r.end);
    }
    if let Some(r) = sel {
        add(r.start, r.end);
    }
    let i = hits.partition_point(|h| h.1 <= a);
    let near = || hits[i..].iter().take_while(|h| h.0 < z);
    for h in near() {
        add(h.0, h.1);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let inside = |r: Option<&Range<u32>>, m: u32| r.is_some_and(|r| r.start <= m && m < r.end);
    cuts.windows(2)
        .map(|w| {
            let m = w[0];
            let mark = if inside(cur, m) {
                Some(Mark2::Cur)
            } else if inside(sel, m) {
                Some(Mark2::Sel)
            } else if near().any(|h| h.0 <= m && m < h.1) {
                Some(Mark2::Hit)
            } else {
                None
            };
            (w[0]..w[1], mark)
        })
        .collect()
}

fn weight(f: &Font, w: FontWeight) -> Font {
    Font {
        weight: if f.weight.0 < w.0 { w } else { f.weight },
        ..f.clone()
    }
}

/// Подпись отрезка проекции `range` блока `b` (картинки пропускаются).
fn build_text(ctx: &Ctx, b: u32, range: Range<u32>, base: &Base) -> TextOut {
    let mut out = TextOut::default();
    let doc = &ctx.doc;
    let pad_run = |bg: Hsla| TextRun {
        len: 1,
        font: ctx.ui.clone(),
        color: base.color,
        background_color: Some(bg),
        underline: None,
        strikethrough: None,
    };
    for r in doc.block_runs(b) {
        if r.image().is_some() {
            continue;
        }
        let a = r.text.start.max(range.start);
        let z = r.text.end.min(range.end);
        if a >= z {
            continue;
        }
        let st = r.style;
        let mono = st.has(RunStyle::CODE) || st.has(RunStyle::KBD) || st.has(RunStyle::RAW);
        let mut font = if mono {
            Font {
                weight: base.font.weight,
                ..ctx.mono.clone()
            }
        } else {
            base.font.clone()
        };
        if st.has(RunStyle::BOLD) {
            font = weight(&font, FontWeight::SEMIBOLD);
        }
        if st.has(RunStyle::ITALIC) {
            font.style = FontStyle::Italic;
        }
        let link = r.link();
        let color = if link.is_some() || st.has(RunStyle::FOOTNOTE) {
            ctx.p.accent
        } else if st.has(RunStyle::RAW) || st.has(RunStyle::STRIKE) {
            ctx.p.secondary
        } else {
            base.color
        };
        let chip = (st.has(RunStyle::CODE) && !st.has(RunStyle::RAW)) || st.has(RunStyle::KBD);
        let bg = chip.then_some(ctx.p.header);
        let underline = (link.is_some() && link == ctx.hover).then_some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(color),
            wavy: false,
        });
        let strike = st.has(RunStyle::STRIKE).then_some(StrikethroughStyle {
            thickness: px(1.0),
            color: Some(ctx.p.secondary),
        });
        if chip && a == r.text.start {
            out.push(" ", pad_run(ctx.p.header), a, true);
        }
        for (sub, mark) in split_marks(a, z, ctx.cur.as_ref(), ctx.sel.as_ref(), &ctx.hits) {
            let (bg, color) = match mark {
                Some(Mark2::Cur) => (Some(ctx.p.accent_fill), ctx.p.panel),
                Some(Mark2::Sel) => (Some(ctx.p.sel), color),
                Some(Mark2::Hit) => (Some(ctx.p.hit), color),
                None => (bg, color),
            };
            let s = &doc.text[sub.start as usize..sub.end as usize];
            out.push(
                s,
                TextRun {
                    len: 0,
                    font: font.clone(),
                    color,
                    background_color: bg,
                    underline,
                    strikethrough: strike,
                },
                sub.start,
                false,
            );
        }
        if chip && z == r.text.end {
            out.push(" ", pad_run(ctx.p.header), z, true);
        }
    }
    out
}

/// Текст отрезка проекции: `StyledText` и запись в реестр мыши.
fn text_el(ctx: &Ctx, b: u32, range: Range<u32>, base: &Base) -> AnyElement {
    let out = build_text(ctx, b, range, base);
    let st = StyledText::new(SharedString::from(out.text)).with_runs(out.runs);
    let ready = Rc::new(Cell::new(false));
    ctx.reg.borrow_mut().push(HitText {
        layout: st.layout().clone(),
        segs: Rc::from(out.segs),
        block: b,
        ready: ready.clone(),
    });
    div()
        .relative()
        .min_w_0()
        .child(st)
        .child(
            canvas(move |_, _, _| ready.set(true), |_, (), _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size(px(0.0)),
        )
        .into_any_element()
}

/// Текст блока с картинками в строке (логотип в заголовке, значки): куски текста и
/// картинки в ряд с переносом.
fn flow(ctx: &Ctx, b: u32, range: Range<u32>, base: &Base, max_w: f32, align: Align) -> AnyElement {
    let doc = &ctx.doc;
    let imgs: Vec<(u32, Range<u32>)> = doc
        .block_runs(b)
        .iter()
        .filter_map(|r| r.image().map(|i| (i, r.text.clone())))
        .filter(|(_, t)| t.start >= range.start && t.end <= range.end)
        .collect();
    if imgs.is_empty() {
        return text_el(ctx, b, range, base);
    }
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(6.0))
        .when(align == Align::Center, Div::justify_center)
        .when(align == Align::Right, Div::justify_end);
    let mut at = range.start;
    for (i, t) in imgs {
        if t.start > at && !doc.text[at as usize..t.start as usize].trim().is_empty() {
            row = row.child(text_el(ctx, b, at..t.start, base));
        }
        row = row.child(image_el(ctx, i, max_w));
        at = t.end;
    }
    if at < range.end && !doc.text[at as usize..range.end as usize].trim().is_empty() {
        row = row.child(text_el(ctx, b, at..range.end, base));
    }
    row.into_any_element()
}

/// Размер картинки: атрибуты, размер файла, не шире колонки.
pub(super) fn fit(
    w: Option<u32>,
    h: Option<u32>,
    nat: Option<(f32, f32)>,
    max_w: f32,
) -> Option<(f32, f32)> {
    #[allow(clippy::cast_precision_loss, reason = "px картинки")]
    let (w, h) = (w.map(|v| v as f32), h.map(|v| v as f32));
    let (w, h) = match (w, h, nat) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((nw, nh))) if nw > 0.0 => (w, w * nh / nw),
        (None, Some(h), Some((nw, nh))) if nh > 0.0 => (h * nw / nh, h),
        (None, None, Some(n)) => n,
        _ => return None,
    };
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let max_w = max_w.max(16.0);
    Some(if w > max_w {
        (max_w, h * max_w / w)
    } else {
        (w, h)
    })
}

/// Плашка не показанной картинки: значок, alt, причина.
fn plaque(ctx: &Ctx, alt: &str, issue: ImageIssue) -> AnyElement {
    let reason = ctx.words.reason(issue);
    let reason = if alt.is_empty() {
        reason
    } else {
        format!("\u{B7} {reason}")
    };
    div()
        .h(px(24.0))
        .px(px(8.0))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .border_1()
        .border_dashed()
        .border_color(ctx.p.secondary)
        .bg(ctx.p.header)
        .rounded(px(4.0))
        .text_size(px(12.0))
        .line_height(px(16.0))
        .whitespace_nowrap()
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(14.0))
                .text_color(ctx.p.secondary)
                .child(SharedString::new_static(ICON_PHOTO)),
        )
        .when(!alt.is_empty(), |d| {
            d.child(
                div()
                    .text_color(ctx.p.text)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SharedString::from(alt.to_owned())),
            )
        })
        .child(
            div()
                .text_color(ctx.p.secondary)
                .child(SharedString::from(reason)),
        )
        .into_any_element()
}

fn skeleton(ctx: &Ctx, w: f32) -> Div {
    div()
        .h(px(24.0))
        .w(px(w))
        .flex_none()
        .rounded(px(4.0))
        .bg(ctx.p.skeleton)
}

/// `data:` URI → байты картинки (base64 или %-кодированный текст, как SVG).
pub(super) fn data_uri(src: &str) -> Option<(ImageFormat, Vec<u8>)> {
    let rest = src.trim().strip_prefix("data:")?;
    let (meta, body) = rest.split_once(',')?;
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let b64 = parts.any(|p| p.trim().eq_ignore_ascii_case("base64"));
    let bytes = if b64 { base64(body)? } else { percent(body) };
    let fmt = ImageFormat::from_mime_type(&mime).or_else(|| sniff(&bytes))?;
    Some((fmt, bytes))
}

fn sniff(b: &[u8]) -> Option<ImageFormat> {
    Some(match b {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [0xFF, 0xD8, ..] => ImageFormat::Jpeg,
        [b'G', b'I', b'F', ..] => ImageFormat::Gif,
        [b'B', b'M', ..] => ImageFormat::Bmp,
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => ImageFormat::Webp,
        _ if b.trim_ascii_start().starts_with(b"<") => ImageFormat::Svg,
        _ => return None,
    })
}

/// Base64 (стандартный и URL-алфавит, без переносов строк).
pub(super) fn base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        }))
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xFF).unwrap_or(0));
        }
    }
    Some(out)
}

fn percent(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Загрузка картинки для `img`: результат кэша ассетов GPUI.
type Loader =
    Rc<dyn Fn(&mut Window, &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>>>;

/// Картинка: `img` с путём (грузит GPUI в фоне), `data:`, скелетон или плашка.
fn image_el(ctx: &Ctx, idx: u32, max_w: f32) -> AnyElement {
    let Some(im) = ctx.doc.images.get(idx as usize) else {
        return div().into_any_element();
    };
    if ctx.imgs.borrow().failed.contains(&idx) {
        return plaque(ctx, &im.alt, ImageIssue::Unsupported);
    }
    let cache = ctx.imgs.clone();
    let loader: Loader = match &im.state {
        ImageState::Blocked(issue) => return plaque(ctx, &im.alt, *issue),
        ImageState::Pending => return skeleton(ctx, 120.0_f32.min(max_w)).into_any_element(),
        ImageState::File(p) => {
            let res = Resource::Path(Arc::from(p.as_path()));
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let r = window.use_asset::<ImgResourceLoader>(&res, cx);
                match &r {
                    None => PENDING.with(|n| n.set(n.get() + 1)),
                    Some(Ok(data)) => note_size(&cache, idx, data, window),
                    Some(Err(_)) => {
                        if cache.borrow_mut().failed.insert(idx) {
                            window.refresh();
                        }
                    }
                }
                r
            })
        }
        ImageState::Data => {
            let data = cache
                .borrow_mut()
                .data
                .entry(idx)
                .or_insert_with(|| {
                    data_uri(&im.src).map(|(f, b)| Arc::new(Image::from_bytes(f, b)))
                })
                .clone();
            let Some(data) = data else {
                return plaque(ctx, &im.alt, ImageIssue::Unsupported);
            };
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let r = data.clone().use_render_image(window, cx);
                if let Some(d) = &r {
                    note_size(&cache, idx, d, window);
                }
                r.map(Ok)
            })
        }
    };
    let nat = ctx.imgs.borrow().sizes.get(&idx).copied();
    let src = move |w: &mut Window, cx: &mut App| loader(w, cx);
    match fit(im.width, im.height, nat, max_w) {
        Some((w, h)) => img(src).w(px(w)).h(px(h)).flex_none().into_any_element(),
        // Размер ещё неизвестен: скелетон, под ним — картинка нулевого размера (грузится).
        None => skeleton(ctx, 120.0_f32.min(max_w))
            .relative()
            .child(img(src).absolute().size(px(0.0)))
            .into_any_element(),
    }
}

fn note_size(cache: &Rc<RefCell<Imgs>>, idx: u32, data: &RenderImage, window: &mut Window) {
    let s = data.size(0);
    #[allow(clippy::cast_precision_loss, reason = "px картинки")]
    let s = (s.width.0 as f32, s.height.0 as f32);
    if cache.borrow_mut().sizes.insert(idx, s) != Some(s) {
        window.refresh();
    }
}

/// Размеры заголовка: кегль, строка, отступ сверху, граница снизу (поле до неё).
fn heading_metrics(level: u8) -> (f32, f32, f32, Option<f32>) {
    match level {
        1 => (26.0, 34.0, 22.0, Some(8.0)),
        2 => (20.0, 28.0, 22.0, Some(6.0)),
        3 => (16.0, 24.0, 18.0, None),
        4 => (14.0, 22.0, 18.0, None),
        _ => (13.0, 20.0, 18.0, None),
    }
}

/// Отступ после элемента: абзацы 12, между пунктами списка — 0.
fn gap_after(ctx: &Ctx, ix: usize) -> f32 {
    let u = &ctx.units[ix];
    let kind = &ctx.doc.blocks[u.block as usize].kind;
    let next = ctx
        .units
        .get(ix + 1)
        .map(|n| &ctx.doc.blocks[n.block as usize].kind);
    match kind {
        BlockKind::Code { .. } | BlockKind::Table(_) if u.part + 1 < u.parts => 0.0,
        BlockKind::Item { .. } if matches!(next, Some(BlockKind::Item { .. })) => 0.0,
        BlockKind::Rule => 0.0,
        BlockKind::Summary | BlockKind::Details { .. } => 8.0,
        _ => 12.0,
    }
}

/// Элемент списка `ix`.
#[allow(clippy::too_many_lines, reason = "вид блока — одна таблица")]
fn render(ctx: &Ctx, ix: usize) -> AnyElement {
    let Some(u) = ctx.units.get(ix).cloned() else {
        return div().into_any_element();
    };
    let doc = &ctx.doc;
    let b = u.block;
    let blk = &doc.blocks[b as usize];
    let k = ctx.k;
    let quoted = doc
        .ancestors(b)
        .any(|a| doc.blocks[a as usize].kind == BlockKind::Quote);
    let color = if quoted { ctx.p.secondary } else { ctx.p.text };
    let base = Base {
        font: ctx.ui.clone(),
        color,
    };
    let lh = (22.0 * k).round();
    // Ширина текста с учётом отступов предков.
    let depth_w = doc
        .ancestors(b)
        .map(|a| match doc.blocks[a as usize].kind {
            BlockKind::Quote => 17.0,
            BlockKind::Item { .. } | BlockKind::FootnoteDef { .. } | BlockKind::Details { .. } => {
                INDENT
            }
            _ => 0.0,
        })
        .sum::<f32>();
    let max_w = (ctx.width - depth_w).max(40.0);
    let text_box = |el: AnyElement| div().text_size(px(ctx.f)).line_height(px(lh)).child(el);
    let gap = gap_after(ctx, ix);
    let content: Div = match &blk.kind {
        BlockKind::Heading(level) => {
            let (fs, flh, top, rule) = heading_metrics(*level);
            let base = Base {
                font: weight(&ctx.ui, FontWeight::SEMIBOLD),
                color: if *level >= 6 { ctx.p.secondary } else { color },
            };
            let el = flow(ctx, b, blk.text.clone(), &base, max_w, blk.align);
            let inner = div()
                .text_size(px((fs * k).round()))
                .line_height(px((flh * k).round()))
                .font_weight(FontWeight::SEMIBOLD)
                .when(blk.align == Align::Center, Div::text_center)
                .when(blk.align == Align::Right, Div::text_right)
                .child(el)
                .when_some(rule, |d, pb| {
                    d.pb(px(pb)).border_b_1().border_color(ctx.p.border)
                });
            // Отступ сверху — вместе с полем 12 под прошлым блоком.
            div()
                .when(ix > 0, |d| d.pt(px((top * k - 12.0).max(0.0).round())))
                .pb(px(gap))
                .child(inner)
        }
        BlockKind::Para => text_box(flow(ctx, b, blk.text.clone(), &base, max_w, blk.align))
            .when(blk.align == Align::Center, Div::text_center)
            .when(blk.align == Align::Right, Div::text_right)
            .pb(px(gap)),
        BlockKind::Item { mark } => {
            let nested = doc
                .ancestors(b)
                .filter(|a| matches!(doc.blocks[*a as usize].kind, BlockKind::Item { .. }))
                .count();
            let done = *mark == Mark::Task(true);
            let base = Base {
                font: ctx.ui.clone(),
                color: if done { ctx.p.secondary } else { color },
            };
            let marker = marker(ctx, *mark, nested, lh);
            div()
                .flex()
                .items_start()
                .min_h(px(24.0 * k))
                .py(px(1.0))
                .pb(px(gap + 1.0))
                .child(marker)
                .child(
                    text_box(flow(
                        ctx,
                        b,
                        blk.text.clone(),
                        &base,
                        max_w - INDENT,
                        blk.align,
                    ))
                    .ml(px(INDENT - MARK_W))
                    .flex_1()
                    .min_w_0(),
                )
        }
        BlockKind::Code { .. } => {
            let first = u.part == 0;
            let last = u.part + 1 == u.parts;
            let el = text_el(
                ctx,
                b,
                u.sub.clone(),
                &Base {
                    font: ctx.mono.clone(),
                    color,
                },
            );
            div().pb(px(gap)).child(
                div()
                    .id(SharedString::from(format!("md-code-{b}-{}", u.part)))
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .bg(ctx.p.header)
                    .border_color(ctx.p.border)
                    .border_l_1()
                    .border_r_1()
                    .px(px(14.0))
                    .when(first, |d| d.border_t_1().rounded_t(px(6.0)).pt(px(10.0)))
                    .when(last, |d| d.border_b_1().rounded_b(px(6.0)).pb(px(10.0)))
                    .child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .text_size(px(12.5 * k))
                            .line_height(px((19.0 * k).round()))
                            .child(el),
                    ),
            )
        }
        BlockKind::Table(_) => div().pb(px(gap)).child(table(ctx, &u, max_w, lh)),
        BlockKind::Rule => div().py(px(20.0)).child(div().h(px(2.0)).bg(ctx.p.border)),
        BlockKind::Image(i) => div()
            .flex()
            .when(blk.align == Align::Center, Div::justify_center)
            .when(blk.align == Align::Right, Div::justify_end)
            .pb(px(gap))
            .child(image_el(ctx, *i, max_w)),
        BlockKind::Details { .. } => div().pb(px(gap)).child(details_head(ctx, b, None, lh)),
        BlockKind::Summary => {
            let details = blk.parent.unwrap_or(b);
            div()
                .pb(px(gap))
                .child(details_head(ctx, details, Some(b), lh))
        }
        BlockKind::RawHtml => {
            let base = Base {
                font: ctx.mono.clone(),
                color: ctx.p.secondary,
            };
            div()
                .text_size(px(12.5 * k))
                .line_height(px((19.0 * k).round()))
                .pb(px(gap))
                .child(text_el(ctx, b, blk.text.clone(), &base))
        }
        BlockKind::Quote | BlockKind::FootnoteDef { .. } | BlockKind::Row { .. } => {
            text_box(flow(ctx, b, blk.text.clone(), &base, max_w, blk.align)).pb(px(gap))
        }
    };
    // Первый блок сноски — с её номером в колонке маркера.
    let foot = blk.parent.and_then(|p| match doc.blocks[p as usize].kind {
        BlockKind::FootnoteDef { n } if p + 1 == b => Some(n),
        _ => None,
    });
    let mut el = match foot {
        Some(n) => div()
            .flex()
            .items_start()
            .child(marker(ctx, Mark::Num(n), 0, lh))
            .child(content.ml(px(INDENT - MARK_W)).flex_1().min_w_0()),
        None => content,
    };
    let mut child = b;
    for a in doc.ancestors(b) {
        el = match doc.blocks[a as usize].kind {
            BlockKind::Quote => div()
                .flex()
                .child(div().w(px(3.0)).flex_none().bg(ctx.p.border))
                .child(div().pl(px(14.0)).flex_1().min_w_0().child(el)),
            BlockKind::Item { .. } => div().pl(px(INDENT)).child(el),
            BlockKind::FootnoteDef { .. } if foot.is_none() || child != b => {
                div().pl(px(INDENT)).child(el)
            }
            BlockKind::Details { .. } if doc.summary_of(a) != Some(child) => {
                div().pl(px(INDENT)).child(el)
            }
            _ => el,
        };
        child = a;
    }
    div()
        .w_full()
        .pl(px(ctx.left))
        .when(ix == 0, |d| d.pt(px(PAD_TOP)))
        .child(div().w(px(ctx.width)).child(el))
        .into_any_element()
}

/// Маркер пункта в колонке 14: «•», «◦», «N.», флажок задачи.
fn marker(ctx: &Ctx, mark: Mark, nested: usize, lh: f32) -> Div {
    let col = div()
        .w(px(MARK_W))
        .h(px(lh))
        .flex_none()
        .flex()
        .items_center()
        .text_color(ctx.p.secondary)
        .text_size(px(ctx.f))
        .whitespace_nowrap();
    match mark {
        Mark::Bullet => {
            col.justify_center()
                .child(SharedString::new_static(if nested.is_multiple_of(2) {
                    "\u{2022}"
                } else {
                    "\u{25E6}"
                }))
        }
        Mark::Num(n) => col.justify_end().child(SharedString::from(format!("{n}."))),
        Mark::Task(done) => col.justify_center().child(
            div()
                .size(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.0))
                .when(done, |d| d.bg(ctx.p.accent_fill))
                .when(!done, |d| d.border_1().border_color(ctx.p.secondary))
                .when(done, |d| {
                    d.child(
                        div()
                            .font_family(ICONS)
                            .text_size(px(10.0))
                            .text_color(ctx.p.panel)
                            .child(SharedString::new_static(ICON_CHECK)),
                    )
                }),
        ),
    }
}

/// Подпись `<details>`: шеврон, текст `<summary>` или «Подробнее»; клик — `MdToggle`.
fn details_head(ctx: &Ctx, details: u32, summary: Option<u32>, lh: f32) -> AnyElement {
    let open = is_open(&ctx.doc, &ctx.toggled, details);
    let label = match summary {
        Some(s) => {
            let base = Base {
                font: ctx.ui.clone(),
                color: ctx.p.text,
            };
            let r = ctx.doc.blocks[s as usize].text.clone();
            flow(ctx, s, r, &base, ctx.width, Align::Left)
        }
        None => div().child(ctx.words.details.clone()).into_any_element(),
    };
    let host = ctx.host.clone();
    let doc_id = ctx.doc_id;
    let spot = ctx.spot;
    div()
        .id(SharedString::from(format!("md-details-{details}")))
        .min_h(px(24.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(|d| d.bg(ctx.p.hover))
        .text_size(px(ctx.f))
        .line_height(px(lh))
        .child(
            div()
                .w(px(MARK_W))
                .flex_none()
                .flex()
                .justify_center()
                .font_family(ICONS)
                .text_size(px(12.0))
                .text_color(ctx.p.secondary)
                .child(SharedString::new_static(if open {
                    ICON_OPEN
                } else {
                    ICON_CLOSED
                })),
        )
        .child(
            div()
                .ml(px(INDENT - MARK_W))
                .flex_1()
                .min_w_0()
                .child(label),
        )
        .on_click(move |_, window, cx| {
            let _ = host.update(cx, |h, cx| {
                if let Some(v) = h.features.get_mut::<Viewer>() {
                    v.md_at(spot).out.push(InboxMsg::MdToggle {
                        doc: doc_id,
                        block: details,
                    });
                }
                h.sync_inbox(window);
                cx.notify();
            });
        })
        .into_any_element()
}

/// Ячейки строки таблицы: отрезки проекции между `\t`.
pub(super) fn cells(text: &str, start: u32) -> Vec<Range<u32>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for part in text.split('\t') {
        let s = start + u32::try_from(at).unwrap_or(0);
        out.push(s..s + u32::try_from(part.len()).unwrap_or(0));
        at += part.len() + 1;
    }
    out
}

/// Таблица (часть до [`CHUNK`] строк): колонки по содержимому, своя горизонтальная
/// прокрутка, если шире колонки.
fn table(ctx: &Ctx, u: &Unit, max_w: f32, lh: f32) -> AnyElement {
    let doc = &ctx.doc;
    let aligns = doc.table_align(u.block);
    let rows: Vec<(u32, bool, Vec<Range<u32>>)> = u
        .sub
        .clone()
        .filter_map(|r| match doc.blocks[r as usize].kind {
            BlockKind::Row { head } => Some((
                r,
                head,
                cells(doc.block_text(r), doc.blocks[r as usize].text.start),
            )),
            _ => None,
        })
        .collect();
    let ncols = rows.iter().map(|r| r.2.len()).max().unwrap_or(0);
    let nrows = rows.len();
    let mut grid = div()
        .flex()
        .flex_none()
        .border_1()
        .border_color(ctx.p.border)
        .rounded(px(6.0))
        .overflow_hidden();
    for c in 0..ncols {
        let align = aligns.get(c).copied().unwrap_or_default();
        let mut col = div()
            .flex()
            .flex_col()
            .flex_none()
            .when(c > 0, |d| d.border_l_1().border_color(ctx.p.border));
        for (i, (r, head, cs)) in rows.iter().enumerate() {
            let base = Base {
                font: if *head {
                    weight(&ctx.ui, FontWeight::SEMIBOLD)
                } else {
                    ctx.ui.clone()
                },
                color: ctx.p.text,
            };
            let el = cs
                .get(c)
                .map(|range| text_el(ctx, *r, range.clone(), &base));
            col = col.child(
                div()
                    .h(px(lh + 10.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .when(align == Align::Center, Div::justify_center)
                    .when(align == Align::Right, Div::justify_end)
                    .when(*head, |d| d.bg(ctx.p.header))
                    .when(i + 1 < nrows, |d| d.border_b_1().border_color(ctx.p.border))
                    .children(el),
            );
        }
        grid = grid.child(col);
    }
    div()
        .id(SharedString::from(format!(
            "md-table-{}-{}",
            u.block, u.part
        )))
        .max_w(px(max_w))
        .flex()
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .text_size(px(ctx.f))
        .line_height(px(lh))
        .child(grid)
        .into_any_element()
}

/// Ключ раскладки: документ, раскрытые `<details>`, шрифт, ширина.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    doc: usize,
    toggled: u64,
    font: f32,
    width: f32,
}

fn toggled_hash(t: &FxHashSet<u64>) -> u64 {
    t.iter().fold(t.len() as u64, |a, k| {
        a ^ k.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(17)
    })
}

/// Позиция handler'а, с которой список уже совпадает.
#[derive(Clone, Debug, PartialEq)]
struct Synced {
    top: u32,
    frac: u32,
    hit: Option<Range<u32>>,
}

/// Совпадения на экране (кэш по образцу и отрезку проекции).
struct HitsCache {
    key: (usize, u32, u32, usize),
    list: Rc<[(u32, u32)]>,
}

/// Выделение мышью (пустое — снято), пока handler его не подтвердил.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pick(Option<Range<u32>>);

/// Наведённая ссылка и где мышь.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Hover {
    link: u32,
    at: Point<Pixels>,
}

/// Состояние документа в фиче вьювера.
#[derive(Default)]
pub(super) struct DocView {
    list: Option<ListState>,
    key: Option<Key>,
    units: Rc<[Unit]>,
    synced: Option<Synced>,
    /// Доля верхнего блока, которую ещё не применить: высота блока неизвестна.
    pending_frac: Option<f32>,
    /// Верх списка, о котором handler уже знает.
    seen: Option<(usize, f32)>,
    /// События handler'у (`MdTop`, `MdToggle`, `MdSelect`) до следующего прохода Core.
    pub(super) out: Vec<InboxMsg>,
    reg: Rc<RefCell<Vec<HitText>>>,
    imgs: Rc<RefCell<Imgs>>,
    hits: Option<HitsCache>,
    /// Шрифт после Ctrl+колеса (до записи в конфиг).
    pub(super) font: Option<f32>,
    /// Выделение мышью: начало в проекции и где нажата кнопка.
    drag: Option<(u32, Point<Pixels>)>,
    /// Выделение, пока handler его не подтвердил.
    local_sel: Option<Pick>,
    hover: Option<Hover>,
    /// Текст для буфера обмена (Ctrl+C) — пишется в кадре.
    pub(super) clip: Option<String>,
    doc: Option<DocId>,
    /// Верх списка до колеса (Ctrl+колесо список не двигает).
    before_wheel: Option<ListOffset>,
    /// Видимые элементы прошлого кадра.
    visible: Range<usize>,
}

/// Шрифт документа из конфига (0 — 14).
pub(super) fn cfg_font(s: &AppState) -> f32 {
    let n = s
        .ext
        .get::<ViewState>()
        .map_or(0, |v| v.viewer.md_font_size);
    if n == 0 { FONT } else { f32::from(n) }
}

impl DocView {
    pub(super) fn clear(&mut self) {
        if self.list.is_none() && self.doc.is_none() {
            return;
        }
        let font = self.font;
        *self = DocView::default();
        self.font = font;
    }

    pub(super) fn font_px(&self, s: &AppState) -> f32 {
        self.font.unwrap_or_else(|| cfg_font(s))
    }

    /// Проход Core и кадр: новый верх списка (колесо, полоса) — `MdTop`; накопленные
    /// события — handler'у. `true` — что-то отправлено.
    pub(super) fn inbox(&mut self, d: &ViewerDoc, v: &ViewState) -> bool {
        self.poll(d);
        if self.out.is_empty() {
            return false;
        }
        let tx = v.inbox.sender();
        for m in self.out.drain(..) {
            tx.send(m);
        }
        true
    }

    fn poll(&mut self, d: &ViewerDoc) {
        let (Some(st), Some(md)) = (self.list.as_ref(), d.md.as_ref()) else {
            return;
        };
        if self.synced.is_none() || self.pending_frac.is_some() || self.doc != Some(d.id) {
            return;
        }
        let top = st.logical_scroll_top();
        let seen = (top.item_ix, f32::from(top.offset_in_item));
        if self.seen == Some(seen) {
            return;
        }
        self.seen = Some(seen);
        let Some(u) = self.units.get(top.item_ix) else {
            return;
        };
        let h = st
            .bounds_for_item(top.item_ix)
            .map_or(0.0, |b| f32::from(b.size.height));
        #[allow(clippy::cast_precision_loss, reason = "части блока — единицы")]
        let frac = {
            let f = if h > 0.0 {
                (seen.1 / h).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ((u.part as f32 + f) / u.parts.max(1) as f32).clamp(0.0, 1.0)
        };
        let same = md.top == u.block && (md.top_frac - frac).abs() < f32::EPSILON;
        self.synced = Some(Synced {
            top: u.block,
            frac: frac.to_bits(),
            hit: md.hit.clone(),
        });
        if !same {
            self.out.push(InboxMsg::MdTop {
                doc: d.id,
                block: u.block,
                frac,
            });
        }
    }

    /// Список догоняет позицию handler'а (клавиши, совпадение F7, Документ ↔ Текст).
    fn sync(&mut self, md: &MdState) {
        let Some(st) = self.list.as_ref() else {
            return;
        };
        if self.units.is_empty() {
            return;
        }
        let want = Synced {
            top: md.top,
            frac: md.top_frac.to_bits(),
            hit: md.hit.clone(),
        };
        let units = &self.units;
        let at = |frac: f32| -> (usize, f32) {
            let ix = unit_of(units, md.top);
            let u = &units[ix];
            if u.block != md.top || u.parts <= 1 {
                return (ix, frac);
            }
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "части блока — единицы"
            )]
            let (part, rest) = {
                let x = frac.clamp(0.0, 1.0) * u.parts as f32;
                let p = (x.floor() as u32).min(u.parts - 1);
                (p, x - p as f32)
            };
            (ix + part as usize, rest)
        };
        if let Some(f) = self.pending_frac {
            let (ix, rest) = at(f);
            if let Some(b) = st.bounds_for_item(ix) {
                let h = f32::from(b.size.height);
                st.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: px(rest * h),
                });
                self.pending_frac = None;
                self.seen = None;
            }
        }
        if self.synced.as_ref() == Some(&want) {
            return;
        }
        let reveal = md.reveal == Some(md.top)
            && md.hit.is_some()
            && self.synced.as_ref().is_none_or(|s| s.hit != md.hit);
        let (ix, rest) = at(md.top_frac);
        st.scroll_to(ListOffset {
            item_ix: ix,
            offset_in_item: px(0.0),
        });
        if rest > 0.0 {
            match st.bounds_for_item(ix) {
                Some(b) => st.scroll_by(b.size.height * rest),
                None => self.pending_frac = Some(md.top_frac),
            }
        }
        if reveal {
            // Совпадение — на треть экрана (S7 §3.6), не к самому верху.
            let h = f32::from(st.viewport_bounds().size.height);
            st.scroll_by(px(-(h / 3.0).round()));
        }
        self.synced = Some(want);
        self.seen = None;
    }

    /// Совпадения F7 на экране: matcher по проекции видимых блоков (без I/O).
    fn screen_hits(&mut self, d: &ViewerDoc, doc: &MdDoc, from_block: u32) -> Rc<[(u32, u32)]> {
        let none = || Rc::from(Vec::new());
        let Some(m) = d.search.as_ref().and_then(|s| s.matcher.clone()) else {
            self.hits = None;
            return none();
        };
        let from = doc
            .blocks
            .get(from_block as usize)
            .map_or(0, |b| b.text.start);
        let len = u32::try_from(doc.text.len()).unwrap_or(u32::MAX);
        let mut to = from.saturating_add(HL_MAX).min(len);
        while to < len && !doc.text.is_char_boundary(to as usize) {
            to += 1;
        }
        let key = (
            std::ptr::from_ref(doc) as usize,
            from,
            to,
            Arc::as_ptr(&m).cast::<()>() as usize,
        );
        if let Some(h) = self.hits.as_ref().filter(|h| h.key == key) {
            return h.list.clone();
        }
        let hay = &doc.text.as_bytes()[from as usize..to as usize];
        let p = m.prepare(hay);
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(r) = p.find_at(at) {
            if r.end <= r.start || out.len() >= HL_COUNT {
                break;
            }
            let s = from + u32::try_from(r.start).unwrap_or(0);
            let e = from + u32::try_from(r.end).unwrap_or(0);
            out.push((s, e));
            at = r.end;
        }
        let list: Rc<[(u32, u32)]> = Rc::from(out);
        self.hits = Some(HitsCache {
            key,
            list: list.clone(),
        });
        list
    }

    /// Выделение для кадра: своё (мышь), пока handler не подтвердил.
    fn sel(&mut self, md: &MdState) -> Option<Range<u32>> {
        match &self.local_sel {
            Some(Pick(s)) if *s == md.sel => {
                self.local_sel = None;
                md.sel.clone()
            }
            Some(Pick(s)) => s.clone(),
            None => md.sel.clone(),
        }
    }

    /// Тело экрана документа: список блоков, полоса прокрутки, подсказка ссылки.
    #[allow(
        clippy::too_many_lines,
        reason = "кадр документа: список, мышь, полоса"
    )]
    #[allow(
        clippy::too_many_arguments,
        reason = "кадр документа: место и ширина области"
    )]
    pub(super) fn body(
        &mut self,
        s: &AppState,
        d: &ViewerDoc,
        theme: &Theme,
        spot: Spot,
        area_w: f32,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Div {
        PENDING.with(|n| n.set(0));
        self.reg.borrow_mut().clear();
        if let Some(t) = self.clip.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
        }
        let p = Paint::of(theme);
        let Some(md) = d.md.as_ref() else {
            return div().flex_1().bg(p.panel);
        };
        let Some(doc) = md.doc.clone() else {
            return div().flex_1().bg(p.panel);
        };
        if self.doc != Some(d.id) {
            self.clear();
            self.doc = Some(d.id);
        }
        let f = self.font_px(s);
        let (left, width) = if spot == Spot::Window {
            let win_w = f32::from(window.viewport_size().width) - W_SCROLL;
            let col = COL_W.min(win_w);
            (
                ((win_w - col) / 2.0).max(0.0) + PAD_X,
                (col - 2.0 * PAD_X).max(80.0),
            )
        } else {
            // Область панели: вся ширина, поля 28, без полосы прокрутки.
            (PAD_X_PANEL, (area_w - 2.0 * PAD_X_PANEL).max(80.0))
        };
        let key = Key {
            doc: Arc::as_ptr(&doc) as usize,
            toggled: toggled_hash(&md.toggled),
            font: f,
            width,
        };
        if self.key.map(|k| k.doc) != Some(key.doc) {
            *self.imgs.borrow_mut() = Imgs::default();
            self.hits = None;
        }
        let st = self
            .list
            .get_or_insert_with(|| ListState::new(0, ListAlignment::Top, px(600.0)))
            .clone();
        if self.key != Some(key) {
            self.units = Rc::from(units(&doc, &md.toggled));
            st.reset_with_uniform_height(self.units.len(), px((30.0 * f / FONT).round()));
            self.key = Some(key);
            self.synced = None;
            self.seen = None;
        }
        self.sync(md);
        // Видимые элементы (статус, полоса, подсветка).
        let top = st.logical_scroll_top();
        let view_h = st.viewport_bounds().size.height;
        let mut last = top.item_ix;
        while last + 1 < self.units.len() {
            match st.bounds_for_item(last + 1) {
                Some(b) if b.top() < st.viewport_bounds().top() + view_h => last += 1,
                _ => break,
            }
        }
        self.visible = top.item_ix..last + 1;
        let from_block = self.units.get(top.item_ix).map_or(0, |u| u.block);
        let hits = self.screen_hits(d, &doc, from_block);
        let sel = self.sel(md);
        let ctx = Rc::new(Ctx {
            doc: doc.clone(),
            units: self.units.clone(),
            p,
            f,
            k: f / FONT,
            left,
            width,
            ui: window.text_style().font(),
            mono: gpui_kit::font(rows::MONO),
            sel,
            cur: md.hit.clone(),
            hits,
            hover: self.hover.map(|h| h.link),
            toggled: md.toggled.clone(),
            reg: self.reg.clone(),
            imgs: self.imgs.clone(),
            host: cx.weak_entity(),
            doc_id: d.id,
            spot,
            words: Words::new(
                s.ext
                    .get::<ViewState>()
                    .map_or(urfin_view::md::IMAGE_MAX_MB, |v| v.viewer.md_image_max_mb),
            ),
        });
        let items = list(st, move |ix, _, _| render(&ctx, ix)).size_full();
        let tip = self.hover.and_then(|h| tooltip(&doc, h, &p));
        let link_hover = self.hover.is_some();
        let doc_el = div()
            .id("md-doc")
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(p.panel)
            .when(link_hover, Styled::cursor_pointer)
            .when(!link_hover, Styled::cursor_text)
            .child(items)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |h, ev: &MouseDownEvent, w, cx| {
                    with_md(h, spot, |md, _, _| md.on_down(ev.position));
                    h.sync_inbox(w);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |h, ev: &MouseMoveEvent, w, cx| {
                let changed = with_md(h, spot, |md, d, _| md.on_move(ev, d)).unwrap_or(false);
                if changed {
                    h.sync_inbox(w);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |h, ev: &MouseUpEvent, w, cx| {
                    let cmds = with_md(h, spot, |md, d, _| md.on_up(ev, d)).unwrap_or_default();
                    h.sync_inbox(w);
                    cx.notify();
                    for c in cmds {
                        h.run(c, w, cx);
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(move |h, ev: &ScrollWheelEvent, w, cx| {
                let cmd = with_md(h, spot, |md, _, s| md.wheel(ev, s)).flatten();
                if let Some(c) = cmd {
                    h.run(c, w, cx);
                }
                cx.notify();
            }));
        let bar = (spot == Spot::Window).then(|| self.scrollbar(d, &doc, &p, cx));
        if let Some(st) = &self.list {
            self.before_wheel = Some(st.logical_scroll_top());
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(doc_el)
            .children(bar)
            .children(tip)
    }

    /// Полоса прокрутки 14: бегунок по исходнику видимых блоков, метки совпадений.
    fn scrollbar(
        &self,
        d: &ViewerDoc,
        doc: &MdDoc,
        p: &Paint,
        cx: &Context<CoreHost>,
    ) -> gpui_kit::Stateful<Div> {
        let len = d.len.max(1);
        let src = |ix: usize, end: bool| {
            self.units.get(ix).map_or(0, |u| {
                let b = &doc.blocks[u.block as usize];
                if end { b.src.end } else { b.src.start }
            })
        };
        let a = src(self.visible.start, false);
        let z = src(self.visible.end.saturating_sub(1), true).max(a);
        #[allow(clippy::cast_precision_loss, reason = "доля файла")]
        let span = (a as f32 / len as f32, (z - a) as f32 / len as f32);
        let marks: Vec<f32> = d.search.as_ref().map_or_else(Vec::new, |s| {
            #[allow(clippy::cast_precision_loss, reason = "доля файла")]
            s.marks
                .iter()
                .map(|m| {
                    let off = doc.text_to_src(u32::try_from(*m).unwrap_or(u32::MAX));
                    off as f32 / len as f32
                })
                .collect()
        });
        let thumb = Rc::new(Cell::new(None::<(f32, f32)>));
        let at = thumb.clone();
        let (bg, th_c, mark_c) = (p.panel, p.secondary.opacity(0.5), p.accent_fill);
        let bar = canvas(
            |_, _, _| {},
            move |b, (), window, _| {
                window.paint_quad(gpui_kit::fill(b, bg));
                let h = f32::from(b.size.height);
                let th = (span.1 * h).clamp(THUMB_MIN.min(h), h);
                let y0 = f32::from(b.origin.y) + (span.0 * h).min(h - th);
                at.set(Some((y0, y0 + th)));
                window.paint_quad(
                    gpui_kit::fill(
                        Bounds::new(point(b.origin.x + px(4.0), px(y0)), size(px(6.0), px(th))),
                        th_c,
                    )
                    .corner_radii(px(3.0)),
                );
                for m in &marks {
                    let y = f32::from(b.origin.y) + m * h;
                    window.paint_quad(gpui_kit::fill(
                        Bounds::new(
                            point(b.origin.x + px(3.0), px(y.floor())),
                            size(px(8.0), px(2.0)),
                        ),
                        mark_c,
                    ));
                }
            },
        )
        .size_full();
        div()
            .id("md-scroll")
            .w(px(W_SCROLL))
            .flex_none()
            .h_full()
            .child(bar)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |h, ev: &MouseDownEvent, w, cx| {
                    let Some((y0, y1)) = thumb.get() else {
                        return;
                    };
                    let y = f32::from(ev.position.y);
                    let dir = if y < y0 {
                        "page_up"
                    } else if y > y1 {
                        "page_down"
                    } else {
                        return;
                    };
                    h.run(Command::new("viewer.scroll").arg("dir", dir), w, cx);
                }),
            )
    }

    fn hit(&self, at: Point<Pixels>) -> Option<Hit> {
        hit_at(&self.reg.borrow(), at)
    }

    fn on_down(&mut self, at: Point<Pixels>) {
        let Some(id) = self.doc else {
            return;
        };
        let hit = self.hit(at);
        self.drag = hit.map(|h| (h.p, at));
        if self.local_sel != Some(Pick(None)) {
            self.local_sel = Some(Pick(None));
            self.out.push(InboxMsg::MdSelect { doc: id, sel: None });
        }
    }

    /// Мышь: выделение при нажатой кнопке, иначе наведение на ссылку. `true` — перерисовать.
    fn on_move(&mut self, ev: &MouseMoveEvent, d: &ViewerDoc) -> bool {
        let Some(id) = self.doc else {
            return false;
        };
        if ev.pressed_button == Some(MouseButton::Left) {
            let Some((a, _)) = self.drag else {
                return false;
            };
            let Some(h) = self.hit(ev.position) else {
                return false;
            };
            let sel = (h.p != a).then(|| a.min(h.p)..a.max(h.p));
            if self.local_sel.as_ref().map(|p| &p.0) == Some(&sel) {
                return false;
            }
            self.local_sel = Some(Pick(sel.clone()));
            self.out.push(InboxMsg::MdSelect { doc: id, sel });
            return true;
        }
        let doc = d.md.as_ref().and_then(|m| m.doc.clone());
        let link = doc.and_then(|doc| {
            self.hit(ev.position)
                .filter(|h| h.exact)
                .and_then(|h| link_at(&doc, h.block, h.p))
        });
        let next = link.map(|l| Hover {
            link: l,
            at: ev.position,
        });
        if next.map(|h| h.link) == self.hover.map(|h| h.link) {
            // Та же ссылка: подсказка остаётся на месте.
            return false;
        }
        self.hover = next;
        true
    }

    /// Отпускание кнопки: без сдвига — клик по ссылке. Команды Core.
    fn on_up(&mut self, ev: &MouseUpEvent, d: &ViewerDoc) -> Vec<Command> {
        let Some((_, down)) = self.drag.take() else {
            return Vec::new();
        };
        let moved = (f32::from(ev.position.x) - f32::from(down.x)).abs()
            + (f32::from(ev.position.y) - f32::from(down.y)).abs();
        if moved > 3.0 {
            return Vec::new();
        }
        let Some(doc) = d.md.as_ref().and_then(|m| m.doc.clone()) else {
            return Vec::new();
        };
        let Some(link) = self
            .hit(ev.position)
            .filter(|h| h.exact)
            .and_then(|h| link_at(&doc, h.block, h.p))
        else {
            return Vec::new();
        };
        let Some(l) = doc.links.get(link as usize) else {
            return Vec::new();
        };
        match &l.target {
            LinkTarget::Anchor { block: Some(b), .. }
            | LinkTarget::Footnote { block: Some(b), .. } => {
                self.out.push(InboxMsg::MdTop {
                    doc: d.id,
                    block: *b,
                    frac: 0.0,
                });
                Vec::new()
            }
            LinkTarget::External(url) => {
                if ev.modifiers.control {
                    crate::sys::open_url(url);
                }
                Vec::new()
            }
            // `.md` — в этом же экране (`viewer.back`), файл или папка — `viewer.reveal`.
            t @ LinkTarget::Path { .. } => t.command().into_iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Ctrl+колесо — шрифт документа (`viewer.md_font_size`); список не двигается.
    fn wheel(&mut self, ev: &ScrollWheelEvent, s: &AppState) -> Option<Command> {
        if !ev.modifiers.control {
            return None;
        }
        if let (Some(st), Some(top)) = (self.list.as_ref(), self.before_wheel) {
            st.scroll_to(top);
        }
        let dy = match ev.delta {
            gpui_kit::ScrollDelta::Lines(p) => p.y,
            gpui_kit::ScrollDelta::Pixels(p) => f32::from(p.y),
        };
        if dy.abs() < f32::EPSILON {
            return None;
        }
        let f = (self.font_px(s) + dy.signum()).clamp(8.0, 32.0);
        self.font = Some(f);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "8…32"
        )]
        let n = f.round() as u32;
        Some(
            Command::new("ui.setting")
                .arg("key", "viewer.md_font_size")
                .arg("value", n.to_string()),
        )
    }

    /// Ctrl+C — видимый текст выделения, Ctrl+A — выделить весь документ (S7 §3.10).
    pub(super) fn key(&mut self, d: &ViewerDoc, ks: &gpui_kit::Keystroke) -> bool {
        let m = &ks.modifiers;
        if !m.control || m.alt || m.shift {
            return false;
        }
        match ks.key.as_str() {
            "c" | "insert" => {
                self.copy(d);
                true
            }
            "a" => {
                self.select_all(d);
                true
            }
            _ => false,
        }
    }

    /// Видимый текст выделения — в буфер обмена (пишется в кадре, `clip`).
    pub(super) fn copy(&mut self, d: &ViewerDoc) {
        let local = match &self.local_sel {
            Some(Pick(sel)) => d.md.as_ref().and_then(|md| {
                let doc = md.doc.as_ref()?;
                let r = sel.clone()?;
                doc.text.get(r.start as usize..r.end as usize)
            }),
            None => d.md_copy_text(false),
        };
        if let Some(t) = local.filter(|t| !t.is_empty()) {
            self.clip = Some(t.to_owned());
        }
    }

    /// Выделить весь документ (проекцию).
    pub(super) fn select_all(&mut self, d: &ViewerDoc) {
        let Some(len) = d.md_doc().map(|doc| doc.text.len()) else {
            return;
        };
        let sel = Some(0..u32::try_from(len).unwrap_or(u32::MAX));
        self.local_sel = Some(Pick(sel.clone()));
        self.out.push(InboxMsg::MdSelect { doc: d.id, sel });
    }

    /// Зоны статуса документа: «Стр A–B из N · P %» по исходнику видимых блоков; итог
    /// поиска или «N картинок не показано»; размер и EOL.
    pub(super) fn zones(&self, d: &ViewerDoc) -> [String; 3] {
        let i18n = urfin_i18n::current();
        let Some(doc) = d.md_doc() else {
            return [String::new(), String::new(), rows::size_eol(d)];
        };
        let sep = urfin_view::index::Sep::of(&d.enc, d.eol);
        let line = |off: u64| {
            d.index
                .line_of_in(&d.pages, off, sep)
                .unwrap_or_else(|_| d.index.line_of(off))
                .get()
        };
        let src = |ix: usize, end: bool| {
            self.units.get(ix).map(|u| {
                let b = &doc.blocks[u.block as usize];
                if end { b.src.end } else { b.src.start }
            })
        };
        let left = match (
            src(self.visible.start, false),
            src(self.visible.end.saturating_sub(1), true),
        ) {
            (Some(a), Some(z)) if !self.units.is_empty() => {
                let mut args = FluentArgs::new();
                let from = line(a);
                let to = line(z.saturating_sub(1).max(a)).max(from);
                args.set("from", from + 1);
                args.set("to", to + 1);
                args.set("total", rows::total_lines(d));
                args.set("pct", rows::pct(a, d.len));
                i18n.fmt("ui-view-lines", &args)
            }
            _ => String::new(),
        };
        let mut middle = rows::search_zone(d);
        if middle.is_empty() {
            middle = hidden_text(doc);
        }
        [left, middle, rows::size_eol(d)]
    }
}

/// «N картинок не показано» (все из сети — «N картинок из сети не показано»).
pub(super) fn hidden_text(doc: &MdDoc) -> String {
    let n = doc.hidden_images();
    if n == 0 {
        return String::new();
    }
    let remote = doc
        .images
        .iter()
        .filter(|i| matches!(i.state, ImageState::Blocked(_)))
        .all(|i| i.state == ImageState::Blocked(ImageIssue::Network));
    let mut a = FluentArgs::new();
    a.set("count", n as u64);
    urfin_i18n::current().fmt(
        if remote {
            "ui-md-hidden-remote"
        } else {
            "ui-md-hidden"
        },
        &a,
    )
}

/// Подсказка наведённой ссылки: цель, у внешней — «Ctrl+клик — открыть в браузере».
fn tooltip(doc: &MdDoc, h: Hover, p: &Paint) -> Option<AnyElement> {
    let l = doc.links.get(h.link as usize)?;
    let hint = matches!(l.target, LinkTarget::External(_))
        .then(|| urfin_i18n::current().get("ui-md-link-open"));
    let body = div()
        .max_w(px(480.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(p.border)
        .bg(p.overlay)
        .text_size(px(12.0))
        .line_height(px(16.0))
        .text_color(p.text)
        .child(SharedString::from(l.dest.clone()))
        .children(hint.map(|t| div().text_color(p.secondary).child(SharedString::from(t))));
    Some(
        deferred(
            anchored()
                .position(point(h.at.x + px(12.0), h.at.y + px(20.0)))
                .snap_to_window()
                .child(body),
        )
        .with_priority(1)
        .into_any_element(),
    )
}

/// Документ места `spot` и его состояние в фиче: `f(view, doc, state)`.
fn with_md<R>(
    h: &mut CoreHost,
    spot: Spot,
    f: impl FnOnce(&mut DocView, &ViewerDoc, &AppState) -> R,
) -> Option<R> {
    let core = h.core.as_ref()?;
    let s = core.state();
    let id = super::doc_at(s, spot)?;
    let d = s.ext.get::<ViewState>()?.viewer(id)?;
    if !d.md_mode() {
        return None;
    }
    let v = h.features.get_mut::<Viewer>()?;
    Some(f(v.md_at(spot), d, s))
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn units_cut_code_and_collapse_details() {
        let mut code = String::from("```\n");
        for i in 0..150 {
            let _ = writeln!(code, "line {i}");
        }
        code.push_str("```\n\n<details>\n<summary>More</summary>\n\nHidden text\n\n</details>\n");
        let doc = urfin_view::md::parse(&code);
        let none = FxHashSet::default();
        let u = units(&doc, &none);
        let code_units: Vec<_> = u
            .iter()
            .filter(|u| matches!(doc.blocks[u.block as usize].kind, BlockKind::Code { .. }))
            .collect();
        assert_eq!(code_units.len(), 3);
        assert_eq!(code_units[0].parts, 3);
        let first = &doc.text[code_units[0].sub.start as usize..code_units[0].sub.end as usize];
        assert_eq!(first.lines().count(), CHUNK);
        assert!(first.starts_with("line 0"));
        let third = &doc.text[code_units[2].sub.start as usize..code_units[2].sub.end as usize];
        assert!(third.starts_with("line 128"), "{third:?}");
        // Свёрнутый `<details>`: только подпись.
        assert!(
            u.iter()
                .any(|u| doc.blocks[u.block as usize].kind == BlockKind::Summary)
        );
        assert!(!u.iter().any(|u| doc.block_text(u.block) == "Hidden text"));
        // Раскрытый — и тело.
        let det = doc
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Details { .. }))
            .map(|b| b.src.start)
            .unwrap();
        let open: FxHashSet<u64> = [det].into_iter().collect();
        let u2 = units(&doc, &open);
        assert!(u2.iter().any(|u| doc.block_text(u.block) == "Hidden text"));
    }

    #[test]
    fn table_rows_in_one_unit() {
        let doc = urfin_view::md::parse("| a | b |\n|---|:-:|\n| 1 | 2 |\n| 3 | 4 |\n");
        let u = units(&doc, &FxHashSet::default());
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].sub.len(), 3);
        let row = u[0].sub.start + 1;
        let c = cells(doc.block_text(row), doc.blocks[row as usize].text.start);
        assert_eq!(c.len(), 2);
        assert_eq!(&doc.text[c[1].start as usize..c[1].end as usize], "2");
        assert_eq!(doc.table_align(u[0].block)[1], Align::Center);
    }

    #[test]
    fn display_maps_back_to_projection() {
        let segs = [
            Seg {
                d: 0,
                len: 4,
                p: 10,
                pad: false,
            },
            Seg {
                d: 4,
                len: 1,
                p: 14,
                pad: true,
            },
            Seg {
                d: 5,
                len: 3,
                p: 14,
                pad: false,
            },
        ];
        assert_eq!(disp_to_proj(&segs, 0), Some(10));
        assert_eq!(disp_to_proj(&segs, 3), Some(13));
        assert_eq!(disp_to_proj(&segs, 4), Some(14));
        assert_eq!(disp_to_proj(&segs, 6), Some(15));
        assert_eq!(disp_to_proj(&segs, 9), Some(17));
    }

    #[test]
    fn marks_split_by_priority() {
        let cur = 4..6;
        let sel = 2..8;
        let parts = split_marks(0, 10, Some(&cur), Some(&sel), &[(8, 9)]);
        let got: Vec<_> = parts.iter().map(|(r, m)| (r.start, r.end, *m)).collect();
        assert_eq!(
            got,
            vec![
                (0, 2, None),
                (2, 4, Some(Mark2::Sel)),
                (4, 6, Some(Mark2::Cur)),
                (6, 8, Some(Mark2::Sel)),
                (8, 9, Some(Mark2::Hit)),
                (9, 10, None),
            ]
        );
    }

    #[test]
    fn image_size_fits_column() {
        assert_eq!(fit(Some(48), None, None, 700.0), None);
        assert_eq!(
            fit(Some(48), None, Some((256.0, 256.0)), 700.0),
            Some((48.0, 48.0))
        );
        assert_eq!(
            fit(Some(1400), Some(200), None, 700.0),
            Some((700.0, 100.0))
        );
        assert_eq!(
            fit(None, None, Some((300.0, 150.0)), 700.0),
            Some((300.0, 150.0))
        );
        assert_eq!(
            fit(None, Some(50), Some((200.0, 100.0)), 700.0),
            Some((100.0, 50.0))
        );
    }

    #[test]
    fn data_uris_decode() {
        assert_eq!(base64("TWFu").as_deref(), Some(&b"Man"[..]));
        assert_eq!(base64("TWE=").as_deref(), Some(&b"Ma"[..]));
        let (f, b) = data_uri("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(f, ImageFormat::Png);
        assert_eq!(&b[..4], &[0x89, b'P', b'N', b'G']);
        let (f, b) = data_uri("data:image/svg+xml,%3Csvg%3E%3C/svg%3E").unwrap();
        assert_eq!(f, ImageFormat::Svg);
        assert_eq!(b, b"<svg></svg>");
        assert!(data_uri("data:text/plain,hello").is_none());
    }

    #[test]
    fn external_schemes_only() {
        assert!(crate::sys::is_external("https://example.org"));
        assert!(crate::sys::is_external("mailto:a@b.c"));
        assert!(!crate::sys::is_external("file:///c:/x"));
        assert!(!crate::sys::is_external("javascript:alert(1)"));
    }
}
