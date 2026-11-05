//! Вкладка пакетов `pkg:` на GPUI (F02-08; `screen-pkginstalled`, `screen-pkgupdates`):
//! заголовок с чипом «Пакеты», строкой менеджеров «winget · scoop» (ход и ошибки
//! проверки) и числом программ, чипы фильтров «Установленные / Обновления N / Найти»,
//! строка «Обновить N» при отметке в «Обновлениях». Тексты — `crate::pkg_view`.

use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Div, SharedString, div, px, relative};
use urfin_core::pkg::{Filter, PkgInfo, Run};
use urfin_core::{AppState, Command, Side, Tab};

use super::panel::Listen;
use super::strip::{Btn, button};
use super::theme::Theme;
use crate::pkg_view;
use crate::tokens::{Metric, Role};

/// Строка чипов фильтров.
pub(super) const CHIPS_H: f32 = 40.0;
/// Строка «Обновить N» внизу панели.
pub(super) const MARK_H: f32 = 52.0;
/// Баннер фоновой проверки.
const NOTICE_H: f32 = 40.0;
const ICON_FONT: &str = "Segoe Fluent Icons";
/// `Package`, `Search`, `Upload` (стрелка вверх в круге — «обновить»), `Warning`.
const GLYPH_PACKAGE: &str = "\u{E7B8}";
const GLYPH_SEARCH: &str = "\u{E721}";
const GLYPH_UPGRADE: &str = "\u{E898}";
const GLYPH_WARN: &str = "\u{E7BA}";
/// Неопределённый прогресс проверки: период прохода отрезка и его доля ширины.
const SWEEP_MS: u128 = 1600;
const SWEEP_W: f32 = 0.3;

/// Сверх шапки и списка у таба: чипы и, при отметке в «Обновлениях», строка «Обновить N».
pub(super) fn extra_h(t: &Tab) -> f32 {
    match pkg_view::filter(t) {
        None => 0.0,
        Some(f) => CHIPS_H + if marking(f, t) { MARK_H } else { 0.0 },
    }
}

fn marking(f: Filter, t: &Tab) -> bool {
    f == Filter::Updates && !t.selection.is_empty()
}

fn run(
    cx: &Listen,
    side: Side,
    cmds: Vec<Command>,
) -> impl Fn(&ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static {
    let name = super::chrome::side_name(side);
    cx.listener(move |h, _: &ClickEvent, w, cx| {
        let mut all = vec![Command::new("panel.focus").arg("side", name)];
        all.extend(cmds.iter().cloned());
        h.run_all(all, w, cx);
    })
}

/// Заголовок вкладки: кнопки «История»/«Закладки», чип «Пакеты», менеджеры через «·»
/// (идёт запрос — вторичный текст с «…», не удался — `modified` со значком), справа
/// «16 программ» / «7 из 16». Пока идёт проверка — по нижней кромке бежит отрезок
/// `accent`.
pub(super) fn path_bar(
    s: &AppState,
    side: Side,
    t: &Tab,
    info: &PkgInfo,
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
    let mut mgrs = div()
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
    for (k, m) in info.managers.iter().enumerate() {
        if k > 0 {
            mgrs = mgrs.child(div().px(px(4.0)).child(SharedString::new_static("·")));
        }
        let name = SharedString::from(m.mgr.as_str());
        mgrs = mgrs.child(match &m.run {
            Run::Idle => div().child(name),
            Run::Busy => div()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(format!("{name}…"))),
            Run::Failed(_) => div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_color(theme.c(Role::Modified))
                .child(
                    div()
                        .font_family(ICON_FONT)
                        .text_size(px(11.0))
                        .child(SharedString::new_static(GLYPH_WARN)),
                )
                .child(name),
        });
    }
    let f = pkg_view::filter(t).unwrap_or(Filter::Installed);
    let search = urfin_core::pkg::feed(s).map(|b| b.view().search.info.clone());
    let note = div()
        .flex_none()
        .ml(px(8.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(pkg_view::head_note_of(
            f,
            info,
            &search.unwrap_or_default(),
        )));
    let sweep = info.busy().then(|| sweep(theme));
    bar.child(super::panel::head_buttons(s, side, true, theme, cx))
        .child(chip(theme))
        .child(mgrs)
        .child(div().flex_1())
        .child(note)
        .children(sweep)
}

/// Чип «Пакеты» перед менеджерами.
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
                .child(SharedString::new_static(GLYPH_PACKAGE)),
        )
        .child(SharedString::from(urfin_i18n::current().get("pkg-chip")))
}

/// Неопределённый прогресс: отрезок `accent` по нижней кромке заголовка, место — по
/// времени (кадр перерисовывают ответы менеджеров).
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

/// Чипы фильтров: текущий — рамка `accent` на `cursor`; у «Обновлений» — число
/// `accent`, у «Найти» — лупа.
pub(super) fn chips(
    s: &AppState,
    side: Side,
    t: &Tab,
    info: &PkgInfo,
    theme: &Theme,
    cx: &Listen,
) -> Div {
    let i18n = urfin_i18n::current();
    let search = urfin_core::pkg::feed(s).map(|b| b.view().search.info.clone());
    let name = super::chrome::side_name(side);
    let current = pkg_view::filter(t);
    let mut row = div()
        .h(px(CHIPS_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(theme.m(Metric::PadRowX)))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider));
    for (kind, key) in pkg_view::FILTERS {
        let on = current.is_some_and(|f| f.as_str() == kind);
        let mut chip = div()
            .id(SharedString::from(format!("pkg-filter-{name}-{kind}")))
            .flex_none()
            .h(px(26.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(6.0))
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
            });
        if kind == "search" {
            chip = chip.child(
                div()
                    .font_family(ICON_FONT)
                    .text_size(px(11.0))
                    .child(SharedString::new_static(GLYPH_SEARCH)),
            );
        }
        // «Найти «7zip»» — с запросом (`screen-pkginstall`).
        let label = match &search {
            Some(q) if kind == "search" => pkg_view::search_chip(q),
            _ => i18n.get(key),
        };
        chip = chip.child(SharedString::from(label));
        if kind == "updates" && info.updates > 0 {
            chip = chip.child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme.c(Role::AccentText))
                    .child(SharedString::from(info.updates.to_string())),
            );
        }
        row = row.child(chip.on_click(run(
            cx,
            side,
            vec![Command::new("pkg.filter").arg("name", kind)],
        )));
    }
    row
}

/// Строка внизу «Обновлений» при отметке (`screen-pkgupdates`): «Обновить N»,
/// «Обновить Enter», «Обновить все N Shift+Enter», «Отмена Esc». Размер загрузки не
/// показываем (решение владельца F02 §8).
pub(super) fn upgrade_bar(
    s: &AppState,
    side: Side,
    t: &Tab,
    info: &PkgInfo,
    theme: &Theme,
    cx: &Listen,
) -> Option<Div> {
    if !pkg_view::filter(t).is_some_and(|f| marking(f, t)) {
        return None;
    }
    let i18n = urfin_i18n::current();
    let name = super::chrome::side_name(side);
    let key = |cmd: &Command, fallback: &str| {
        super::view::key_for(s, cmd).unwrap_or_else(|| fallback.to_owned())
    };
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", u64::from(t.selection.len()));
    let head = i18n.fmt("pkg-upgrade-n", &a);
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", info.updates);
    let all_label = i18n.fmt("pkg-upgrade-all", &a);
    let upgrade = Command::new("pkg.upgrade");
    let all = Command::new("pkg.upgrade_all");
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
                    .child(SharedString::new_static(GLYPH_UPGRADE)),
            )
            .child(
                div()
                    .whitespace_nowrap()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme.c(Role::TextPrimary))
                    .child(SharedString::from(head)),
            )
            .child(
                button(
                    theme,
                    format!("pkg-upgrade-{name}"),
                    i18n.get("pkg-upgrade"),
                    "Enter",
                    Btn::Primary,
                )
                .on_click(run(cx, side, vec![upgrade])),
            )
            .child(
                button(
                    theme,
                    format!("pkg-upgrade-all-{name}"),
                    all_label,
                    &key(&all, "Shift+Enter"),
                    Btn::Normal,
                )
                .on_click(run(cx, side, vec![all])),
            )
            .child(
                button(
                    theme,
                    format!("pkg-unmark-{name}"),
                    i18n.get("pkg-cancel"),
                    "Esc",
                    Btn::Normal,
                )
                .on_click(run(cx, side, vec![Command::new("sel.none")])),
            ),
    )
}

/// Высота строки «Найти пакет»: поле с кнопками и строка ответов менеджеров.
pub(super) const H_SEARCH: f32 = 76.0;
/// `Sync` — значок «ищет».
const GLYPH_BUSY: &str = "\u{E895}";

/// Строка «Найти пакет [..] в [Все ▾] [Найти Enter]» внизу вкладки и ответы менеджеров
/// под ней (`screen-pkgsearch`): «winget · 4», «scoop ищет…», «choco не ответил».
pub(super) fn search_bar(
    s: &AppState,
    field: Option<&super::field::Field>,
    input: Option<Div>,
    text: &str,
    theme: &Theme,
    click: super::overlays::ClickAll<'_>,
) -> Div {
    use super::strip::{Tone, container};
    let i18n = urfin_i18n::current();
    let info = urfin_core::pkg::feed(s)
        .map(|b| b.view().search.info.clone())
        .unwrap_or_default();
    let scope = div()
        .id("scope-btn-pkg")
        .relative()
        .flex_none()
        .h(px(30.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::FillControl))
        .text_color(theme.c(Role::TextPrimary))
        .child(SharedString::from(pkg_view::scope_label(info.scope)))
        .child(
            div()
                .text_size(px(10.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static("▾")),
        )
        .children(field.map(super::field::Field::probe_drop))
        .on_click(click(vec![Command::new("pkg.search_in")]));
    let row = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .flex_none()
                .font_family(ICON_FONT)
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static(GLYPH_PACKAGE)),
        )
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(SharedString::from(i18n.get("pkg-search-title"))),
        )
        .children(input)
        .child(
            div()
                .flex_none()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("pkg-search-in"))),
        )
        .child(scope)
        .child(
            button(
                theme,
                "pkg-search-go".into(),
                i18n.get("pkg-search-go"),
                "Enter",
                Btn::Primary,
            )
            .on_click(click(vec![Command::new("pkg.search").arg("q", text)])),
        );
    let mut line = div()
        .flex()
        .items_center()
        .gap(px(14.0))
        .text_size(px(12.0))
        .whitespace_nowrap()
        .overflow_hidden();
    for (text, look) in pkg_view::answers(&info) {
        line = line.child(match look {
            pkg_view::AnswerLook::Done => div()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(text)),
            pkg_view::AnswerLook::Busy => div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(
                    div()
                        .font_family(ICON_FONT)
                        .text_size(px(11.0))
                        .child(SharedString::new_static(GLYPH_BUSY)),
                )
                .child(SharedString::from(text)),
            pkg_view::AnswerLook::Warn => div()
                .text_color(theme.c(Role::Modified))
                .child(SharedString::from(text)),
        });
    }
    container(theme, Tone::Plain, 12.0, 9.0)
        .gap(px(8.0))
        .h(px(H_SEARCH))
        .child(row)
        .child(line)
}

/// Тело мешочка менеджеров (`screen-pkgmgrpick`): «Все — параллельно», найденные
/// менеджеры, ненайденный — серым «не найден на компьютере», черта и порядок.
/// `sel` — пункт под курсором клавиатуры.
pub(super) fn scope_body(
    s: &AppState,
    sel: usize,
    theme: &Theme,
    pick: &dyn Fn(&'static str) -> super::overlays::Handler,
) -> (Div, usize) {
    let pinfo = urfin_core::pkg::feed(s)
        .map(|b| b.info())
        .unwrap_or_default();
    let cur = urfin_core::pkg::feed(s)
        .map(|b| b.view().search.info.scope)
        .unwrap_or_default();
    let order: Vec<urfin_core::pkg::Mgr> = s
        .ext
        .get::<urfin_core::pkg::PkgModel>()
        .map_or_else(|| urfin_core::pkg::Mgr::ALL.to_vec(), |p| p.config.order.clone());
    let items = pkg_view::scope_items(&pinfo);
    let n = items.len();
    let mut body = div().flex().flex_col().text_size(px(12.5));
    for (i, (scope, label, hint, ok)) in items.into_iter().enumerate() {
        let on = scope == cur;
        let mut item = div()
            .id(SharedString::from(format!("pkg-scope-{i}")))
            .h(px(super::pouch::H_ITEM))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(4.0))
            .when(i == sel, |d| d.bg(theme.c(Role::FillCursor)))
            .when(i != sel && ok, |d| {
                d.hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
            })
            .text_color(theme.c(if ok {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            }))
            .child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .font_family(ICON_FONT)
                    .text_size(px(11.0))
                    .child(SharedString::new_static(if on { "\u{E73E}" } else { "" })),
            )
            .child(div().flex_1().child(SharedString::from(label)))
            .child(
                div()
                    .flex_none()
                    .pl(px(16.0))
                    .text_size(px(11.5))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(hint)),
            );
        if ok {
            item = item.on_click(pick(pkg_view::scope_arg(scope)));
        }
        body = body.child(item);
    }
    let body = body
        .child(
            div()
                .my(px(4.0))
                .mx(px(6.0))
                .h(px(1.0))
                .bg(theme.c(Role::StrokeDivider)),
        )
        .child(
            div()
                .h(px(24.0))
                .px(px(32.0))
                .flex()
                .items_center()
                .text_size(px(11.5))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(pkg_view::order_note(&order))),
        );
    (body, n)
}

/// Пункт мешочка менеджеров под номером `i` (`None` — недоступен).
pub(super) fn scope_at(s: &AppState, i: usize) -> Option<&'static str> {
    let pinfo = urfin_core::pkg::feed(s)
        .map(|b| b.info())
        .unwrap_or_default();
    pkg_view::scope_items(&pinfo)
        .get(i)
        .filter(|x| x.3)
        .map(|x| pkg_view::scope_arg(x.0))
}

/// Номер текущей области в мешочке.
pub(super) fn scope_index(s: &AppState) -> usize {
    let cur = urfin_core::pkg::feed(s)
        .map(|b| b.view().search.info.scope)
        .unwrap_or_default();
    let pinfo = urfin_core::pkg::feed(s)
        .map(|b| b.info())
        .unwrap_or_default();
    pkg_view::scope_items(&pinfo)
        .iter()
        .position(|x| x.0 == cur)
        .unwrap_or(0)
}

/// Фильтр, к которому возвращает Esc в строке поиска.
pub(super) fn search_back(s: &AppState) -> &'static str {
    urfin_core::pkg::feed(s)
        .and_then(|b| b.view().search.info.back)
        .unwrap_or(Filter::Installed)
        .as_str()
}

/// Баннер фоновой проверки (`screen-pkgbanner`, F02 §4): строка 40 в зоне событий над
/// командной строкой, фон `header`, черта `accent` сверху; значок пакета, полужирное
/// «Доступно N обновлений · M важных», вторичное «проверено по расписанию …» и сразу за
/// ним «Показать» (основная) и «Позже».
pub(super) fn notice_bar(
    s: &AppState,
    theme: &Theme,
    click: super::overlays::Click<'_>,
) -> Option<Div> {
    let n = urfin_core::pkg::notice(s)?;
    let (head, when) = pkg_view::notice(n, &theme.fmt, pkg_view::now_ns());
    let i18n = urfin_i18n::current();
    let btn = |id: &str, label: String, kind: Btn, cmd: &'static str| {
        button(theme, id.to_owned(), label, "", kind).on_click(click(Command::new(cmd)))
    };
    Some(
        div()
            .h(px(NOTICE_H))
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
                    .font_family(ICON_FONT)
                    .text_color(theme.c(Role::AccentText))
                    .child(SharedString::new_static(GLYPH_PACKAGE)),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(SharedString::from(head)),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(when)),
            )
            .child(div().w(px(6.0)).flex_none())
            .child(btn(
                "pkgbanner-show",
                i18n.get("pkg-banner-show"),
                Btn::Primary,
                "pkg.banner.show",
            ))
            .child(btn(
                "pkgbanner-later",
                i18n.get("pkg-later"),
                Btn::Normal,
                "pkg.banner.later",
            )),
    )
}

/// Сцены `--screens` (имена — как у макетов `docs/design/screens/`): фикстура
/// `screens.pkg`, без менеджеров. Последняя закрывает вкладку: следующие сцены
/// опираются на прежние табы.
pub(super) const SCENES: &[super::screens::Scene] = &[
    super::screens::Scene {
        name: "pkginstalled-plain",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "screens.pkg",
        ],
    },
    // Курсор и отметка — следующей сценой, когда листинг вкладки загружен.
    super::screens::Scene {
        name: "screen-pkginstalled",
        steps: &["panel.cursor.to name=Firefox"],
    },
    super::screens::Scene {
        name: "pkgupdates-plain",
        steps: &["pkg.filter name=updates"],
    },
    super::screens::Scene {
        name: "screen-pkgupdates",
        steps: &[
            "panel.cursor.to name=\"Node.js LTS\"",
            "sel.toggle",
            "panel.cursor.to name=\"PowerShell 7\"",
            "sel.toggle",
            "panel.cursor.to name=\"Visual Studio Code\"",
            "sel.toggle",
        ],
    },
    // Поиск (F02-09): строка открыта, ответы winget и scoop.
    super::screens::Scene {
        name: "pkgsearch-plain",
        steps: &["sel.none", "screens.pkg step=search"],
    },
    super::screens::Scene {
        name: "screen-pkgsearch",
        steps: &["panel.cursor.to name=7zip.7zip"],
    },
    // Мешочек менеджеров от кнопки «Все ▾».
    super::screens::Scene {
        name: "screen-pkgmgrpick",
        steps: &["pkg.search_in"],
    },
    // Установка (F02-10): ход в строке группы, job-бар, ожидание UAC.
    super::screens::Scene {
        name: "screen-pkginstall",
        steps: &["pkg.search_in", "screens.pkg step=install"],
    },
    // Задачи установки отменены: снимок — когда их строки ушли из job-бара.
    super::screens::Scene {
        name: "pkgtab-closed",
        steps: &["screens.pkg stop=true", super::screens::WAIT_JOBS],
    },
    // Проверка по расписанию (F02-11): баннер над командной строкой; «Позже» убирает.
    super::screens::Scene {
        name: "screen-pkgbanner",
        steps: &["screens.pkg step=banner"],
    },
    super::screens::Scene {
        name: "pkgbanner-later",
        steps: &["pkg.banner.later"],
    },
];
