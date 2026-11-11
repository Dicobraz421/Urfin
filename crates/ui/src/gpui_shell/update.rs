//! Баннер «Вышла версия X» (решение 2026-11-11 «self-update»): строка 40 в зоне событий
//! над командной строкой — как баннер проверки пакетов (`screen-pkgbanner`) и «Урфин
//! обновлён» (`screen-whatsnew`): фон `header`, черта `accent` сверху, стрелка вверх в
//! круге, полужирное «Вышла версия X», вторичное «сейчас установлена Y» и сразу за ним
//! «Установить» (основная) и «Позже». Ход скачивания — job-бар.

use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, div, px};
use urfin_core::{AppState, Command};

use super::strip::{Btn, button};
use super::theme::Theme;
use crate::tokens::{Metric, Role};

const BAR_H: f32 = 40.0;
/// `Upload` Segoe Fluent Icons — стрелка вверх в круге, как на `screen-whatsnew`.
const GLYPH: &str = "\u{E898}";

pub(super) fn offer_bar(
    s: &AppState,
    theme: &Theme,
    click: super::overlays::Click<'_>,
) -> Option<Div> {
    let version = crate::update::offer(s)?;
    let i18n = urfin_i18n::current();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("version", version.to_owned());
    a.set("current", crate::version());
    let btn = |id: &str, label: String, kind: Btn, cmd: &'static str| {
        button(theme, id.to_owned(), label, "", kind).on_click(click(Command::new(cmd)))
    };
    Some(
        div()
            .h(px(BAR_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(theme.m(Metric::PadRowX)))
            .bg(theme.c(Role::Header))
            .border_t_2()
            .border_color(theme.c(Role::AccentFill))
            .text_size(px(12.5))
            .child(
                div()
                    .flex_none()
                    .font_family("Segoe Fluent Icons")
                    .text_color(theme.c(Role::AccentText))
                    .child(SharedString::new_static(GLYPH)),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(SharedString::from(i18n.fmt("ui-update-offer", &a))),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(i18n.fmt("ui-update-offer-current", &a))),
            )
            .child(div().w(px(6.0)).flex_none())
            .child(btn(
                "update-install",
                i18n.get("ui-update-install"),
                Btn::Primary,
                "update.install",
            ))
            .child(btn(
                "update-later",
                i18n.get("ui-update-later"),
                Btn::Normal,
                "update.later",
            )),
    )
}
