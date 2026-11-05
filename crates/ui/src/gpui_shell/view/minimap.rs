//! Мини-карта редактора (S7-33, S7 §5.3, ADR-0023 §7, `screen-editor.png`): 88 px
//! справа, черта `border` слева; строка — 3 px штрихами слов (1 px на колонку), шаг 4;
//! рамка видимой части. Окно строк вокруг видимой части — из кэша страниц документа (как
//! VS Code на длинных файлах): доля прокрутки текста = доля прокрутки карты, на длинном
//! документе окно едет пропорционально; файл целиком не читается. Клик — `ScrollTo` на
//! эту строку. Штрих слова — цветом роли подсветки под его первым символом (S7-35), без
//! подсветки — `text.tertiary`. Строки совпадений поиска — полосой `highlight` под
//! штрихами.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{Bounds, canvas, fill, outline, point, px, size};
use smallvec::SmallVec;
use urfin_view::EditorDoc;
use urfin_view::layout::{LayoutSpec, RowText, render_row, row_up, rows_down};
use urfin_view::syntax::{Tag, spans_in};

use super::super::theme::Theme;
use super::syntax::SynPalette;
use crate::tokens::Role;

/// Ширина карты.
pub(super) const W: f32 = 88.0;
/// Шаг строки и высота штриха.
const PITCH: f32 = 4.0;
const BAR_H: f32 = 3.0;
/// Поле слева и пикселей на колонку.
const PAD: f32 = 8.0;
const COL: f32 = 1.0;
/// Колонок строки на карте не больше.
const MAX_COLS: u32 = 200;

/// Первая строка окна карты: видимые строки `[a, b)` из `total`, на карту влезает `fit`.
pub(super) fn first_line(a: u64, b: u64, total: u64, fit: u64) -> u64 {
    if total <= fit {
        return 0;
    }
    let vis = b.saturating_sub(a).max(1);
    if total <= vis {
        return 0;
    }
    // Доля прокрутки текста = доля прокрутки карты.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строки"
    )]
    let m0 = (a as f64 / (total - vis) as f64 * (total - fit) as f64).round() as u64;
    m0.min(total - fit)
}

/// Штрихи строки: (колонка, длина) непробельных кусков (табы уже раскрыты раскладкой).
fn runs(line: &str) -> SmallVec<[(u16, u16); 8]> {
    let mut out = SmallVec::new();
    let mut start: Option<u16> = None;
    let mut col = 0u16;
    for ch in line.chars() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, col - s));
            }
        } else if start.is_none() {
            start = Some(col);
        }
        col += 1;
    }
    if let Some(s) = start {
        out.push((s, col - s));
    }
    out
}

/// Строки окна карты: начало каждой (смещение документа) и штрихи.
#[derive(Default)]
pub(super) struct Lines {
    key: Option<(u64, u64, u32, u64, u64, usize)>,
    pub starts: Vec<u64>,
    /// Конец последней строки окна.
    pub end: u64,
    bars: Vec<SmallVec<[(u16, u16, Tag); 8]>>,
}

/// Состояние карты между кадрами: строки окна и где карта нарисована (клик).
#[derive(Default)]
pub(super) struct Map {
    pub lines: Rc<RefCell<Lines>>,
    /// Верх первой строки (px окна).
    pub top: Rc<std::cell::Cell<f32>>,
    scratch: RowText,
}

impl Map {
    /// Строки окна вокруг видимой части: от верха экрана вверх на `back` логических
    /// строк, `fit` строк вниз — только по кэшам страниц (нет — сколько есть).
    pub(super) fn update(&mut self, d: &EditorDoc, first: Option<u64>, vis: u64, fit: u64) {
        let Some(e) = d.engine.as_deref() else {
            return;
        };
        let total = e.text.line_of(e.len()).get() + 1;
        let syn = d.hl_spans();
        let syn_key = syn.map_or(0, |a| Arc::as_ptr(a).cast::<()>() as usize);
        let key = (e.text.edits(), e.top, e.top_sub, fit, total, syn_key);
        if self.lines.borrow().key == Some(key) {
            return;
        }
        let a = first.unwrap_or_else(|| e.text.line_of(e.top).get());
        let m0 = first_line(a, a + vis, total, fit);
        let seg = e.seg_layout();
        let v = e.text.view();
        let mut p = (e.top, 0);
        for _ in m0..a {
            if p == (0, 0) {
                break;
            }
            match row_up(&v, p, &seg) {
                Ok(q) => p = q,
                Err(_) => break,
            }
        }
        let n = usize::try_from(fit).unwrap_or(usize::MAX);
        let rows = if let Ok(r) = rows_down(&v, p, n, &seg) {
            r
        } else {
            let (mut lo, mut hi) = (0usize, n.min(512));
            let mut best = SmallVec::new();
            while hi - lo > 1 {
                let mid = usize::midpoint(lo, hi);
                if let Ok(r) = rows_down(&v, p, mid, &seg) {
                    lo = mid;
                    best = r;
                } else {
                    hi = mid;
                }
            }
            best
        };
        let wide = LayoutSpec {
            width_cols: MAX_COLS,
            ..seg
        };
        let mut l = Lines {
            key: Some(key),
            starts: Vec::with_capacity(rows.len()),
            end: rows.last().map_or(e.top, |r| r.next),
            bars: Vec::with_capacity(rows.len()),
        };
        for r in &rows {
            l.starts.push(r.start);
            let bars = if render_row(&v, r, 0, &wide, &mut self.scratch).is_ok() {
                let rt = &self.scratch;
                #[allow(clippy::cast_possible_truncation, reason = "символов — сотни")]
                let nch = rt.text.chars().count() as u32;
                let (a, z) = super::rows::shown_bytes(rt, nch, r);
                let list = syn.map_or(&[][..], |l| spans_in(l, a, z));
                runs(&rt.text)
                    .into_iter()
                    .map(|(c, n)| {
                        let tag = rt
                            .byte_at(u32::from(c))
                            .and_then(|b| list.iter().find(|s| s.start <= b && b < s.end))
                            .map_or(Tag::Plain, |s| s.tag);
                        (c, n, tag)
                    })
                    .collect()
            } else {
                SmallVec::new()
            };
            l.bars.push(bars);
        }
        *self.lines.borrow_mut() = l;
    }
}

/// Карта: черта слева, штрихи строк окна, рамка видимой части `vis` (байты), строки
/// совпадений `marks` (начала, по возрастанию).
pub(super) fn map(
    m: &Map,
    vis: (u64, u64),
    theme: &Theme,
    marks: Option<Arc<[u64]>>,
) -> impl IntoElement {
    let lines = m.lines.clone();
    let top_cell = m.top.clone();
    let ink = theme.c(Role::TextTertiary);
    let frame_bg = theme.c(Role::FillSubtleHover);
    let frame_line = theme.c(Role::StrokeCard);
    let mark = theme.c(Role::Highlight);
    let pal = SynPalette::of(theme);
    canvas(
        |_, _, _| {},
        move |b: Bounds<gpui_kit::Pixels>, (), window, _cx| {
            let l = lines.borrow();
            let top = f32::from(b.top()) + 2.0;
            top_cell.set(top);
            // Штрихов — тысячи: один слой вместо места каждого в `BoundsTree` (четверть
            // кадра редактора, S7-36).
            window.paint_layer(b, |window| {
                let line_of = |off: u64| l.starts.partition_point(|s| *s <= off).saturating_sub(1);
                // Рамка видимой части.
                if !l.starts.is_empty() && vis.1 > vis.0 {
                    let (i0, i1) = (line_of(vis.0), line_of(vis.1.saturating_sub(1)) + 1);
                    #[allow(clippy::cast_precision_loss, reason = "строки")]
                    let y0 = top + i0 as f32 * PITCH - 1.0;
                    #[allow(clippy::cast_precision_loss, reason = "строки")]
                    let y1 = (top + i1 as f32 * PITCH + 1.0).min(f32::from(b.bottom()));
                    if y1 > y0 {
                        let r = Bounds::new(
                            point(b.left() + px(2.0), px(y0)),
                            size(b.size.width - px(4.0), px(y1 - y0)),
                        );
                        window.paint_quad(fill(r, frame_bg));
                        window.paint_quad(outline(r, frame_line, gpui_kit::BorderStyle::Solid));
                    }
                }
                let max_cols = (f32::from(b.size.width) - PAD - 2.0) / COL;
                let marks: &[u64] = marks.as_deref().unwrap_or(&[]);
                for (k, bars) in l.bars.iter().enumerate() {
                    #[allow(clippy::cast_precision_loss, reason = "строки")]
                    let y = top + k as f32 * PITCH;
                    if y + PITCH > f32::from(b.bottom()) {
                        break;
                    }
                    let (a, z) = (l.starts[k], l.starts.get(k + 1).copied().unwrap_or(l.end));
                    let i = marks.partition_point(|m| *m < a);
                    if marks.get(i).is_some_and(|m| *m < z.max(a + 1)) {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(b.left() + px(PAD - 2.0), px(y - 0.5)),
                                size(b.size.width - px(PAD), px(PITCH)),
                            ),
                            mark,
                        ));
                    }
                    for &(c, n, tag) in bars {
                        let (c, n) = (f32::from(c), f32::from(n));
                        if c >= max_cols {
                            break;
                        }
                        let w = n.min(max_cols - c) * COL;
                        window.paint_quad(fill(
                            Bounds::new(
                                point(b.left() + px(PAD + c * COL), px(y)),
                                size(px(w), px(BAR_H)),
                            ),
                            pal.color(tag).unwrap_or(ink),
                        ));
                    }
                }
            });
        },
    )
    .size_full()
}

/// Строки, которые влезают на карту высотой `h`.
pub(super) fn fit(h: f32) -> u64 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строки"
    )]
    let n = ((h - 4.0) / PITCH).max(1.0) as u64;
    n
}

/// Начало строки карты под точкой `y` окна.
pub(super) fn line_at(m: &Map, y: f32) -> Option<u64> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строки"
    )]
    let k = ((y - m.top.get()).max(0.0) / PITCH) as usize;
    let l = m.lines.borrow();
    l.starts.get(k).or(l.starts.last()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_follows_scroll() {
        // Всё влезает — с начала.
        assert_eq!(first_line(0, 29, 41, 100), 0);
        // Начало и конец текста — начало и конец карты.
        assert_eq!(first_line(0, 40, 10_000, 150), 0);
        assert_eq!(first_line(9_960, 10_000, 10_000, 150), 9_850);
        let mid = first_line(4_980, 5_020, 10_000, 150);
        assert!((4_900..=4_960).contains(&mid), "{mid}");
        // 10 ГБ: окно рядом с видимой частью, а не с начала файла.
        let far = first_line(100_000_000, 100_000_040, 200_000_000, 150);
        assert!(far <= 100_000_000 && 100_000_000 - far < 150, "{far}");
    }

    #[test]
    fn word_runs() {
        assert_eq!(runs("  ab cd").as_slice(), &[(2, 2), (5, 2)]);
        assert_eq!(runs("    x").as_slice(), &[(4, 1)]);
        assert!(runs("   ").is_empty());
    }
}
