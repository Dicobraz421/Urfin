//! Панель инструментов редактора (S7-23, S7 §5.8, DESIGN «Панель инструментов
//! редактора», `screen-editor.png`): строка 36 под шапкой — кнопки 28 со значками Segoe
//! Fluent Icons, разделители групп, растяжка, справа значения «UTF-8 ▾» и «LF ▾» с
//! «мешочками». Кнопка — команда, как клавиша (через Core); подсказка — название и
//! клавиша из активного keymap, у недоступной — причина. Фокус остаётся в тексте: кнопки
//! не фокусируемые. Markdown-группа и «Предпросмотр» — S7-24.

use gpui_kit::prelude::*;
use gpui_kit::{Context, Div, FontWeight, Hsla, SharedString, Window, div, px};
use urfin_core::{AppState, Command};

use super::super::overlays::Click;
use super::super::theme::Theme;
use crate::tokens::Role;

/// Высота строки и кнопки.
pub(super) const H: f32 = 36.0;
const BTN: f32 = 28.0;
const PAD_X: f32 = 8.0;
const ICON_FONT: &str = "Segoe Fluent Icons";
const ICON: f32 = 16.0;
const TEXT: f32 = 12.5;
const VALUE_PAD: f32 = 8.0;
const CHEVRON_GAP: f32 = 6.0;
const CHEVRON: f32 = 8.0;

/// Что панели знать о документе.
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые состояния кнопок"
)]
pub(super) struct Tb {
    /// Документ изменён или новый.
    pub dirty: bool,
    pub undo: bool,
    pub redo: bool,
    pub wrap: bool,
    pub readonly: bool,
    pub enc: SharedString,
    pub eol: SharedString,
}

/// Кнопка: id, значок, ключ названия, команда.
struct Btn {
    id: &'static str,
    icon: char,
    title: &'static str,
    cmd: &'static str,
}

const SAVE: Btn = Btn {
    id: "tb-save",
    icon: '\u{E74E}',
    title: "ui-edit-tb-save",
    cmd: "editor.save",
};
const UNDO: Btn = Btn {
    id: "tb-undo",
    icon: '\u{E7A7}',
    title: "ui-edit-tb-undo",
    cmd: "editor.undo",
};
const REDO: Btn = Btn {
    id: "tb-redo",
    icon: '\u{E7A6}',
    title: "ui-edit-tb-redo",
    cmd: "editor.redo",
};
const FIND: Btn = Btn {
    id: "tb-find",
    icon: '\u{E721}',
    title: "ui-edit-tb-find",
    cmd: "editor.search",
};
const REPLACE: Btn = Btn {
    id: "tb-replace",
    icon: '\u{E8AC}',
    title: "ui-edit-tb-replace",
    cmd: "editor.replace",
};
const WRAP: Btn = Btn {
    id: "tb-wrap",
    icon: '\u{E751}',
    title: "ui-edit-tb-wrap",
    cmd: "editor.wrap",
};

/// Клавиша команды в активном keymap (первая), в фокусе редактора.
pub(in crate::gpui_shell) fn key_of(s: &AppState, cmd: &'static str) -> Option<String> {
    key_for(s, &Command::new(cmd))
}

/// Клавиша команды с аргументами в активном keymap (первая).
pub(in crate::gpui_shell) fn key_for(s: &AppState, cmd: &Command) -> Option<String> {
    let km = s.ext.get::<urfin_keymap::ActiveKeymap>()?;
    km.keymap.keys_for(cmd, s).first().map(ToString::to_string)
}

/// Подсказка: строки «название `text` и клавиша `secondary` через 8» (DESIGN).
struct Tip {
    lines: Vec<(SharedString, Option<SharedString>)>,
    bg: Hsla,
    border: Hsla,
    fg: Hsla,
    fg2: Hsla,
}

impl gpui_kit::Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .py(px(4.0))
            .px(px(8.0))
            .flex()
            .flex_col()
            .rounded(px(4.0))
            .border_1()
            .border_color(self.border)
            .bg(self.bg)
            .text_size(px(12.0))
            .text_color(self.fg);
        for (title, key) in &self.lines {
            col = col.child(
                div()
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(title.clone())
                    .children(key.clone().map(|k| div().text_color(self.fg2).child(k))),
            );
        }
        col
    }
}

fn tip(
    el: gpui_kit::Stateful<Div>,
    theme: &Theme,
    title: String,
    key: Option<String>,
) -> gpui_kit::Stateful<Div> {
    tip_lines(el, theme, vec![(title, key)])
}

/// Подсказка кнопки в несколько строк (кнопка «История» панели — две команды).
pub(in crate::gpui_shell) fn tip_lines(
    el: gpui_kit::Stateful<Div>,
    theme: &Theme,
    lines: Vec<(String, Option<String>)>,
) -> gpui_kit::Stateful<Div> {
    let (bg, border, fg, fg2) = (
        theme.c(Role::LayerPanel),
        theme.c(Role::StrokeCard),
        theme.c(Role::TextPrimary),
        theme.c(Role::TextSecondary),
    );
    let lines: Vec<(SharedString, Option<SharedString>)> = lines
        .into_iter()
        .map(|(t, k)| (SharedString::from(t), k.map(SharedString::from)))
        .collect();
    el.tooltip(move |_, cx| {
        let lines = lines.clone();
        cx.new(|_| Tip {
            lines,
            bg,
            border,
            fg,
            fg2,
        })
        .into()
    })
}

/// Кнопка-значок: `off` — причина недоступности (подсказка вместо названия).
fn button(
    b: &Btn,
    active: bool,
    off: Option<String>,
    s: &AppState,
    theme: &Theme,
    click: Click<'_>,
) -> gpui_kit::Stateful<Div> {
    let i18n = urfin_i18n::current();
    let mut el = div()
        .id(b.id)
        .flex_none()
        .w(px(BTN))
        .h(px(BTN))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .font_family(ICON_FONT)
        .text_size(px(ICON))
        .child(SharedString::from(b.icon.to_string()));
    if active {
        el = el
            .bg(theme.c(Role::FillCursor))
            .text_color(theme.c(Role::CursorText));
    } else {
        el = el.text_color(theme.c(Role::TextPrimary));
    }
    if let Some(reason) = off {
        return tip(el.opacity(0.4), theme, reason, None);
    }
    let hover = theme.c(Role::CurrentLine);
    let press = theme.c(Role::StrokeDivider);
    let el = el
        .hover(move |d| d.bg(hover))
        .active(move |d| d.bg(press))
        .on_click(click(Command::new(b.cmd)));
    tip(el, theme, i18n.get(b.title), key_of(s, b.cmd))
}

fn sep(theme: &Theme) -> Div {
    div()
        .flex_none()
        .mx(px(6.0))
        .w(px(1.0))
        .h(px(20.0))
        .bg(theme.c(Role::StrokeDivider))
}

/// Ширина кнопки значения («UTF-8 ▾»): текст, шеврон, поля.
pub(super) fn value_w(m: &super::head::Measure, text: &str) -> f32 {
    m.w(text, TEXT) + CHEVRON_GAP + CHEVRON + 2.0 * VALUE_PAD + 2.0
}

/// Содержимое кнопки значения: текст и шеврон `secondary`.
pub(super) fn value_inner(text: &SharedString, theme: &Theme) -> Div {
    div()
        .h(px(BTN))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(CHEVRON_GAP))
        .text_size(px(TEXT))
        .text_color(theme.c(Role::TextPrimary))
        .child(text.clone())
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(CHEVRON))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(crate::icons::CHEVRON_DOWN.to_string())),
        )
}

/// Кнопки значений справа: левый край и ширина каждой (якоря «мешочков»), окно шириной
/// `win_w`.
pub(super) fn values_x(m: &super::head::Measure, t: &Tb, win_w: f32) -> [(f32, f32); 2] {
    let w_eol = value_w(m, &t.eol);
    let w_enc = value_w(m, &t.enc);
    let x_eol = win_w - PAD_X - w_eol;
    let x_enc = x_eol - 2.0 - w_enc;
    [(x_enc, w_enc), (x_eol, w_eol)]
}

#[allow(
    clippy::too_many_arguments,
    reason = "кнопка значения: подпись, команда, место"
)]
fn value(
    id: &'static str,
    text: &SharedString,
    w: f32,
    title: &str,
    cmd: &'static str,
    s: &AppState,
    theme: &Theme,
    click: Click<'_>,
) -> gpui_kit::Stateful<Div> {
    let hover = theme.c(Role::CurrentLine);
    let press = theme.c(Role::StrokeDivider);
    let el = div()
        .id(id)
        .flex_none()
        .w(px(w))
        .rounded(px(4.0))
        .child(value_inner(text, theme))
        .hover(move |d| d.bg(hover))
        .active(move |d| d.bg(press))
        .on_click(click(Command::new(cmd)));
    tip(el, theme, urfin_i18n::current().get(title), key_of(s, cmd))
}

/// Строка панели.
pub(super) fn toolbar(
    t: &Tb,
    s: &AppState,
    theme: &Theme,
    click: Click<'_>,
    m: &super::head::Measure,
) -> Div {
    let i18n = urfin_i18n::current();
    let ro = t.readonly.then(|| i18n.get("ui-edit-tb-readonly"));
    let save_off = if t.dirty {
        ro.clone()
    } else {
        Some(i18n.get("ui-edit-tb-not-modified"))
    };
    let undo_off = ro
        .clone()
        .or_else(|| (!t.undo).then(|| i18n.get("ui-edit-tb-no-undo")));
    let redo_off = ro
        .clone()
        .or_else(|| (!t.redo).then(|| i18n.get("ui-edit-tb-no-redo")));
    let [(_, w_enc), (_, w_eol)] = values_x(m, t, m.win_w);
    div()
        .h(px(H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0))
        .px(px(PAD_X))
        .bg(theme.c(Role::LayerPanel))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .font_weight(FontWeight::NORMAL)
        .child(button(&SAVE, false, save_off, s, theme, click))
        .child(button(&UNDO, false, undo_off, s, theme, click))
        .child(button(&REDO, false, redo_off, s, theme, click))
        .child(sep(theme))
        .child(button(&FIND, false, None, s, theme, click))
        .child(button(&REPLACE, false, ro, s, theme, click))
        .child(sep(theme))
        .child(button(&WRAP, t.wrap, None, s, theme, click))
        .child(div().flex_1())
        .child(value(
            "tb-enc",
            &t.enc,
            w_enc,
            "ui-edit-tb-encoding",
            "editor.codepage_menu",
            s,
            theme,
            click,
        ))
        .child(value(
            "tb-eol",
            &t.eol,
            w_eol,
            "ui-edit-tb-eol",
            "editor.eol_menu",
            s,
            theme,
            click,
        ))
}
