//! Список над cmdline (S6 §3.1 слой inline-оверлеев): история Alt+F8 (S6-16) и
//! варианты Tab (S6-17). Модель — `TermState.menu` (крейт term), здесь только вид:
//! плашка у левого края над строкой ввода, до 12 строк, выбранная — `FillCursor`.

use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, Window, div, px};
use urfin_core::AppState;

use super::pouch;
use super::theme::Theme;
use crate::tokens::{Metric, Role};

/// Ширина плашки, px (как поле дополнения).
const W: f32 = 720.0;
/// Заголовок и подсказка клавиш истории.
const H_HEAD: f32 = 24.0;

/// Плашка списка или `None`, если он закрыт.
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "заголовок, строки и подсказка — одна плашка"
)]
pub(super) fn view(s: &AppState, theme: &Theme, window: &Window) -> Option<Div> {
    let st = s.ext.get::<urfin_term::TermState>()?;
    let m = st.menu.as_ref()?;
    let i18n = urfin_i18n::current();
    let history = m.is_history();
    let rows = m.rows(&st.history);
    if rows.is_empty() && !history {
        return None;
    }
    let (top, _) = m.window();
    let win = window.viewport_size();
    let x = theme.m(Metric::PadRowX);
    let w = W.min(f32::from(win.width) - 2.0 * x).max(200.0);
    let shown = rows.len().max(1);
    #[allow(clippy::cast_precision_loss, reason = "строк до 12")]
    let body_h = shown as f32 * pouch::H_ITEM;
    let head_h = if history { 2.0 * H_HEAD } else { 0.0 };
    let h = body_h + head_h + 8.0;
    let bottom = theme.m(Metric::HCmdline) + theme.m(Metric::HStatus) + theme.m(Metric::HKeybar);
    let y = (f32::from(win.height) - bottom - h - 2.0).max(0.0);
    let mut body = div().flex().flex_col();
    if let urfin_term::menu::MenuKind::History { filter, all } = &m.kind {
        let title = if *all {
            i18n.get("term-history-all")
        } else {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("shell", m.shell.to_string());
            i18n.fmt("term-history-title", &a)
        };
        body = body.child(
            div()
                .h(px(H_HEAD))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .font_family("Cascadia Mono")
                        .text_color(theme.c(Role::TextPrimary))
                        .child(SharedString::from(format!("{filter}▏"))),
                ),
        );
    }
    if rows.is_empty() {
        body = body.child(
            div()
                .h(px(pouch::H_ITEM))
                .px(px(10.0))
                .flex()
                .items_center()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("term-history-empty"))),
        );
    }
    for (k, item) in rows.into_iter().enumerate() {
        let selected = top + k == m.sel;
        let hint = if item.lock {
            i18n.get("term-history-pinned")
        } else if item.native {
            i18n.get("term-history-native")
        } else {
            item.hint.to_string()
        };
        body = body.child(
            div()
                .h(px(pouch::H_ITEM))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(4.0))
                .when(selected, |d| d.bg(theme.c(Role::FillCursor)))
                .child(
                    div()
                        .flex_none()
                        .w(px(14.0))
                        .font_family("Segoe Fluent Icons")
                        .text_size(px(12.0))
                        .text_color(theme.c(Role::TextSecondary))
                        // Закреплённая — булавка.
                        .child(SharedString::new_static(if item.lock {
                            "\u{E718}"
                        } else {
                            ""
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_family("Cascadia Mono")
                        .text_size(px(12.5))
                        .child(SharedString::from(item.text)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(hint)),
                ),
        );
    }
    if history {
        body = body.child(
            div()
                .h(px(H_HEAD))
                .px(px(10.0))
                .flex()
                .items_center()
                .text_size(px(11.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("term-history-keys"))),
        );
    }
    Some(pouch::pouch(theme, x, y, w, None, body))
}
