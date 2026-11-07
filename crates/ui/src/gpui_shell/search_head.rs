//! Заголовок панели результатов поиска `search:` (S8-11, `screen-searchpanel`,
//! `screen-searchgrep`): чип «Поиск», запрос, справа «найдено N · просмотрено M» или
//! «N файлов · время», пока поиск идёт — тонкий неопределённый прогресс под заголовком.
//! Тексты — `crate::search_panel`.

use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, div, px};
use urfin_core::search::SearchStore;
use urfin_core::{AppState, Side, Tab};

use super::panel::Listen;
use super::theme::Theme;
use crate::search_panel;
use crate::tokens::{Metric, Role};

const ICON_FONT: &str = "Segoe Fluent Icons";
/// Лупа (`Search`) в чипе.
const GLYPH_SEARCH: &str = "\u{E721}";

/// Чип «Поиск» перед запросом (как «Анализ» у `du:`).
fn chip(theme: &Theme) -> Div {
    div()
        .flex_none()
        .mr(px(8.0))
        .h(px(20.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::FillControl))
        .text_size(px(11.5))
        .text_color(theme.c(Role::TextPrimary))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(10.0))
                .child(SharedString::new_static(GLYPH_SEARCH)),
        )
        .child(SharedString::from(
            urfin_i18n::current().get("ui-search-chip"),
        ))
}

/// Заголовок панели результатов таба `t` со стором `store`.
pub(super) fn path_bar(
    s: &AppState,
    side: Side,
    t: &Tab,
    store: &SearchStore,
    theme: &Theme,
    field: Option<&super::field::Field>,
    cx: &Listen,
) -> Div {
    let active = s.active == side;
    let bar = div()
        .relative()
        .h(px(theme.m(Metric::HBreadcrumb)))
        .flex_none()
        .flex()
        .items_center()
        .px(px(theme.m(Metric::PadRowX)))
        .text_size(px(12.5))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .when(active && theme.vivid(), |d| d.bg(theme.c(Role::AccentSoft)));
    if let Some(input) = super::field::path_input(field, side) {
        return bar.child(input);
    }
    let query = search_panel::query(s, t);
    let text = query.map_or_else(String::new, search_panel::PanelQuery::line);
    let kinds = query.map(|q| q.kinds).unwrap_or_default();
    let line = div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(theme.c(if active {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        }))
        .when(active, |d| {
            d.border_b_2()
                .border_color(theme.c(Role::AccentFill))
                .pb(px(3.0))
                .mt(px(5.0))
        })
        .child(SharedString::from(text));
    let note = div()
        .flex_none()
        .ml(px(8.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(search_panel::head_note(store, kinds)));
    // Неопределённый прогресс на всю ширину: дорожка `border`, отрезок `accent`.
    let progress = (!store.is_done()).then(|| {
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(-1.0))
            .h(px(2.0))
            .overflow_hidden()
            .bg(theme.c(Role::StrokeControl))
            .child(super::du::sweep(theme).top_0())
    });
    bar.child(super::panel::head_buttons(s, side, true, theme, cx))
        .child(chip(theme))
        .child(line)
        .child(div().flex_1())
        .child(note)
        .children(progress)
}

/// Отпечаток заголовка: запрос, ход поиска и место бегущего отрезка.
pub(super) fn print(s: &AppState, t: &Tab) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let store = search_panel::store(s, t)?;
    let mut h = std::hash::DefaultHasher::new();
    search_panel::query(s, t).hash(&mut h);
    (
        store.files_len(),
        store.info.files.load(std::sync::atomic::Ordering::Relaxed),
        store.is_done(),
    )
        .hash(&mut h);
    if !store.is_done() {
        // Отрезок прогресса движется: кадр на каждый тик.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() / 16)
            .hash(&mut h);
    }
    Some(h.finish())
}
