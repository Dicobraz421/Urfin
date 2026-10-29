//! Каркас окна на GPUI (G-05, S2 §1.4, §2.12): заголовок 40 px с табами каждой панели
//! над своей карточкой и кнопками «─ □ ✕» (системный заголовок скрыт, зоны —
//! `WindowControlArea`: перетаскивание, Snap Layouts на «□»), cmdline и строка F-клавиш.

use gpui_kit::prelude::*;
use gpui_kit::{
    ClickEvent, Context, Div, Hsla, IntoElement, Render, SharedString, Stateful, WeakEntity,
    Window, WindowControlArea, div, px, rgb,
};
use urfin_core::{AppState, Command, Side, Tab};

use super::CoreHost;
use super::panel::Listen;
use super::theme::Theme;
use crate::keybar::{Item, Mods};
use crate::tokens::{Metric, Role};

/// Шрифт иконок ОС (S2 §1.6).
const ICON_FONT: &str = "Segoe Fluent Icons";
const W_CAPTION: f32 = 46.0;
const W_TAB_MAX: f32 = 220.0;
/// Таб (D-02): поля и скругление сверху.
const TAB_PAD_X: f32 = 14.0;
const TAB_RADIUS: f32 = 7.0;
/// Отступ заголовка слева.
pub(super) const TITLE_PAD_L: f32 = 12.0;
/// Таб не уже (№67) и «+», «› N» справа от набора.
const W_TAB_MIN: f32 = 84.0;
const W_PLUS: f32 = 32.0;
const W_MORE: f32 = 46.0;
const TAB_GAP: f32 = 2.0;
/// Окно уже — keybar показывает F3–F9 слева, статус — без средней зоны (№68).
pub(super) const NARROW_W: f32 = 900.0;

/// Раскладка набора табов (№67): ширина видимых (`None` — скрыт) и левый край «› N».
pub(super) struct TabLayout {
    pub widths: Vec<Option<f32>>,
    pub more_x: Option<f32>,
}

impl TabLayout {
    pub(super) fn hidden(&self) -> impl Iterator<Item = usize> + '_ {
        self.widths
            .iter()
            .enumerate()
            .filter(|(_, w)| w.is_none())
            .map(|(i, _)| i)
    }
}

/// Ширина наборов табов по сторонам для ширин панелей (как в [`panel_tabs`]).
pub(super) fn tabs_avail(widths: [Option<f32>; 2]) -> [f32; 2] {
    let caption = 3.0 * W_CAPTION;
    match widths {
        [Some(l), Some(r)] => [l - TITLE_PAD_L, r - TITLE_PAD_L - caption],
        [Some(w), None] | [None, Some(w)] => [w - TITLE_PAD_L - caption; 2],
        [None, None] => [0.0; 2],
    }
}

/// Названия табов панели: одноимённые — «имя · родитель» (№67).
pub(super) fn tab_titles(s: &AppState, side: Side) -> Vec<String> {
    let p = s.panel(side);
    let names: Vec<String> = p.tabs.iter().map(tab_title).collect();
    p.tabs
        .iter()
        .zip(&names)
        .map(|(t, n)| {
            let dup = names.iter().filter(|m| m.eq_ignore_ascii_case(n)).count() > 1;
            match t.loc.path.parent().and_then(|q| q.file_name()) {
                Some(parent) if dup => format!("{n} · {}", parent.to_string_lossy()),
                _ => n.clone(),
            }
        })
        .collect()
}

/// Ширина таба по тексту (оценка без раскладки шрифта: 7 px на знак).
fn natural(title: &str, close: bool) -> f32 {
    #[allow(clippy::cast_precision_loss, reason = "длина названия")]
    let text = title.chars().count() as f32 * 7.0;
    (text + 2.0 * TAB_PAD_X + if close { 20.0 } else { 0.0 }).min(W_TAB_MAX)
}

/// Раскладка табов в ширину `avail`: влезают — по тексту; нет — поровну, не уже 84;
/// и так нет — окно табов вокруг активного, остальные — в «› N» (№67).
#[allow(clippy::many_single_char_names, reason = "состояние, панель, ширины")]
pub(super) fn tab_layout(s: &AppState, side: Side, avail: f32) -> TabLayout {
    let p = s.panel(side);
    let titles = tab_titles(s, side);
    let focus = s.active == side;
    let n = titles.len();
    let nat: Vec<f32> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| natural(t, i == p.active && focus))
        .collect();
    #[allow(clippy::cast_precision_loss, reason = "табов десятки")]
    let gaps = n.saturating_sub(1) as f32 * TAB_GAP;
    let room = avail - W_PLUS - gaps;
    if nat.iter().sum::<f32>() <= room {
        return TabLayout {
            widths: nat.into_iter().map(Some).collect(),
            more_x: None,
        };
    }
    #[allow(clippy::cast_precision_loss, reason = "табов десятки")]
    let even = room / n.max(1) as f32;
    if even >= W_TAB_MIN {
        return TabLayout {
            widths: nat.into_iter().map(|w| Some(w.min(even))).collect(),
            more_x: None,
        };
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "сколько табов влезает"
    )]
    let k = (((avail - W_PLUS - W_MORE) / (W_TAB_MIN + TAB_GAP))
        .floor()
        .max(1.0)) as usize;
    let start = (p.active + 1).saturating_sub(k);
    let mut widths = vec![None; n];
    let mut x = 0.0;
    for (i, w) in widths.iter_mut().enumerate().skip(start).take(k) {
        *w = Some(W_TAB_MIN);
        let _ = i;
        x += W_TAB_MIN + TAB_GAP;
    }
    TabLayout {
        widths,
        more_x: Some(x + W_PLUS),
    }
}
/// «Закрыть» при наведении — красный Windows.
const CLOSE_HOVER: u32 = 0x00c4_2b1c;

/// Имя таба: последний компонент пути (у корня — сам путь).
pub(super) fn tab_title(t: &Tab) -> String {
    t.loc.path.file_name().map_or_else(
        || t.loc.path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Набор табов панели (D-02): таб 32, поля 14, скругление 7 сверху. Активный таб
/// активной панели — фон `panel`, текст и крестик; активный таб другой панели — фон
/// `panel`, `secondary`, без крестика; остальные — без фона, `secondary`. Свой «+».
/// `width` — ширина набора (по панели); `None` — до конца заголовка.
#[allow(
    clippy::too_many_lines,
    reason = "табы, «+», «› N», зона перетаскивания"
)]
fn tabs(
    s: &AppState,
    side: Side,
    theme: &Theme,
    click: &impl Fn(Command) -> super::overlays::Handler,
    width: Option<f32>,
    avail: f32,
) -> Div {
    let p = s.panel(side);
    let layout = tab_layout(s, side, avail);
    let titles = tab_titles(s, side);
    let squeezed = layout.more_x.is_some();
    let focus = s.active == side;
    let name = side_name(side);
    let mut row = div()
        .min_w_0()
        .h_full()
        .flex()
        .items_end()
        .gap(px(2.0))
        .overflow_hidden();
    row = match width {
        Some(w) => row.w(px(w)).flex_none(),
        None => row.flex_1(),
    };
    for (i, t) in p.tabs.iter().enumerate() {
        let Some(tw) = layout.widths.get(i).copied().flatten() else {
            continue;
        };
        let active = i == p.active;
        let id = SharedString::from(format!("tab-{name}-{}", t.id.0));
        let tab = div()
            .id(id)
            .h(px(theme.m(Metric::HTab)))
            // Сжатые табы — ровно по раскладке: «› N» встаёт туда, где его ждёт «мешочек».
            .when(squeezed, |d| d.w(px(tw)))
            .max_w(px(tw))
            .flex_none()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(TAB_PAD_X))
            .rounded_t(px(TAB_RADIUS))
            .text_color(theme.c(if active && focus {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            }))
            .when(active, |d| d.bg(theme.c(Role::LayerPanel)))
            .when(!active, |d| {
                d.hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(SharedString::from(titles[i].clone())),
            )
            .when(active && focus, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("tab-close-{name}")))
                        .text_size(px(11.0))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::new_static("×"))
                        .on_click(click(Command::new("tab.close").arg("side", name))),
                )
            })
            .on_click(click(
                Command::new("tab.goto")
                    .arg("side", name)
                    .arg("n", i64::try_from(i + 1).unwrap_or(1)),
            ));
        row = row.child(tab);
    }
    let hidden = layout.hidden().count();
    row.child(
        div()
            .id(SharedString::from(format!("tab-new-{name}")))
            .h(px(theme.m(Metric::HTab)))
            .px(px(12.0))
            .flex()
            .items_center()
            .text_color(theme.c(Role::TextSecondary))
            .hover(|d| d.text_color(theme.c(Role::TextPrimary)))
            .child(SharedString::new_static("+"))
            .on_click(click(Command::new("tab.new").arg("side", name))),
    )
    // Переполнение (№67): «› N» — «мешочек» со скрытыми табами.
    .when(hidden > 0, |d| {
        d.child(
            div()
                .id(SharedString::from(format!("tabs-more-{name}")))
                .h(px(theme.m(Metric::HTab) - 4.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .rounded_t(px(TAB_RADIUS))
                .bg(theme.c(Role::LayerPanel))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::new_static("›"))
                .child(SharedString::from(hidden.to_string()))
                .on_click(click(
                    Command::new("ui.menu").arg("menu", format!("tabs-{name}").as_str()),
                )),
        )
    })
    // Пустое место справа от табов — заголовок окна: перетаскивание, двойной клик — развернуть.
    .child(
        div()
            .id(SharedString::from(format!("drag-{name}")))
            .flex_1()
            .min_w(px(8.0))
            .h_full()
            .window_control_area(WindowControlArea::Drag),
    )
}

pub(super) fn side_name(side: Side) -> &'static str {
    if side == Side::Left { "left" } else { "right" }
}

fn caption(
    id: &'static str,
    glyph: char,
    area: WindowControlArea,
    theme: &Theme,
    hover: Hsla,
    fg_hover: Hsla,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(W_CAPTION))
        .h(px(theme.m(Metric::HTab)))
        .flex()
        .items_center()
        .justify_center()
        .font_family(ICON_FONT)
        .text_size(px(10.0))
        .text_color(theme.c(Role::TextSecondary))
        .hover(move |d| d.bg(hover).text_color(fg_hover))
        .window_control_area(area)
        .child(SharedString::from(glyph.to_string()))
}

/// Заголовок вкладки настроек (D-08): значок панелей (вернуться) и таб «Настройки ×».
pub(super) fn settings_tab(
    theme: &Theme,
    click: &impl Fn(Command) -> super::overlays::Handler,
) -> Div {
    let i18n = urfin_i18n::current();
    let close = || Command::new("ui.settings").arg("close", true);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_end()
        .gap(px(6.0))
        .child(
            div()
                .id("settings-back")
                .h(px(theme.m(Metric::HTab)))
                .px(px(TAB_PAD_X))
                .flex()
                .items_center()
                .font_family(ICON_FONT)
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static("\u{E8A1}"))
                .on_click(click(close())),
        )
        .child(
            div()
                .id("settings-tab")
                .h(px(theme.m(Metric::HTab)))
                .px(px(TAB_PAD_X))
                .flex()
                .items_center()
                .gap(px(10.0))
                .rounded_t(px(TAB_RADIUS))
                .bg(theme.c(Role::LayerPanel))
                .child(SharedString::from(i18n.get("ui-set-title")))
                .child(
                    div()
                        .id("settings-close")
                        .text_size(px(11.0))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::new_static("×"))
                        .on_click(click(close())),
                ),
        )
        .child(
            div()
                .id("drag-settings")
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag),
        )
}

/// Наборы табов (содержимое заголовка, когда меню F9 закрыто): у каждой панели над ней,
/// граница — по сплиттеру (линия 1×16). Скрытая панель — без набора; в терминале
/// (Ctrl+O) табов нет (D-02).
pub(super) fn panel_tabs(
    s: &AppState,
    theme: &Theme,
    click: &impl Fn(Command) -> super::overlays::Handler,
    widths: [Option<f32>; 2],
) -> Div {
    let row = div().flex_1().min_w_0().h_full().flex().flex_row();
    match widths {
        [Some(l), Some(r)] => row
            .child(tabs(
                s,
                Side::Left,
                theme,
                click,
                Some(l - TITLE_PAD_L),
                l - TITLE_PAD_L,
            ))
            .child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .mt(px(8.0))
                    .flex_none()
                    .self_center()
                    .bg(theme.c(Role::StrokeDivider)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl(px(TITLE_PAD_L))
                    .flex()
                    .child(tabs(
                        s,
                        Side::Right,
                        theme,
                        click,
                        None,
                        r - TITLE_PAD_L - 3.0 * W_CAPTION,
                    )),
            ),
        [Some(w), None] => row.child(tabs(
            s,
            Side::Left,
            theme,
            click,
            None,
            w - TITLE_PAD_L - 3.0 * W_CAPTION,
        )),
        [None, Some(w)] => row.child(tabs(
            s,
            Side::Right,
            theme,
            click,
            None,
            w - TITLE_PAD_L - 3.0 * W_CAPTION,
        )),
        [None, None] => row.child(
            div()
                .id("drag-term")
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag),
        ),
    }
}

/// Заголовок окна (D-02): [наборы табов][метка контекста][─ □ ✕]. Кнопки 46×32 у верха,
/// значки `secondary`; метка повышенных прав/удалённого сеанса — рамкой цвета контекста.
pub(super) fn titlebar(
    content: Div,
    theme: &Theme,
    maximized: bool,
    badge: Option<(Hsla, String)>,
) -> Div {
    let hover = theme.c(Role::FillSubtleHover);
    let primary = theme.c(Role::TextPrimary);
    let white: Hsla = rgb(0x00ff_ffff).into(); // colors: allow «Закрыть» Windows: белый на красном
    div()
        .h(px(theme.m(Metric::HTitlebar)))
        .flex_none()
        .flex()
        .flex_row()
        .pl(px(TITLE_PAD_L))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_row()
                .child(content),
        )
        .child(
            div()
                .flex_none()
                .self_start()
                .h(px(theme.m(Metric::HTab)))
                .flex()
                .items_center()
                .children(badge.map(|(color, label)| {
                    div()
                        .mr(px(12.0))
                        .px(px(8.0))
                        .py(px(2.0))
                        .border_1()
                        .border_color(color)
                        .rounded(px(4.0))
                        .text_color(color)
                        .text_size(px(12.0))
                        .child(SharedString::from(label))
                }))
                .child(caption(
                    "min",
                    '\u{E921}',
                    WindowControlArea::Min,
                    theme,
                    hover,
                    primary,
                ))
                .child(caption(
                    "max",
                    if maximized { '\u{E923}' } else { '\u{E922}' },
                    WindowControlArea::Max,
                    theme,
                    hover,
                    primary,
                ))
                .child(caption(
                    "close",
                    '\u{E8BB}',
                    WindowControlArea::Close,
                    theme,
                    rgb(CLOSE_HOVER).into(),
                    white,
                )),
        )
}

/// Cmdline скрыт: открыты настройки (D-08) или терминал в виде «экран» за спрятанными
/// панелями (S6 §3.2: сетка во всю область, ввод — в PTY).
pub(super) fn cmdline_hidden(s: &AppState) -> bool {
    let u = s.ext.get::<crate::UiState>();
    if u.is_some_and(|u| u.settings.is_some()) {
        return true;
    }
    u.is_some_and(|u| u.panels_hidden)
        && s.ext
            .get::<urfin_term::TermState>()
            .is_some_and(urfin_term::TermState::screen_view)
}

/// Промпт shell для cmdline (S6 §3.2): ячейки последней строки промпта; цвета — как
/// в терминале (oh-my-posh, starship), фон терминала по умолчанию — прозрачный.
pub(super) fn prompt_cells(s: &AppState, dark: bool) -> Option<Vec<urfin_term::vt::grid::Cell>> {
    s.ext
        .get::<urfin_term::TermState>()?
        .prompt_cells(&super::term_view::defaults(dark))
        .filter(|c| c.iter().any(|c| c.ch != ' '))
}

fn prompt_el(cells: &[urfin_term::vt::grid::Cell], dark: bool) -> Div {
    let bg = super::term_view::defaults(dark).bg;
    let mut row = div().flex().items_center();
    let mut i = 0;
    while i < cells.len() {
        let c = cells[i];
        let mut j = i + 1;
        while j < cells.len() && (cells[j].fg, cells[j].bg, cells[j].bold) == (c.fg, c.bg, c.bold) {
            j += 1;
        }
        let text: String = cells[i..j]
            .iter()
            .filter(|x| !x.spacer)
            .map(|x| x.ch)
            .collect();
        let mut run = div()
            .text_color(super::term_view::hsla(c.fg))
            .child(SharedString::from(text));
        if c.bg != bg {
            run = run.bg(super::term_view::hsla(c.bg));
        }
        if c.bold {
            run = run.font_weight(gpui_kit::FontWeight::BOLD);
        }
        row = row.child(run);
        i = j;
    }
    row
}

/// Командная строка (D-02, S6-15): промпт shell (с интеграцией) или `путь>` активной
/// панели, набранная строка и каретка-блок `accent` на символе под курсором (на нём
/// — текст цвета фона); фон `panel`, линия сверху, моно 12,5.
pub(super) fn cmdline(s: &AppState, theme: &Theme) -> Div {
    let path = s.panel(s.active).tab().loc.path.display().to_string();
    let prompt = prompt_cells(s, theme.tokens.dark());
    let (text, cursor, ghost) =
        s.ext
            .get::<urfin_term::TermState>()
            .map_or((String::new(), 0, None), |t| {
                (
                    t.line.text().to_owned(),
                    t.line.cursor(),
                    t.ghost().map(str::to_owned),
                )
            });
    let (before, rest) = text.split_at(cursor.min(text.len()));
    let mut chars = rest.chars();
    let at = chars.next();
    let after: String = chars.collect();
    let caret = div()
        .h(px(16.0))
        .flex()
        .items_center()
        .bg(theme.c(Role::AccentFill));
    let caret = match at {
        Some(c) => caret
            .text_color(theme.c(Role::LayerPanel))
            .child(SharedString::from(c.to_string())),
        None => caret.w(px(8.0)),
    };
    div()
        .h(px(theme.m(Metric::HCmdline)))
        .flex_none()
        .flex()
        .items_center()
        .px(px(theme.m(Metric::PadRowX)))
        .bg(theme.c(Role::LayerPanel))
        .border_t_1()
        .border_color(theme.c(Role::StrokeDivider))
        .font_family("Cascadia Mono")
        .text_size(px(CLI_TEXT))
        .text_color(theme.c(Role::TextPrimary))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(match &prompt {
            Some(cells) => prompt_el(cells, theme.tokens.dark()),
            None => div()
                .mr(px(6.0))
                .child(SharedString::from(format!("{path}>"))),
        })
        .child(
            div()
                .flex()
                .items_center()
                .child(SharedString::from(before.to_owned()))
                .child(caret)
                .child(SharedString::from(after))
                // Ghost-текст из истории (S6 §6.2): → или End принимают.
                .children(ghost.map(|g| {
                    div()
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(g))
                })),
        )
}

/// Статус-строка (D-02, №61): свойства файла · выделение · свободно; фон `header`.
pub(super) fn status(zones: &[String; 3], narrow: bool, theme: &Theme) -> Div {
    div()
        .h(px(theme.m(Metric::HStatus)))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px(px(theme.m(Metric::PadRowX)))
        .bg(theme.c(Role::Header))
        .border_t_1()
        .border_color(theme.c(Role::StrokeDivider))
        .text_size(px(SMALL_TEXT))
        .text_color(theme.c(Role::TextSecondary))
        .children(zones.iter().enumerate().map(|(i, z)| {
            // Узкое окно: средняя зона прячется первой (№68).
            let text = if narrow && i == 1 {
                String::new()
            } else {
                z.clone()
            };
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(SharedString::from(text))
        }))
}

/// Клавиши keybar, пока открыта встроенная строка (D-04): только дополнительные —
/// клавиши самой строки на её кнопках. `None` — обычные F-клавиши.
pub(super) fn strip_keys(s: &AppState) -> Option<Vec<(String, String)>> {
    let i18n = urfin_i18n::current();
    let qs = s
        .ext
        .get::<urfin_keymap::KeymapView>()
        .is_some_and(|k| k.qs.is_some());
    if qs {
        return Some(vec![
            ("Ctrl+Enter".into(), i18n.get("ui-keys-qs-next")),
            ("↑".into(), i18n.get("ui-keys-qs-prev")),
            ("Esc".into(), i18n.get("ui-keys-close")),
        ]);
    }
    let ui = s.ext.get::<crate::UiState>();
    // Настройки (D-08).
    if ui.is_some_and(|u| u.settings.is_some()) {
        return Some(vec![
            ("Tab".into(), i18n.get("ui-keys-section")),
            ("Esc".into(), i18n.get("ui-keys-close")),
        ]);
    }
    // Приветствие (D-02, `screen-firstrun.png`).
    if ui.is_some_and(|u| u.welcome.is_some()) && ui.is_some_and(|u| u.draft.is_none()) {
        return Some(vec![
            ("← →".into(), i18n.get("ui-keys-preset")),
            (i18n.get("ui-key-space"), i18n.get("ui-keys-flag")),
            ("Tab".into(), i18n.get("ui-keys-next-field")),
        ]);
    }
    // Раскрытый список задач (F11, D-06).
    if ui.is_some_and(|u| u.jobs_expanded) {
        return Some(vec![
            ("↑ ↓".into(), i18n.get("ui-keys-job")),
            (i18n.get("ui-key-space"), i18n.get("ui-keys-pause-resume")),
            ("Del".into(), i18n.get("ui-keys-cancel-job")),
            ("F11".into(), i18n.get("ui-keys-collapse")),
        ]);
    }
    // Дополнение поля (№70): его клавиши.
    if ui.is_some_and(|u| u.complete.as_ref().is_some_and(|c| !c.items.is_empty())) {
        return Some(vec![
            ("Tab".into(), i18n.get("ui-keys-complete-next")),
            ("↑ ↓".into(), i18n.get("ui-keys-choose")),
            ("Enter".into(), i18n.get("ui-keys-insert")),
            ("Esc".into(), i18n.get("ui-keys-close")),
        ]);
    }
    // Открытое меню или «мешочек»: его клавиши.
    if let Some(open) = ui
        .and_then(crate::menu::open_id)
        .filter(|id| *id != "drives")
    {
        let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
        return Some(match super::menu::Kind::of(open, s.active) {
            super::menu::Kind::Bar => vec![
                k("← →", "ui-keys-menu"),
                k("↑ ↓", "ui-keys-item"),
                k("Enter", "ui-keys-choose"),
                k("Esc", "ui-keys-close"),
            ],
            super::menu::Kind::Sort(_) => vec![
                k("↑ ↓", "ui-keys-item"),
                k("Enter", "ui-keys-choose"),
                (i18n.get("ui-key-space"), i18n.get("ui-keys-toggle")),
                k("Esc", "ui-keys-close"),
            ],
            super::menu::Kind::Bookmarks(_) | super::menu::Kind::History(_) => vec![
                k("Enter", "ui-keys-go"),
                k("Shift+Enter", "ui-keys-other-panel"),
                k("Esc", "ui-keys-close"),
            ],
            super::menu::Kind::Tabs(_) => vec![
                k("↑ ↓", "ui-keys-tab"),
                k("Enter", "ui-keys-go"),
                k("Esc", "ui-keys-close"),
            ],
            super::menu::Kind::Context(_) | super::menu::Kind::Columns(_) => vec![
                k("↑ ↓", "ui-keys-item"),
                k("Enter", "ui-keys-choose"),
                k("Esc", "ui-keys-close"),
            ],
        });
    }
    // Переименование на месте: строки внизу нет — клавиши поля в keybar.
    if ui.is_some_and(|u| u.rename.is_some()) {
        return Some(vec![
            ("Enter".into(), i18n.get("ui-keys-apply")),
            ("Esc".into(), i18n.get("ui-keys-cancel")),
            ("Tab".into(), i18n.get("ui-keys-name-ext")),
        ]);
    }
    let draft = ui.is_some_and(|u| u.draft.is_some());
    (draft || s.prompts.active().is_some()).then(Vec::new)
}

/// Строка клавиш (D-02): «F1 Помощь» — клавиша цветом текста, подпись `secondary`;
/// 10 клавиш — по ширине, меньше — слева с шагом 28. Клик — команда клавиши. Пока
/// открыта встроенная строка — её дополнительные клавиши (`extra`, D-04).
pub(super) fn keybar(
    items: &[Item],
    mods: Mods,
    extra: Option<&[(String, String)]>,
    narrow: bool,
    theme: &Theme,
    click: &impl Fn(Command) -> super::overlays::Handler,
) -> Div {
    if let Some(keys) = extra {
        return div()
            .h(px(theme.m(Metric::HKeybar)))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(KEYBAR_GAP))
            .px(px(theme.m(Metric::PadRowX)))
            .text_size(px(SMALL_TEXT))
            .text_color(theme.c(Role::TextSecondary))
            .children(keys.iter().map(|(k, v)| {
                div()
                    .flex()
                    .gap(px(4.0))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .text_color(theme.c(Role::TextPrimary))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(SharedString::from(k.clone())),
                    )
                    .child(SharedString::from(v.clone()))
            }));
    }
    // Узкое окно — F3–F9 слева (F1, F2, F10 — через F9; №68).
    let keys = if narrow { 3..=9 } else { 1..=10 };
    let mut shown: Vec<&Item> = items.iter().filter(|i| keys.contains(&i.n)).collect();
    shown.sort_by_key(|i| i.n);
    let full = shown.len() >= 10 && !narrow;
    let mut bar = div()
        .h(px(theme.m(Metric::HKeybar)))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(KEYBAR_GAP))
        .px(px(theme.m(Metric::PadRowX)))
        .when(full, Styled::justify_between)
        .text_size(px(SMALL_TEXT))
        .text_color(theme.c(Role::TextSecondary));
    for it in shown {
        let n = it.n;
        bar = bar.child(
            div()
                .id(SharedString::from(format!("fkey-{n}-{mods:?}")))
                .flex()
                .gap(px(4.0))
                .whitespace_nowrap()
                .child(
                    div()
                        .text_color(theme.c(Role::TextPrimary))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(SharedString::from(format!("F{n}"))),
                )
                .child(
                    div()
                        .when(!it.enabled, |d| d.text_color(theme.c(Role::TextDisabled)))
                        .child(SharedString::from(it.label.clone())),
                )
                .when(it.enabled, |d| {
                    d.hover(|d| d.text_color(theme.c(Role::TextPrimary)))
                        .on_click(click(it.cmd.clone()))
                }),
        );
    }
    bar
}

/// Текст командной строки и мелкий текст низа окна (D-02), px.
const CLI_TEXT: f32 = 12.5;
const SMALL_TEXT: f32 = 12.0;
/// Шаг клавиш, когда их меньше десяти.
const KEYBAR_GAP: f32 = 28.0;

/// Часть низа окна — своя кэшируемая view (G-07, S2-20): статус-строка меняется на
/// каждом шаге курсора, F-клавиши и cmdline — редко; одной view они пересобирались бы
/// вместе каждый кадр. Заголовок и табы не кэшируются: gpui не повторяет
/// `WindowControlArea` из кэша (перетаскивание, «─ □ ✕», Snap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bottom {
    Cmdline,
    Status,
    Keys,
}

impl Bottom {
    pub const ALL: [Bottom; 3] = [Bottom::Cmdline, Bottom::Status, Bottom::Keys];

    /// Высота части, px (cmdline спрятан у вида «экран» терминала).
    pub fn h(self, s: &AppState, theme: &Theme) -> f32 {
        match self {
            Bottom::Cmdline if cmdline_hidden(s) => 0.0,
            Bottom::Cmdline => theme.m(Metric::HCmdline),
            Bottom::Status => theme.m(Metric::HStatus),
            Bottom::Keys => theme.m(Metric::HKeybar),
        }
    }
}

pub(super) struct ChromeView {
    pub host: WeakEntity<CoreHost>,
    pub part: Bottom,
}

impl Render for ChromeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = f32::from(window.viewport_size().width) < NARROW_W;
        let Some(host) = self.host.upgrade() else {
            return div();
        };
        let h = host.read(cx);
        let Some(core) = h.core.as_ref() else {
            return div();
        };
        let s = core.state();
        let body = div().size_full().flex().flex_col();
        match self.part {
            Bottom::Cmdline if cmdline_hidden(s) => body,
            Bottom::Cmdline => body.child(cmdline(s, &h.theme)),
            Bottom::Status => body.child(status(
                &crate::status::zones(s, &h.theme.fmt),
                narrow,
                &h.theme,
            )),
            Bottom::Keys => {
                let l = Listen(self.host.clone());
                let click = |cmd: Command| -> super::overlays::Handler {
                    Box::new(l.listener(move |h: &mut CoreHost, _: &ClickEvent, w, cx| {
                        h.run(cmd.clone(), w, cx);
                    }))
                };
                body.child(keybar(
                    h.keybar.items(h.mods),
                    h.mods,
                    strip_keys(s).as_deref(),
                    narrow,
                    &h.theme,
                    &click,
                ))
            }
        }
    }
}

/// Отпечаток части низа: cmdline — путь активной панели, строка ввода, промпт; статус —
/// зоны; F-клавиши — подписи, модификаторы, клавиши встроенной строки. Всем — тема,
/// язык, узкое окно.
pub(super) fn bottom_print(
    s: &AppState,
    part: Bottom,
    items: &[Item],
    mods: Mods,
    look: u64,
    narrow: bool,
    fmt: &crate::fmt_cell::FmtCfg,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    (look, urfin_i18n::epoch(), narrow, part as u8).hash(&mut h);
    match part {
        Bottom::Cmdline => {
            s.panel(s.active).tab().loc.path.hash(&mut h);
            // Строка ввода (S6-15) и вид «экран» терминала (cmdline скрыт).
            if let Some(t) = s.ext.get::<urfin_term::TermState>() {
                (t.line.text(), t.line.cursor(), t.ghost()).hash(&mut h);
            }
            cmdline_hidden(s).hash(&mut h);
            // Промпт shell (тема — в `look`).
            prompt_cells(s, true).hash(&mut h);
        }
        Bottom::Status => crate::status::zones(s, fmt).hash(&mut h),
        Bottom::Keys => {
            format!("{mods:?}").hash(&mut h);
            strip_keys(s).hash(&mut h);
            for it in items {
                (it.n, &it.label, it.enabled).hash(&mut h);
            }
        }
    }
    h.finish()
}
