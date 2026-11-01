//! Шапка вьювера и её меню (S7 §6, `screen-viewer.png`, `screen-viewerenc.png`): вкладки
//! экранов в заголовке окна, шапка 36 — значок, путь к папке, «только чтение», чип
//! кодировки и сегменты режимов; «мешочек» кодировок и меню режимов от них; клавиши
//! keybar по макету.

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, Font, Hsla, SharedString, TextRun, Window,
    WindowControlArea, div, px,
};
use urfin_core::{AppState, Command};
use urfin_view::{CpGroup, Mode, ViewState, ViewerDoc, codepage_menu};

use super::super::CoreHost;
use super::super::overlays::Click;
use super::super::pouch::{H_ITEM, Notch, place, pouch};
use super::super::theme::Theme;
use super::rows::H_HEAD;
use super::{Menu, MenuKind, Viewer, enc_label, shown};
use crate::tokens::{Metric, Role};

const ICON_FONT: &str = "Segoe Fluent Icons";
const TAB_PAD_X: f32 = 14.0;
const TAB_RADIUS: f32 = 7.0;
/// Поля шапки, зазор чипа и сегментов.
const PAD_X: f32 = 12.0;
const GAP_CHIP: f32 = 16.0;
/// Чип и сегменты.
const H_CTRL: f32 = 26.0;
const CHIP_PAD: f32 = 10.0;
const SEG_PAD: f32 = 14.0;
const TEXT: f32 = 13.0;
const SMALL: f32 = 12.0;
/// Пунктов «мешочка» на виду (остальные — прокруткой).
pub(super) const VISIBLE: usize = 11;
const W_ENC: f32 = 300.0;
const W_MODE: f32 = 200.0;
const H_DIVIDER: f32 = 9.0;

/// Ширина текста шрифтом окна.
pub(super) struct Measure {
    font: Font,
    sys: std::sync::Arc<gpui_kit::WindowTextSystem>,
    pub(super) win_w: f32,
    pub(super) win_h: f32,
}

impl Measure {
    pub(super) fn new(window: &Window) -> Measure {
        Measure {
            font: window.text_style().font(),
            sys: window.text_system().clone(),
            win_w: f32::from(window.viewport_size().width),
            win_h: f32::from(window.viewport_size().height),
        }
    }

    pub(super) fn w(&self, text: &str, size: f32) -> f32 {
        let run = TextRun {
            len: text.len(),
            font: self.font.clone(),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        f32::from(
            self.sys
                .shape_line(SharedString::from(text.to_owned()), px(size), &[run], None)
                .width,
        )
        .ceil()
    }

    /// Ширина чипа кодировки.
    fn chip(&self, d: &ViewerDoc) -> f32 {
        let mut w = 2.0 * CHIP_PAD + 2.0 + self.w(&enc_label(d.enc.enc.name()), TEXT) + 6.0 + 8.0;
        if d.enc.auto {
            w += 6.0 + self.w(&urfin_i18n::current().get("ui-view-enc-auto"), SMALL);
        }
        w
    }

    /// Ширины сегментов «Текст | Разметка | Hex».
    fn segs(&self) -> [f32; 3] {
        modes().map(|(_, key)| self.w(&urfin_i18n::current().get(key), TEXT) + 2.0 * SEG_PAD)
    }
}

fn modes() -> [(Mode, &'static str); 3] {
    [
        (Mode::Text, "ui-view-mode-text"),
        (Mode::Markup, "ui-view-mode-markup"),
        (Mode::Hex, "ui-view-mode-hex"),
    ]
}

fn mode_arg(m: Mode) -> &'static str {
    match m {
        Mode::Text => "text",
        Mode::Markup => "markup",
        Mode::Hex => "hex",
    }
}

/// Всплывающая подсказка кнопки заголовка: текст на плашке `overlay`.
struct Tip {
    text: SharedString,
    bg: Hsla,
    border: Hsla,
    color: Hsla,
}

impl gpui_kit::Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(self.border)
            .bg(self.bg)
            .text_size(px(SMALL))
            .text_color(self.color)
            .child(self.text.clone())
    }
}

/// Путь для показа: локальный — путь ОС, иначе uri.
fn path_text(l: &urfin_core::Location) -> String {
    if l.is_local() {
        l.path.display().to_string()
    } else {
        l.to_uri()
    }
}

/// Вкладки экранов в заголовке окна (S7 §3.9): кнопка «две панели» (к панелям, экран не
/// закрывается; путь — в подсказке), вкладки документов — имя и × у активной, клик по
/// другой — `viewer.screen`. Список экранов (F12) — кнопка «Экраны» справа (S7-25).
#[allow(
    clippy::too_many_lines,
    reason = "кнопка панелей, вкладки, место перетаскивания"
)]
pub(super) fn tabs(s: &AppState, theme: &Theme, click: Click<'_>) -> Option<Div> {
    let active = shown(s).or_else(|| super::editor::shown(s))?;
    let v = s.ext.get::<ViewState>()?;
    let close = if v.viewer(active).is_some() {
        "viewer.close"
    } else {
        "editor.close"
    };
    let back = Command::new("panel.focus").arg("side", crate::side_arg(s.active));
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("path", path_text(&s.active_tab().loc));
    let tip = SharedString::from(urfin_i18n::current().fmt("ui-view-to-panels", &a));
    let (bg, border, color) = (
        theme.c(Role::LayerOverlay),
        theme.c(Role::StrokeCard),
        theme.c(Role::TextPrimary),
    );
    let mut row = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_end()
        .gap(px(6.0))
        .child(
            div()
                .id("viewer-back")
                .h(px(theme.m(Metric::HTab)))
                .px(px(TAB_PAD_X))
                .flex()
                .items_center()
                .font_family(ICON_FONT)
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static("\u{E8A1}"))
                .hover(|d| d.text_color(color))
                .tooltip(move |_, cx| {
                    let text = tip.clone();
                    cx.new(|_| Tip {
                        text,
                        bg,
                        border,
                        color,
                    })
                    .into()
                })
                .on_click(click(back)),
        );
    for (id, loc, _) in v.screen_list() {
        let mut name = loc
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Изменённый документ редактора — ● после имени (`screen-editor.png`).
        if v.editor(id).is_some_and(|d| d.dirty) {
            name.push_str(" ●");
        }
        let on = id == active;
        let mut tab = div()
            .id(SharedString::from(format!("viewer-tab-{}", id.0)))
            .h(px(theme.m(Metric::HTab)))
            .px(px(TAB_PAD_X))
            .flex()
            .items_center()
            .gap(px(10.0))
            .rounded_t(px(TAB_RADIUS))
            .text_size(px(SMALL))
            .child(SharedString::from(name));
        if on {
            tab = tab.bg(theme.c(Role::LayerPanel)).child(
                div()
                    .id("viewer-close")
                    .text_size(px(11.0))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::new_static("×"))
                    .on_click(click(Command::new(close))),
            );
        } else {
            tab = tab
                .text_color(theme.c(Role::TextSecondary))
                .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
                .on_click(click(
                    Command::new("viewer.screen").arg("doc", i64::from(id.0)),
                ));
        }
        row = row.child(tab);
    }
    Some(
        row.child(
            div()
                .id("drag-viewer")
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag),
        ),
    )
}

/// Содержимое чипа кодировки: «UTF-8 авто ⌄».
fn chip_inner(d: &ViewerDoc, theme: &Theme) -> Div {
    div()
        .h(px(H_CTRL))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .text_size(px(TEXT))
        .child(enc_label(d.enc.enc.name()))
        .children(d.enc.auto.then(|| {
            div()
                .text_size(px(SMALL))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(
                    urfin_i18n::current().get("ui-view-enc-auto"),
                ))
        }))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(8.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(crate::icons::CHEVRON_DOWN.to_string())),
        )
}

/// Сегменты режимов; Разметка недоступна — `text.disabled`, без клика.
fn segments(d: &ViewerDoc, theme: &Theme, m: &Measure, click: Option<Click<'_>>) -> Div {
    let w = m.segs();
    let mut row = div()
        .h(px(H_CTRL))
        .flex()
        .flex_none()
        .overflow_hidden()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::LayerPanel));
    for (k, (mode, key)) in modes().into_iter().enumerate() {
        let on = d.mode == mode;
        let ok = mode != Mode::Markup || d.markup_ok();
        let mut seg = div()
            .id(SharedString::from(format!("viewer-mode-{k}")))
            .w(px(w[k]))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(TEXT))
            .child(SharedString::from(urfin_i18n::current().get(key)));
        if on {
            seg = seg.bg(theme.c(Role::FillCursor));
        }
        if !ok {
            seg = seg.text_color(theme.c(Role::TextDisabled));
        } else if let Some(click) = click
            && !on
        {
            seg = seg
                .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
                .on_click(click(
                    Command::new("viewer.mode").arg("mode", mode_arg(mode)),
                ));
        }
        row = row.child(seg);
    }
    row
}

/// Шапка 36: кнопка «История» (Alt+F11, S7-25), значок, путь к папке, «только чтение»;
/// справа чип кодировки и режимы.
pub(super) fn header(
    s: &AppState,
    d: &ViewerDoc,
    theme: &Theme,
    click: Click<'_>,
    m: &Measure,
) -> Div {
    let folder = d
        .loc
        .path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    div()
        .h(px(H_HEAD))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(PAD_X))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .child(super::lists::history_button(s, theme, click))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(14.0))
                .text_color(theme.c(Role::AccentText))
                .child(SharedString::from(
                    crate::icons::glyph(urfin_vfs::icons::IconClass::Doc).to_string(),
                )),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(TEXT))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(folder)),
        )
        .child(
            div()
                .flex_none()
                .h(px(18.0))
                .px(px(6.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(Role::StrokeControl))
                .text_size(px(11.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(
                    urfin_i18n::current().get("ui-view-readonly"),
                )),
        )
        .child(div().flex_1())
        .child(
            div()
                .id("viewer-enc")
                .flex_none()
                .w(px(m.chip(d)))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(Role::StrokeControl))
                .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
                .child(chip_inner(d, theme).h(px(H_CTRL - 2.0)))
                .on_click(click(Command::new("viewer.codepage_menu"))),
        )
        .child(segments(d, theme, m, Some(click)).ml(px(GAP_CHIP - 8.0)))
}

/// Пункт меню.
pub(super) struct Item {
    pub label: SharedString,
    pub hint: String,
    pub cmd: Command,
    pub enabled: bool,
    /// Черта над пунктом (новая группа).
    pub divider: bool,
    /// Выбран сейчас (✓).
    pub current: bool,
}

/// Подпись группы кодировки справа (`screen-viewerenc.png`).
pub(super) fn enc_hint(group: CpGroup, name: &str) -> String {
    let key = match (group, name) {
        (CpGroup::Unicode, _) => "ui-enc-unicode",
        (CpGroup::Cyrillic, "windows-1251") => "ui-enc-cyr-windows",
        (CpGroup::Cyrillic, "IBM866") => "ui-enc-cyr-dos",
        (CpGroup::Cyrillic, "KOI8-R") => "ui-enc-cyr-unix",
        (CpGroup::Cyrillic, "KOI8-U") => "ui-enc-ukrainian",
        (CpGroup::Cyrillic, "ISO-8859-5") => "ui-enc-cyr-iso",
        (CpGroup::Cyrillic, _) => "ui-enc-cyr-mac",
        (CpGroup::Western, _) => "ui-enc-western",
        (CpGroup::Other, _) => "ui-enc-other",
    };
    urfin_i18n::current().get(key)
}

/// Пункты меню: кодировки («Авто» и группы) или режимы.
pub(super) fn items(kind: MenuKind, d: &ViewerDoc) -> Vec<Item> {
    let i18n = urfin_i18n::current();
    match kind {
        MenuKind::Enc => {
            let mut out = vec![Item {
                label: i18n.get("ui-enc-auto").into(),
                hint: i18n.get("ui-enc-auto-hint"),
                cmd: Command::new("viewer.codepage").arg("cp", "auto"),
                enabled: true,
                divider: false,
                current: false,
            }];
            for (group, list) in codepage_menu() {
                for (k, e) in list.iter().enumerate() {
                    out.push(Item {
                        label: enc_label(e.name()),
                        hint: enc_hint(group, e.name()),
                        cmd: Command::new("viewer.codepage").arg("cp", e.name()),
                        enabled: true,
                        divider: k == 0,
                        current: d.enc.enc.name() == e.name(),
                    });
                }
            }
            out
        }
        MenuKind::Mode => modes()
            .into_iter()
            .map(|(m, key)| Item {
                label: i18n.get(key).into(),
                hint: String::new(),
                cmd: Command::new("viewer.mode").arg("mode", mode_arg(m)),
                enabled: m != Mode::Markup || d.markup_ok(),
                divider: false,
                current: d.mode == m,
            })
            .collect(),
    }
}

/// Пункт текущего значения (с него начинается выбор).
pub(super) fn current(_kind: MenuKind, _d: &ViewerDoc, items: &[Item]) -> usize {
    items.iter().position(|i| i.current).unwrap_or(0)
}

/// Первый видимый пункт, чтобы выбранный был на виду.
pub(super) fn keep_visible(first: usize, sel: usize, n: usize) -> usize {
    let max_first = n.saturating_sub(VISIBLE);
    if sel < first {
        sel
    } else if sel >= first + VISIBLE {
        sel + 1 - VISIBLE
    } else {
        first
    }
    .min(max_first)
}

/// Клавиши keybar вьювера (по макетам): текст, Hex, открытое меню.
pub(super) fn keys(d: &ViewerDoc, menu: Option<MenuKind>) -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    match menu {
        Some(MenuKind::Enc) => vec![
            k("↑ ↓", "ui-keys-select"),
            k("Enter", "ui-keys-apply"),
            k("F8", "ui-keys-cycle"),
            k("Esc", "ui-keys-close"),
        ],
        Some(MenuKind::Mode) => vec![
            k("↑ ↓", "ui-keys-select"),
            k("Enter", "ui-keys-apply"),
            k("Esc", "ui-keys-close"),
        ],
        None if d.mode == Mode::Hex => vec![
            k("Shift+F4", "keybar-viewer-mode"),
            k("F7", "keybar-viewer-search"),
            k("F8", "keybar-viewer-codepage"),
            k("Alt+F8", "keybar-viewer-goto-offset"),
            k("Esc", "keybar-viewer-close"),
        ],
        // Markdown-документ: перенос всегда по словам, F2 не действует (S7 §3.10).
        None if d.md_mode() => vec![
            k("F7", "keybar-viewer-search"),
            k("F8", "keybar-viewer-codepage"),
            k("Shift+F4", "keybar-viewer-mode"),
            k("Alt+F8", "keybar-viewer-goto"),
            k("Esc", "keybar-viewer-close"),
        ],
        None => vec![
            k("F2", "keybar-viewer-wrap"),
            k("F7", "keybar-viewer-search"),
            k("F8", "keybar-viewer-codepage"),
            k("Alt+F8", "keybar-viewer-goto"),
            k("Esc", "keybar-viewer-close"),
        ],
    }
}

/// Строка пункта: ✓, подпись (у текущей кодировки детекта — метка «авто»), подсказка.
fn item_row(
    i: usize,
    it: &Item,
    sel: bool,
    auto: bool,
    theme: &Theme,
    cx: &Context<CoreHost>,
) -> AnyElement {
    let cmd = it.cmd.clone();
    let mut row = div()
        .id(SharedString::from(format!("viewer-menu-{i}")))
        .h(px(H_ITEM))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .text_size(px(TEXT))
        .child(
            div()
                .w(px(22.0))
                .flex_none()
                .font_family(ICON_FONT)
                .text_size(px(10.0))
                .children(
                    it.current
                        .then(|| SharedString::from(crate::icons::ACCEPT.to_string())),
                ),
        )
        .child(it.label.clone())
        .children((it.current && auto).then(|| {
            div()
                .ml(px(8.0))
                .px(px(6.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(Role::AccentFill))
                .text_size(px(11.0))
                .child(SharedString::from(
                    urfin_i18n::current().get("ui-view-enc-auto"),
                ))
        }))
        .child(div().flex_1().min_w(px(16.0)))
        .child(
            div()
                .text_size(px(SMALL))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(it.hint.clone())),
        );
    if sel {
        row = row.bg(theme.c(Role::FillCursor));
    }
    if it.enabled {
        row = row
            .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
            .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                if let Some(v) = h.features.get_mut::<Viewer>() {
                    v.menu = None;
                }
                h.run(cmd.clone(), w, cx);
            }));
    } else {
        row = row.text_color(theme.c(Role::TextDisabled));
    }
    row.into_any_element()
}

/// Открытое меню: щит под ним (клик мимо закрывает) и «мешочек» от чипа или сегментов.
pub(super) fn menu(
    m: Menu,
    d: &ViewerDoc,
    theme: &Theme,
    me: &Measure,
    cx: &Context<CoreHost>,
) -> Vec<AnyElement> {
    let seg_w: f32 = me.segs().iter().sum::<f32>() + 2.0;
    let seg_x = me.win_w - PAD_X - seg_w;
    let (ax, aw, w) = match m.kind {
        MenuKind::Enc => {
            let cw = me.chip(d);
            (seg_x - (GAP_CHIP - 8.0) - 8.0 - cw, cw, W_ENC)
        }
        MenuKind::Mode => (seg_x, seg_w, W_MODE),
    };
    let (left, nx) = place(ax, aw, w, me.win_w, w - aw);
    let top = (H_HEAD - H_CTRL) / 2.0;
    let content = match m.kind {
        MenuKind::Enc => div()
            .h(px(H_HEAD - top))
            .flex()
            .flex_col()
            .child(chip_inner(d, theme).h(px(H_CTRL))),
        MenuKind::Mode => div()
            .h(px(H_HEAD - top))
            .flex()
            .flex_col()
            .child(segments(d, theme, me, None)),
    };
    let notch = Notch {
        x: nx,
        w: aw,
        h: H_HEAD - top,
        content: content.into_any_element(),
    };
    figure(m, d, theme, (left, top, w), Some(notch), cx)
}

/// Чипы быстрого просмотра (`screen-quickview.png`): высота 22, текст 12, поля 0 6 0 8,
/// рамка, скругление 5; между чипами 8.
const QV_CHIP_H: f32 = 22.0;
const QV_GAP: f32 = 8.0;
const QV_SMALL: f32 = 12.0;
const QV_W_MODE: f32 = 160.0;

/// Подпись режима документа.
fn mode_label(d: &ViewerDoc) -> String {
    let key = modes()
        .into_iter()
        .find(|(m, _)| *m == d.mode)
        .map_or("ui-view-mode-text", |(_, k)| k);
    urfin_i18n::current().get(key)
}

/// Ширины чипов кодировки и режима в шапке быстрого просмотра.
fn qv_widths(d: &ViewerDoc, me: &Measure) -> (f32, f32) {
    let i18n = urfin_i18n::current();
    let pad = 8.0 + 4.0 + 8.0 + 6.0 + 2.0;
    let mut enc = pad + me.w(&enc_label(d.enc.enc.name()), QV_SMALL);
    if d.enc.auto {
        enc += 4.0 + me.w(&i18n.get("ui-view-enc-auto"), 11.0);
    }
    (enc, pad + me.w(&mode_label(d), QV_SMALL))
}

/// Левые края чипов кодировки и режима в области шириной `area_w` (справа, поле 12).
fn qv_at(d: &ViewerDoc, me: &Measure, area_w: f32) -> ((f32, f32), (f32, f32)) {
    let (ew, mw) = qv_widths(d, me);
    let mx = area_w - PAD_X - mw;
    ((mx - QV_GAP - ew, ew), (mx, mw))
}

/// Содержимое чипа: подпись (у кодировки детекта — «авто») и шеврон.
fn qv_chip_inner(label: SharedString, auto: bool, theme: &Theme) -> Div {
    div()
        .h(px(QV_CHIP_H - 2.0))
        .pl(px(8.0))
        .pr(px(6.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .text_size(px(QV_SMALL))
        .text_color(theme.c(Role::TextPrimary))
        .child(label)
        .children(auto.then(|| {
            div()
                .text_size(px(11.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(
                    urfin_i18n::current().get("ui-view-enc-auto"),
                ))
        }))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(8.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(crate::icons::CHEVRON_DOWN.to_string())),
        )
}

/// Чипы «кодировка ▾» и «режим ▾» шапки быстрого просмотра (S7-25): клик переводит фокус
/// в область и открывает «мешочек» от своего чипа (как Shift+F8 / Shift+F4).
pub(super) fn qv_chips(
    d: &ViewerDoc,
    side: urfin_core::Side,
    theme: &Theme,
    me: &Measure,
    click_all: super::super::overlays::ClickAll<'_>,
) -> Div {
    let (ew, mw) = qv_widths(d, me);
    let focus = Command::new("viewer.quick")
        .arg("side", super::quick::side_name(side))
        .arg("path", d.loc.clone())
        .arg("focus", true);
    let chip = |id: &'static str, w: f32, inner: Div, cmd: Command| {
        div()
            .id(id)
            .flex_none()
            .w(px(w))
            .h(px(QV_CHIP_H))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .border_1()
            .border_color(theme.c(Role::StrokeControl))
            .bg(theme.c(Role::LayerPanel))
            .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
            .child(inner)
            .on_click(click_all(vec![focus.clone(), cmd]))
    };
    div()
        .flex()
        .items_center()
        .gap(px(QV_GAP))
        .child(chip(
            "qv-enc",
            ew,
            qv_chip_inner(enc_label(d.enc.enc.name()), d.enc.auto, theme),
            Command::new("viewer.codepage_menu"),
        ))
        .child(chip(
            "qv-mode",
            mw,
            qv_chip_inner(SharedString::from(mode_label(d)), false, theme),
            Command::new("viewer.mode"),
        ))
}

/// Меню быстрого просмотра (Shift+F8, Shift+F4, клик по чипу): «мешочек» от своего чипа
/// в шапке области (высота `head_h`), правый угол прямой; координаты — от верха области
/// под шапкой.
pub(super) fn qv_menu(
    m: Menu,
    d: &ViewerDoc,
    theme: &Theme,
    me: &Measure,
    (area_w, head_h): (f32, f32),
    cx: &Context<CoreHost>,
) -> Vec<AnyElement> {
    let (enc, mode) = qv_at(d, me, area_w);
    let ((ax, aw), w, inner) = match m.kind {
        MenuKind::Enc => (
            enc,
            W_ENC,
            qv_chip_inner(enc_label(d.enc.enc.name()), d.enc.auto, theme),
        ),
        MenuKind::Mode => (
            mode,
            QV_W_MODE,
            qv_chip_inner(SharedString::from(mode_label(d)), false, theme),
        ),
    };
    let w = w.min(area_w);
    let (left, nx) = place(ax, aw, w, area_w, w - aw);
    let top = (head_h - QV_CHIP_H) / 2.0;
    let notch = Notch {
        x: nx,
        w: aw,
        h: head_h - top,
        content: div()
            .h_full()
            .flex()
            .flex_col()
            .child(div().h(px(QV_CHIP_H)).flex().items_center().child(inner))
            .into_any_element(),
    };
    figure(m, d, theme, (left, top - head_h, w), Some(notch), cx)
}

/// Щит (клик мимо закрывает) и фигура меню: левый верхний угол и ширина, корешок.
fn figure(
    m: Menu,
    d: &ViewerDoc,
    theme: &Theme,
    (left, top, w): (f32, f32, f32),
    notch: Option<Notch>,
    cx: &Context<CoreHost>,
) -> Vec<AnyElement> {
    let items = items(m.kind, d);
    let n = items.len();
    let sel = m.sel.unwrap_or_else(|| current(m.kind, d, &items));
    let first = keep_visible(m.first, sel, n);
    let mut body = div().flex().flex_col().relative();
    for (i, it) in items.iter().enumerate().skip(first).take(VISIBLE) {
        if it.divider && i != first {
            body = body.child(
                div()
                    .h(px(H_DIVIDER))
                    .flex()
                    .items_center()
                    .child(div().h(px(1.0)).flex_1().bg(theme.c(Role::StrokeDivider))),
            );
        }
        body = body.child(item_row(i, it, i == sel, d.enc.auto, theme, cx));
    }
    if n > VISIBLE {
        // Позиция списка: тонкий бегунок справа.
        #[allow(clippy::cast_precision_loss, reason = "пунктов — десятки")]
        let (t, h) = (first as f32 / n as f32, VISIBLE as f32 / n as f32);
        body = body.child(
            div()
                .absolute()
                .right(px(-2.0))
                .top(gpui_kit::relative(t))
                .h(gpui_kit::relative(h))
                .w(px(3.0))
                .rounded(px(2.0))
                .bg(theme.c(Role::TextTertiary)),
        );
    }
    let fig = pouch(theme, left, top, w, notch, body)
        .id("viewer-menu")
        .occlude()
        .on_scroll_wheel(
            cx.listener(move |h, ev: &gpui_kit::ScrollWheelEvent, _, cx| {
                let dy = f32::from(ev.delta.pixel_delta(px(H_ITEM)).y);
                if let Some(v) = h.features.get_mut::<Viewer>()
                    && let Some(m) = v.menu.as_mut()
                {
                    #[allow(clippy::cast_possible_truncation, reason = "пунктов — десятки")]
                    let k = (dy / H_ITEM).round() as isize;
                    let max = n.saturating_sub(VISIBLE).cast_signed();
                    m.first = (first.cast_signed() - k).clamp(0, max).cast_unsigned();
                    cx.notify();
                }
            }),
        );
    let shield = div()
        .id("viewer-menu-shield")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .on_click(cx.listener(|h, _: &ClickEvent, _, cx| {
            if let Some(v) = h.features.get_mut::<Viewer>() {
                v.menu = None;
            }
            cx.notify();
        }));
    vec![shield.into_any_element(), fig.into_any_element()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_stays_visible() {
        assert_eq!(keep_visible(0, 3, 40), 0);
        assert_eq!(keep_visible(0, 11, 40), 1);
        assert_eq!(keep_visible(10, 2, 40), 2);
        assert_eq!(keep_visible(0, 39, 40), 29);
        assert_eq!(keep_visible(5, 2, 5), 0);
    }
}
