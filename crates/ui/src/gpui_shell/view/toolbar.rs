//! Панель инструментов редактора (S7-23, S7 §5.8, DESIGN «Панель инструментов
//! редактора», `screen-editor.png`): строка 36 под шапкой — кнопки 28 со значками Segoe
//! Fluent Icons, разделители групп, растяжка, справа значения «UTF-8 ▾» и «LF ▾» с
//! «мешочками». Кнопка — команда, как клавиша (через Core); подсказка — название и
//! клавиша из активного keymap, у недоступной — причина. Фокус остаётся в тексте: кнопки
//! не фокусируемые. S7-24 — Markdown-группа (active — по строке курсора), «Предпросмотр»
//! и уход не поместившихся кнопок в «⋯» (меню — у фичи).

use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Div, FontWeight, Hsla, PathBuilder, SharedString, Window, canvas, div, point, px,
};
use urfin_core::{AppState, Command};
use urfin_view::edit::MdCtx;

use super::super::overlays::{Click, Handler};
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
/// Шаг между кнопками; разделитель с полями.
const GAP: f32 = 2.0;
const W_SEP: f32 = 13.0;
/// Кнопка «H ▾»: поля, буква 15 semibold, значок заголовка.
const H_PAD: f32 = 6.0;
const H_TEXT: f32 = 15.0;
/// «Предпросмотр»: поля, зазор между значком и текстом.
const PV_PAD: f32 = 10.0;
const PV_GAP: f32 = 7.0;
/// Значок «Нумерованный» — свой, рисуется ([`numbered`]).
const OWN_ICON: char = '\0';

/// Markdown-документ: группа кнопок и «Предпросмотр».
#[derive(Clone, Copy)]
pub(super) struct MdTb {
    pub ctx: MdCtx,
    /// Предпросмотр включён.
    pub preview: bool,
    /// Файл больше предела документа: кнопка серая.
    pub too_big: bool,
}

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
    /// Язык документа — Markdown.
    pub md: Option<MdTb>,
}

/// Кнопка: id, значок, ключ названия, команда.
pub(super) struct Btn {
    id: &'static str,
    icon: char,
    pub title: &'static str,
    pub cmd: &'static str,
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

const BOLD: Btn = Btn {
    id: "tb-bold",
    icon: '\u{E8DD}',
    title: "ui-edit-tb-bold",
    cmd: "editor.md.bold",
};
const ITALIC: Btn = Btn {
    id: "tb-italic",
    icon: '\u{E8DB}',
    title: "ui-edit-tb-italic",
    cmd: "editor.md.italic",
};
const HEADING: Btn = Btn {
    id: "tb-heading",
    icon: 'H',
    title: "ui-edit-tb-heading",
    cmd: "editor.md.heading",
};
const LIST: Btn = Btn {
    id: "tb-list",
    icon: '\u{E8FD}',
    title: "ui-edit-tb-list",
    cmd: "editor.md.list",
};
const NUMBERED: Btn = Btn {
    id: "tb-numbered",
    icon: OWN_ICON,
    title: "ui-edit-tb-numbered",
    cmd: "editor.md.numbered",
};
const TASK: Btn = Btn {
    id: "tb-task",
    icon: '\u{E73A}',
    title: "ui-edit-tb-task",
    cmd: "editor.md.task",
};
const LINK: Btn = Btn {
    id: "tb-link",
    icon: '\u{E71B}',
    title: "ui-edit-tb-link",
    cmd: "editor.md.link",
};
const CODE: Btn = Btn {
    id: "tb-code",
    icon: '\u{E943}',
    title: "ui-edit-tb-code",
    cmd: "editor.md.code",
};
const TABLE: Btn = Btn {
    id: "tb-table",
    icon: '\u{E80A}',
    title: "ui-edit-tb-table",
    cmd: "editor.md.table",
};
pub(super) const PREVIEW: Btn = Btn {
    id: "tb-preview",
    icon: '\u{F57C}',
    title: "ui-edit-tb-preview",
    cmd: "editor.preview",
};

/// Место в строке кнопок.
#[derive(Clone, Copy)]
enum Slot {
    Btn(&'static Btn),
    Heading,
    Sep,
}

const BASE: [Slot; 8] = [
    Slot::Btn(&SAVE),
    Slot::Btn(&UNDO),
    Slot::Btn(&REDO),
    Slot::Sep,
    Slot::Btn(&FIND),
    Slot::Btn(&REPLACE),
    Slot::Sep,
    Slot::Btn(&WRAP),
];
const MD: [Slot; 12] = [
    Slot::Sep,
    Slot::Btn(&BOLD),
    Slot::Btn(&ITALIC),
    Slot::Heading,
    Slot::Sep,
    Slot::Btn(&LIST),
    Slot::Btn(&NUMBERED),
    Slot::Btn(&TASK),
    Slot::Sep,
    Slot::Btn(&LINK),
    Slot::Btn(&CODE),
    Slot::Btn(&TABLE),
];

fn slots(md: bool) -> impl Iterator<Item = Slot> {
    BASE.into_iter().chain(MD.into_iter().filter(move |_| md))
}

/// Где что стоит в строке: сколько мест поместилось, виден ли «Предпросмотр», куда
/// привязаны «мешочки» заголовка и «⋯».
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Lay {
    pub shown: usize,
    pub total: usize,
    pub preview: bool,
    pub x_heading: f32,
    pub w_heading: f32,
    pub x_more: f32,
}

/// Ширина «H ▾» с подписью `label`.
fn heading_w(m: &super::head::Measure, label: &str) -> f32 {
    m.w(label, H_TEXT) + CHEVRON_GAP + CHEVRON + 2.0 * H_PAD
}

/// Ширина «Предпросмотра»: значок, текст, поля.
fn preview_w(m: &super::head::Measure) -> f32 {
    let title = urfin_i18n::current().get(PREVIEW.title);
    ICON + PV_GAP + m.w(&title, TEXT) + 2.0 * PV_PAD
}

/// Правые края мест: `i`-е кончается на `ends[i]`.
fn slot_ends(t: &Tb, m: &super::head::Measure, label: &str) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let mut x = PAD_X;
    for s in slots(t.md.is_some()) {
        let w = match s {
            Slot::Btn(_) => BTN,
            Slot::Heading => heading_w(m, label),
            Slot::Sep => W_SEP,
        };
        out.push((x, w));
        x += w + GAP;
    }
    out
}

/// Раскладка строки при ширине окна `m.win_w`.
pub(super) fn layout(t: &Tb, m: &super::head::Measure, label: &str) -> Lay {
    let places = slot_ends(t, m, label);
    let total = places.len();
    let [(x_enc, _), _] = values_x(m, t, m.win_w);
    let edge = |preview: bool| {
        if preview {
            x_enc - 2.0 * GAP - W_SEP - preview_w(m) - GAP
        } else {
            x_enc - GAP
        }
    };
    let fit = |limit: f32| -> usize {
        (1..=total)
            .rev()
            .find(|&n| {
                let end = places[n - 1].0 + places[n - 1].1;
                n == total && end <= limit || end + GAP + BTN <= limit
            })
            .unwrap_or(1)
    };
    let md = t.md.is_some();
    let mut preview = md;
    let mut shown = fit(edge(preview));
    if preview && shown == 1 {
        preview = false;
        shown = fit(edge(false));
    }
    // Разделитель в конце строки не нужен.
    let seps = slots(md).take(shown).collect::<Vec<_>>();
    if matches!(seps.last(), Some(Slot::Sep)) {
        shown -= 1;
    }
    let last = places[shown - 1];
    let (x_heading, w_heading) = places
        .iter()
        .copied()
        .zip(slots(md))
        .find(|(_, s)| matches!(s, Slot::Heading))
        .map_or((0.0, 0.0), |(p, _)| p);
    Lay {
        shown,
        total,
        preview,
        x_heading,
        w_heading,
        x_more: last.0 + last.1 + GAP,
    }
}

/// Команды не поместившихся кнопок (меню «⋯»): от места `from`.
pub(super) fn hidden(md: bool, from: usize) -> Vec<&'static Btn> {
    slots(md)
        .skip(from)
        .filter_map(|s| match s {
            Slot::Btn(b) => Some(b),
            Slot::Heading => Some(&HEADING),
            Slot::Sep => None,
        })
        .collect()
}

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
#[allow(clippy::too_many_arguments, reason = "кнопка: вид, состояние, команда")]
fn button(
    b: &'static Btn,
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
        .text_size(px(ICON));
    let color = if active {
        el = el.bg(theme.c(Role::FillCursor));
        theme.c(Role::CursorText)
    } else {
        theme.c(Role::TextPrimary)
    };
    el = el.text_color(color);
    el = if b.icon == OWN_ICON {
        el.child(numbered(color))
    } else {
        el.child(SharedString::from(b.icon.to_string()))
    };
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

/// Значок «Нумерованный»: цифры 1–3 и три линии, штрих 1 (DESIGN; в шрифте такого нет).
fn numbered(color: Hsla) -> impl IntoElement {
    const STROKES: [&[(f32, f32)]; 6] = [
        // 1
        &[(2.0, 2.5), (3.5, 1.5), (3.5, 5.5)],
        // 2
        &[(1.8, 7.0), (3.0, 6.2), (4.6, 7.0), (4.6, 7.8), (1.8, 10.3), (4.8, 10.3)],
        // 3
        &[
            (1.8, 11.4),
            (4.6, 11.4),
            (3.0, 12.9),
            (4.8, 13.8),
            (4.6, 14.9),
            (3.0, 15.2),
            (1.8, 14.6),
        ],
        &[(7.0, 3.5), (15.0, 3.5)],
        &[(7.0, 8.5), (15.0, 8.5)],
        &[(7.0, 13.5), (15.0, 13.5)],
    ];
    canvas(
        |_, _, _| {},
        move |b, (), window, _| {
            let at = |(x, y): (f32, f32)| point(b.origin.x + px(x), b.origin.y + px(y));
            for pts in STROKES {
                let mut pb = PathBuilder::stroke(px(1.0));
                pb.move_to(at(pts[0]));
                for &q in &pts[1..] {
                    pb.line_to(at(q));
                }
                if let Ok(path) = pb.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .w(px(ICON))
    .h(px(ICON))
    .flex_none()
}

/// Кнопка «H ▾»: на заголовке — его уровень, меню — от команды без уровня.
fn heading_button(
    md: MdTb,
    label: &str,
    off: Option<String>,
    s: &AppState,
    theme: &Theme,
    click: Click<'_>,
) -> gpui_kit::Stateful<Div> {
    let active = md.ctx.heading > 0;
    let fg = if active {
        theme.c(Role::CursorText)
    } else {
        theme.c(Role::TextPrimary)
    };
    let el = div()
        .id("tb-heading")
        .flex_none()
        .h(px(BTN))
        .px(px(H_PAD))
        .flex()
        .items_center()
        .gap(px(CHEVRON_GAP))
        .rounded(px(4.0))
        .text_color(fg)
        .when(active, |d| d.bg(theme.c(Role::FillCursor)))
        .child(
            div()
                .text_size(px(H_TEXT))
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(label.to_owned())),
        )
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(CHEVRON))
                .text_color(if active {
                    fg
                } else {
                    theme.c(Role::TextSecondary)
                })
                .child(SharedString::from(crate::icons::CHEVRON_DOWN.to_string())),
        );
    if let Some(reason) = off {
        return tip(el.opacity(0.4), theme, reason, None);
    }
    let hover = theme.c(Role::CurrentLine);
    let press = theme.c(Role::StrokeDivider);
    let el = el
        .hover(move |d| d.bg(hover))
        .active(move |d| d.bg(press))
        .on_click(click(Command::new(HEADING.cmd)));
    tip(
        el,
        theme,
        urfin_i18n::current().get(HEADING.title),
        key_of(s, HEADING.cmd),
    )
}

/// «Предпросмотр»: значок и текст, переключатель.
fn preview_button(
    md: MdTb,
    w: f32,
    s: &AppState,
    theme: &Theme,
    click: Click<'_>,
) -> gpui_kit::Stateful<Div> {
    let i18n = urfin_i18n::current();
    let title = i18n.get(PREVIEW.title);
    let fg = if md.preview {
        theme.c(Role::CursorText)
    } else {
        theme.c(Role::TextPrimary)
    };
    let el = div()
        .id(PREVIEW.id)
        .flex_none()
        .w(px(w))
        .h(px(BTN))
        .px(px(PV_PAD))
        .flex()
        .items_center()
        .gap(px(PV_GAP))
        .rounded(px(4.0))
        .text_size(px(TEXT))
        .text_color(fg)
        .when(md.preview, |d| d.bg(theme.c(Role::FillCursor)))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(ICON))
                .child(SharedString::from(PREVIEW.icon.to_string())),
        )
        .child(SharedString::from(title.clone()));
    if md.too_big {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("mb", urfin_view::md::DOC_MAX >> 20);
        return tip(
            el.opacity(0.4),
            theme,
            i18n.fmt("ui-edit-tb-too-big", &a),
            None,
        );
    }
    let hover = theme.c(Role::CurrentLine);
    let press = theme.c(Role::StrokeDivider);
    let el = el
        .hover(move |d| d.bg(hover))
        .active(move |d| d.bg(press))
        .on_click(click(Command::new(PREVIEW.cmd)));
    tip(el, theme, title, key_of(s, PREVIEW.cmd))
}

/// Кнопка активна по контексту курсора.
fn is_active(b: &Btn, t: &Tb) -> bool {
    let wrap = t.wrap && b.id == WRAP.id;
    let Some(md) = t.md else {
        return wrap;
    };
    let c = md.ctx;
    wrap || match b.id {
        "tb-bold" => c.bold,
        "tb-italic" => c.italic,
        "tb-code" => c.code,
        "tb-list" => c.list,
        "tb-numbered" => c.numbered,
        "tb-task" => c.task,
        _ => false,
    }
}

fn sep(theme: &Theme) -> Div {
    div()
        .flex_none()
        .mx(px((W_SEP - 1.0) / 2.0))
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

/// Строка панели; `more` — клик по «⋯» (есть, когда кнопки не поместились).
#[allow(
    clippy::too_many_lines,
    reason = "строка панели: кнопки, «⋯», справа значения"
)]
pub(super) fn toolbar(
    t: &Tb,
    s: &AppState,
    theme: &Theme,
    click: Click<'_>,
    m: &super::head::Measure,
    lay: &Lay,
    more: Handler,
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
    let label = t
        .md
        .map_or_else(SharedString::default, |x| super::md_edit::heading_label(x.ctx));
    let mut row = div()
        .h(px(H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .bg(theme.c(Role::LayerPanel))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .font_weight(FontWeight::NORMAL);
    for slot in slots(t.md.is_some()).take(lay.shown) {
        row = match slot {
            Slot::Sep => row.child(sep(theme)),
            Slot::Heading => match t.md {
                Some(x) => row.child(heading_button(x, &label, ro.clone(), s, theme, click)),
                None => row,
            },
            Slot::Btn(b) => {
                let off = match b.id {
                    "tb-save" => save_off.clone(),
                    "tb-undo" => undo_off.clone(),
                    "tb-redo" => redo_off.clone(),
                    "tb-find" | "tb-wrap" => None,
                    _ => ro.clone(),
                };
                // Палитра B: «Сохранить» — `accent`, пока есть несохранённое.
                let dirty = b.id == "tb-save" && off.is_none() && theme.vivid();
                row.child(
                    button(b, is_active(b, t), off, s, theme, click)
                        .when(dirty, |d| d.text_color(theme.c(Role::AccentFill))),
                )
            }
        };
    }
    if lay.shown < lay.total {
        let hover = theme.c(Role::CurrentLine);
        let press = theme.c(Role::StrokeDivider);
        row = row.child(tip(
            div()
                .id("tb-more")
                .flex_none()
                .w(px(BTN))
                .h(px(BTN))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .text_size(px(ICON))
                .text_color(theme.c(Role::TextPrimary))
                .child(SharedString::new_static("\u{22EF}"))
                .hover(move |d| d.bg(hover))
                .active(move |d| d.bg(press))
                .on_click(more),
            theme,
            i18n.get("ui-edit-tb-more"),
            None,
        ));
    }
    row = row.child(div().flex_1());
    if let Some(x) = t.md.filter(|_| lay.preview) {
        row = row
            .child(preview_button(x, preview_w(m), s, theme, click))
            .child(sep(theme));
    }
    row.child(value(
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
