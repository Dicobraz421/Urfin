//! Строки «Найти» и «Перейти» внизу документа (S7 §3.5, §3.6; `screen-editreplace.png` —
//! ряд «Найти», `screen-editgoto.png`). Компонент общий для вьювера и редактора (S7-18):
//! поле (`InputState` gpui-kit — IME, выделение, буфер обмена), флажки поиска, разбор
//! клавиш строки в действия и разметка рядов. Команды из действий собирает владелец
//! (`viewer.*` / `editor.*`).
//!
//! Строка открыта, пока фокус Core — `Focus::Overlay(Find)`; клавиши, которые строка не
//! разбирает, получает поле, а не keymap (`ShellFeature::editing`). Alt+C/W/R/H — по месту
//! клавиши, на любой раскладке (как сочетания keymap, [`super::super::input::stroke_key`]).

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Div, Entity, Keystroke, SharedString, Subscription, Window, div, px};
use urfin_keymap::ev::Key;

use super::super::CoreHost;
use super::super::overlays::Handler;
use super::super::strip::{Btn, Tone, button, check, container};
use super::super::theme::Theme;
use super::rows::MONO;
use crate::tokens::Role;

/// Высота строки: поля 10, поле и кнопки 30, черта 2 (`screen-editgoto.png`).
pub(super) const H_BAR: f32 = 52.0;
/// Ширина поля «Найти» и «Перейти» по макетам.
const W_FIND: f32 = 340.0;
const W_GOTO: f32 = 96.0;
const GAP: f32 = 10.0;

/// Что открыто: поиск или переход.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum BarMode {
    Find,
    Goto,
}

/// Флажок поиска.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Flag {
    Case,
    Word,
    Regex,
    Hex,
}

impl Flag {
    pub(super) const ALL: [Flag; 4] = [Flag::Case, Flag::Word, Flag::Regex, Flag::Hex];

    fn label(self) -> &'static str {
        match self {
            Flag::Case => "ui-find-case",
            Flag::Word => "ui-find-word",
            Flag::Regex => "ui-find-regex",
            Flag::Hex => "ui-find-hex",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Flag::Case => "find-case",
            Flag::Word => "find-word",
            Flag::Regex => "find-regex",
            Flag::Hex => "find-hex",
        }
    }

    /// Клавиша флажка (Alt+буква): подпись keybar и место клавиши.
    pub(super) fn key(self) -> (&'static str, Key) {
        match self {
            Flag::Case => ("Alt+C", Key::C),
            Flag::Word => ("Alt+W", Key::W),
            Flag::Regex => ("Alt+R", Key::R),
            Flag::Hex => ("Alt+H", Key::H),
        }
    }
}

/// Флажки поиска: «Регистр» — с учётом регистра; «Регулярное» и «Hex» — взаимоисключающие.
#[allow(
    clippy::struct_excessive_bools,
    reason = "четыре независимых флажка строки"
)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Flags {
    pub case: bool,
    pub word: bool,
    pub regex: bool,
    pub hex: bool,
}

impl Flags {
    pub(super) fn get(self, f: Flag) -> bool {
        match f {
            Flag::Case => self.case,
            Flag::Word => self.word,
            Flag::Regex => self.regex,
            Flag::Hex => self.hex,
        }
    }

    /// Флажок что-то значит при остальных: у Hex нет регистра и слова, у regex — слова.
    pub(super) fn enabled(self, f: Flag) -> bool {
        match f {
            Flag::Case => !self.hex,
            Flag::Word => !self.hex && !self.regex,
            Flag::Regex | Flag::Hex => true,
        }
    }

    pub(super) fn toggle(&mut self, f: Flag) {
        if !self.enabled(f) {
            return;
        }
        match f {
            Flag::Case => self.case = !self.case,
            Flag::Word => self.word = !self.word,
            Flag::Regex => {
                self.regex = !self.regex;
                self.hex &= !self.regex;
            }
            Flag::Hex => {
                self.hex = !self.hex;
                self.regex &= !self.hex;
            }
        }
    }
}

/// Что значит клавиша в строке.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum BarAction {
    /// Enter (Shift+Enter — назад).
    Commit {
        back: bool,
    },
    /// Esc.
    Close,
    Toggle(Flag),
    /// F7 в строке перехода — к поиску, Alt+F8 в строке поиска — к переходу.
    Switch(BarMode),
    /// Shift+F7 / Alt+F7: следующее / предыдущее без закрытия строки.
    Again {
        back: bool,
    },
    /// Ничего не делать, но и полю не отдавать (Tab).
    Swallow,
}

/// Поле строки: `InputState` и его текст (зеркало — подписка на правку).
struct BarField {
    input: Entity<InputState>,
    text: Rc<RefCell<String>>,
    _sub: Subscription,
}

/// Строка поиска или перехода документа `owner`.
pub(super) struct FindBar {
    pub mode: BarMode,
    pub flags: Flags,
    /// Чья строка (id документа).
    pub owner: u32,
    /// Текст нового поля (поле создаётся в кадре: нужно окно).
    init: String,
    field: Option<BarField>,
}

impl FindBar {
    pub(super) fn new(mode: BarMode, owner: u32, init: String, flags: Flags) -> FindBar {
        FindBar {
            mode,
            flags,
            owner,
            init,
            field: None,
        }
    }

    /// Поле уже создано (клавиши — ему).
    pub(super) fn has_field(&self) -> bool {
        self.field.is_some()
    }

    /// Набранный текст.
    pub(super) fn text(&self) -> String {
        self.field
            .as_ref()
            .map_or_else(|| self.init.clone(), |f| f.text.borrow().clone())
    }

    /// Другой ряд с новым текстом: поле пересоздаётся в следующем кадре.
    pub(super) fn switch(&mut self, mode: BarMode, init: String) {
        self.mode = mode;
        self.init = init;
        self.field = None;
    }

    /// Поле — под строку: новое получает фокус, текст выделен. Зовётся в кадре.
    pub(super) fn sync(&mut self, window: &mut Window, cx: &mut Context<CoreHost>) {
        if self.field.is_some() {
            return;
        }
        let init = std::mem::take(&mut self.init);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(init.clone()));
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        let text = Rc::new(RefCell::new(init));
        let mirror = Rc::clone(&text);
        let sub = cx.subscribe_in(
            &input,
            window,
            move |_h: &mut CoreHost, input, ev: &InputEvent, _window, cx| {
                if matches!(ev, InputEvent::Change) {
                    *mirror.borrow_mut() = input.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );
        self.field = Some(BarField {
            input,
            text,
            _sub: sub,
        });
    }

    /// Клавиша строки; `None` — клавиша поля (печать, стрелки, буфер обмена).
    pub(super) fn key(&self, ks: &Keystroke) -> Option<BarAction> {
        let m = ks.modifiers;
        let plain = !m.alt && !m.control && !m.platform;
        match ks.key.as_str() {
            "enter" if !m.alt && !m.control => {
                return Some(BarAction::Commit { back: m.shift });
            }
            "escape" => return Some(BarAction::Close),
            "tab" => return Some(BarAction::Swallow),
            "f7" if plain && !m.shift => {
                return Some(match self.mode {
                    BarMode::Find => BarAction::Commit { back: false },
                    BarMode::Goto => BarAction::Switch(BarMode::Find),
                });
            }
            "f7" if plain && m.shift => return Some(BarAction::Again { back: false }),
            "f7" if m.alt && !m.control => return Some(BarAction::Again { back: true }),
            "f8" if m.alt && !m.control && self.mode == BarMode::Find => {
                return Some(BarAction::Switch(BarMode::Goto));
            }
            _ => {}
        }
        if self.mode == BarMode::Find && m.alt && !m.control && !m.platform {
            let k = super::super::input::stroke_key(ks)?;
            return Flag::ALL
                .into_iter()
                .find(|f| f.key().1 == k)
                .map(BarAction::Toggle);
        }
        None
    }

    /// Поле во всю ширину обёртки (моно, как текст документа).
    fn input(&self, w: f32) -> Option<Div> {
        let f = self.field.as_ref()?;
        Some(
            div()
                .w(px(w))
                .flex_none()
                .font_family(MONO)
                .child(Input::new(&f.input).small()),
        )
    }

    /// Ряд «Найти» (`screen-editreplace.png`): поле, «N из M», флажки Регистр, Слово,
    /// Регулярное, Hex. `counter` — итог и признак ошибки (`critical`).
    pub(super) fn find_row(
        &self,
        theme: &Theme,
        counter: (String, bool),
        toggle: &dyn Fn(Flag) -> Handler,
    ) -> Div {
        let i18n = urfin_i18n::current();
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(title(i18n.get("ui-find")))
            .children(self.input(W_FIND))
            .child(note(theme, counter.0, counter.1).min_w(px(52.0)));
        let mut checks = div().flex().items_center().gap(px(16.0)).ml(px(6.0));
        for f in Flag::ALL {
            let on = self.flags.get(f);
            let enabled = self.flags.enabled(f);
            let mut c = check(theme, f.id().to_owned(), on, i18n.get(f.label()), enabled);
            if f == Flag::Regex {
                c = c.child(
                    div()
                        .font_family(MONO)
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::new_static(".*")),
                );
            }
            if enabled {
                c = c.on_click(toggle(f));
            }
            checks = checks.child(c);
        }
        row = row.child(checks);
        container(theme, Tone::Plain, 12.0, 10.0).child(row)
    }

    /// Ряд «Перейти» (`screen-editgoto.png`): подпись, поле, подсказка «из N · строка»
    /// (ошибка разбора и ожидание индекса — на её месте), Перейти (Enter) / Отмена (Esc).
    pub(super) fn goto_row(
        &self,
        theme: &Theme,
        label: String,
        hint: (String, bool),
        go: Handler,
        cancel: Handler,
    ) -> Div {
        let i18n = urfin_i18n::current();
        let row = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(title(label))
            .children(self.input(W_GOTO))
            .child(note(theme, hint.0, hint.1))
            .child(
                div().ml(px(8.0)).child(
                    button(
                        theme,
                        "goto-go".into(),
                        i18n.get("ui-button-go"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(go),
                ),
            )
            .child(
                button(
                    theme,
                    "goto-cancel".into(),
                    i18n.get("ui-button-cancel"),
                    "Esc",
                    Btn::Normal,
                )
                .on_click(cancel),
            );
        container(theme, Tone::Plain, 12.0, 10.0).child(row)
    }
}

fn title(text: String) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .child(SharedString::from(text))
}

/// Пояснение ряда: мелко, `secondary`; ошибка — `critical`.
fn note(theme: &Theme, text: String, bad: bool) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(12.0))
        .text_color(theme.c(if bad {
            Role::CriticalFg
        } else {
            Role::TextSecondary
        }))
        .child(SharedString::from(text))
}

/// Клавиши keybar при открытой строке: только те, что не подписаны на кнопках (№61).
pub(super) fn keys(mode: BarMode) -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    match mode {
        BarMode::Find => {
            let mut v = vec![k("Shift+Enter", "ui-keys-qs-prev")];
            v.extend(Flag::ALL.into_iter().map(|f| k(f.key().0, f.label())));
            v.push(k("Esc", "ui-keys-close"));
            v
        }
        BarMode::Goto => vec![k("F7", "keybar-viewer-search")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_and_hex_exclude_each_other() {
        let mut f = Flags::default();
        f.toggle(Flag::Regex);
        assert!(f.regex && !f.enabled(Flag::Word));
        f.toggle(Flag::Hex);
        assert!(f.hex && !f.regex);
        assert!(!f.enabled(Flag::Case) && !f.enabled(Flag::Word));
        let before = f;
        f.toggle(Flag::Case);
        assert_eq!(f, before, "у Hex нет регистра");
        f.toggle(Flag::Hex);
        f.toggle(Flag::Word);
        assert!(f.word && !f.hex && !f.regex);
    }

    fn ks(key: &str, alt: bool, shift: bool) -> Keystroke {
        let mut k = Keystroke::parse(key).unwrap();
        k.modifiers.alt = alt;
        k.modifiers.shift = shift;
        k
    }

    #[test]
    fn keys_map_to_actions() {
        let find = FindBar::new(BarMode::Find, 1, String::new(), Flags::default());
        let goto = FindBar::new(BarMode::Goto, 1, String::new(), Flags::default());
        assert_eq!(
            find.key(&ks("enter", false, true)),
            Some(BarAction::Commit { back: true })
        );
        assert_eq!(
            find.key(&ks("escape", false, false)),
            Some(BarAction::Close)
        );
        assert_eq!(
            find.key(&ks("c", true, false)),
            Some(BarAction::Toggle(Flag::Case))
        );
        assert_eq!(
            find.key(&ks("h", true, false)),
            Some(BarAction::Toggle(Flag::Hex))
        );
        assert_eq!(find.key(&ks("c", false, false)), None, "печать — полю");
        assert_eq!(goto.key(&ks("c", true, false)), None);
        assert_eq!(
            goto.key(&ks("f7", false, false)),
            Some(BarAction::Switch(BarMode::Find))
        );
        assert_eq!(
            find.key(&ks("f8", true, false)),
            Some(BarAction::Switch(BarMode::Goto))
        );
        assert_eq!(
            find.key(&ks("f7", true, false)),
            Some(BarAction::Again { back: true })
        );
        assert_eq!(
            find.key(&ks("f7", false, true)),
            Some(BarAction::Again { back: false })
        );
    }
}
