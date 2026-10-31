//! Фичи окна (ADR-0022): состояние фичи и её ответы хосту живут в её модуле, хост
//! зовёт фичи по списку [`Features::new`] — по строке на фичу. Новая фича — модуль с
//! `impl ShellFeature` и строка в списке; `mod.rs`, ветки событий и клавиш не правятся.

use std::any::Any;
use std::time::{Duration, Instant};

use gpui_kit::{AnyElement, Context, Div, FocusHandle, Keystroke, Window};
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

/// Что фича ставит в область панелей.
pub(super) enum Main {
    /// Поверх панелей, во всю их площадь (сетка терминала при спрятанных панелях).
    Cover(AnyElement),
    /// Вместо панелей (вкладка настроек); важнее `Cover`.
    Replace(AnyElement),
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

    /// Команды по времени (после событий прохода Core): пауза курсора быстрого просмотра
    /// прошла — `viewer.quick`. Будильник ставит `tick`.
    fn due(&mut self, _s: &AppState, _now: Instant) -> Vec<Command> {
        Vec::new()
    }

    /// Размер окна в проходе Core: команда Core, если фиче он важен (размер PTY).
    fn sync(&mut self, _theme: &Theme, _window: &mut Window) -> Option<Command> {
        None
    }

    /// Очереди событий Core (не команды): размер окна документа (`InboxMsg::Viewport`
    /// вьювера). `true` — событие отправлено, хост будит Core. Зовётся в проходе Core и
    /// в кадре.
    fn inbox(&mut self, _s: &AppState, _theme: &Theme, _window: &mut Window) -> bool {
        false
    }

    /// Содержимое заголовка окна вместо наборов табов панелей (вкладки экранов).
    fn tabs(&self, _s: &AppState, _theme: &Theme, _click: Click<'_>) -> Option<Div> {
        None
    }

    /// Клавиша до панелей. `Some` — фича её забрала (команды Core).
    fn key(&mut self, _s: &AppState, _ks: &Keystroke) -> Option<Vec<Command>> {
        None
    }

    /// У фичи открыто своё поле ввода (строка «Найти» вьювера): клавиши, которые `key`
    /// не забрал, идут полю, а не keymap.
    fn editing(&self, _s: &AppState) -> bool {
        false
    }

    /// Поля ввода фичи — под состояние, в кадре (нужны окно и контекст): новое поле
    /// получает фокус; закрытое возвращает его окну (`focus`).
    fn input(
        &mut self,
        _s: &AppState,
        _focus: &FocusHandle,
        _window: &mut Window,
        _cx: &mut Context<CoreHost>,
    ) {
    }

    /// Область панелей. Зовётся каждый кадр у всех фич (`hidden` — панели спрятаны):
    /// фича без своего вида сбрасывает кэши кадра и отвечает `None`.
    fn main(
        &mut self,
        _f: &Frame<'_>,
        _hidden: bool,
        _window: &mut Window,
        _cx: &Context<CoreHost>,
    ) -> Option<Main> {
        None
    }

    /// Встроенная строка внизу панелей, когда нет вопроса, черновика и быстрого поиска
    /// (приветствие, итог сравнения). Первая фича со строкой — её.
    fn strip(&self, _f: &Frame<'_>, _cx: &Context<CoreHost>) -> Option<Div> {
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
            Box::new(super::settings::Settings),
            Box::new(super::view::Viewer::default()),
            Box::new(super::welcome::Welcome::default()),
            Box::new(super::compare::Compare::default()),
        ])
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &dyn ShellFeature> {
        self.0.iter().map(AsRef::as_ref)
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn ShellFeature>> {
        self.0.iter_mut()
    }

    /// Фича по типу.
    pub(super) fn get<T: ShellFeature>(&self) -> Option<&T> {
        self.0
            .iter()
            .find_map(|f| (f.as_ref() as &dyn Any).downcast_ref::<T>())
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

    /// Какая-то фича держит поле ввода.
    pub(super) fn editing(&self, s: &AppState) -> bool {
        self.0.iter().any(|f| f.editing(s))
    }
}
