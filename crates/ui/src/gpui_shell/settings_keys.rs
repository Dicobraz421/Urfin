//! Раздел настроек «Клавиатура» (`screen-settings-keys.png`, решение 2026-11-05): пресет,
//! поиск, таблица Команда / Клавиши / Где, захват и конфликт. Таблица виртуальная: рисуются
//! только видимые строки. Состояние и правила — `crate::keys_settings`; здесь поле поиска,
//! положение прокрутки и элементы.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Div, Entity, FocusHandle, Keystroke, ScrollWheelEvent, SharedString, Subscription,
    Window, div, px,
};
use urfin_core::{AppState, Command};
use urfin_keymap::{ActiveKeymap, KeyRow};

use super::CoreHost;
use super::overlays::{ClickAll, Handler};
use super::settings::segments;
use super::theme::Theme;
use crate::keys_settings::{self, KeysUi, KeysView};
use crate::tokens::Role;

/// Строка таблицы и строка с пояснением (`screen-settings-keys.png`).
const H_ROW: f32 = 34.0;
const H_ROW_NOTE: f32 = 40.0;
const H_HEAD: f32 = 28.0;
const W_KEYS: f32 = 276.0;
const W_WHERE: f32 = 162.0;
const W_SEARCH: f32 = 280.0;
/// Заголовок окна, статус и клавиши, поля раздела, заголовок и пояснение над таблицей.
const CHROME_H: f32 = 40.0 + 54.0 + 32.0 + 36.0 + 34.0 + 12.0;
/// Сверху вниз: Add, Cancel, Undo (Segoe Fluent Icons).
const ICON_ADD: &str = "\u{E710}";
const ICON_REMOVE: &str = "\u{E711}";
const ICON_RESET: &str = "\u{E7A7}";

fn keys_cmd() -> Command {
    Command::new("ui.keys")
}

/// Поле поиска и прокрутка раздела.
#[derive(Default)]
pub(super) struct Page {
    input: Option<(Entity<InputState>, Subscription)>,
    focused: Rc<Cell<bool>>,
    /// Вернуть фокус окну в следующем кадре.
    blur: bool,
    /// Первая видимая строка.
    top: usize,
    /// Прошлый курсор: прокрутка идёт за ним, только когда он сдвинулся.
    last_sel: usize,
    last_filter: String,
    /// Строк на странице для PgUp/PgDn.
    visible: usize,
}

impl Page {
    pub(super) fn editing(&self) -> bool {
        self.focused.get()
    }

    /// Поле — под раздел, в кадре.
    pub(super) fn sync(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let open = s
            .ext
            .get::<crate::UiState>()
            .is_some_and(|u| u.settings.as_deref() == Some("keys"));
        if !open {
            if self.focused.get() {
                self.focused.set(false);
                window.focus(focus, cx);
            }
            self.input = None;
            self.blur = false;
            return;
        }
        if self.input.is_none() {
            self.input = Some(self.field(s, window, cx));
        }
        if let Some((input, _)) = &self.input {
            let filter = s
                .ext
                .get::<KeysUi>()
                .map_or(String::new(), |k| k.filter.to_string());
            if !self.focused.get() && input.read(cx).value() != filter.as_str() {
                input.update(cx, |st, cx| st.set_value(filter, window, cx));
            }
        }
        if self.blur {
            self.blur = false;
            self.focused.set(false);
            window.focus(focus, cx);
        }
    }

    fn field(
        &self,
        s: &AppState,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> (Entity<InputState>, Subscription) {
        let init = s
            .ext
            .get::<KeysUi>()
            .map_or(String::new(), |k| k.filter.to_string());
        let hint = urfin_i18n::current().get("ui-keys-filter");
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(hint)
                .default_value(init)
        });
        let mirror = Rc::clone(&self.focused);
        let sub = cx.subscribe_in(
            &input,
            window,
            move |h: &mut CoreHost, input, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::Focus | InputEvent::Blur) {
                    mirror.set(matches!(ev, InputEvent::Focus));
                    cx.notify();
                } else if matches!(ev, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    let same = h.core.as_ref().is_some_and(|c| {
                        c.state()
                            .ext
                            .get::<KeysUi>()
                            .is_some_and(|k| k.filter == text)
                    });
                    if !same {
                        h.run(keys_cmd().arg("filter", text), window, cx);
                    }
                }
            },
        );
        (input, sub)
    }

    fn focus_field(&self, window: &mut Window, cx: &mut Context<CoreHost>) {
        if let Some((input, _)) = &self.input {
            self.focused.set(true);
            input.update(cx, |st, cx| st.focus(window, cx));
        }
    }

    /// Клавиши раздела. Захват до сюда не доходит: нажатия забирает конвейер ввода.
    pub(super) fn key(
        &mut self,
        s: &AppState,
        ks: &Keystroke,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let k = s.ext.get::<KeysUi>();
        let filtered = k.is_some_and(|k| !k.filter.is_empty());
        let key = ks.key.as_str();
        if self.focused.get() {
            return match key {
                "escape" if filtered => Some(vec![keys_cmd().arg("filter", "")]),
                "escape" | "enter" | "down" => {
                    self.blur = true;
                    cx.notify();
                    Some(Vec::new())
                }
                _ => None,
            };
        }
        if k.is_some_and(|k| k.ask.is_some()) {
            return Some(match key {
                "enter" => vec![keys_cmd().arg("confirm", true)],
                "escape" => vec![keys_cmd().arg("cancel", true)],
                _ => Vec::new(),
            });
        }
        let v = keys_settings::view(s)?;
        let n = v.shown.len();
        let sel = k.map_or(0, |k| k.sel).min(n.saturating_sub(1));
        let chip = k.map_or(0, |k| k.chip);
        let chips = v.row(sel).map_or(0, |r| r.chips.len());
        let to = |i: usize| vec![keys_cmd().arg("sel", i64::try_from(i).unwrap_or(0))];
        let page = self.visible.max(1);
        let ctrl = ks.modifiers.control;
        Some(match key {
            "up" => to(sel.saturating_sub(1)),
            "down" => to((sel + 1).min(n.saturating_sub(1))),
            "pageup" => to(sel.saturating_sub(page)),
            "pagedown" => to((sel + page).min(n.saturating_sub(1))),
            "home" => to(0),
            "end" => to(n.saturating_sub(1)),
            "left" => {
                vec![keys_cmd().arg("chip", i64::try_from(chip.saturating_sub(1)).unwrap_or(0))]
            }
            "right" => vec![keys_cmd().arg(
                "chip",
                i64::try_from((chip + 1).min(chips.saturating_sub(1))).unwrap_or(0),
            )],
            "enter" => vec![keys_cmd().arg("capture", "replace")],
            "insert" => vec![keys_cmd().arg("capture", "add")],
            "delete" => vec![keys_cmd().arg("remove", true)],
            "backspace" => vec![keys_cmd().arg("reset", true)],
            "f" if ctrl => {
                self.focus_field(window, cx);
                Vec::new()
            }
            "escape" if filtered => vec![keys_cmd().arg("filter", "")],
            "escape" | "tab" => return None,
            _ => {
                // Печать начинает поиск: символ получит поле.
                let printable = ks.key_char.as_deref().is_some_and(|c| {
                    !ctrl && !ks.modifiers.alt && c.chars().all(|c| !c.is_control())
                });
                if !printable {
                    return Some(Vec::new());
                }
                self.focus_field(window, cx);
                return None;
            }
        })
    }

    /// Прокрутка колесом: курсор не двигается.
    fn wheel(&mut self, ev: &ScrollWheelEvent, total: usize) {
        let dy = f32::from(ev.delta.pixel_delta(px(H_ROW)).y);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "строк немного, значение уже округлено и положительно"
        )]
        let step = (dy.abs() / H_ROW).ceil().max(1.0) as usize;
        self.top = if dy > 0.0 {
            self.top.saturating_sub(step)
        } else {
            (self.top + step).min(total.saturating_sub(1))
        };
    }

    /// Первая видимая строка: за курсором, если он сдвинулся.
    fn follow(
        &mut self,
        v: &KeysView,
        filter: &str,
        sel: usize,
        view_h: f32,
        note: &dyn Fn(usize) -> bool,
    ) {
        let n = v.shown.len();
        if filter != self.last_filter {
            filter.clone_into(&mut self.last_filter);
            self.top = 0;
        }
        if sel != self.last_sel {
            self.last_sel = sel;
            if sel < self.top {
                self.top = sel;
            }
            let height = |i: usize| if note(i) { H_ROW_NOTE } else { H_ROW };
            while self.top < sel && (self.top..=sel).map(height).sum::<f32>() > view_h {
                self.top += 1;
            }
        }
        self.top = self.top.min(n.saturating_sub(1));
    }

    #[allow(
        clippy::too_many_lines,
        clippy::many_single_char_names,
        reason = "раздел сверху вниз: шапка, пояснение, таблица"
    )]
    pub(super) fn section(
        &mut self,
        s: &AppState,
        theme: &Theme,
        click: ClickAll<'_>,
        win_h: f32,
        cx: &Context<CoreHost>,
    ) -> Div {
        let i18n = urfin_i18n::current();
        let k = s.ext.get::<KeysUi>();
        let v = keys_settings::view(s);
        let preset = s
            .ext
            .get::<ActiveKeymap>()
            .map_or_else(|| "far".to_owned(), |a| a.cfg.preset.to_string());
        let search = div()
            .flex_none()
            .w(px(W_SEARCH))
            .h(px(28.0))
            .flex()
            .items_center()
            .rounded(px(4.0))
            .border_1()
            .border_b_2()
            .border_color(theme.c(if self.focused.get() {
                Role::AccentFill
            } else {
                Role::StrokeControl
            }))
            .bg(theme.c(Role::FillControlInput))
            .children(self.input.as_ref().map(|(input, _)| {
                Input::new(input)
                    .small()
                    .appearance(false)
                    .text_size(px(13.0))
            }));
        let head = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(16.0))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(SharedString::from(i18n.get("ui-set-keys"))),
            )
            .child(segments(
                theme,
                "preset",
                &[
                    ("urfin", "Urfin".into()),
                    ("far", "Far".into()),
                    ("dn", "DN".into()),
                ],
                &preset,
                click,
                &|p| vec![Command::new("keymap.preset").arg("name", p)],
            ))
            .child(div().flex_1())
            .child(search);
        let link = |id: &'static str, text: String, cmds: Vec<Command>| {
            div()
                .id(id)
                .text_color(theme.c(Role::AccentText))
                .hover(Styled::underline)
                .child(SharedString::from(text))
                .on_click(click(cmds))
        };
        let help = div()
            .flex_none()
            .mt(px(8.0))
            .mb(px(12.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .text_size(px(12.0))
            .text_color(theme.c(Role::TextSecondary))
            .child(
                div()
                    .min_w_0()
                    .child(SharedString::from(i18n.get("ui-keys-help"))),
            )
            .child(div().flex_1())
            .child(link(
                "keys-open",
                i18n.get("ui-keys-open-file"),
                vec![Command::new("keymap.open")],
            ))
            .child(link(
                "keys-reload",
                i18n.get("ui-keys-reload"),
                vec![Command::new("keymap.reload")],
            ))
            .child(link(
                "keys-reset-all",
                i18n.get("ui-keys-reset-all"),
                vec![Command::new("keymap.reset")],
            ));
        let mut table = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.c(Role::StrokeDivider))
            .overflow_hidden()
            .child(table_head(theme));
        if let Some(v) = &v {
            let total = v.shown.len();
            let sel = k.map_or(0, |k| k.sel).min(total.saturating_sub(1));
            let view_h = (win_h - CHROME_H - H_HEAD).max(H_ROW);
            let has_note = |i: usize| v.row(i).is_some_and(|r| row_note(k, r));
            let filter = k.map_or("", |k| k.filter.as_str());
            self.follow(v, filter, sel, view_h, &has_note);
            let mut y = 0.0;
            let mut rows = div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .relative()
                .id("keys-rows")
                .on_scroll_wheel(cx.listener(move |h, ev: &ScrollWheelEvent, _, cx| {
                    if let Some(p) = h.features.get_mut::<super::settings::Settings>() {
                        p.keys.wheel(ev, total);
                        cx.notify();
                    }
                }));
            let mut i = self.top;
            let mut shown = 0;
            while i < total && y < view_h {
                let Some(r) = v.row(i) else { break };
                let h = if row_note(k, r) { H_ROW_NOTE } else { H_ROW };
                rows = rows.child(row(theme, click, k, r, i, i == sel, h));
                y += h;
                i += 1;
                shown += 1;
            }
            self.visible = shown;
            if total > shown && shown > 0 {
                rows = rows.child(thumb(theme, self.top, shown, total, view_h));
            }
            table = table.child(rows);
        }
        div()
            .h_full()
            .flex()
            .flex_col()
            .child(head)
            .child(help)
            .child(table)
    }
}

/// Есть ли второй ряд у строки: конфликт, вопрос или пояснение.
fn row_note(k: Option<&KeysUi>, r: &KeyRow) -> bool {
    !r.conflicts.is_empty()
        || k.is_some_and(|k| {
            k.ask.as_ref().is_some_and(|a| a.capture.row.is(r))
                || k.note.as_ref().is_some_and(|n| n.row.is(r))
        })
}

fn table_head(theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    let cell = |key: &str| SharedString::from(i18n.get(key));
    div()
        .flex_none()
        .h(px(H_HEAD))
        .px(px(12.0))
        .flex()
        .items_center()
        .bg(theme.c(Role::Header))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .text_size(px(12.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(div().flex_1().min_w_0().child(cell("ui-keys-col-command")))
        .child(
            div()
                .flex_none()
                .w(px(W_KEYS))
                .child(cell("ui-keys-col-keys")),
        )
        .child(
            div()
                .flex_none()
                .w(px(W_WHERE))
                .child(cell("ui-keys-col-where")),
        )
}

/// Плашка клавиши: нижняя граница 2 (DESIGN.md).
fn chip(theme: &Theme, id: String, text: String, on: bool) -> gpui_kit::Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .flex_none()
        .h(px(20.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_b_2()
        .border_color(theme.c(if on {
            Role::AccentFill
        } else {
            Role::StrokeControl
        }))
        .bg(theme.c(Role::FillControl))
        .text_size(px(12.0))
        .child(SharedString::from(text))
}

fn action(theme: &Theme, id: String, icon: &'static str) -> gpui_kit::Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .flex_none()
        .w(px(22.0))
        .h(px(20.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .font_family("Segoe Fluent Icons")
        .text_size(px(11.0))
        .text_color(theme.c(Role::TextSecondary))
        .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
        .child(SharedString::new_static(icon))
}

/// Клик по плашке или кнопке: строка под ней его не получает (она отменила бы захват).
fn only(h: Handler) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    move |e, w, cx| {
        cx.stop_propagation();
        h(e, w, cx);
    }
}

/// Подпись под командой (`context_remote`, 11.5).
fn note_line(theme: &Theme, text: String) -> Div {
    div()
        .text_size(px(11.5))
        .line_height(px(13.0))
        .text_color(theme.c(Role::ContextRemote))
        .whitespace_nowrap()
        .overflow_hidden()
        .child(SharedString::from(text))
}

fn one(key: &str, name: &str, value: String) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set(name.to_owned(), value);
    urfin_i18n::current().fmt(key, &a)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "строка таблицы собирается в одном месте"
)]
fn row(
    theme: &Theme,
    click: ClickAll<'_>,
    k: Option<&KeysUi>,
    r: &KeyRow,
    i: usize,
    on: bool,
    h: f32,
) -> Div {
    let i18n = urfin_i18n::current();
    let n = i64::try_from(i).unwrap_or(0);
    let capture = k.and_then(|k| k.capture.as_ref()).filter(|c| c.row.is(r));
    let ask = k
        .and_then(|k| k.ask.as_ref())
        .filter(|a| a.capture.row.is(r));
    let note = k.and_then(|k| k.note.as_ref()).filter(|x| x.row.is(r));
    let sel_chip = if on {
        k.map_or(0, |k| k.chip)
    } else {
        usize::MAX
    };
    let mut title = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .child(
            div()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(SharedString::from(r.title.clone())),
        );
    if let Some(a) = ask {
        let names = a.others.join("», «");
        title = title.child(note_line(theme, one("ui-keys-ask", "other", names)));
    } else if let Some(n) = note {
        title = title.child(note_line(theme, i18n.get(n.key)));
    } else if let Some(other) = r.conflicts.first() {
        let key = if r.conflict_first {
            "ui-keys-conflict-here"
        } else {
            "ui-keys-conflict"
        };
        title = title.child(note_line(theme, one(key, "other", other.clone())));
    }
    let mut keys = div()
        .flex_none()
        .w(px(W_KEYS))
        .flex()
        .items_center()
        .gap(px(6.0));
    if capture.is_some() {
        keys = keys.child(
            div()
                .flex_none()
                .h(px(20.0))
                .px(px(9.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(Role::AccentFill))
                .text_size(px(12.0))
                .text_color(theme.c(Role::AccentText))
                .child(SharedString::from(i18n.get("ui-keys-capture"))),
        );
    } else if let Some(a) = ask {
        let confirm = keys_cmd().arg("confirm", true);
        let cancel = keys_cmd().arg("cancel", true);
        keys = keys
            .child(chip(
                theme,
                format!("keys-new-{i}"),
                a.chord.to_string(),
                true,
            ))
            .child(
                chip(
                    theme,
                    format!("keys-yes-{i}"),
                    i18n.get("ui-keys-replace"),
                    false,
                )
                .on_click(only(click(vec![confirm]))),
            )
            .child(
                chip(
                    theme,
                    format!("keys-no-{i}"),
                    i18n.get("ui-keys-cancel"),
                    false,
                )
                .on_click(only(click(vec![cancel]))),
            );
    } else {
        let mut list = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .overflow_hidden();
        if r.alt_letter {
            list = list.child(chip(
                theme,
                format!("keys-letter-{i}"),
                i18n.get("ui-keys-alt-letter"),
                false,
            ));
        }
        for (j, c) in r.chips.iter().enumerate() {
            let pick = keys_cmd()
                .arg("sel", n)
                .arg("chip", i64::try_from(j).unwrap_or(0))
                .arg("cancel", true);
            let start = keys_cmd()
                .arg("sel", n)
                .arg("chip", i64::try_from(j).unwrap_or(0))
                .arg("capture", "replace");
            list = list.child(
                chip(
                    theme,
                    format!("keys-chip-{i}-{j}"),
                    c.label.to_string(),
                    j == sel_chip,
                )
                .on_click(only(click(vec![if on { start } else { pick }]))),
            );
        }
        keys = keys.child(list);
        if on {
            let at = |c: Command| vec![c];
            keys = keys.child(
                action(theme, format!("keys-add-{i}"), ICON_ADD)
                    .on_click(only(click(at(keys_cmd().arg("capture", "add"))))),
            );
            if !r.chips.is_empty() {
                keys = keys.child(
                    action(theme, format!("keys-del-{i}"), ICON_REMOVE)
                        .on_click(only(click(at(keys_cmd().arg("remove", true))))),
                );
            }
            if r.changed {
                keys = keys.child(
                    action(theme, format!("keys-undo-{i}"), ICON_RESET)
                        .on_click(only(click(at(keys_cmd().arg("reset", true))))),
                );
            }
        }
    }
    let bg = if capture.is_some() || on {
        Some(Role::CurrentLine)
    } else if !r.conflicts.is_empty() || ask.is_some() {
        Some(Role::Header)
    } else {
        None
    };
    let line = div()
        .id(SharedString::from(format!("keys-row-{i}")))
        .h(px(h))
        .flex_none()
        .px(px(12.0))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .when_some(bg, |d, b| d.bg(theme.c(b)))
        .child(title)
        .child(keys)
        .child(
            div()
                .flex_none()
                .w(px(W_WHERE))
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .whitespace_nowrap()
                .overflow_hidden()
                .child(SharedString::from(i18n.get(r.place))),
        )
        .on_click(click(vec![keys_cmd().arg("sel", n).arg("cancel", true)]));
    div().child(line)
}

/// Положение списка: полоска справа (только показ).
fn thumb(theme: &Theme, top: usize, shown: usize, total: usize, view_h: f32) -> Div {
    #[allow(clippy::cast_precision_loss, reason = "строк сотни")]
    let (t, s, n) = (top as f32, shown as f32, total as f32);
    div()
        .absolute()
        .right(px(2.0))
        .top(px(view_h * t / n))
        .w(px(3.0))
        .h(px((view_h * s / n).max(16.0)))
        .rounded(px(2.0))
        .bg(theme.c(Role::StrokeControl))
}
