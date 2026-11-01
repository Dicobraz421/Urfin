//! Полоса изменений редактора (S7 §5.3, `screen-editor.png`): 3 px слева от номеров —
//! `accent` у изменённой строки, `type.exec` у добавленной, черта `modified` над местом
//! удалённых. Строки — из `LineChanges` документа (diff в фоне), их место на экране — из
//! раскладки `EditorState` этого же кадра: полоса рисуется после текста.

use gpui_kit::base::input::{EditorState, RopeExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{Bounds, Entity, Hsla, canvas, fill, point, px, size};
use urfin_view::{Change, LineChanges};

use super::super::theme::Theme;
use crate::tokens::Role;

/// Ширина полосы.
pub(super) const W: f32 = 3.0;

/// Полоса во всю высоту тела редактора (кладётся поверх, после текста).
pub(super) fn bar(
    state: &Entity<EditorState>,
    changes: &LineChanges,
    wrap: bool,
    theme: &Theme,
) -> impl IntoElement {
    let state = state.clone();
    let lines = changes.lines.clone();
    let colors = [
        theme.c(Role::TypeExec),
        theme.c(Role::AccentFill),
        theme.c(Role::Modified),
    ];
    canvas(
        |_, _, _| {},
        move |b: Bounds<gpui_kit::Pixels>, (), window, cx| {
            if lines.is_empty() {
                return;
            }
            let st = state.read(cx);
            let Some(rows) = st.visible_row_range() else {
                return;
            };
            let Some(lh) = st.line_height() else {
                return;
            };
            let rope = st.text();
            // Без переноса строка = ряд; с переносом строка не ниже своего ряда.
            let lo = if wrap { 0 } else { rows.start };
            let hi = rows.end;
            let from = lines.partition_point(|(l, _)| (*l as usize) < lo);
            for (line, change) in lines[from..].iter().take_while(|(l, _)| *l as usize <= hi) {
                let off = rope.line_start_offset(*line as usize);
                let Some(r) = st.range_to_bounds(&(off..off)) else {
                    continue;
                };
                let (top, h, color): (f32, f32, Hsla) = match change {
                    Change::Added => (f32::from(r.top()), f32::from(lh), colors[0]),
                    Change::Modified => (f32::from(r.top()), f32::from(lh), colors[1]),
                    Change::Deleted => (f32::from(r.top()) - 1.0, 2.0, colors[2]),
                };
                let y0 = top.max(f32::from(b.top()));
                let y1 = (top + h).min(f32::from(b.bottom()));
                if y1 <= y0 {
                    continue;
                }
                window.paint_quad(fill(
                    Bounds::new(point(b.left(), px(y0)), size(px(W), px(y1 - y0))),
                    color,
                ));
            }
        },
    )
    .absolute()
    .left_0()
    .top_0()
    .w(px(W))
    .h_full()
}
