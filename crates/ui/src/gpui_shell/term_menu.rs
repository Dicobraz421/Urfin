//! Список над cmdline (S6 §3.1 слой inline-оверлеев): история Alt+F8 (S6-16) и
//! варианты Tab (S6-17). Модель — `TermState.menu` (крейт term), здесь только вид:
//! «мешочек» вверх от командной строки (S7-25b, DESIGN «Общие правила»): у истории
//! корешок — значок часов и фильтр с кареткой на месте поля ввода
//! (`screen-cmdhistory.png`), у дополнения — набранное слово с кареткой
//! (`screen-cmdcomplete.png`, список 400). До 12 строк, выбранная — `FillCursor`.

use gpui_kit::prelude::*;
use gpui_kit::{Div, Hsla, SharedString, TextRun, Window, div, px};
use urfin_core::AppState;

use super::pouch::{self, Notch};
use super::theme::Theme;
use crate::tokens::{Metric, Role};

/// Ширина списка истории и дополнения, px.
const W_HISTORY: f32 = 860.0;
const W_COMPLETE: f32 = 400.0;
/// Строка сегментов истории и высота сегмента.
const H_HEAD: f32 = 24.0;
const SEG_H: f32 = 26.0;
/// Шрифт командной строки (как `chrome::cmdline`).
const MONO: &str = "Cascadia Mono";
const CLI_TEXT: f32 = 12.5;
/// Поле `path>` без промпта shell отстоит от ввода на 6 (`chrome::cmdline`).
const PATH_GAP: f32 = 6.0;
/// Корешок: поля, значок часов, каретка.
const NOTCH_PAD: f32 = 6.0;
const CLOCK: f32 = 14.0;
const CARET: f32 = 8.0;

/// Где в командной строке начинается ввод и ширина колонки моно-шрифта.
struct Field {
    x: f32,
    cw: f32,
}

/// Начало ввода по раскладке строки: поле строки, промпт (ячейки моно или `путь>`).
fn field(s: &AppState, theme: &Theme, window: &Window) -> Field {
    let run = TextRun {
        len: 1,
        font: gpui_kit::font(MONO),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let cw = f32::from(
        window
            .text_system()
            .shape_line(SharedString::new_static("0"), px(CLI_TEXT), &[run], None)
            .width,
    );
    #[allow(clippy::cast_precision_loss, reason = "колонок промпта — десятки")]
    let prompt = if let Some(cells) = super::chrome::prompt_cells(s, theme.tokens.dark()) {
        cells.len() as f32 * cw
    } else {
        let path = s.panel(s.active).tab().loc.path.display().to_string();
        (path.chars().count() + 1) as f32 * cw + PATH_GAP
    };
    Field {
        x: theme.m(Metric::PadRowX) + prompt,
        cw,
    }
}

/// Начало слова под курсором (байт) в строке `base` до курсора `cursor`.
fn word_start(base: &str, cursor: usize) -> usize {
    let head = base.get(..cursor.min(base.len())).unwrap_or(base);
    head.rfind(char::is_whitespace).map_or(0, |i| i + 1)
}

/// Каретка-блок `accent` (как в командной строке).
fn caret(theme: &Theme) -> Div {
    div()
        .w(px(CARET))
        .h(px(16.0))
        .flex_none()
        .bg(theme.c(Role::AccentFill))
}

/// «Мешочек» списка или `None`, если он закрыт.
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "корешок, заголовок, строки и подсказка — одна фигура"
)]
pub(super) fn view(s: &AppState, theme: &Theme, window: &Window) -> Option<Div> {
    let st = s.ext.get::<urfin_term::TermState>()?;
    let m = st.menu.as_ref()?;
    let i18n = urfin_i18n::current();
    let history = m.is_history();
    let rows = m.rows(&st.history);
    if rows.is_empty() && !history {
        return None;
    }
    let (top, _) = m.window();
    let win_w = f32::from(window.viewport_size().width);
    let f = field(s, theme, window);
    let mono = |t: String| {
        div()
            .font_family(MONO)
            .text_size(px(CLI_TEXT))
            .text_color(theme.c(Role::TextPrimary))
            .whitespace_nowrap()
            .child(SharedString::from(t))
    };
    // Корешок — по раскладке поля: история — часы и фильтр от начала ввода, дополнение
    // — набранное слово (по его x) с кареткой.
    #[allow(clippy::cast_precision_loss, reason = "символов в строке — сотни")]
    let (ax, aw, content, w) = match &m.kind {
        urfin_term::menu::MenuKind::History { filter, .. } => {
            let tw = filter.chars().count() as f32 * f.cw;
            let aw = 2.0 * NOTCH_PAD + CLOCK + 6.0 + tw + CARET;
            let content = div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .font_family("Segoe Fluent Icons")
                        .text_size(px(12.0))
                        .text_color(theme.c(Role::AccentText))
                        .child(SharedString::new_static("\u{E81C}")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(mono(filter.clone()))
                        .child(caret(theme)),
                );
            (f.x - NOTCH_PAD, aw, content, W_HISTORY)
        }
        urfin_term::menu::MenuKind::Complete { .. } => {
            // Слово строки под курсором (вариант уже подставлен — Tab по кругу).
            let line = st.line.text();
            let cur = st.line.cursor().min(line.len());
            let at = word_start(line, cur);
            let end = line
                .get(cur..)
                .and_then(|r| r.find(char::is_whitespace))
                .map_or(line.len(), |i| cur + i);
            let head = line.get(at..cur).unwrap_or_default().to_owned();
            let mut rest = line.get(cur..end).unwrap_or_default().chars();
            let under = rest.next();
            let tail: String = rest.collect();
            let before = line.get(..at).unwrap_or_default().chars().count() as f32;
            let n = line.get(at..end).unwrap_or_default().chars().count();
            // Каретка на символе — в ширине слова; в конце — своя ширина (ниже).
            let tw = n as f32 * f.cw - if under.is_some() { CARET } else { 0.0 };
            let block = match under {
                Some(c) => caret(theme)
                    .w_auto()
                    .flex()
                    .items_center()
                    .child(mono(c.to_string()).text_color(theme.c(Role::LayerPanel))),
                None => caret(theme),
            };
            let content = div()
                .flex()
                .items_center()
                .child(mono(head))
                .child(block)
                .child(mono(tail));
            (
                f.x + before * f.cw - NOTCH_PAD,
                2.0 * NOTCH_PAD + tw + CARET,
                content,
                W_COMPLETE,
            )
        }
    };
    let ax = ax.max(0.0);
    let w = w
        .min(win_w - ax - theme.m(Metric::PadRowX))
        .max(aw.max(200.0));
    let (left, nx) = pouch::place(ax, aw, w, win_w, 0.0);
    let mut body = div().flex().flex_col();
    // Сверху — сегменты «shell | Все shells» и Ctrl+R (`screen-cmdhistory.png`);
    // клавиши — в keybar.
    if let urfin_term::menu::MenuKind::History { all, .. } = &m.kind {
        let seg = |label: String, on: bool| {
            div()
                .h(px(SEG_H))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(if on {
                    Role::AccentFill
                } else {
                    Role::StrokeControl
                }))
                .when(on, |d| d.bg(theme.c(Role::FillCursor)))
                .text_color(theme.c(Role::TextPrimary))
                .child(SharedString::from(label))
        };
        body = body.child(
            div()
                .h(px(H_HEAD + 14.0))
                .px(px(6.0))
                .mb(px(4.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .border_b_1()
                .border_color(theme.c(Role::StrokeDivider))
                .text_size(px(12.0))
                // Shell ещё не известен (демо, до поиска shells) — «shell».
                .child(seg(
                    if m.shell.is_empty() {
                        "shell".to_owned()
                    } else {
                        m.shell.to_string()
                    },
                    !*all,
                ))
                .child(seg(i18n.get("ui-cmdhist-all"), *all))
                .child(
                    div()
                        .ml(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::new_static("Ctrl+R")),
                ),
        );
    }
    if rows.is_empty() {
        body = body.child(
            div()
                .h(px(pouch::H_ITEM))
                .px(px(10.0))
                .flex()
                .items_center()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("term-history-empty"))),
        );
    }
    for (k, item) in rows.into_iter().enumerate() {
        let selected = top + k == m.sel;
        let hint = if item.lock {
            i18n.get("term-history-pinned")
        } else if item.native {
            i18n.get("term-history-native")
        } else {
            item.hint.to_string()
        };
        body = body.child(
            div()
                .h(px(pouch::H_ITEM))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(4.0))
                .when(selected, |d| d.bg(theme.c(Role::FillCursor)))
                .child(
                    div()
                        .flex_none()
                        .w(px(14.0))
                        .font_family("Segoe Fluent Icons")
                        .text_size(px(12.0))
                        .text_color(theme.c(Role::TextSecondary))
                        // Закреплённая — булавка.
                        .child(SharedString::new_static(if item.lock {
                            "\u{E718}"
                        } else {
                            ""
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_family(MONO)
                        .text_size(px(CLI_TEXT))
                        .child(SharedString::from(item.text)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(hint)),
                ),
        );
    }
    let notch = Notch {
        x: nx,
        w: aw,
        h: theme.m(Metric::HCmdline),
        content: content.into_any_element(),
    };
    // Низ корешка — низ командной строки: над статусом и keybar.
    let bottom = theme.m(Metric::HStatus) + theme.m(Metric::HKeybar);
    Some(pouch::pouch_up(theme, left, bottom, w, Some(notch), body))
}

#[cfg(test)]
mod tests {
    use super::word_start;

    #[test]
    fn completed_word_starts_after_space() {
        assert_eq!(word_start("cargo nextest run -p urfin-", 27), 21);
        assert_eq!(word_start("dir", 3), 0);
        assert_eq!(word_start("a b", 2), 2);
    }
}
