//! Мини-карта редактора (S7 §5.3, `screen-editor.png`): 88 px справа, черта `border`
//! слева; строка — 3 px штрихами слов (1 px на колонку), шаг 4; рамка видимой части.
//! Окно карты ездит вместе с текстом (как в VS Code): длинный файл показывается куском
//! вокруг видимой части. Рисуется после текста — раскладка `EditorState` того же кадра.
//! Клик — курсор на эту строку. Цвета подсветки — после S7-21 (пока один цвет текста).

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::input::{EditorState, RopeExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{Bounds, Entity, canvas, fill, outline, point, px, size};

use super::super::theme::Theme;
use crate::tokens::Role;

/// Ширина карты.
pub(super) const W: f32 = 88.0;
/// Шаг строки и высота штриха.
const PITCH: f32 = 4.0;
const BAR_H: f32 = 3.0;
/// Поле слева и пикселей на колонку.
const PAD: f32 = 8.0;
const COL: f32 = 1.0;

/// Где карта стояла в прошлом кадре: первая строка и верх (окно) — для клика.
pub(super) type Geo = Rc<Cell<(usize, f32)>>;

/// Первая строка окна карты: видимые строки `[a, b)` из `total`, на карту влезает `fit`.
pub(super) fn first_line(a: usize, b: usize, total: usize, fit: usize) -> usize {
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
    let m0 = (a as f64 / (total - vis) as f64 * (total - fit) as f64).round() as usize;
    m0.min(total - fit)
}

/// Строка карты под точкой `y` окна.
pub(super) fn line_at(geo: (usize, f32), y: f32) -> usize {
    let (m0, top) = geo;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строки"
    )]
    let k = ((y - top).max(0.0) / PITCH) as usize;
    m0 + k
}

/// Штрихи строки: (колонка, длина) непробельных кусков.
fn runs(line: &str, tab: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut col = 0usize;
    let mut start: Option<usize> = None;
    for ch in line.chars() {
        if ch == '\r' {
            break;
        }
        let w = if ch == '\t' { tab - col % tab } else { 1 };
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, col - s));
            }
        } else if start.is_none() {
            start = Some(col);
        }
        col += w;
        if col > 200 {
            break;
        }
    }
    if let Some(s) = start {
        out.push((s, col - s));
    }
    out
}

/// Карта: черта слева, штрихи строк окна, рамка видимой части.
pub(super) fn map(
    state: &Entity<EditorState>,
    geo: &Geo,
    tab: usize,
    theme: &Theme,
) -> impl IntoElement {
    let state = state.clone();
    let geo = geo.clone();
    let ink = theme.c(Role::TextTertiary);
    let frame_bg = theme.c(Role::FillSubtleHover);
    let frame_line = theme.c(Role::StrokeCard);
    let tab = tab.max(1);
    canvas(
        |_, _, _| {},
        move |b: Bounds<gpui_kit::Pixels>, (), window, cx| {
            let st = state.read(cx);
            let rope = st.text();
            let mut total = rope.lines_len();
            if total > 1 && rope.len() > 0 && rope.char_at(rope.len() - 1) == Some('\n') {
                total -= 1;
            }
            let rows = st.visible_row_range().unwrap_or(0..0);
            let top = f32::from(b.top()) + 2.0;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "строки"
            )]
            let fit = ((f32::from(b.size.height) - 4.0) / PITCH).max(1.0) as usize;
            let m0 = first_line(rows.start, rows.end, total, fit);
            geo.set((m0, top));
            // Рамка видимой части.
            #[allow(clippy::cast_precision_loss, reason = "строки")]
            if rows.end > rows.start {
                let y0 = top + (rows.start.saturating_sub(m0)) as f32 * PITCH - 1.0;
                let y1 = (top + (rows.end.min(total).saturating_sub(m0)) as f32 * PITCH + 1.0)
                    .min(f32::from(b.bottom()));
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
            for (k, line) in (m0..total.min(m0 + fit)).enumerate() {
                let text = rope.slice_line(line).to_string();
                #[allow(clippy::cast_precision_loss, reason = "строки")]
                let y = top + k as f32 * PITCH;
                for (c, n) in runs(&text, tab) {
                    #[allow(clippy::cast_precision_loss, reason = "колонки")]
                    let (c, n) = (c as f32, n as f32);
                    if c >= max_cols {
                        break;
                    }
                    let w = n.min(max_cols - c) * COL;
                    window.paint_quad(fill(
                        Bounds::new(
                            point(b.left() + px(PAD + c * COL), px(y)),
                            size(px(w), px(BAR_H)),
                        ),
                        ink,
                    ));
                }
            }
        },
    )
    .size_full()
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
    }

    #[test]
    fn word_runs() {
        assert_eq!(runs("  ab cd", 4), vec![(2, 2), (5, 2)]);
        assert_eq!(runs("\tx", 4), vec![(4, 1)]);
        assert_eq!(runs("ab\r", 4), vec![(0, 2)]);
        assert_eq!(line_at((10, 100.0), 109.0), 12);
    }
}
