//! Вкладка анализа места `du:` на GPUI (F01-05…F01-08; `screen-duscan`, `screen-dulist`,
//! `screen-ducached`, `screen-dutypes`, `screen-dutemp`): заголовок с чипом «Анализ»,
//! путём, итогами и неопределённым прогрессом, строка чипов фильтров (у «Старых» —
//! пороги), предупреждение «Временных», строка отметки «Отмечено N · освободится X».
//! Тексты и расчёты — `crate::du_view`; карта во второй панели — `du_map`.

use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Div, SharedString, div, px, relative};
use urfin_core::{AppState, Command, Side, Tab};
use urfin_vfs::du::{DuFilter, DuInfo};

use super::panel::Listen;
use super::strip::{Btn, button};
use super::theme::Theme;
use crate::du_view;
use crate::tokens::{Metric, Role};

/// Строка чипов фильтров.
pub(super) const CHIPS_H: f32 = 40.0;
/// Строка отметки внизу панели (F01-06).
pub(super) const MARK_H: f32 = 48.0;
/// Предупреждение «Временных» в две строки (`screen-dutemp`).
pub(super) const WARN_H: f32 = 52.0;
const GLYPH_WARNING: &str = "\u{E7BA}";
const ICON_FONT: &str = "Segoe Fluent Icons";
/// Три столбика (как на макете), `CheckboxComposite`.
const GLYPH_CHART: &str = "\u{EC39}";
const GLYPH_CHECKED: &str = "\u{E73A}";
/// Копия (`Copy`) — строка «Дубликатов».
const GLYPH_COPY: &str = "\u{E8C8}";
/// Неопределённый прогресс: период прохода отрезка и его доля ширины.
const SWEEP_MS: u128 = 1600;
const SWEEP_W: f32 = 0.3;

/// Сверх шапки у таба анализа: чипы и предупреждение «Временных».
pub(super) fn head_extra(t: &Tab) -> f32 {
    if !du_view::is_du(t) {
        return 0.0;
    }
    CHIPS_H
        + if du_view::filter(t) == Some(DuFilter::Temp) {
            WARN_H
        } else {
            0.0
        }
}

/// Сверх шапки и списка у таба: чипы, предупреждение и, при отметке, строка «Отмечено»
/// (у «Дубликатов» — всегда: сводка и «Проверить хэшем») — для сетки строк.
pub(super) fn extra_h(t: &Tab) -> f32 {
    if !du_view::is_du(t) {
        return 0.0;
    }
    let bar = !t.selection.is_empty()
        || matches!(du_view::filter(t), Some(DuFilter::Dups | DuFilter::Temp));
    head_extra(t) + if bar { MARK_H } else { 0.0 }
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

/// Чип строки фильтров: выбран — рамка `accent` на `cursor`.
fn chip_el(id: String, label: String, on: bool, theme: &Theme) -> gpui_kit::Stateful<Div> {
    div()
        .id(SharedString::from(id))
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
        .child(SharedString::from(label))
}

/// Чипы фильтров (F01 §3.5): выбран — вид вкладки; клик — `du.filter` (Ctrl+1…6). У
/// «Старых» справа — пороги 6 мес · 1 год · 2 года · 5 лет (`du.old_age`).
pub(super) fn chips(t: &Tab, side: Side, theme: &Theme, cx: &Listen) -> Div {
    let i18n = urfin_i18n::current();
    let name = super::chrome::side_name(side);
    let cur = du_view::filter(t).unwrap_or(DuFilter::Dirs);
    let mut row = div()
        .h(px(CHIPS_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(theme.m(Metric::PadRowX)))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider));
    for (kind, key) in du_view::FILTERS {
        let on = cur.id() == kind;
        row =
            row.child(
                chip_el(format!("du-filter-{name}-{kind}"), i18n.get(key), on, theme)
                    .on_click(run(cx, side, Command::new("du.filter").arg("kind", kind))),
            );
    }
    if cur == DuFilter::Old {
        let age = du_view::age(t).unwrap_or_default();
        row = row.child(div().flex_1()).child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("du-age-label"))),
        );
        for (id, key) in du_view::AGES {
            row = row.child(
                chip_el(
                    format!("du-age-{name}-{id}"),
                    i18n.get(key),
                    age == id,
                    theme,
                )
                .on_click(run(cx, side, Command::new("du.old_age").arg("age", id))),
            );
        }
    }
    row
}

/// Предупреждение «Временных» (`screen-dutemp`): фон `caution`, черта сверху; правила
/// только подсказывают — ничего не отмечено.
pub(super) fn temp_warning(t: &Tab, theme: &Theme) -> Option<Div> {
    (du_view::filter(t) == Some(DuFilter::Temp)).then(|| {
        div()
            .h(px(WARN_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(theme.m(Metric::PadRowX)))
            .bg(theme.c(Role::CautionBg))
            .border_t_2()
            .border_color(theme.c(Role::CautionFg))
            .text_size(px(12.5))
            .text_color(theme.c(Role::TextPrimary))
            .child(
                div()
                    .flex_none()
                    .font_family(ICON_FONT)
                    .text_color(theme.c(Role::CautionFg))
                    .child(SharedString::new_static(GLYPH_WARNING)),
            )
            .child(div().min_w_0().child(SharedString::from(
                urfin_i18n::current().get("du-temp-warning"),
            )))
    })
}

/// Строка «Дубликатов» (`screen-dudups`): «9 групп, хэшем проверены 3 · отмечено 4 ·
/// освободится 536 МБ», «Проверить хэшем N групп Ctrl+H» (идёт проверка — «Проверяю
/// хэшем…») и «В Корзину F8».
fn dups_bar(s: &AppState, side: Side, t: &Tab, theme: &Theme, cx: &Listen) -> Div {
    let i18n = urfin_i18n::current();
    let name = super::chrome::side_name(side);
    let info = du_view::dups_info(s, t).unwrap_or_default();
    let (head, marked) = du_view::dups_line(&info, t);
    let unverified = info.groups - info.checked;
    let verify = Command::new("du.verify_hash");
    let del = Command::new("ops.delete");
    let del_key = super::view::key_for(s, &del).unwrap_or_default();
    let mut bar = div()
        .h(px(MARK_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(16.0))
        .bg(theme.c(Role::Header))
        .border_t_2()
        .border_color(theme.c(Role::AccentFill))
        .text_size(px(13.0))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_color(theme.c(Role::AccentText))
                .child(SharedString::new_static(GLYPH_COPY)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .whitespace_nowrap()
                .min_w_0()
                .overflow_hidden()
                .text_color(theme.c(Role::TextPrimary))
                .child(SharedString::from(format!("{head} ·")))
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(SharedString::from(marked)),
                ),
        );
    if info.hashing {
        bar = bar.child(
            div()
                .flex_none()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("du-dups-hashing"))),
        );
    } else if unverified > 0 {
        let key = super::view::key_for(s, &verify).unwrap_or_default();
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("count", unverified);
        bar = bar.child(
            button(
                theme,
                format!("du-verify-{name}"),
                i18n.fmt("du-dups-verify", &a),
                &key,
                Btn::Primary,
            )
            .on_click(run(cx, side, verify)),
        );
    }
    bar.child(
        button(
            theme,
            format!("du-dups-delete-{name}"),
            i18n.get("du-dups-trash"),
            &del_key,
            Btn::Normal,
        )
        .on_click(run(cx, side, del)),
    )
}

/// Строка «Временных» (решение 2026-11-02 «du-filters» №3): без отметки — «Найдено N ·
/// X» и «Отметить все» (`du.clean_select`); с отметкой — «Отмечено N · освободится X»,
/// «Очистить выбранное…» (`du.clean`: сводка и подтверждение, затем Корзина путём F8) и
/// «Снять отметку». Сама строка ничего не отмечает.
fn temp_bar(s: &AppState, side: Side, t: &Tab, theme: &Theme, cx: &Listen) -> Div {
    let i18n = urfin_i18n::current();
    let name = super::chrome::side_name(side);
    let bar = div()
        .h(px(MARK_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(14.0))
        .bg(theme.c(Role::Header))
        .border_t_2()
        .border_color(theme.c(Role::AccentFill));
    if t.selection.is_empty() {
        let found = du_view::status(s, t).1.unwrap_or_default();
        return bar
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(found)),
            )
            .child(
                button(
                    theme,
                    format!("du-temp-all-{name}"),
                    i18n.get("du-temp-select-all"),
                    "",
                    Btn::Normal,
                )
                .on_click(run(cx, side, Command::new("du.clean_select"))),
            );
    }
    let (count, bytes) = du_view::marked(t);
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", u64::from(count));
    let head = i18n.fmt("du-marked", &a);
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("size", urfin_i18n::fmt::bytes_with(&i18n, bytes));
    let frees = i18n.fmt("du-frees", &a);
    bar.child(
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
            .min_w_0()
            .overflow_hidden()
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
            format!("du-temp-clean-{name}"),
            i18n.get("du-temp-clean"),
            "",
            Btn::Danger,
        )
        .on_click(run(cx, side, Command::new("du.clean"))),
    )
    .child(
        button(
            theme,
            format!("du-temp-unmark-{name}"),
            i18n.get("du-unmark"),
            "",
            Btn::Normal,
        )
        .on_click(run(cx, side, Command::new("sel.none"))),
    )
}

/// Строка отметки (F01-06, `screen-dulist`): «☑ Отмечено N · освободится X», «Удалить в
/// Корзину F8» и «Снять отметку». Без отметки — нет; у «Дубликатов» — своя строка.
pub(super) fn mark_bar(
    s: &AppState,
    side: Side,
    t: &Tab,
    theme: &Theme,
    cx: &Listen,
) -> Option<Div> {
    if !du_view::is_du(t) {
        return None;
    }
    if du_view::filter(t) == Some(DuFilter::Dups) {
        return Some(dups_bar(s, side, t, theme, cx));
    }
    if du_view::filter(t) == Some(DuFilter::Temp) {
        return Some(temp_bar(s, side, t, theme, cx));
    }
    if t.selection.is_empty() {
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
    // Карта во второй панели (F01-07): посчитано сейчас; отметка — следующей сценой.
    super::screens::Scene {
        name: "dumap-plain",
        steps: &["sel.none", "screens.du state=done", "du.map"],
    },
    super::screens::Scene {
        name: "screen-dumap",
        steps: &[
            "panel.cursor.to name=Фото",
            "sel.toggle",
            "panel.cursor.to name=setup.exe",
            "sel.toggle",
            "panel.cursor.home",
        ],
    },
    // Фильтры (F01-08): «Типы» с раскрытыми архивами, «Временные». Строки названий
    // групп — на языке сцен (русский, как макеты).
    super::screens::Scene {
        name: "dutypes-plain",
        steps: &["sel.none", "du.filter kind=types"],
    },
    super::screens::Scene {
        name: "screen-dutypes",
        steps: &[
            "du.filter kind=types group=archive",
            "panel.cursor.to name=Архивы",
        ],
    },
    // «Дубликаты» (F01-09): группы фикстуры (уровни заданы, хэширования нет), совпавшие
    // по хэшу отмечены — когда листинг вида загружен.
    super::screens::Scene {
        name: "dudups-plain",
        steps: &["du.filter kind=dups"],
    },
    super::screens::Scene {
        name: "screen-dudups",
        steps: &[
            "panel.cursor.home",
            "panel.cursor.down",
            "panel.cursor.down",
        ],
    },
    super::screens::Scene {
        name: "dutemp-plain",
        steps: &["sel.none", "du.filter kind=temp"],
    },
    super::screens::Scene {
        name: "screen-dutemp",
        steps: &["panel.cursor.home", "panel.cursor.down"],
    },
    // Очистка «Временных» (решение 2026-11-02 «du-filters» №3): «Отметить все» и вопрос
    // со сводкой; ответа нет — ничего не удаляется.
    super::screens::Scene {
        name: "dutemp-marked",
        steps: &["du.clean_select"],
    },
    super::screens::Scene {
        name: "dutemp-clean",
        steps: &["du.clean"],
    },
];
