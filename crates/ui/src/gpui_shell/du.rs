//! Вкладка анализа места `du:` на GPUI (F01-05, F01-06; `screen-duscan`, `screen-dulist`,
//! `screen-ducached`): заголовок с чипом «Анализ», путём, итогами и неопределённым
//! прогрессом, строка чипов фильтров, строка отметки «Отмечено N · освободится X».
//! Тексты и расчёты — `crate::du_view`.

use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Div, SharedString, div, px, relative};
use urfin_core::{AppState, Command, Side, Tab};
use urfin_vfs::du::DuInfo;

use super::panel::Listen;
use super::strip::{Btn, button};
use super::theme::Theme;
use crate::du_view;
use crate::tokens::{Metric, Role};

/// Строка чипов фильтров.
pub(super) const CHIPS_H: f32 = 40.0;
/// Строка отметки внизу панели (F01-06).
pub(super) const MARK_H: f32 = 48.0;
const ICON_FONT: &str = "Segoe Fluent Icons";
/// Три столбика (как на макете), `CheckboxComposite`.
const GLYPH_CHART: &str = "\u{EC39}";
const GLYPH_CHECKED: &str = "\u{E73A}";
/// Неопределённый прогресс: период прохода отрезка и его доля ширины.
const SWEEP_MS: u128 = 1600;
const SWEEP_W: f32 = 0.3;

/// Сверх шапки и списка у таба: чипы и, при отметке, строка «Отмечено» (для сетки строк).
pub(super) fn extra_h(t: &Tab) -> f32 {
    if !du_view::is_du(t) {
        return 0.0;
    }
    CHIPS_H + if t.selection.is_empty() { 0.0 } else { MARK_H }
}

fn run(
    cx: &Listen,
    side: Side,
    cmd: Command,
) -> impl Fn(&ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static {
    let name = super::chrome::side_name(side);
    cx.listener(move |h, _: &ClickEvent, w, cx| {
        h.run_all(
            vec![Command::new("panel.focus").arg("side", name), cmd.clone()],
            w,
            cx,
        );
    })
}

/// Заголовок вкладки: кнопки «История»/«Закладки», чип «Анализ», путь от корня анализа
/// (сегменты — переходы внутри анализа), справа итоги («просмотрено N файлов · X»,
/// «N файлов · X») или «посчитано N назад» с кнопкой «Обновить Ctrl+R». Пока идёт
/// подсчёт — по нижней кромке бежит отрезок `accent` (неопределённый прогресс).
pub(super) fn path_bar(
    s: &AppState,
    side: Side,
    info: &DuInfo,
    theme: &Theme,
    field: Option<&super::field::Field>,
    cx: &Listen,
) -> Div {
    let active = s.active == side;
    let name = super::chrome::side_name(side);
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
    let chip = chip(theme);
    let mut segs = div()
        .flex()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
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
        });
    for (k, (label, loc)) in du_view::crumbs(info).into_iter().enumerate() {
        if k > 0 {
            segs = segs.child(div().child(SharedString::new_static("\\")));
        }
        segs = segs.child(
            div()
                .id(SharedString::from(format!("du-crumb-{name}-{k}")))
                .hover(Styled::underline)
                .child(SharedString::from(label))
                .on_click(run(
                    cx,
                    side,
                    Command::new("panel.go").arg("side", name).arg("path", loc),
                )),
        );
    }
    let note = div()
        .flex_none()
        .ml(px(8.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(du_view::head_note(info)));
    // Открыт из кэша: «Обновить» с клавишей (`screen-ducached`).
    let refresh = (info.from_cache && !info.scanning).then(|| refresh_button(s, side, theme, cx));
    let sweep = info.scanning.then(|| sweep(theme));
    bar.child(super::panel::head_buttons(s, side, true, theme, cx))
        .child(chip)
        .child(segs)
        .child(div().flex_1())
        .child(note)
        .children(refresh)
        .children(sweep)
}

/// Чип «Анализ» перед путём.
fn chip(theme: &Theme) -> Div {
    div()
        .flex_none()
        .mr(px(8.0))
        .h(px(20.0))
        .px(px(6.0))
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
                .text_size(px(11.0))
                .child(SharedString::new_static(GLYPH_CHART)),
        )
        .child(SharedString::from(urfin_i18n::current().get("du-chip")))
}

/// «Обновить Ctrl+R» у открытого из кэша.
fn refresh_button(s: &AppState, side: Side, theme: &Theme, cx: &Listen) -> gpui_kit::Stateful<Div> {
    let name = super::chrome::side_name(side);
    let cmd = Command::new("du.refresh");
    let key = super::view::key_for(s, &cmd).unwrap_or_default();
    div()
        .id(SharedString::from(format!("du-refresh-{name}")))
        .flex_none()
        .ml(px(10.0))
        .h(px(22.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::LayerPanel))
        .text_size(px(11.5))
        .text_color(theme.c(Role::TextPrimary))
        .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
        .child(SharedString::from(urfin_i18n::current().get("du-refresh")))
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(key)),
        )
        .on_click(run(cx, side, cmd))
}

/// Неопределённый прогресс: отрезок `accent` по нижней кромке заголовка, место — по
/// времени (кадр заголовка перерисовывают тики скана).
fn sweep(theme: &Theme) -> Div {
    #[allow(clippy::cast_precision_loss, reason = "доля периода")]
    let phase = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
        % SWEEP_MS) as f32
        / SWEEP_MS as f32;
    div()
        .absolute()
        .bottom(px(-1.0))
        .h(px(2.0))
        .left(relative(phase * (1.0 + SWEEP_W) - SWEEP_W))
        .w(relative(SWEEP_W))
        .bg(theme.c(Role::AccentFill))
}

/// Чипы фильтров (F01 §3.5): «По папкам» выбран; остальные — команды `du.filter`
/// (фильтры — F01-08).
pub(super) fn chips(side: Side, theme: &Theme, cx: &Listen) -> Div {
    let i18n = urfin_i18n::current();
    let name = super::chrome::side_name(side);
    let mut row = div()
        .h(px(CHIPS_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(theme.m(Metric::PadRowX)))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider));
    for (k, (kind, key)) in du_view::FILTERS.into_iter().enumerate() {
        let on = k == 0;
        row = row.child(
            div()
                .id(SharedString::from(format!("du-filter-{name}-{kind}")))
                .flex_none()
                .h(px(26.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .border_1()
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextPrimary))
                .when(on, |d| {
                    d.border_color(theme.c(Role::AccentFill))
                        .bg(theme.c(Role::FillCursor))
                })
                .when(!on, |d| {
                    d.border_color(theme.c(Role::StrokeControl))
                        .bg(theme.c(Role::LayerPanel))
                        .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
                })
                .child(SharedString::from(i18n.get(key)))
                .on_click(run(cx, side, Command::new("du.filter").arg("kind", kind))),
        );
    }
    row
}

/// Строка отметки (F01-06, `screen-dulist`): «☑ Отмечено N · освободится X», «Удалить в
/// Корзину F8» и «Снять отметку». Без отметки — нет.
pub(super) fn mark_bar(
    s: &AppState,
    side: Side,
    t: &Tab,
    theme: &Theme,
    cx: &Listen,
) -> Option<Div> {
    if !du_view::is_du(t) || t.selection.is_empty() {
        return None;
    }
    let i18n = urfin_i18n::current();
    let name = super::chrome::side_name(side);
    let (count, bytes) = du_view::marked(t);
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", u64::from(count));
    let head = i18n.fmt("du-marked", &a);
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("size", urfin_i18n::fmt::bytes_with(&i18n, bytes));
    let frees = i18n.fmt("du-frees", &a);
    let del = Command::new("ops.delete");
    let key = super::view::key_for(s, &del).unwrap_or_default();
    Some(
        div()
            .h(px(MARK_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(14.0))
            .bg(theme.c(Role::Header))
            .border_t_2()
            .border_color(theme.c(Role::AccentFill))
            .child(
                div()
                    .font_family(ICON_FONT)
                    .text_color(theme.c(Role::AccentText))
                    .child(SharedString::new_static(GLYPH_CHECKED)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.c(Role::TextPrimary))
                            .child(SharedString::from(head)),
                    )
                    .child(
                        div()
                            .text_color(theme.c(Role::TextSecondary))
                            .child(SharedString::from(format!("· {frees}"))),
                    ),
            )
            .child(
                button(
                    theme,
                    format!("du-delete-{name}"),
                    i18n.get("du-delete-trash"),
                    &key,
                    Btn::Danger,
                )
                .on_click(run(cx, side, del)),
            )
            .child(
                button(
                    theme,
                    format!("du-unmark-{name}"),
                    i18n.get("du-unmark"),
                    "",
                    Btn::Normal,
                )
                .on_click(run(cx, side, Command::new("sel.none"))),
            ),
    )
}

/// Сцены `--screens` (имена — как у макетов `docs/design/screens/`): фикстура
/// `screens.du`, без скана настоящей ФС. Вкладку закрывает первая сцена вьювера.
pub(super) const SCENES: &[super::screens::Scene] = &[
    super::screens::Scene {
        name: "screen-duscan",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "screens.du state=scan",
        ],
    },
    // Посчитано; отметка — следующей сценой, когда листинг новой вкладки загружен.
    super::screens::Scene {
        name: "dulist-plain",
        steps: &["screens.du state=done"],
    },
    super::screens::Scene {
        name: "screen-dulist",
        steps: &[
            "panel.cursor.to name=.git",
            "sel.toggle",
            "panel.cursor.to name=Фото",
            "sel.toggle",
        ],
    },
    super::screens::Scene {
        name: "screen-ducached",
        steps: &["sel.none", "screens.du state=cached", "panel.cursor.home"],
    },
];
