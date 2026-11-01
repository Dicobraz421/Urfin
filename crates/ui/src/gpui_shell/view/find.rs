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
use gpui_kit::{
    Context, Div, Entity, Focusable as _, Keystroke, SharedString, Subscription, Window, div, px,
};
use urfin_keymap::ev::Key;

use super::super::CoreHost;
use super::super::overlays::Handler;
use super::super::strip::{Btn, Tone, button, check, container};
use super::super::theme::Theme;
use super::rows::MONO;
use crate::tokens::Role;

/// Высота строки: поля 10, поле и кнопки 30, черта 2 (`screen-editgoto.png`).
pub(super) const H_BAR: f32 = 52.0;
/// Между рядами «Найти» и «Заменить на» (`screen-editreplace.png`).
const ROW_GAP: f32 = 7.0;
/// Подписи рядов поиска и замены — одной ширины: поля друг под другом.
const W_LABEL: f32 = 84.0;
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
    /// Редактор: Enter в «Заменить на» — заменить и к следующему, Ctrl+Alt+Enter — все.
    Replace {
        all: bool,
    },
    /// Редактор: Tab — между «Найти» и «Заменить на».
    Tab,
    /// Редактор: Ctrl+F7 — открыть ряд «Заменить на».
    OpenReplace,
}

/// Поле строки: `InputState` и его текст (зеркало — подписка на правку).
struct BarField {
    input: Entity<InputState>,
    text: Rc<RefCell<String>>,
    _sub: Subscription,
}

impl BarField {
    /// Новое поле с текстом `init`; `focus` — фокус ему, текст выделен.
    fn new(init: String, focus: bool, window: &mut Window, cx: &mut Context<CoreHost>) -> BarField {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(init.clone()));
        if focus {
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        }
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
        BarField {
            input,
            text,
            _sub: sub,
        }
    }
}

/// Строка поиска или перехода документа `owner`.
#[allow(clippy::struct_excessive_bools, reason = "независимые признаки строки")]
pub(super) struct FindBar {
    pub mode: BarMode,
    pub flags: Flags,
    /// Чья строка (id документа).
    pub owner: u32,
    /// Текст нового поля (поле создаётся в кадре: нужно окно).
    init: String,
    field: Option<BarField>,
    /// Строка редактора (S7-18): без Hex, с рядом «Заменить на» и Ctrl+F7.
    editor: bool,
    /// Ряд «Заменить на» открыт; текст его нового поля.
    repl_on: bool,
    repl_init: String,
    repl: Option<BarField>,
    /// Фокус — у поля «Заменить на».
    in_repl: bool,
}

impl FindBar {
    pub(super) fn new(mode: BarMode, owner: u32, init: String, flags: Flags) -> FindBar {
        FindBar {
            mode,
            flags,
            owner,
            init,
            field: None,
            editor: false,
            repl_on: false,
            repl_init: String::new(),
            repl: None,
            in_repl: false,
        }
    }

    /// Строка редактора; `repl` — открыть и ряд «Заменить на» с этим текстом.
    pub(super) fn editor(
        mode: BarMode,
        owner: u32,
        init: String,
        flags: Flags,
        repl: Option<String>,
    ) -> FindBar {
        let mut b = FindBar::new(
            mode,
            owner,
            init,
            Flags {
                hex: false,
                ..flags
            },
        );
        b.editor = true;
        if let Some(r) = repl {
            b.repl_on = true;
            b.repl_init = r;
        }
        b
    }

    /// Ряд «Заменить на» открыт (в строке поиска).
    pub(super) fn has_replace(&self) -> bool {
        self.repl_on && self.mode == BarMode::Find
    }

    /// Открыть ряд «Заменить на»; `focus` — фокус ему.
    pub(super) fn open_replace(&mut self, init: String, focus: bool) {
        if !self.repl_on {
            self.repl_on = true;
            self.repl_init = init;
            self.repl = None;
        }
        self.in_repl = focus;
    }

    /// Текст «Заменить на».
    pub(super) fn repl_text(&self) -> String {
        self.repl
            .as_ref()
            .map_or_else(|| self.repl_init.clone(), |f| f.text.borrow().clone())
    }

    /// Фокус у «Заменить на» (Enter — заменить).
    pub(super) fn in_replace(&self) -> bool {
        self.has_replace() && self.in_repl
    }

    /// Кто из полей держит фокус окна (клик мышью мимо Tab).
    pub(super) fn track_focus(&mut self, window: &Window, cx: &gpui_kit::App) {
        if let Some(r) = &self.repl
            && r.input.read(cx).focus_handle(cx).is_focused(window)
        {
            self.in_repl = true;
        } else if let Some(f) = &self.field
            && f.input.read(cx).focus_handle(cx).is_focused(window)
        {
            self.in_repl = false;
        }
    }

    /// Tab: фокус — другому полю, его текст выделен.
    pub(super) fn focus_other(&mut self, window: &mut Window, cx: &mut Context<CoreHost>) {
        if !self.has_replace() {
            return;
        }
        self.in_repl = !self.in_repl;
        let target = if self.in_repl {
            self.repl.as_ref()
        } else {
            self.field.as_ref()
        };
        if let Some(f) = target {
            f.input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
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
        if self.repl_on {
            // Ряд «Заменить на» пересоздаётся с прежним текстом.
            self.repl_init = self.repl_text();
            self.repl = None;
        }
        self.in_repl = false;
    }

    /// Поля — под строку: новое получает фокус (если фокус — его), текст выделен.
    /// Зовётся в кадре.
    pub(super) fn sync(&mut self, window: &mut Window, cx: &mut Context<CoreHost>) {
        if self.field.is_none() {
            let init = std::mem::take(&mut self.init);
            let focus = !self.in_replace();
            self.field = Some(BarField::new(init, focus, window, cx));
        }
        if self.has_replace() && self.repl.is_none() {
            let init = std::mem::take(&mut self.repl_init);
            let focus = self.in_repl;
            self.repl = Some(BarField::new(init, focus, window, cx));
        }
    }

    /// Клавиша строки; `None` — клавиша поля (печать, стрелки, буфер обмена).
    pub(super) fn key(&self, ks: &Keystroke) -> Option<BarAction> {
        let m = ks.modifiers;
        let plain = !m.alt && !m.control && !m.platform;
        match ks.key.as_str() {
            "enter" if m.alt && m.control && self.has_replace() => {
                return Some(BarAction::Replace { all: true });
            }
            "enter" if !m.alt && !m.control => {
                if self.in_replace() && !m.shift {
                    return Some(BarAction::Replace { all: false });
                }
                return Some(BarAction::Commit { back: m.shift });
            }
            "escape" => return Some(BarAction::Close),
            "tab" if self.has_replace() => return Some(BarAction::Tab),
            "tab" => return Some(BarAction::Swallow),
            "f7" if self.editor && m.control && !m.alt && !m.platform && !m.shift => {
                return Some(BarAction::OpenReplace);
            }
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
            return self
                .flags_shown()
                .find(|f| f.key().1 == k)
                .map(BarAction::Toggle);
        }
        None
    }

    /// Флажки ряда «Найти»: у редактора нет Hex.
    fn flags_shown(&self) -> impl Iterator<Item = Flag> + use<> {
        let editor = self.editor;
        Flag::ALL
            .into_iter()
            .filter(move |f| !(editor && *f == Flag::Hex))
    }

    /// Поле во всю ширину обёртки (моно, как текст документа).
    fn input(&self, w: f32) -> Option<Div> {
        Some(field_el(self.field.as_ref()?, w))
    }

    /// Подпись ряда: при двух рядах — общей ширины (поля друг под другом).
    fn label(&self, text: String) -> Div {
        let t = title(text);
        if self.has_replace() {
            t.w(px(W_LABEL))
        } else {
            t
        }
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
            .child(self.label(i18n.get("ui-find")))
            .children(self.input(W_FIND))
            .child(note(theme, counter.0, counter.1).min_w(px(52.0)));
        let mut checks = div().flex().items_center().gap(px(16.0)).ml(px(6.0));
        for f in self.flags_shown() {
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
        container(theme, Tone::Plain, 12.0, 10.0)
            .gap(px(ROW_GAP))
            .child(row)
    }

    /// Ряд «Заменить на» (`screen-editreplace.png`): поле, Заменить (Enter), Заменить все
    /// (Ctrl+Alt+Enter), подсказка групп — в «Регулярном». Идёт вторым в `find_row`.
    pub(super) fn replace_row(
        &self,
        theme: &Theme,
        enabled: bool,
        one: Handler,
        all: Handler,
    ) -> Option<Div> {
        if !self.has_replace() {
            return None;
        }
        let i18n = urfin_i18n::current();
        let mut b_one = button(
            theme,
            "replace-one".into(),
            i18n.get("ui-replace-one"),
            "Enter",
            Btn::Primary,
        );
        let mut b_all = button(
            theme,
            "replace-all".into(),
            i18n.get("ui-replace-all"),
            "Ctrl+Alt+Enter",
            Btn::Normal,
        );
        if enabled {
            b_one = b_one.on_click(one);
            b_all = b_all.on_click(all);
        } else {
            b_one = b_one.opacity(0.6);
            b_all = b_all.opacity(0.6);
        }
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(self.label(i18n.get("ui-replace")))
            .children(self.repl.as_ref().map(|f| field_el(f, W_FIND)))
            .child(div().ml(px(8.0)).child(b_one))
            .child(b_all);
        if self.flags.regex {
            row = row.child(note(theme, i18n.get("ui-replace-groups"), false).ml(px(8.0)));
        }
        Some(row)
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

/// Поле `f` шириной `w` (моно, как текст документа).
fn field_el(f: &BarField, w: f32) -> Div {
    div()
        .w(px(w))
        .flex_none()
        .font_family(MONO)
        .child(Input::new(&f.input).small())
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

/// Клавиши keybar строки редактора (`screen-editreplace.png`, `screen-editgoto.png`).
pub(super) fn editor_keys(mode: BarMode, replace: bool) -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    match mode {
        BarMode::Find => {
            let mut v = vec![k("Shift+Enter", "ui-keys-qs-prev")];
            if replace {
                v.push(k("Tab", "ui-keys-find-replace"));
            }
            v.extend(
                [Flag::Case, Flag::Word, Flag::Regex]
                    .into_iter()
                    .map(|f| k(f.key().0, f.label())),
            );
            v.push(k("Esc", "ui-keys-close"));
            v
        }
        BarMode::Goto => vec![
            k("F7", "keybar-editor-search"),
            k("Ctrl+F7", "keybar-editor-replace"),
        ],
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

    #[test]
    fn editor_bar_keys() {
        let mut ctrl_f7 = ks("f7", false, false);
        ctrl_f7.modifiers.control = true;
        let viewer = FindBar::new(BarMode::Find, 1, String::new(), Flags::default());
        assert_eq!(viewer.key(&ctrl_f7), None, "у вьювера замены нет");
        let mut b = FindBar::editor(BarMode::Find, 1, String::new(), Flags::default(), None);
        assert_eq!(b.key(&ks("h", true, false)), None, "у редактора нет Hex");
        assert_eq!(b.key(&ctrl_f7), Some(BarAction::OpenReplace));
        assert_eq!(b.key(&ks("tab", false, false)), Some(BarAction::Swallow));
        b.open_replace(String::new(), true);
        assert!(b.has_replace() && b.in_replace());
        assert_eq!(b.key(&ks("tab", false, false)), Some(BarAction::Tab));
        assert_eq!(
            b.key(&ks("enter", false, false)),
            Some(BarAction::Replace { all: false })
        );
        assert_eq!(
            b.key(&ks("enter", false, true)),
            Some(BarAction::Commit { back: true })
        );
        let mut all = ks("enter", true, false);
        all.modifiers.control = true;
        assert_eq!(b.key(&all), Some(BarAction::Replace { all: true }));
        b.switch(BarMode::Goto, String::new());
        assert!(!b.has_replace());
        assert_eq!(b.key(&all), None);
        b.switch(BarMode::Find, String::new());
        assert!(b.has_replace() && !b.in_replace(), "ряд замены остаётся");
        assert_eq!(
            b.key(&ks("enter", false, false)),
            Some(BarAction::Commit { back: false })
        );
    }
}
