//! «Мешочек» (D-05, DESIGN.md, №65): всплывающий список и его якорь — одна фигура.
//! Корешок (открытая кнопка, ячейка заголовка, путь) фона `panel` без нижней границы
//! перетекает в список; у корешка внутри края списка — вогнутые скругления 8 с обеих
//! сторон, у края списка — прямой угол. Без якоря — плашка (контекстное меню). Вся
//! фигура — с тенью `shadow` (смещение 8, размытие 16).
//!
//! Контур рисует один путь (`PathBuilder`): заливка и обводка 1 px. Корешок и список
//! кладутся поверх контура обычными `div`. Список над источником (командная строка,
//! кнопка внизу) — та же фигура вверх ([`pouch_up`]), корешок снизу.

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Bounds, Div, PathBuilder, Pixels, Point, canvas, div, point, px};

use super::theme::Theme;
use crate::tokens::Role;

/// Скругление фигуры и вогнутых углов у корешка.
pub(super) const RADIUS: f32 = 8.0;
/// Строка списка.
pub(super) const H_ITEM: f32 = 28.0;

/// Корешок: смещение от левого края списка, ширина, высота и содержимое.
pub(super) struct Notch {
    pub x: f32,
    pub w: f32,
    pub h: f32,
    pub content: AnyElement,
}

/// Где поставить список шириной `w` под якорем `[ax, ax + aw]` в окне шириной `win`:
/// левый край списка и смещение корешка. Список не выходит за окно; у края окна корешок
/// встаёт на край списка (прямой угол).
pub(super) fn place(ax: f32, aw: f32, w: f32, win: f32, prefer: f32) -> (f32, f32) {
    let left = (ax - prefer).clamp(0.0, (win - w).max(0.0));
    let mut x = ax - left;
    // Вогнутому углу нужно место: ближе радиуса к краю — корешок на край.
    if x < RADIUS {
        x = 0.0;
    }
    if w - (x + aw) < RADIUS {
        x = (w - aw).max(0.0);
    }
    (left, x)
}

/// Фигура в окне: левый верхний угол (`left`, `top` — верх корешка или плашки),
/// ширина списка `w`, корешок и тело списка.
pub(super) fn pouch(
    theme: &Theme,
    left: f32,
    top: f32,
    w: f32,
    notch: Option<Notch>,
    body: Div,
) -> Div {
    figure(theme, w, notch, body, false)
        .left(px(left))
        .top(px(top))
}

/// Фигура вверх (список над источником): корешок снизу, `bottom` — расстояние от низа
/// контейнера до низа корешка (командная строка, кнопка внизу).
pub(super) fn pouch_up(
    theme: &Theme,
    left: f32,
    bottom: f32,
    w: f32,
    notch: Option<Notch>,
    body: Div,
) -> Div {
    figure(theme, w, notch, body, true)
        .left(px(left))
        .bottom(px(bottom))
}

/// Фигура без положения: контур, тень, корешок сверху (`up` — снизу) и тело.
fn figure(theme: &Theme, w: f32, notch: Option<Notch>, body: Div, up: bool) -> Div {
    let bg = theme.c(Role::LayerPanel);
    let border = theme.c(Role::StrokeCard);
    let shadow = gpui_kit::BoxShadow {
        color: theme.c(Role::ShadowOverlay),
        offset: point(px(0.0), px(8.0)),
        blur_radius: px(16.0),
        spread_radius: px(0.0),
        inset: false,
    };
    let nh = notch.as_ref().map_or(0.0, |n| n.h);
    let geo = notch.as_ref().map(|n| (n.x, n.w, n.h));
    let outline = canvas(
        |_, _, _| {},
        move |b, (), window, _| {
            let Some(path) = outline(b, geo, false, up) else {
                return;
            };
            window.paint_path(path, bg);
            if let Some(stroke) = outline(b, geo, true, up) {
                window.paint_path(stroke, border);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    // Тень — у тела списка (корешок её не отбрасывает).
    let body_shadow = div()
        .absolute()
        .left_0()
        .right_0()
        .rounded(px(RADIUS))
        .shadow(vec![shadow]);
    let body_shadow = if up {
        body_shadow.top_0().bottom(px(nh))
    } else {
        body_shadow.top(px(nh)).bottom_0()
    };
    let fig = div()
        .absolute()
        .w(px(w))
        .flex()
        .flex_col()
        .child(body_shadow)
        .child(outline);
    let notch_row = notch.map(|n| {
        div()
            .h(px(n.h))
            .flex()
            .child(div().w(px(n.x)).flex_none())
            .child(
                div()
                    .w(px(n.w))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(n.content),
            )
    });
    let body = body.relative().p(px(4.0));
    if up {
        fig.child(body).children(notch_row)
    } else {
        fig.children(notch_row).child(body)
    }
}

/// Контур фигуры по часовой стрелке (экранные координаты, y вниз): выпуклые углы —
/// дуга по часовой, вогнутые у корешка — против. `stroke` — обводка 1 px по
/// полупиксельной линии; `up` — корешок снизу (контур отражён по вертикали).
#[allow(clippy::many_single_char_names, reason = "геометрия контура")]
fn outline(
    b: Bounds<Pixels>,
    notch: Option<(f32, f32, f32)>,
    stroke: bool,
    up: bool,
) -> Option<gpui_kit::Path<Pixels>> {
    let r = RADIUS;
    let inset = if stroke { 0.5 } else { 0.0 };
    let ox = f32::from(b.origin.x) + inset;
    let oy = f32::from(b.origin.y) + inset;
    let w = f32::from(b.size.width) - 2.0 * inset;
    let h = f32::from(b.size.height) - 2.0 * inset;
    let p = |x: f32, y: f32| -> Point<Pixels> {
        point(px(ox + x), px(oy + if up { h - y } else { y }))
    };
    let mut pb = if stroke {
        PathBuilder::stroke(px(1.0))
    } else {
        PathBuilder::fill()
    };
    let rr = point(px(r), px(r));
    // Отражение меняет направление обхода дуг.
    let arc = |pb: &mut PathBuilder, to: Point<Pixels>, convex: bool| {
        pb.arc_to(rr, px(0.0), false, convex != up, to);
    };
    match notch {
        None => {
            pb.move_to(p(r, 0.0));
            pb.line_to(p(w - r, 0.0));
            arc(&mut pb, p(w, r), true);
            pb.line_to(p(w, h - r));
            arc(&mut pb, p(w - r, h), true);
            pb.line_to(p(r, h));
            arc(&mut pb, p(0.0, h - r), true);
            pb.line_to(p(0.0, r));
            arc(&mut pb, p(r, 0.0), true);
        }
        Some((nx, nw, nh)) => {
            let (x0, x1, y1) = (nx, nx + nw, nh);
            let at_left = x0 < 0.5;
            let at_right = x1 > w - 0.5;
            pb.move_to(p(x0 + r, 0.0));
            pb.line_to(p(x1 - r, 0.0));
            arc(&mut pb, p(x1, r), true);
            if at_right {
                // Корешок у правого края: его бок продолжается краем списка.
                pb.line_to(p(w, h - r));
            } else {
                pb.line_to(p(x1, y1 - r));
                arc(&mut pb, p(x1 + r, y1), false);
                pb.line_to(p(w - r, y1));
                arc(&mut pb, p(w, y1 + r), true);
                pb.line_to(p(w, h - r));
            }
            arc(&mut pb, p(w - r, h), true);
            pb.line_to(p(r, h));
            arc(&mut pb, p(0.0, h - r), true);
            if at_left {
                pb.line_to(p(0.0, r));
            } else {
                pb.line_to(p(0.0, y1 + r));
                arc(&mut pb, p(r, y1), true);
                pb.line_to(p(x0 - r, y1));
                arc(&mut pb, p(x0, y1 - r), false);
                pb.line_to(p(x0, r));
            }
            arc(&mut pb, p(x0 + r, 0.0), true);
        }
    }
    pb.close();
    pb.build().ok()
}

#[cfg(test)]
mod tests {
    use super::place;

    #[test]
    fn list_stays_in_window_and_snaps_notch_to_edges() {
        // Якорь далеко от краёв: список сдвинут на `prefer` левее якоря.
        assert_eq!(place(300.0, 60.0, 330.0, 1200.0, 40.0), (260.0, 40.0));
        // У левого края окна: список от 0, корешок на краю (прямой угол).
        assert_eq!(place(4.0, 60.0, 290.0, 1200.0, 40.0), (0.0, 0.0));
        // У правого края: список прижат к окну, корешок не дальше его края.
        let (left, x) = place(1150.0, 50.0, 330.0, 1200.0, 40.0);
        assert_eq!(left, 870.0);
        assert_eq!(x, 280.0);
    }
}
