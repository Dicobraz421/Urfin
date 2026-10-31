//! Строки вьювера (S7 §6): геометрия окна, видимые строки по кэшу страниц, кэш
//! `ShapedLine`, один `canvas` на поле номеров и строки, полоса прокрутки с метками,
//! зоны статуса. Чтения файла здесь нет: `rows_down`/`render_row` — по резидентным
//! страницам, на `Missing` — скелетон (страницы просит handler по `Viewport`).

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{
    Bounds, Context, Div, Font, Hsla, Pixels, ShapedLine, SharedString, TextAlign, TextRun, Window,
    canvas, div, point, px, size,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use urfin_core::Command;
use urfin_i18n::FluentArgs;
use urfin_view::index::{LineGuess, Sep};
use urfin_view::layout::{RowText, VRow, hex_geom, render_row, rows_down};
use urfin_view::search::Pattern;
use urfin_view::{LayoutSpec, Mode, ViewerDoc};

use super::super::CoreHost;
use super::super::theme::Theme;
use super::MenuKind;
use crate::tokens::{Metric, Role};

/// Моноширинный шрифт вьювера (S7 §6: Cascadia Mono, запасные — системные).
pub(super) const MONO: &str = "Cascadia Mono";
/// Шапка документа.
pub(super) const H_HEAD: f32 = 36.0;
/// Поле номеров строк (минимум).
const W_GUTTER: f32 = 52.0;
/// Полоса прокрутки.
const W_SCROLL: f32 = 14.0;
/// Отступ текста от поля номеров: текст и Hex.
const PAD_TEXT: f32 = 16.0;
const PAD_HEX: f32 = 20.0;
/// Поле справа от текста до полосы прокрутки.
const PAD_RIGHT: f32 = 8.0;
/// Номер строки — от правого края поля.
const NUM_PAD_R: f32 = 12.0;
/// Минимальная высота бегунка.
const THUMB_MIN: f32 = 24.0;
/// Подсветка совпадений — по видимым байтам не больше (S7 §3.6).
const HL_MAX: u64 = 64 << 10;
/// Совпадений на экране — не больше.
const HL_COUNT: usize = 4096;

/// Строка 22 px при шрифте 13 (макет), пропорционально размеру шрифта.
pub(super) fn row_h(font: f32) -> f32 {
    (font * 22.0 / 13.0).round()
}

fn mono() -> Font {
    gpui_kit::font(MONO)
}

/// Ширина символа моно-шрифта.
fn advance(font: f32, window: &mut Window) -> f32 {
    let run = TextRun {
        len: 1,
        font: mono(),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let w = window
        .text_system()
        .shape_line(SharedString::new_static("M"), px(font), &[run], None)
        .width;
    f32::from(w).max(1.0)
}

/// Геометрия окна текста.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Geo {
    pub font: f32,
    pub cw: f32,
    pub row_h: f32,
    /// Ширина поля номеров (смещений в Hex).
    pub gutter: f32,
    /// Отступ текста от поля.
    pub pad: f32,
    /// Линейка Hex над строками (0 — нет).
    pub ruler: f32,
    pub cols: u32,
    pub rows: u32,
}

fn digits(mut n: u64) -> u32 {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Геометрия по окну: тело — между шапкой документа и статусом с keybar (cmdline
/// скрыт, статус и keybar окна — внутри экрана вьювера); `bar` — высота открытой строки
/// «Найти» / «Перейти» над статусом.
pub(super) fn geo(d: &ViewerDoc, theme: &Theme, font: f32, bar: f32, window: &mut Window) -> Geo {
    let win = window.viewport_size();
    let cw = advance(font, window);
    let rh = row_h(font);
    let hex = d.mode == Mode::Hex;
    #[allow(clippy::cast_precision_loss, reason = "цифр — единицы")]
    let gutter = if hex {
        let off_w = if d.len > 1 << 32 { 12.0 } else { 8.0 };
        (off_w * cw + 30.0).round()
    } else {
        let n = digits(d.index.lines.max(999)) as f32;
        W_GUTTER.max((n * cw + 26.0).round())
    };
    let pad = if hex { PAD_HEX } else { PAD_TEXT };
    let ruler = if hex { rh } else { 0.0 };
    let w = f32::from(win.width) - gutter - pad - W_SCROLL - PAD_RIGHT;
    let h = f32::from(win.height)
        - theme.m(Metric::HTitlebar)
        - H_HEAD
        - theme.m(Metric::HStatus)
        - theme.m(Metric::HKeybar)
        - bar
        - ruler;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "колонок и строк в окне — до тысяч"
    )]
    let (cols, rows) = (
        (w / cw).floor().max(8.0) as u32,
        (h / rh).floor().max(1.0) as u32,
    );
    Geo {
        font,
        cw,
        row_h: rh,
        gutter,
        pad,
        ruler,
        cols,
        rows,
    }
}

/// Подсветка в строке.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    /// Совпадение поиска на экране — `highlight`.
    Hit,
    /// Текущее совпадение (выделение) в тексте — `accent.fill`.
    Sel,
    /// Выделение Hex — `fill.cursor`; `true` — байт курсора с рамкой `accent`.
    Hex(bool),
}

/// Строка кадра.
enum Line {
    Row {
        shaped: Rc<Shaped>,
        /// Номер строки (смещение Hex — в `shaped.num`) — в поле слева.
        num: Option<Box<ShapedLine>>,
        /// Подсветка: `(x0, x1, вид)` от начала текста; рисуется по порядку.
        marks: SmallVec<[(f32, f32, Mark); 4]>,
    },
    /// Страницы ещё нет: полоса `fill.subtle.hover` доли ширины.
    Skeleton(f32),
}

/// Кадр: строки и сведения для статуса.
pub(super) struct Screen {
    lines: Vec<Line>,
    ruler: Option<Box<ShapedLine>>,
    /// Номер (с 0) первой строки на экране: точный или оценка.
    pub first: Option<LineGuess>,
    /// Номер последней строки на экране (точный).
    pub last: Option<u64>,
    /// Байты на экране.
    pub from: u64,
    pub to: u64,
}

/// Ключ кэша строки: байты, горизонталь, раскладка, стиль, выделение в строке.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    start: u64,
    end: u64,
    cut: bool,
    hscroll: u32,
    spec: u64,
    style: u64,
    sel: Option<(u64, u64)>,
    /// Совпадения в строке (хэш).
    hl: u64,
}

struct Shaped {
    text: ShapedLine,
    num: Option<ShapedLine>,
    /// Выделение в тексте, px от начала.
    sel_x: Option<(f32, f32)>,
    /// Совпадения в тексте, px от начала.
    hl_x: SmallVec<[(f32, f32); 4]>,
}

/// Совпадения на экране: ключ — видимые байты и образец.
struct Hits {
    key: (u64, u64, usize),
    list: Rc<[(u64, u64)]>,
}

/// Кэш `ShapedLine` на два экрана; прокрутка на строку переиспользует остальные.
#[derive(Default)]
pub(super) struct Rows {
    map: FxHashMap<Key, (Rc<Shaped>, u64)>,
    frame: u64,
    scratch: RowText,
    hits: Option<Hits>,
}

/// Байты строки `r`, покрытые выделением.
fn sel_in(sel: Option<&Range<u64>>, r: &VRow) -> Option<(u64, u64)> {
    let s = sel?;
    let a = s.start.max(r.start);
    let b = s.end.min(r.end);
    (a < b || (s.start == s.end && s.start == r.start)).then_some((a, b))
}

/// Символ строки, с которого начинается байт `b` (обратное к `RowText::byte_at`).
fn char_of(rt: &RowText, n: u32, b: u64) -> u32 {
    let (mut lo, mut hi) = (0u32, n);
    while lo < hi {
        let mid = u32::midpoint(lo, hi);
        if rt.byte_at(mid).is_some_and(|x| x < b) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Байт строки, с которого начинается символ `ci`.
fn byte_idx(text: &str, ci: u32) -> usize {
    text.char_indices()
        .nth(ci as usize)
        .map_or(text.len(), |(i, _)| i)
}

/// Runs по цвету символа.
fn runs(text: &str, font: &Font, color_of: impl Fn(usize) -> Hsla) -> Vec<TextRun> {
    let mut out: Vec<TextRun> = Vec::new();
    for (ci, ch) in text.chars().enumerate() {
        let color = color_of(ci);
        let len = ch.len_utf8();
        match out.last_mut() {
            Some(r) if r.color == color => r.len += len,
            _ => out.push(TextRun {
                len,
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }),
        }
    }
    out
}

fn hash_of(v: &impl std::hash::Hash) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault};
    BuildHasherDefault::<rustc_hash::FxHasher>::default().hash_one(v)
}

/// Видимые строки: целиком или — пока страниц нет — сколько получится сверху.
fn visible(d: &ViewerDoc, spec: &LayoutSpec, n: usize) -> (SmallVec<[VRow; 64]>, bool) {
    let pos = d.pos(spec);
    if let Ok(r) = rows_down(&d.pages, pos, n, spec) {
        (r, false)
    } else {
        let (mut lo, mut hi) = (0usize, n);
        let mut best = SmallVec::new();
        while hi - lo > 1 {
            let mid = usize::midpoint(lo, hi);
            if let Ok(r) = rows_down(&d.pages, pos, mid, spec) {
                lo = mid;
                best = r;
            } else {
                hi = mid;
            }
        }
        (best, true)
    }
}

/// Совпадения строки `r` (обрезанные по ней).
fn hits_in(list: &[(u64, u64)], r: &VRow) -> SmallVec<[(u64, u64); 4]> {
    let i = list.partition_point(|h| h.1 <= r.start);
    list[i..]
        .iter()
        .take_while(|h| h.0 < r.end.max(r.start + 1))
        .map(|h| (h.0.max(r.start), h.1.min(r.end.max(r.start + 1))))
        .collect()
}

impl Rows {
    pub(super) fn clear(&mut self) {
        self.map.clear();
        self.hits = None;
    }

    /// Совпадения образца на экране: matcher по видимым байтам (не больше 64 КБ), без I/O —
    /// только резидентные страницы (S7 §3.6).
    fn hits(&mut self, d: &ViewerDoc, from: u64, to: u64) -> Rc<[(u64, u64)]> {
        let none = || Rc::from(Vec::new());
        let Some(m) = d.search.as_ref().and_then(|s| s.matcher.clone()) else {
            self.hits = None;
            return none();
        };
        if to <= from || to - from > HL_MAX {
            return none();
        }
        let key = (from, to, Arc::as_ptr(&m).cast::<()>() as usize);
        if let Some(h) = self.hits.as_ref().filter(|h| h.key == key) {
            return h.list.clone();
        }
        let Ok(bytes) = d.pages.get(from..to) else {
            return none();
        };
        let buf = bytes.to_vec();
        let p = m.prepare(&buf);
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(r) = p.find_at(at) {
            if r.end <= r.start || out.len() >= HL_COUNT {
                break;
            }
            out.push((from + r.start as u64, from + r.end as u64));
            at = r.end;
        }
        let list: Rc<[(u64, u64)]> = Rc::from(out);
        self.hits = Some(Hits {
            key,
            list: list.clone(),
        });
        list
    }

    /// Кадр: строки по кэшу страниц, `ShapedLine` из кэша или заново.
    #[allow(
        clippy::too_many_lines,
        reason = "текст и Hex — один проход по строкам"
    )]
    pub(super) fn build(
        &mut self,
        d: &ViewerDoc,
        g: &Geo,
        theme: &Theme,
        window: &mut Window,
    ) -> Screen {
        self.frame += 1;
        let spec = d.layout(g.cols);
        let hex = spec.mode == Mode::Hex;
        let n = g.rows as usize;
        let (rows, missing) = if d.is_ready() {
            visible(d, &spec, n)
        } else {
            (SmallVec::new(), true)
        };
        let spec_key = hash_of(&format!("{spec:?}"));
        let style = hash_of(&(theme.generation(), g.font.to_bits()));
        let font = mono();
        let c = |r: Role| theme.c(r);
        let hg = hex_geom(spec.hex_cols, d.len);
        // Номера строк: от точного номера первой целой строки на экране.
        let sep = Sep::of(&d.enc, d.eol);
        let first_whole = rows.iter().position(|r| !r.cont);
        let mut line = None;
        let mut first = None;
        if !hex {
            if let Some(j) = first_whole {
                match d.index.line_of_in(&d.pages, rows[j].start, sep) {
                    Ok(LineGuess::Exact(k)) if j == 0 => {
                        line = Some(k);
                        first = Some(LineGuess::Exact(k));
                    }
                    Ok(LineGuess::Exact(k)) => line = Some(k),
                    _ => {}
                }
            }
            if first.is_none()
                && let Some(r) = rows.first()
            {
                first = Some(match d.index.line_of(r.seg) {
                    LineGuess::Exact(k) if r.cont => LineGuess::Approx(k),
                    x => x,
                });
            }
        }
        let mut last = None;
        let mut lines = Vec::with_capacity(n);
        let hits = match (rows.first(), rows.last()) {
            (Some(a), Some(z)) => self.hits(d, a.start, z.end),
            _ => Rc::from(Vec::new()),
        };
        for (i, r) in rows.iter().enumerate() {
            let mut num_text = None;
            if hex {
                num_text = Some(format!("{:0w$X}", r.start, w = hg.off_w as usize));
            } else if Some(i) >= first_whole
                && let Some(k) = line.as_mut()
            {
                if !r.cont && Some(i) != first_whole {
                    *k += 1;
                }
                if !r.cont {
                    num_text = Some((*k + 1).to_string());
                }
                last = Some(*k);
            }
            let sel = sel_in(d.sel.as_ref(), r);
            let row_hits = hits_in(&hits, r);
            let key = Key {
                start: r.start,
                end: r.end,
                cut: r.cut,
                hscroll: if hex { 0 } else { d.hscroll },
                spec: spec_key,
                style,
                sel: if hex { None } else { sel },
                hl: if hex || row_hits.is_empty() {
                    0
                } else {
                    hash_of(&row_hits)
                },
            };
            let shaped = if let Some((s, used)) = self.map.get_mut(&key) {
                *used = self.frame;
                Some(s.clone())
            } else if render_row(&d.pages, r, d.hscroll, &spec, &mut self.scratch).is_ok() {
                let s = Rc::new(if hex {
                    shape_hex(&self.scratch, d, r, &spec, g, theme, &font, window)
                } else {
                    shape_text(&self.scratch, sel, &row_hits, g, theme, &font, window)
                });
                self.map.insert(key, (s.clone(), self.frame));
                Some(s)
            } else {
                None
            };
            let Some(s) = shaped else {
                lines.push(Line::Skeleton(skeleton_w(r.start)));
                continue;
            };
            let num = num_text
                .filter(|_| !hex)
                .map(|t| Box::new(shape_plain(&t, c(Role::TextSecondary), g, &font, window)));
            let mut marks = SmallVec::new();
            if hex {
                hex_hits(&row_hits, r, &spec, g, &mut marks);
                hex_marks(d.sel.as_ref(), r, &spec, g, &mut marks);
            } else {
                marks.extend(s.hl_x.iter().map(|&(a, b)| (a, b, Mark::Hit)));
                if let Some(x) = s.sel_x {
                    marks.push((x.0, x.1, Mark::Sel));
                }
            }
            lines.push(Line::Row {
                shaped: s,
                num,
                marks,
            });
        }
        if missing {
            #[allow(clippy::cast_possible_truncation, reason = "строк на экране — десятки")]
            for k in lines.len()..n {
                lines.push(Line::Skeleton(skeleton_w(k as u64 * 7919)));
            }
        }
        // Кэш — на два экрана: старые строки уходят.
        if self.map.len() > 2 * n + 8 {
            let keep = self.frame.saturating_sub(1);
            self.map.retain(|_, (_, used)| *used >= keep);
        }
        let ruler = hex.then(|| {
            Box::new({
                let cur = d
                    .sel
                    .as_ref()
                    .map(|s| s.start % u64::from(spec.hex_cols.max(1)));
                shape_ruler(spec.hex_cols, cur, g, theme, &font, window)
            })
        });
        Screen {
            lines,
            ruler,
            first,
            last,
            from: rows.first().map_or(d.top, |r| r.start),
            to: rows.last().map_or(d.top, |r| r.next),
        }
    }
}

/// Доля ширины скелетона: «рваный» край по строке.
fn skeleton_w(seed: u64) -> f32 {
    #[allow(clippy::cast_precision_loss, reason = "доля от 0 до 1")]
    let k = (seed.wrapping_mul(2_654_435_761) >> 7) % 50;
    #[allow(clippy::cast_precision_loss, reason = "доля от 0 до 1")]
    let f = 0.3 + k as f32 / 100.0;
    f
}

fn shape_plain(text: &str, color: Hsla, g: &Geo, font: &Font, window: &mut Window) -> ShapedLine {
    let runs = [TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    window
        .text_system()
        .shape_line(SharedString::from(text.to_owned()), px(g.font), &runs, None)
}

/// Строка текста: U+FFFD — `critical.fg`, заменители и `↩` — `text.tertiary`, выделение
/// (совпадение поиска) — `accent.on` на `accent.fill`; прочие совпадения на экране —
/// фон `highlight` (`hits`).
#[allow(
    clippy::too_many_arguments,
    reason = "строка, выделение, совпадения, стиль"
)]
fn shape_text(
    rt: &RowText,
    sel: Option<(u64, u64)>,
    hits: &[(u64, u64)],
    g: &Geo,
    theme: &Theme,
    font: &Font,
    window: &mut Window,
) -> Shaped {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "символов в строке окна — сотни"
    )]
    let n = rt.text.chars().count() as u32;
    let sel_ch = sel.map(|(a, b)| char_of(rt, n, a)..char_of(rt, n, b).max(char_of(rt, n, a) + 1));
    let mut text = rt.text.clone();
    if rt.cut {
        text.push('↩');
    }
    let (primary, tertiary, bad, on) = (
        theme.c(Role::TextPrimary),
        theme.c(Role::TextTertiary),
        theme.c(Role::CriticalFg),
        theme.c(Role::AccentOn),
    );
    let inside = |list: &[Range<u32>], ci: u32| list.iter().any(|r| r.contains(&ci));
    let runs = runs(&text, font, |ci| {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "символов в строке окна — сотни"
        )]
        let ci = ci as u32;
        if ci >= n {
            tertiary
        } else if sel_ch.as_ref().is_some_and(|r| r.contains(&ci)) {
            on
        } else if inside(&rt.bad, ci) {
            bad
        } else if inside(&rt.dim, ci) {
            tertiary
        } else {
            primary
        }
    });
    let line =
        window
            .text_system()
            .shape_line(SharedString::from(text.clone()), px(g.font), &runs, None);
    let x_of = |r: Range<u32>| {
        let x0 = line.x_for_index(byte_idx(&text, r.start));
        let x1 = if r.end >= n {
            line.x_for_index(byte_idx(&text, n)).max(x0 + px(g.cw))
        } else {
            line.x_for_index(byte_idx(&text, r.end))
        };
        (f32::from(x0), f32::from(x1))
    };
    let sel_x = sel_ch.map(x_of);
    let hl_x = hits
        .iter()
        .filter_map(|&(a, b)| {
            let (ca, cb) = (char_of(rt, n, a), char_of(rt, n, b));
            (cb > ca).then(|| x_of(ca..cb))
        })
        .collect();
    Shaped {
        text: line,
        num: None,
        sel_x,
        hl_x,
    }
}

/// Колонка (символ от начала байтов) байта `i` строки Hex.
fn hex_col(i: u32) -> u32 {
    i * 3 + i / 8
}

/// Строка Hex: смещение — в поле слева, байты и текст — строкой; нули и непечатаемые
/// байты, `·` текста — `text.secondary`.
#[allow(
    clippy::too_many_arguments,
    reason = "строка, документ, раскладка, стиль"
)]
fn shape_hex(
    rt: &RowText,
    d: &ViewerDoc,
    r: &VRow,
    spec: &LayoutSpec,
    g: &Geo,
    theme: &Theme,
    font: &Font,
    window: &mut Window,
) -> Shaped {
    let hg = hex_geom(spec.hex_cols, d.len);
    let off: String = rt.text.chars().take(hg.off_w as usize).collect();
    let rest: String = rt.text.chars().skip(hg.bytes_at as usize).collect();
    let bytes = d
        .pages
        .get(r.start..r.end)
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let (primary, secondary) = (theme.c(Role::TextPrimary), theme.c(Role::TextSecondary));
    let text_at = hg.text_at - hg.bytes_at;
    let chars: Vec<char> = rest.chars().collect();
    let runs = runs(&rest, font, |ci| {
        #[allow(clippy::cast_possible_truncation, reason = "колонок Hex — сотни")]
        let col = ci as u32;
        if col >= text_at {
            return if chars.get(ci) == Some(&'\u{B7}') {
                secondary
            } else {
                primary
            };
        }
        // Байт под колонкой: группы по 8, по 3 символа на байт.
        let i =
            (0..u32::from(spec.hex_cols)).find(|&i| (hex_col(i)..hex_col(i) + 2).contains(&col));
        match i.and_then(|i| bytes.get(i as usize)) {
            Some(&b) if b == 0 || !(0x20..0x7F).contains(&b) => secondary,
            _ => primary,
        }
    });
    let line = window
        .text_system()
        .shape_line(SharedString::from(rest), px(g.font), &runs, None);
    Shaped {
        text: line,
        num: Some(shape_plain(&off, secondary, g, font, window)),
        sel_x: None,
        hl_x: SmallVec::new(),
    }
}

/// Совпадения Hex на экране: байты в обеих колонках.
fn hex_hits(
    hits: &[(u64, u64)],
    r: &VRow,
    spec: &LayoutSpec,
    g: &Geo,
    out: &mut SmallVec<[(f32, f32, Mark); 4]>,
) {
    let hg = hex_geom(spec.hex_cols, u64::MAX >> 1);
    let text_at = hg.text_at - hg.bytes_at;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "байт в строке — до 32"
    )]
    for &(a, b) in hits {
        for at in a.max(r.start)..b.min(r.end) {
            let i = (at - r.start) as u32;
            let x = hex_col(i) as f32 * g.cw;
            out.push((x - g.cw * 0.25, x + 2.25 * g.cw, Mark::Hit));
            let x = (text_at + i) as f32 * g.cw;
            out.push((x, x + g.cw, Mark::Hit));
        }
    }
}

/// Курсор и выделение Hex: в байтах и в тексте; рамка — у первого байта.
fn hex_marks(
    sel: Option<&Range<u64>>,
    r: &VRow,
    spec: &LayoutSpec,
    g: &Geo,
    out: &mut SmallVec<[(f32, f32, Mark); 4]>,
) {
    let Some(s) = sel else {
        return;
    };
    let hg = hex_geom(spec.hex_cols, u64::MAX >> 1);
    let text_at = hg.text_at - hg.bytes_at;
    let (a, b) = (s.start.max(r.start), s.end.max(s.start + 1).min(r.end));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "байт в строке — до 32"
    )]
    for at in a..b {
        let i = (at - r.start) as u32;
        let cursor = at == s.start;
        let x = hex_col(i) as f32 * g.cw;
        out.push((x - g.cw * 0.25, x + 2.25 * g.cw, Mark::Hex(cursor)));
        let x = (text_at + i) as f32 * g.cw;
        out.push((x, x + g.cw, Mark::Hex(cursor)));
    }
}

/// Линейка `00 … 1F` над байтами; колонка курсора — `accent.text`.
fn shape_ruler(
    cols: u8,
    cur: Option<u64>,
    g: &Geo,
    theme: &Theme,
    font: &Font,
    window: &mut Window,
) -> ShapedLine {
    use std::fmt::Write;
    let mut text = String::new();
    for i in 0..cols {
        if i > 0 {
            text.push(' ');
            if i % 8 == 0 {
                text.push(' ');
            }
        }
        let _ = write!(text, "{i:02X}");
    }
    let (sec, acc) = (theme.c(Role::TextSecondary), theme.c(Role::AccentText));
    let runs = runs(&text, font, |ci| {
        #[allow(clippy::cast_possible_truncation, reason = "колонок Hex — сотни")]
        let col = ci as u32;
        let hit = cur.is_some_and(|c| {
            #[allow(clippy::cast_possible_truncation, reason = "байт в строке — до 32")]
            let c = c as u32;
            (hex_col(c)..hex_col(c) + 2).contains(&col)
        });
        if hit { acc } else { sec }
    });
    window
        .text_system()
        .shape_line(SharedString::from(text), px(g.font), &runs, None)
}

/// Где нарисован бегунок (окно, px): клик выше — страница вверх, ниже — вниз.
pub(super) type Thumb = Rc<Cell<Option<(f32, f32)>>>;

/// Цвета кадра.
#[derive(Clone, Copy)]
struct Paint {
    bg: Hsla,
    gutter: Hsla,
    divider: Hsla,
    skeleton: Hsla,
    sel: Hsla,
    hit: Hsla,
    cursor: Hsla,
    accent: Hsla,
    thumb: Hsla,
    mark: Hsla,
}

/// Тело экрана: поле номеров и строки (один `canvas`), полоса прокрутки 14.
pub(super) fn body(
    screen: Screen,
    g: &Geo,
    d: &ViewerDoc,
    theme: &Theme,
    thumb: &Thumb,
    cx: &Context<CoreHost>,
) -> Div {
    let p = Paint {
        bg: theme.c(Role::LayerPanel),
        gutter: theme.c(Role::Header),
        divider: theme.c(Role::StrokeDivider),
        skeleton: theme.c(Role::FillSubtleHover),
        sel: theme.c(Role::AccentFill),
        hit: theme.c(Role::Highlight),
        cursor: theme.c(Role::FillCursor),
        accent: theme.c(Role::AccentFill),
        thumb: theme.c(Role::TextTertiary).opacity(0.5),
        mark: theme.c(Role::CriticalFg),
    };
    let g = *g;
    let hex = d.mode == Mode::Hex;
    let grid = canvas(
        |_, _, _| {},
        move |b, (), window, cx| paint(&screen, &g, hex, p, b, window, cx),
    )
    .flex_1()
    .h_full();
    // Бегунок: позиция — `top/len`, размер — доля видимых байт.
    let len = d.len.max(1);
    #[allow(clippy::cast_precision_loss, reason = "доля файла")]
    let span = (
        d.top.min(len) as f32 / len as f32,
        (d.view_rows.max(1) as f32 * row_bytes(d)).min(len as f32) / len as f32,
    );
    let marks: Arc<[u64]> = d
        .search
        .as_ref()
        .map_or_else(|| Arc::from(Vec::new()), |s| s.marks.clone());
    let at = thumb.clone();
    let bar = canvas(
        |_, _, _| {},
        move |b, (), window, _| {
            window.paint_quad(gpui_kit::fill(b, p.bg));
            let h = f32::from(b.size.height);
            let th = (span.1 * h).clamp(THUMB_MIN.min(h), h);
            let y0 = f32::from(b.origin.y) + (span.0 * h).min(h - th);
            at.set(Some((y0, y0 + th)));
            let x = b.origin.x + px(4.0);
            window.paint_quad(
                gpui_kit::fill(
                    Bounds::new(point(x, px(y0)), size(px(6.0), px(th))),
                    p.thumb,
                )
                .corner_radii(px(3.0)),
            );
            #[allow(clippy::cast_precision_loss, reason = "доля файла")]
            for m in marks.iter() {
                let y = f32::from(b.origin.y) + (*m as f32 / len as f32) * h;
                window.paint_quad(gpui_kit::fill(
                    Bounds::new(
                        point(b.origin.x + px(3.0), px(y.floor())),
                        size(px(8.0), px(2.0)),
                    ),
                    p.mark,
                ));
            }
        },
    )
    .size_full();
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_row()
        .child(grid)
        .child(
            div()
                .id("viewer-scroll")
                .w(px(W_SCROLL))
                .flex_none()
                .h_full()
                .child(bar)
                .on_mouse_down(
                    gpui_kit::MouseButton::Left,
                    cx.listener(|h, ev: &gpui_kit::MouseDownEvent, w, cx| {
                        let Some((y0, y1)) = h
                            .features
                            .get::<super::Viewer>()
                            .and_then(|v| v.thumb.get())
                        else {
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
                ),
        )
}

/// Байт на строку для бегунка: Hex — точно, текст — средняя длина по индексу.
fn row_bytes(d: &ViewerDoc) -> f32 {
    #[allow(clippy::cast_precision_loss, reason = "оценка длины строки")]
    if d.mode == Mode::Hex {
        f32::from(d.view_layout().hex_cols.max(1))
    } else {
        d.index
            .upto
            .checked_div(d.index.lines)
            .map_or(80.0, |b| b.max(1) as f32)
    }
}

#[allow(clippy::many_single_char_names, reason = "геометрия кадра")]
fn paint(
    s: &Screen,
    g: &Geo,
    hex: bool,
    p: Paint,
    b: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) {
    window.paint_quad(gpui_kit::fill(b, p.bg));
    let o = b.origin;
    // Поле номеров — под линейкой Hex.
    window.paint_quad(gpui_kit::fill(
        Bounds::new(
            point(o.x, o.y + px(g.ruler)),
            size(px(g.gutter), b.size.height - px(g.ruler)),
        ),
        p.gutter,
    ));
    let rh = px(g.row_h);
    let tx = o.x + px(g.gutter + g.pad);
    window.with_content_mask(Some(gpui_kit::ContentMask { bounds: b }), |window| {
        if let Some(r) = &s.ruler {
            let _ = r.paint(point(tx, o.y), rh, TextAlign::Left, None, window, cx);
            window.paint_quad(gpui_kit::fill(
                Bounds::new(point(o.x, o.y + rh - px(1.0)), size(b.size.width, px(1.0))),
                p.divider,
            ));
        }
        #[allow(clippy::cast_precision_loss, reason = "строк на экране — десятки")]
        for (i, line) in s.lines.iter().enumerate() {
            let y = o.y + px(g.ruler) + rh * i as f32;
            match line {
                Line::Skeleton(f) => {
                    let w = (b.size.width - px(g.gutter + g.pad + W_SCROLL)) * *f;
                    window.paint_quad(
                        gpui_kit::fill(
                            Bounds::new(point(tx, y + px(6.0)), size(w, rh - px(12.0))),
                            p.skeleton,
                        )
                        .corner_radii(px(3.0)),
                    );
                }
                Line::Row { shaped, num, marks } => {
                    for &(x0, x1, mark) in marks {
                        let r = Bounds::new(
                            point(tx + px(x0), y + px(1.0)),
                            size(px(x1 - x0), rh - px(2.0)),
                        );
                        match mark {
                            Mark::Hit => {
                                window.paint_quad(gpui_kit::fill(r, p.hit).corner_radii(px(2.0)));
                            }
                            Mark::Sel => {
                                window.paint_quad(gpui_kit::fill(r, p.sel).corner_radii(px(2.0)));
                            }
                            Mark::Hex(cursor) => {
                                window.paint_quad(gpui_kit::fill(r, p.cursor));
                                if cursor {
                                    window.paint_quad(
                                        gpui_kit::outline(
                                            r,
                                            p.accent,
                                            gpui_kit::BorderStyle::Solid,
                                        )
                                        .corner_radii(px(2.0)),
                                    );
                                }
                            }
                        }
                    }
                    {
                        let _ =
                            shaped
                                .text
                                .paint(point(tx, y), rh, TextAlign::Left, None, window, cx);
                    }
                    if let Some(n) = num.as_deref().or(shaped.num.as_ref()) {
                        let x = if hex {
                            o.x + px(17.0)
                        } else {
                            o.x + px(g.gutter - NUM_PAD_R) - n.width
                        };
                        let _ = n.paint(point(x, y), rh, TextAlign::Left, None, window, cx);
                    }
                }
            }
        }
    });
}

/// Число строк документа: разделителей по индексу и строка без разделителя в конце.
pub(super) fn total_lines(d: &ViewerDoc) -> u64 {
    if d.len == 0 {
        return 0;
    }
    let sep = Sep::of(&d.enc, d.eol);
    let u = sep.unit();
    let want: SmallVec<[u8; 2]> = match sep {
        Sep::Byte(b) => smallvec::smallvec![b],
        Sep::Le(b) => smallvec::smallvec![b, 0],
        Sep::Be(b) => smallvec::smallvec![0, b],
    };
    let ends = d
        .pages
        .get(d.len.saturating_sub(u)..d.len)
        .is_ok_and(|t| t.to_vec().as_slice() == want.as_slice());
    d.index.lines + u64::from(!ends)
}

pub(super) fn pct(a: u64, len: u64) -> u64 {
    if len == 0 {
        return 100;
    }
    u64::try_from(u128::from(a.min(len)) * 100 / u128::from(len)).unwrap_or(100)
}

pub(super) fn pattern_text(p: &Pattern) -> String {
    match p {
        Pattern::Text { s, .. } | Pattern::Regex { s, .. } => s.clone(),
        Pattern::Hex(v) => v
            .iter()
            .map(|b| b.map_or_else(|| "??".to_owned(), |b| format!("{b:02X}")))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Итог поиска для середины статуса.
pub(super) fn search_zone(d: &ViewerDoc) -> String {
    let i18n = urfin_i18n::current();
    let Some(s) = &d.search else {
        return String::new();
    };
    let mut a = FluentArgs::new();
    let text = pattern_text(&s.req.pat);
    if let Some(e) = &s.error {
        a.set("error", e.to_string());
        return if matches!(s.req.pat, Pattern::Hex(_)) {
            i18n.get("ui-find-bad-hex")
        } else {
            i18n.fmt("ui-find-bad-regex", &a)
        };
    }
    if s.running {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "проценты"
        )]
        a.set("pct", (s.progress() * 100.0).round() as u64);
        return i18n.fmt("ui-view-searching", &a);
    }
    if s.not_found {
        a.set("text", text);
        return i18n.fmt("ui-view-not-found", &a);
    }
    if s.count.done && s.count.n > 0 {
        a.set("count", s.count.n as u64);
        a.set("text", text);
        return i18n.fmt("ui-view-marks", &a);
    }
    String::new()
}

/// Курсор Hex: «Смещение 0x0000004E · 54h = 84 · «T»».
fn offset_zone(d: &ViewerDoc) -> Option<String> {
    let at = d.sel.as_ref()?.start;
    let b = d.pages.get(at..at + 1).ok()?.get(0)?;
    let ch = d.enc.table().map_or_else(
        || {
            if b.is_ascii() {
                char::from(b)
            } else {
                '\u{B7}'
            }
        },
        |t| t.0[usize::from(b)],
    );
    let ch = if ch.is_control() { '\u{B7}' } else { ch };
    let w = hex_geom(16, d.len).off_w as usize;
    let mut a = FluentArgs::new();
    a.set("offset", format!("0x{at:0w$X}"));
    a.set("hex", format!("{b:02X}"));
    a.set("dec", u64::from(b));
    a.set("char", ch.to_string());
    Some(urfin_i18n::current().fmt("ui-view-offset", &a))
}

/// Зоны статуса (S7 §6): позиция · итог поиска (курсор Hex, подсказка меню) · размер и
/// EOL.
pub(super) fn zones(d: &ViewerDoc, s: &Screen, menu: Option<MenuKind>) -> [String; 3] {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    let hex = d.mode == Mode::Hex;
    let left = if !d.is_ready() {
        String::new()
    } else if hex {
        let w = hex_geom(16, d.len).off_w as usize;
        a.set("from", format!("0x{:0w$X}", s.from));
        a.set(
            "to",
            format!("0x{:0w$X}", s.to.saturating_sub(1).max(s.from)),
        );
        a.set("pct", pct(s.from, d.len));
        i18n.fmt("ui-view-offsets", &a)
    } else {
        match s.first {
            Some(LineGuess::Exact(k)) => {
                a.set("from", k + 1);
                a.set("to", s.last.unwrap_or(k) + 1);
                a.set("total", total_lines(d));
                a.set("pct", pct(s.to, d.len));
                i18n.fmt("ui-view-lines", &a)
            }
            Some(LineGuess::Approx(k)) => {
                a.set("line", k + 1);
                let mut out = i18n.fmt("ui-view-line-approx", &a);
                if !d.index.done {
                    a.set("pct", pct(d.index.upto, d.len));
                    out.push_str(" · ");
                    out.push_str(&i18n.fmt("ui-view-indexing", &a));
                }
                out
            }
            None => String::new(),
        }
    };
    let middle = match menu {
        Some(MenuKind::Enc) => i18n.get("ui-view-enc-rereads"),
        _ if d.goto_wait.is_some() => {
            a.set("pct", pct(d.index.upto, d.len));
            i18n.fmt("ui-view-wait-index", &a)
        }
        _ if hex => offset_zone(d).unwrap_or_else(|| search_zone(d)),
        _ => search_zone(d),
    };
    [left, middle, size_eol(d)]
}

/// Правая зона статуса: размер и EOL (в Hex — только размер).
pub(super) fn size_eol(d: &ViewerDoc) -> String {
    let i18n = urfin_i18n::current();
    let size = urfin_i18n::fmt::bytes_with(&i18n, d.len);
    if d.mode == Mode::Hex || !d.is_ready() {
        return size;
    }
    let eol = match d.eol {
        urfin_view::Eol::Lf => "LF".to_owned(),
        urfin_view::Eol::CrLf => "CRLF".to_owned(),
        urfin_view::Eol::Cr => "CR".to_owned(),
        urfin_view::Eol::Mixed => i18n.get("ui-view-eol-mixed"),
    };
    format!("{size} · {eol}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_columns_follow_groups() {
        assert_eq!(hex_col(0), 0);
        assert_eq!(hex_col(7), 21);
        assert_eq!(hex_col(8), 25);
        assert_eq!(hex_col(16), 50);
    }

    #[test]
    fn percent_and_digits() {
        assert_eq!(pct(0, 0), 100);
        assert_eq!(pct(27, 212), 12);
        assert_eq!(pct(500, 100), 100);
        assert_eq!(digits(999), 3);
        assert_eq!(digits(1000), 4);
    }

    #[test]
    fn row_hits_are_clipped() {
        let r = VRow {
            start: 10,
            end: 20,
            next: 21,
            cont: false,
            cut: false,
            seg: 10,
            sub: 0,
        };
        let list = [(2, 4), (8, 12), (15, 16), (19, 25), (30, 31)];
        assert_eq!(
            hits_in(&list, &r).as_slice(),
            &[(10, 12), (15, 16), (19, 20)]
        );
    }

    #[test]
    fn hex_pattern_text() {
        let p = Pattern::Hex(vec![Some(0xDE), None, Some(0x0F)]);
        assert_eq!(pattern_text(&p), "DE ?? 0F");
    }
}
