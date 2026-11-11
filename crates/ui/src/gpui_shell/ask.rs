//! Поле строки вопроса (S8-36, S8 §3.7, ответ В5 «А»): у вопроса с вводом
//! (`PromptSpec::input`) — поле на общей строке вопроса ([`super::overlays::prompt_bar`]);
//! `secret` — с маской, как ряд пароля строки распаковки (S8-29). Так спрашивает пароль
//! архив при входе, F3, F5 и `arc.test` (`JobHandle::ask`, S8-35).
//!
//! Enter и кнопка ответа — `prompt.reply { id, choice, input }`: текст поля берётся в
//! момент ответа (в кадре не держится), поле сразу очищается. Esc — `prompt.cancel`.
//! Ввод не попадает в историю полей (поле не [`super::field::Field`]), сессию и логи.

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Context, Entity, FocusHandle, Focusable as _, Keystroke, Window};
use urfin_core::{AppState, Args, Command, Prompt};

use super::CoreHost;
use super::features::ShellFeature;
use super::overlays::{self, Handler};

/// Событие сцены `screen-arcpw`: текст поля (`text`).
pub(super) const DEMO: &str = "screens.ask.text";

/// Видимый вопрос с полем ввода.
fn asking(s: &AppState) -> Option<&Prompt> {
    s.prompts.active().filter(|p| p.spec.input.is_some())
}

/// Ответ вариантом `i` с текстом поля.
pub(super) fn answer(p: &Prompt, i: usize, input: &str) -> Option<Command> {
    overlays::reply(p, i, false).map(|c| c.arg("input", input))
}

/// Поле видимого вопроса.
#[derive(Default)]
pub(super) struct AskField {
    /// Вопрос (id) и его поле.
    field: Option<(u64, Entity<InputState>)>,
    /// Сцена `screen-arcpw`: текст поля.
    demo: Option<String>,
}

impl AskField {
    /// Поле вопроса `p`, если оно уже есть (кадр после открытия вопроса).
    pub(super) fn input_of(&self, p: &Prompt) -> Option<&Entity<InputState>> {
        self.field
            .as_ref()
            .filter(|(id, _)| *id == p.id.0)
            .map(|(_, f)| f)
    }

    /// Текст поля для ответа на `p`; поле очищается.
    fn take(&self, p: &Prompt, window: &mut Window, cx: &mut Context<CoreHost>) -> Option<String> {
        let f = self.input_of(p)?;
        let text = f.read(cx).value().to_string();
        f.update(cx, |s, cx| s.set_value("", window, cx));
        Some(text)
    }
}

impl ShellFeature for AskField {
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        _s: &AppState,
        _cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        (id == DEMO).then(|| {
            self.demo = Some(args.str("text").unwrap_or_default().to_owned());
            Vec::new()
        })
    }

    /// Enter — ответ вариантом по умолчанию с текстом поля, Esc — отмена; остальное —
    /// полю ([`ShellFeature::editing`]).
    fn key_in(
        &mut self,
        s: &AppState,
        ks: &Keystroke,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let p = asking(s)?;
        self.input_of(p)?;
        match ks.key.as_str() {
            "escape" => Some(vec![overlays::cancel(p)]),
            "enter" => {
                let text = self.take(p, window, cx)?;
                Some(
                    answer(p, usize::from(p.spec.default), &text)
                        .into_iter()
                        .collect(),
                )
            }
            _ => None,
        }
    }

    fn editing(&self, s: &AppState) -> bool {
        asking(s).is_some_and(|p| self.input_of(p).is_some())
    }

    fn input(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let Some(p) = asking(s) else {
            // Вопроса нет: поле забыто, фокус — окну.
            if let Some((_, f)) = self.field.take() {
                let had = f.read(cx).focus_handle(cx).is_focused(window);
                f.update(cx, |st, cx| st.set_value("", window, cx));
                if had {
                    window.focus(focus, cx);
                }
            }
            self.demo = None;
            return;
        };
        if self.input_of(p).is_none() {
            // Новый вопрос (и повтор после неверного пароля) — новое пустое поле.
            if let Some((_, old)) = self.field.take() {
                old.update(cx, |st, cx| st.set_value("", window, cx));
            }
            let secret = p.spec.secret;
            let init = p.spec.input.clone().unwrap_or_default();
            let f = cx.new(|cx| InputState::new(window, cx).masked(secret));
            f.update(cx, |st, cx| {
                st.set_value(init, window, cx);
                st.focus(window, cx);
                st.select_all(window, cx);
            });
            self.field = Some((p.id.0, f));
        }
        if let Some(text) = self.demo.take()
            && let Some(f) = self.input_of(p)
        {
            f.update(cx, |st, cx| st.set_value(text, window, cx));
        }
    }
}

impl CoreHost {
    /// Поле видимого вопроса для кадра.
    pub(super) fn ask_input(&self, p: &Prompt) -> Option<Entity<InputState>> {
        self.features.get::<AskField>()?.input_of(p).cloned()
    }

    /// Кнопка ответа вопроса с полем: команда собирается по клику.
    pub(super) fn ask_click(cx: &Context<CoreHost>, i: usize) -> Handler {
        Box::new(
            cx.listener(move |h: &mut CoreHost, _: &ClickEvent, window, cx| {
                let Some(p) = h.core.as_ref().and_then(|c| asking(c.state())) else {
                    return;
                };
                let Some(a) = h.features.get::<AskField>() else {
                    return;
                };
                let Some(text) = a.take(p, window, cx) else {
                    return;
                };
                if let Some(c) = answer(p, i, &text) {
                    h.run(c, window, cx);
                }
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::answer;
    use urfin_core::{Anchor, Choice, PromptSpec, Prompts};

    /// Ответ — выбранный вариант и текст поля; без «для всех».
    #[test]
    fn ask_answer_carries_input() {
        let choice = |id: &str| Choice {
            id: id.into(),
            label: id.into(),
            key: '\0',
        };
        let spec = PromptSpec {
            anchor: Anchor::Cmdline,
            title: "Пароль для a.7z".into(),
            body: String::new(),
            choices: vec![choice("ok"), choice("cancel")].into(),
            default: 0,
            input: Some(String::new()),
            allow_all: false,
            detail: None,
            secret: true,
        };
        let mut q = Prompts::default();
        let (id, _rx) = q.open(spec, None);
        let p = q.active().unwrap();
        let c = answer(p, 0, "секрет").unwrap();
        assert_eq!(c.id.as_str(), "prompt.reply");
        assert_eq!(c.args.int("id"), i64::try_from(id.0).ok());
        assert_eq!(c.args.str("choice"), Some("ok"));
        assert_eq!(c.args.str("input"), Some("секрет"));
        assert_eq!(c.args.bool("all"), None);
        assert!(answer(p, 5, "x").is_none());
    }
}
