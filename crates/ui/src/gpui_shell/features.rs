//! Фичи окна (ADR-0022): состояние фичи и её ответы хосту живут в её модуле, хост
//! зовёт фичи по списку [`Features::new`] — по строке на фичу. Новая фича — модуль с
//! `impl ShellFeature` и строка в списке; `mod.rs`, ветки событий и клавиш не правятся.

use std::any::Any;
use std::time::{Duration, Instant};

use gpui_kit::{AnyElement, Context, Keystroke, Window};
use urfin_core::{AppState, Args, Command};

use super::CoreHost;
use super::overlays::{Click, ClickAll};
use super::theme::Theme;

/// Что фиче нужно для отрисовки кадра.
pub(super) struct Frame<'a> {
    pub s: &'a AppState,
    pub theme: &'a Theme,
    /// Клик → команда Core (через хост, как мышь).
    pub click: Click<'a>,
    pub click_all: ClickAll<'a>,
}

/// Фича окна. Все методы — с пустой реализацией: фича отвечает только на своё.
pub(super) trait ShellFeature: Any {
    /// `UiEvent::Ext` из Core. `None` — событие не этой фичи; `Some` — команды Core
    /// (бывает пусто).
    fn ext(
        &mut self,
        _id: &str,
        _args: &Args,
        _s: &AppState,
        _cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        None
    }

    /// После событий прохода Core (выборки). `Some(d)` — перерисовать и разбудить окно
    /// через `d`.
    fn tick(&mut self, _s: &AppState, _now: Instant) -> Option<Duration> {
        None
    }

    /// Размер окна в проходе Core: команда Core, если фиче он важен (размер PTY).
    fn sync(&mut self, _theme: &Theme, _window: &mut Window) -> Option<Command> {
        None
    }

    /// Клавиша до панелей. `Some` — фича её забрала (команды Core).
    fn key(&mut self, _s: &AppState, _ks: &Keystroke) -> Option<Vec<Command>> {
        None
    }

    /// Вместо панелей, когда они спрятаны (`hidden`); при видимых панелях — сбросить
    /// кэши кадра и вернуть `None`.
    fn main(
        &mut self,
        _f: &Frame<'_>,
        _hidden: bool,
        _window: &mut Window,
        _cx: &Context<CoreHost>,
    ) -> Option<AnyElement> {
        None
    }

    /// Слой у низа области панелей, на всю ширину (раскрытый список задач).
    fn overlay(&self, _f: &Frame<'_>, _cx: &Context<CoreHost>) -> Option<AnyElement> {
        None
    }

    /// Строка зоны событий над командной строкой (job-бар), занимает место.
    fn bar(&self, _f: &Frame<'_>) -> Option<AnyElement> {
        None
    }
}

/// Фичи окна по порядку опроса.
pub(super) struct Features(Vec<Box<dyn ShellFeature>>);

impl Features {
    pub(super) fn new() -> Features {
        Features(vec![
            Box::new(super::term_view::Term::default()),
            Box::new(super::jobs::Jobs::default()),
        ])
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &dyn ShellFeature> {
        self.0.iter().map(AsRef::as_ref)
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn ShellFeature>> {
        self.0.iter_mut()
    }

    /// Фича по типу (обработчики мыши и кликов из её же модуля).
    pub(super) fn get_mut<T: ShellFeature>(&mut self) -> Option<&mut T> {
        self.0
            .iter_mut()
            .find_map(|f| (f.as_mut() as &mut dyn Any).downcast_mut::<T>())
    }

    /// Первая фича, забравшая событие.
    pub(super) fn ext(
        &mut self,
        id: &str,
        args: &Args,
        s: &AppState,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        self.0.iter_mut().find_map(|f| f.ext(id, args, s, cx))
    }

    /// Первая фича, забравшая клавишу.
    pub(super) fn key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        self.0.iter_mut().find_map(|f| f.key(s, ks))
    }
}
