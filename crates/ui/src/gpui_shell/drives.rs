//! Список дисков на GPUI (D-03, `screen-drives.png`): «корешок» с именем тома над
//! путём панели и список 400 под ним — одна фигура (у края корешка угол прямой).
//! Разделы «Диски / Сеть / Места»; строка 40 — значок, имя, метка, полоса заполнения
//! (больше 75 % — `modified`), свободно. Клавиши: ↑↓, Home/End, Enter, Esc.

use gpui_kit::prelude::*;
use gpui_kit::{Div, Keystroke, SharedString, div, px, relative};
use urfin_core::Command;
use urfin_i18n::FluentArgs;

use super::overlays::ClickAll;
use super::theme::Theme;
use crate::drives::{Drives, Item, Section};
use crate::tokens::{Metric, Role};

const W_LIST: f32 = 400.0;
const H_ITEM: f32 = 40.0;
const H_STUB: f32 = 25.0;
const RADIUS: f32 = 8.0;
/// Заполнение тома, после которого полоса — `modified`.
const FULL_WARN: f32 = 0.75;

/// Выбор: свой (стрелки) или предложенный при открытии (том текущего каталога).
pub(super) fn selected(own: Option<usize>, d: &Drives, n: usize) -> usize {
    own.unwrap_or(d.sel).min(n.saturating_sub(1))
}

/// Строки списка: профили SSH — из состояния SSH на этот кадр.
pub(super) fn items<'a>(d: &'a Drives, s: &urfin_core::AppState, theme: &Theme) -> Vec<Item<'a>> {
    d.list(crate::ssh::rows(s, &theme.fmt, crate::ssh::now_ns()))
}

/// Клавиша при открытом списке: команды и новый выбор. На профиле SSH: Ins — новое
/// подключение, F4 — изменить, Del — отключить (F03 §2.8).
pub(super) fn key(
    d: &Drives,
    items: &[Item<'_>],
    sel: usize,
    ks: &Keystroke,
) -> (Vec<Command>, usize) {
    let n = items.len();
    let mut next = sel;
    let at = items.get(sel);
    match (ks.key.as_str(), at) {
        ("insert", _) => return (crate::ssh::new_cmds(d.side), sel),
        ("f4", Some(Item::Ssh(r))) => return (crate::ssh::edit_cmds(&r.profile, d.side), sel),
        ("delete", Some(Item::Ssh(r))) => {
            return (
                vec![Command::new("ssh.disconnect").arg("profile", r.profile.clone())],
                sel,
            );
        }
        _ => {}
    }
    match ks.key.as_str() {
        "escape" => return (vec![Command::new("ui.cancel")], sel),
        "enter" => return (d.open_cmds(at), sel),
        "up" if n > 0 => next = (sel + n - 1) % n,
        "down" if n > 0 => next = (sel + 1) % n,
        "home" => next = 0,
        "end" => next = n.saturating_sub(1),
        _ => {}
    }
    (Vec::new(), next)
}

/// Корешок и список: `x` — левый край панели, `top` — низ заголовка окна.
#[allow(
    clippy::too_many_lines,
    reason = "корешок, разделы и строки сверху вниз"
)]
pub(super) fn flyout(
    d: &Drives,
    items: &[Item<'_>],
    sel: usize,
    x: f32,
    theme: &Theme,
    click: ClickAll<'_>,
) -> Div {
    let i18n = urfin_i18n::current();
    let border = theme.c(Role::StrokeDivider);
    let panel = theme.c(Role::LayerPanel);
    let top = theme.m(Metric::HTitlebar);
    let head = theme.m(Metric::HBreadcrumb);
    let pad = theme.m(Metric::PadRowX);
    // Корешок — том под выбором; на профиле и месте — том, с которого открыли.
    let root = match items.get(sel) {
        Some(Item::Drive(i)) if i.section != Section::Places => i.name.clone(),
        _ => d
            .items
            .get(d.sel)
            .filter(|i| i.section == Section::Drives)
            .map_or_else(String::new, |i| i.name.clone()),
    };
    // Корешок: рамка без низа, перетекает в список.
    let stub = div()
        .absolute()
        .top(px(top + head - H_STUB))
        .left(px(x + pad))
        .h(px(H_STUB + 1.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .bg(panel)
        .border_t_1()
        .border_l_1()
        .border_r_1()
        .border_color(border)
        .rounded_t(px(RADIUS))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .child(SharedString::from(root))
        .child(
            div()
                .text_size(px(10.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static("▾")),
        );
    let mut list = div()
        .absolute()
        .top(px(top + head))
        .left(px(x + pad))
        .w(px(W_LIST))
        .pt(px(6.0))
        .px(px(4.0))
        .pb(px(4.0))
        .flex()
        .flex_col()
        .bg(panel)
        .border_1()
        .border_color(border)
        .rounded_tr(px(RADIUS))
        .rounded_b(px(RADIUS));
    let mut section = None;
    for (i, item) in items.iter().enumerate() {
        if section != Some(item.section()) {
            section = Some(item.section());
            let key = match item.section() {
                Section::Drives => "ui-drives-section-drives",
                Section::Network => "ui-drives-section-network",
                Section::Ssh => "net-drives-section",
                Section::Places => "ui-drives-section-places",
            };
            list = list.child(
                div()
                    .pt(px(8.0))
                    .px(px(10.0))
                    .pb(px(4.0))
                    .text_size(px(11.5))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(i18n.get(key))),
            );
        }
        let on = i == sel;
        let fg = |r: Role| theme.c(if on { Role::CursorText } else { r });
        let it = match item {
            Item::Drive(it) => *it,
            Item::Ssh(r) => {
                list = list.child(
                    super::ssh::drive_row(r, on, theme)
                        .id(SharedString::from(format!("drive-{i}")))
                        .on_click(click(d.open_cmds(Some(item)))),
                );
                continue;
            }
            Item::NewSsh => {
                list = list.child(
                    super::ssh::new_row(on, theme)
                        .id(SharedString::from(format!("drive-{i}")))
                        .on_click(click(d.open_cmds(Some(item)))),
                );
                continue;
            }
        };
        let glyph = match it.section {
            Section::Drives => '\u{EDA2}',
            Section::Network | Section::Ssh => '\u{E968}',
            Section::Places => '\u{E707}',
        };
        let used = it
            .space
            .filter(|(_, total)| *total > 0)
            .map(|(free, total)| {
                #[allow(clippy::cast_precision_loss, reason = "доля для полосы")]
                let k = 1.0 - free as f32 / total as f32;
                k.clamp(0.0, 1.0)
            });
        let right = if it.offline {
            i18n.get("ui-drives-offline")
        } else {
            it.space.map_or_else(String::new, |(free, _)| {
                let mut a = FluentArgs::new();
                a.set("bytes", urfin_i18n::fmt::bytes_with(&i18n, free));
                i18n.fmt("ui-drives-free", &a)
            })
        };
        let mut text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .items_baseline()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(fg(Role::TextPrimary))
                            .child(SharedString::from(it.name.clone())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.0))
                            .text_color(fg(Role::TextSecondary))
                            .child(SharedString::from(it.sub.clone())),
                    ),
            );
        if let Some(k) = used {
            let bar = theme.c(if k > FULL_WARN {
                Role::Modified
            } else {
                Role::AccentFill
            });
            text = text.child(
                div()
                    .h(px(4.0))
                    .rounded(px(2.0))
                    .bg(border)
                    .overflow_hidden()
                    .child(div().h_full().w(relative(k)).bg(bar)),
            );
        }
        list = list.child(
            div()
                .id(SharedString::from(format!("drive-{i}")))
                .min_h(px(H_ITEM))
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(4.0))
                .when(on, |e| e.bg(theme.c(Role::FillCursor)))
                .when(!on, |e| e.hover(|e| e.bg(theme.c(Role::FillSubtleHover))))
                .child(
                    div()
                        .font_family("Segoe Fluent Icons")
                        .text_size(px(16.0))
                        // Палитра B: значки дисков — `accent`.
                        .text_color(fg(if it.offline {
                            Role::Modified
                        } else if theme.vivid() {
                            Role::AccentFill
                        } else {
                            Role::TextSecondary
                        }))
                        .child(SharedString::from(glyph.to_string())),
                )
                .child(text)
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(px(12.0))
                        .text_color(fg(if it.offline {
                            Role::Modified
                        } else {
                            Role::TextSecondary
                        }))
                        .child(SharedString::from(right)),
                )
                .on_click(click(d.open_cmds(Some(item)))),
        );
    }
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(list)
        .child(stub)
}
