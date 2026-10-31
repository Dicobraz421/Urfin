//! Экран вьювера (S7-07, S7 §6): фича окна `Viewer` (ADR-0022) — `Main::Replace` на месте
//! панелей, вкладки экранов в заголовке, шапка 36, поле номеров и строки одним `canvas`
//! (кэш `ShapedLine`), полоса прокрутки 14 с метками, статус и keybar вьювера.
//!
//! Позиция, прокрутка и страницы — в handler `viewer`: UI шлёт размер окна событием
//! `InboxMsg::Viewport` (решение 2026-10-30 «viewer-viewport»), колесо и клавиши — командой
//! `viewer.scroll`. Кадр читает только кэш страниц: чего нет — скелетон.
//!
//! S7-10: строки «Найти» (F7) и «Перейти» (Alt+F8) над статусом — [`find::FindBar`],
//! пока фокус Core — `Focus::Overlay(Find)`; подсветка всех совпадений на экране, полоса
//! прогресса поиска под шапкой.
//!
//! S7-22a: Markdown в «Разметке» — документ ([`doc`]): `gpui::list` блоков вместо строк.
//!
//! S7-11: выделение мышью (drag, Shift+клик, двойной клик; Hex — в обеих колонках) —
//! `InboxMsg::Select`, перетаскивание бегунка — `InboxMsg::ScrollTo`, `viewer.copy` —
//! текст от handler'а (`UiEvent::Ext`) в буфер обмена, кнопка мыши «Назад» — `viewer.back`.
//!
//! S7-13: быстрый просмотр и `viewer.dock` — тело области панели ([`quick`]).
//!
//! S7-14: список экранов (F12, `ui.screens`) и история просмотра (Alt+F11,
//! `viewer.history`) — плашка [`lists`] поверх экрана или панелей.
#![allow(
    clippy::many_single_char_names,
    reason = "кадр: d — документ, g — геометрия, r — строка, s — состояние"
)]

mod doc;
mod find;
mod head;
mod lists;
mod quick;
mod rows;

use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, FocusHandle, Keystroke, SharedString, Window, div, px,
};
use urfin_core::{AppState, Args, Command, Focus, OverlayKind, Side};
use urfin_i18n::FluentArgs;
use urfin_view::search::Pattern;
use urfin_view::{DocId, InboxMsg, Mode, Placement, ViewState, ViewerDoc};

use find::{BarAction, BarMode, FindBar, Flag, Flags};

use super::CoreHost;
use super::features::{Frame, Main, ShellFeature};
use super::overlays::Click;
use super::screens::Scene;
use super::theme::Theme;
use crate::tokens::Metric;

/// Документ на экране: активный экран — вьювер в окне, фокус у него (или у строки
/// поиска над ним).
pub(super) fn shown(s: &AppState) -> Option<DocId> {
    if !matches!(s.focus, Focus::Viewer | Focus::Overlay(OverlayKind::Find)) {
        return None;
    }
    let v = s.ext.get::<ViewState>()?;
    let id = v.active_viewer()?;
    (v.viewer(id)?.placement == Placement::Window).then_some(id)
}

fn doc(s: &AppState, id: DocId) -> Option<&ViewerDoc> {
    s.ext.get::<ViewState>()?.viewer(id)
}

/// Где документ: экран вьювера в окне или область панели (быстрый просмотр, dock).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Spot {
    Window,
    Panel(Side),
}

/// Документ места `spot`.
pub(super) fn doc_at(s: &AppState, spot: Spot) -> Option<DocId> {
    match spot {
        Spot::Window => shown(s),
        Spot::Panel(side) => s.ext.get::<ViewState>()?.quick[side.idx()],
    }
}

/// Документ с фокусом: экран в окне или область панели (Tab).
pub(super) fn focused(s: &AppState) -> Option<(DocId, Spot)> {
    if let Some(id) = shown(s) {
        return Some((id, Spot::Window));
    }
    quick::focused(s).map(|(side, id)| (id, Spot::Panel(side)))
}

/// Текст и Hex места `spot` и его документ: `f(view, doc)` (обработчики мыши).
fn with_text<R>(
    h: &mut CoreHost,
    spot: Spot,
    f: impl FnOnce(&mut rows::TextView, &ViewerDoc) -> R,
) -> Option<R> {
    let core = h.core.as_ref()?;
    let s = core.state();
    let d = doc(s, doc_at(s, spot)?)?;
    let v = h.features.get_mut::<Viewer>()?;
    Some(f(v.text_at(spot), d))
}

/// Тело области панели `side` (быстрый просмотр, вьювер `viewer.dock`) — хосту окна.
pub(in crate::gpui_shell) fn panel_body(
    features: &mut super::features::Features,
    f: &Frame<'_>,
    side: Side,
    panel_w: f32,
    window: &mut Window,
    cx: &Context<CoreHost>,
) -> Option<AnyElement> {
    features
        .get_mut::<Viewer>()?
        .panel_body(f, side, panel_w, window, cx)
}

/// Область панели показывает документ (быстрый просмотр, `viewer.dock`).
pub(in crate::gpui_shell) fn panel_shows(s: &AppState, side: Side) -> bool {
    quick::shows(s, side)
}

/// Сцена `--screens` готова к снимку: документ открыт, окно известно handler'у, видимые
/// страницы на месте, индекс (для номеров) и поиск закончены.
pub(super) fn settled(s: &AppState) -> bool {
    let Some(d) = shown(s).and_then(|id| doc(s, id)) else {
        return quick_settled(s);
    };
    // Документ Markdown: разобран целиком, картинки из файлов загружены.
    let md_ready = !d.md_mode()
        || (d.md.as_ref().is_some_and(|m| !m.parsing)
            && d.md_doc().is_some_and(|m| m.complete)
            && !doc::images_pending());
    d.is_ready()
        && d.view_rows > 0
        && !d.realign
        && d.reveal.is_none()
        && d.pages.inflight() == 0
        && (d.mode == Mode::Hex || d.index.done)
        && md_ready
        && d.search
            .as_ref()
            .is_none_or(|x| !x.running && (x.count.done || x.hit.is_none()))
}

/// Быстрый просмотр готов к снимку: документ цели открыт, окно известно, страницы на
/// месте, индекс (у файла целиком в памяти) и разбор Markdown закончены.
fn quick_settled(s: &AppState) -> bool {
    let Some(v) = s.ext.get::<ViewState>() else {
        return true;
    };
    [Side::Left, Side::Right].into_iter().all(|side| {
        if !quick::shows(s, side) {
            return true;
        }
        let urfin_view::QuickTarget::File { loc: target, .. } = urfin_view::quick_target(s, side)
        else {
            return true;
        };
        if v.qnote[side.idx()]
            .as_ref()
            .is_some_and(|n| n.loc == target)
        {
            return true;
        }
        let Some(d) = v.in_panel(side).filter(|d| d.loc == target || !d.quick) else {
            return false;
        };
        let md_ready = !d.md_mode()
            || (d.md.as_ref().is_some_and(|m| !m.parsing)
                && d.md_doc().is_some_and(|m| m.complete)
                && !doc::images_pending());
        d.is_ready()
            && d.view_rows > 0
            && !d.realign
            && d.pages.inflight() == 0
            && (d.kind != Some(urfin_view::SourceKind::Owned) || d.index.done)
            && md_ready
    })
}

/// Меню от шапки: «мешочек» кодировок (Shift+F8, клик по чипу) или режимов (Shift+F4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuKind {
    Enc,
    Mode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Menu {
    pub kind: MenuKind,
    pub doc: DocId,
    /// Выбранный пункт; `None` — встать на текущий (открытие, F8 по кругу).
    pub sel: Option<usize>,
    /// Первый видимый пункт списка.
    pub first: usize,
}

/// Фича окна: вьювер на месте панелей.
#[derive(Default)]
pub(super) struct Viewer {
    /// Размер моно-шрифта после Ctrl+колеса; `None` — `[viewer] font_size` или `type.mono`.
    font: Option<f32>,
    /// Последний отправленный `Viewport`.
    sent: Option<(DocId, u32, u32)>,
    pub(super) menu: Option<Menu>,
    /// Строки, мышь и бегунок экрана в окне (S7-07, S7-11).
    text: rows::TextView,
    /// Дробные строки колеса (тачпад).
    wheel: (f32, f32),
    /// Строка «Найти» / «Перейти» (S7-10).
    bar: Option<FindBar>,
    /// Markdown-документ (S7-22a).
    md: doc::DocView,
    /// Области панелей: быстрый просмотр и `viewer.dock` (S7-13).
    quick: [quick::Slot; 2],
    /// Список экранов (F12) или история (Alt+F11), S7-14.
    list: Option<lists::List>,
}

/// Флажки строки по образцу прошлого поиска.
fn flags_of(p: &Pattern) -> Flags {
    match p {
        Pattern::Text { case, word, .. } => Flags {
            case: *case,
            word: *word,
            ..Flags::default()
        },
        Pattern::Regex { case, .. } => Flags {
            case: *case,
            regex: true,
            ..Flags::default()
        },
        Pattern::Hex(_) => Flags {
            hex: true,
            ..Flags::default()
        },
    }
}

/// Образец, который соберёт handler из `viewer.search` с этими флажками (`None` — hex не
/// разобран: ошибку покажет handler).
fn pattern_of(text: &str, f: Flags) -> Option<Pattern> {
    Some(if f.hex {
        Pattern::Hex(urfin_view::search::hex::parse(text).ok()?)
    } else if f.regex {
        Pattern::Regex {
            s: text.to_owned(),
            case: f.case,
        }
    } else {
        Pattern::Text {
            s: text.to_owned(),
            case: f.case,
            word: f.word,
        }
    })
}

/// `viewer.search` по строке.
fn search_cmd(text: &str, f: Flags, back: bool) -> Command {
    let kind = if f.hex {
        "hex"
    } else if f.regex {
        "regex"
    } else {
        "text"
    };
    let mut c = Command::new("viewer.search")
        .arg("text", text)
        .arg("kind", kind)
        .arg("case", f.case);
    if kind == "text" {
        c = c.arg("word", f.word);
    }
    if back {
        c = c.arg("back", true);
    }
    c
}

fn again(back: bool) -> Command {
    Command::new(if back {
        "viewer.search_prev"
    } else {
        "viewer.search_next"
    })
}

/// «N из M» строки поиска; `true` — ошибка или «не найдено» (`critical`).
fn counter(d: &ViewerDoc) -> (String, bool) {
    let i18n = urfin_i18n::current();
    let Some(s) = &d.search else {
        return (String::new(), false);
    };
    let mut a = FluentArgs::new();
    if let Some(e) = &s.error {
        return match (e, &s.req.pat) {
            (urfin_view::SearchError::Empty, _) => (String::new(), false),
            (_, Pattern::Hex(_)) => (i18n.get("ui-find-bad-hex"), true),
            _ => {
                a.set("error", e.to_string());
                (i18n.fmt("ui-find-bad-regex", &a), true)
            }
        };
    }
    if s.running {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "проценты"
        )]
        a.set("pct", (s.progress() * 100.0).round() as u64);
        return (i18n.fmt("ui-view-searching", &a), false);
    }
    if s.not_found {
        return (i18n.get("ui-find-none"), true);
    }
    if s.hit.is_none() || !s.count.done {
        return (String::new(), false);
    }
    match s.ordinal() {
        Some(n) => {
            a.set("n", n as u64);
            a.set("total", s.count.n as u64);
            let key = if s.count.more {
                "ui-find-of-more"
            } else {
                "ui-find-of"
            };
            (i18n.fmt(key, &a), false)
        }
        None => (String::new(), false),
    }
}

/// Подсказка строки перехода: «из N · строка», ожидание индекса или ошибка разбора.
fn goto_hint(d: &ViewerDoc, text: &str) -> (String, bool) {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    if d.goto_wait.is_some() {
        a.set("pct", rows::pct(d.index.upto, d.len));
        return (i18n.fmt("ui-view-wait-index", &a), false);
    }
    if !text.trim().is_empty() && urfin_view::goto::parse(text).is_err() {
        return (i18n.get("ui-goto-invalid"), true);
    }
    if d.mode == Mode::Hex {
        a.set("total", format!("0x{:X}", d.len));
        return (i18n.fmt("ui-goto-of-offset", &a), false);
    }
    let total = if d.index.done {
        rows::total_lines(d).to_string()
    } else {
        format!("≈{}", d.index.line_of(d.len).get() + 1)
    };
    a.set("total", total);
    (i18n.fmt("ui-goto-of-line", &a), false)
}

/// Все места документа: окно и области панелей.
const SPOTS: [Spot; 3] = [
    Spot::Window,
    Spot::Panel(Side::Left),
    Spot::Panel(Side::Right),
];

impl Viewer {
    /// Ближайший шаг автопрокрутки выделения из всех мест.
    fn auto_wait(&self, now: Instant) -> Option<Duration> {
        [&self.text, &self.quick[0].text, &self.quick[1].text]
            .into_iter()
            .filter_map(|t| t.auto_wait(now))
            .min()
    }

    /// Текст и Hex места.
    fn text_at(&mut self, spot: Spot) -> &mut rows::TextView {
        match spot {
            Spot::Window => &mut self.text,
            Spot::Panel(side) => &mut self.quick[side.idx()].text,
        }
    }

    /// Документ Markdown места.
    fn md_at(&mut self, spot: Spot) -> &mut doc::DocView {
        match spot {
            Spot::Window => &mut self.md,
            Spot::Panel(side) => &mut self.quick[side.idx()].md,
        }
    }

    /// Открытая строка «Найти» / «Перейти» документа с фокусом (экран или область).
    fn bar_of(&self, s: &AppState) -> Option<&FindBar> {
        let (id, _) = focused(s)?;
        self.bar
            .as_ref()
            .filter(|b| b.owner == id.0 && s.focus == Focus::Overlay(OverlayKind::Find))
    }

    /// Высота строки над статусом (окно текста меньше на неё).
    fn bar_h(&self, s: &AppState) -> f32 {
        if self.bar_of(s).is_some() {
            find::H_BAR
        } else {
            0.0
        }
    }

    /// Строка: открыть (или оставить открытой) в нужном режиме.
    fn open_bar(&mut self, mode: BarMode, s: &AppState, id: DocId) {
        if self
            .bar
            .as_ref()
            .is_some_and(|b| b.owner == id.0 && b.mode == mode)
        {
            return;
        }
        self.menu = None;
        let d = doc(s, id);
        let last = d.and_then(|d| d.search.as_ref()).map(|x| &x.req.pat);
        let flags = last.map_or_else(
            || Flags {
                hex: d.is_some_and(|d| d.mode == Mode::Hex),
                ..Flags::default()
            },
            flags_of,
        );
        let init = match mode {
            BarMode::Find => last.map(rows::pattern_text).unwrap_or_default(),
            BarMode::Goto => String::new(),
        };
        let flags = self.bar.as_ref().map_or(flags, |b| b.flags);
        self.bar = Some(FindBar::new(mode, id.0, init, flags));
    }

    /// Действие строки → команды `viewer.*`.
    fn bar_action(&mut self, s: &AppState, act: BarAction) -> Vec<Command> {
        let Some(d) = focused(s).and_then(|(id, _)| doc(s, id)) else {
            return Vec::new();
        };
        let Some(bar) = self.bar.as_mut() else {
            return Vec::new();
        };
        match act {
            BarAction::Close => vec![Command::new("viewer.search_cancel")],
            BarAction::Swallow => Vec::new(),
            BarAction::Toggle(f) => {
                bar.flags.toggle(f);
                Vec::new()
            }
            BarAction::Again { back } => vec![again(back)],
            BarAction::Switch(mode) => {
                let init = match mode {
                    BarMode::Find => d
                        .search
                        .as_ref()
                        .map(|x| rows::pattern_text(&x.req.pat))
                        .unwrap_or_default(),
                    BarMode::Goto => String::new(),
                };
                bar.switch(mode, init);
                // Ожидание индекса переходом не нужно строке поиска.
                if mode == BarMode::Find && d.goto_wait.is_some() {
                    return vec![
                        Command::new("viewer.search_cancel"),
                        Command::new("viewer.search"),
                    ];
                }
                Vec::new()
            }
            BarAction::Commit { back } => {
                let text = bar.text();
                match bar.mode {
                    BarMode::Find => {
                        if text.is_empty() {
                            return Vec::new();
                        }
                        let same = d.search.as_ref().is_some_and(|x| {
                            x.error.is_none()
                                && pattern_of(&text, bar.flags).as_ref() == Some(&x.req.pat)
                        });
                        if same {
                            vec![again(back)]
                        } else {
                            vec![search_cmd(&text, bar.flags, back)]
                        }
                    }
                    BarMode::Goto => {
                        if urfin_view::goto::parse(&text).is_err() {
                            return Vec::new();
                        }
                        vec![Command::new("viewer.goto").arg("to", text.trim())]
                    }
                }
            }
        }
    }

    fn font_px(&self, s: &AppState, theme: &Theme) -> f32 {
        self.font.unwrap_or_else(|| {
            let cfg = s.ext.get::<ViewState>().map_or(0, |v| v.viewer.font_size);
            if cfg > 0 {
                f32::from(cfg)
            } else {
                theme.m(Metric::TypeMono)
            }
        })
    }

    /// Колесо над строками: 3 строки на щелчок, Shift — по горизонтали; Ctrl — размер
    /// моно-шрифта вьювера (S7 §3.7).
    fn wheel(
        &mut self,
        ev: &gpui_kit::ScrollWheelEvent,
        s: &AppState,
        theme: &Theme,
    ) -> Option<Command> {
        let font = self.font_px(s, theme);
        let row_h = rows::row_h(font);
        if ev.modifiers.control {
            let dy = match ev.delta {
                gpui_kit::ScrollDelta::Lines(p) => p.y,
                gpui_kit::ScrollDelta::Pixels(p) => f32::from(p.y),
            };
            if dy.abs() < f32::EPSILON {
                return None;
            }
            self.font = Some((font + dy.signum()).clamp(8.0, 32.0));
            return None;
        }
        let (dir, n) = self.wheel_steps(ev, row_h)?;
        Some(Command::new("viewer.scroll").arg("dir", dir).arg("n", n))
    }

    /// Колесо области панели (без Ctrl): прокрутка её документа.
    pub(super) fn wheel_scroll(
        &mut self,
        ev: &gpui_kit::ScrollWheelEvent,
        row_h: f32,
    ) -> Option<urfin_view::Scroll> {
        if ev.modifiers.control {
            return None;
        }
        let (dir, n) = self.wheel_steps(ev, row_h)?;
        urfin_view::Scroll::parse(dir, Some(n))
    }

    /// Колесо → направление и шаги: 3 строки на щелчок, Shift — по горизонтали (4 колонки).
    fn wheel_steps(
        &mut self,
        ev: &gpui_kit::ScrollWheelEvent,
        row_h: f32,
    ) -> Option<(&'static str, i64)> {
        let (dx, dy) = match ev.delta {
            gpui_kit::ScrollDelta::Lines(p) => (p.x * 3.0, p.y * 3.0),
            gpui_kit::ScrollDelta::Pixels(p) => (f32::from(p.x) / row_h, f32::from(p.y) / row_h),
        };
        let (dx, dy) = if ev.modifiers.shift {
            (dy, 0.0)
        } else {
            (dx, dy)
        };
        self.wheel.0 += dx;
        self.wheel.1 += dy;
        let take = |w: &mut f32| {
            let whole = w.trunc();
            *w -= whole;
            whole
        };
        let (x, y) = (take(&mut self.wheel.0), take(&mut self.wheel.1));
        #[allow(clippy::cast_possible_truncation, reason = "строк за событие немного")]
        let (x, y) = (x as i64, y as i64);
        // `+` — вверх (как у терминала).
        match (x, y) {
            (_, y) if y > 0 => Some(("up", y)),
            (_, y) if y < 0 => Some(("down", -y)),
            (x, _) if x > 0 => Some(("left", x * 4)),
            (x, _) if x < 0 => Some(("right", -x * 4)),
            _ => None,
        }
    }

    /// Открытый список экранов или истории забирает клавиши (S7-14). F12 и Alt+F11 в
    /// фокусе вьювера открывают их здесь: в пресетах keymap они пока только в контексте
    /// панелей (строки `[context.viewer]` — полоса keymap).
    fn list_key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        if self.list.as_ref().is_some_and(|l| l.stale(s)) {
            self.list = None;
        }
        if let Some(l) = self.list.as_mut() {
            return Some(match l.key(s, ks) {
                lists::Key::Close(cmds) => {
                    self.list = None;
                    cmds
                }
                lists::Key::Stay => Vec::new(),
            });
        }
        let m = &ks.modifiers;
        if s.focus != Focus::Viewer || m.control || m.shift || m.platform {
            return None;
        }
        match (ks.key.as_str(), m.alt) {
            ("f12", false) => Some(vec![Command::new("ui.screens")]),
            ("f11", true) => Some(vec![Command::new("viewer.history")]),
            _ => None,
        }
    }

    /// Клавиши открытого меню шапки; остальное — keymap (контекст `viewer`).
    fn menu_key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        let m = self.menu?;
        // Экран в окне или быстрый просмотр в фокусе (S7-13).
        let Some(d) = focused(s)
            .map(|(id, _)| id)
            .filter(|id| *id == m.doc)
            .and_then(|id| doc(s, id))
        else {
            self.menu = None;
            return None;
        };
        let items = head::items(m.kind, d);
        let n = items.len();
        let mut sel = m.sel.unwrap_or_else(|| head::current(m.kind, d, &items));
        let step = |sel: usize, down: bool| -> usize {
            let mut k = sel;
            for _ in 0..n {
                k = if down { (k + 1) % n } else { (k + n - 1) % n };
                if items[k].enabled {
                    return k;
                }
            }
            sel
        };
        match ks.key.as_str() {
            "up" => sel = step(sel, false),
            "down" => sel = step(sel, true),
            "home" => sel = 0,
            "end" => sel = n.saturating_sub(1),
            "pageup" => sel = sel.saturating_sub(head::VISIBLE),
            "pagedown" => sel = (sel + head::VISIBLE).min(n.saturating_sub(1)),
            "enter" => {
                self.menu = None;
                return Some(
                    items
                        .get(sel)
                        .filter(|i| i.enabled)
                        .map(|i| i.cmd.clone())
                        .into_iter()
                        .collect(),
                );
            }
            "escape" => {
                self.menu = None;
                return Some(Vec::new());
            }
            "f8" if m.kind == MenuKind::Enc => {
                // По кругу (`codepage_ring`): выбор встаёт на новую кодировку.
                self.menu = Some(Menu { sel: None, ..m });
                return Some(vec![Command::new("viewer.codepage")]);
            }
            _ => return Some(Vec::new()),
        }
        self.menu = Some(Menu {
            sel: Some(sel),
            first: head::keep_visible(m.first, sel, n),
            ..m
        });
        Some(Vec::new())
    }
}

impl ShellFeature for Viewer {
    /// `viewer.codepage_menu` — «мешочек» кодировок, `viewer.mode` без аргумента — меню
    /// режимов (S7-08: handler просит меню событием `UiEvent::Ext`).
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        s: &AppState,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let kind = match id {
            // S7-11: выделение декодировано handler'ом — в буфер обмена; у документа
            // Markdown текст выделения — в UI (проекция).
            "viewer.copy" | "viewer.select_all" => {
                let id_doc = urfin_view::handler::ext_doc(args)?;
                if let Some(text) = args.str("text") {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_owned()));
                    return Some(Vec::new());
                }
                let (fid, spot) = focused(s).filter(|(f, _)| *f == id_doc)?;
                let d = doc(s, fid)?;
                let md = self.md_at(spot);
                if id == "viewer.copy" {
                    md.copy(d);
                    if let Some(t) = md.clip.take() {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(t));
                    }
                } else {
                    md.select_all(d);
                }
                return Some(Vec::new());
            }
            // S7-14: список экранов (F12) и история (Alt+F11, пусто — тост у handler'а).
            "ui.screens" | "viewer.history" => {
                let kind = if id == "ui.screens" {
                    lists::ListKind::Screens
                } else {
                    lists::ListKind::History(match args.str("kind") {
                        Some("view") => Some(urfin_view::history::HistKind::View),
                        Some("edit") => Some(urfin_view::history::HistKind::Edit),
                        _ => None,
                    })
                };
                self.menu = None;
                self.list = Some(lists::List::new(kind, s));
                return Some(Vec::new());
            }
            "viewer.codepage_menu" => MenuKind::Enc,
            "viewer.mode" => MenuKind::Mode,
            "viewer.search" | "viewer.goto" => {
                let doc = urfin_view::handler::ext_doc(args)?;
                let mode = if id == "viewer.goto" {
                    BarMode::Goto
                } else {
                    BarMode::Find
                };
                self.open_bar(mode, s, doc);
                return Some(Vec::new());
            }
            _ => return None,
        };
        let doc = urfin_view::handler::ext_doc(args)?;
        self.menu = Some(Menu {
            kind,
            doc,
            sel: None,
            first: 0,
        });
        Some(Vec::new())
    }

    fn key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        if let Some(c) = self.list_key(s, ks) {
            return Some(c);
        }
        if let Some(c) = self.menu_key(s, ks) {
            return Some(c);
        }
        if self.bar_of(s).is_none() {
            // Быстрый просмотр: Tab, Esc, F3 (S7 §4).
            if let Some(c) = Self::quick_key(s, ks) {
                return Some(c);
            }
            // Документ: Ctrl+C — видимый текст выделения, Ctrl+A — весь (S7 §3.10).
            let (id, spot) = focused(s)?;
            let d = doc(s, id).filter(|d| d.md_mode())?;
            return self.md_at(spot).key(d, ks).then(Vec::new);
        }
        let act = self.bar_of(s)?.key(ks)?;
        Some(self.bar_action(s, act))
    }

    fn editing(&self, s: &AppState) -> bool {
        self.bar_of(s).is_some_and(FindBar::has_field)
    }

    /// Поле строки: создать под открытую строку; строка закрылась — фокус окну.
    fn input(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        if self.bar_of(s).is_some() {
            if let Some(b) = self.bar.as_mut() {
                b.sync(window, cx);
            }
        } else if let Some(b) = self.bar.take()
            && b.has_field()
        {
            window.focus(focus, cx);
        }
    }

    /// Пока идёт поиск — перерисовка 30 раз в секунду (прогресс, S7 §3.6); быстрый
    /// просмотр ждёт паузы курсора (50 мс); выделение тянут за край — шаг автопрокрутки.
    fn tick(&mut self, s: &AppState, now: Instant) -> Option<Duration> {
        let quick = self.quick_tick(s, now);
        let search = focused(s)
            .and_then(|(id, _)| doc(s, id))
            .and_then(|d| d.search.as_ref())
            .is_some_and(|x| x.running)
            .then_some(Duration::from_millis(33));
        [quick, search, self.auto_wait(now)]
            .into_iter()
            .flatten()
            .min()
    }

    /// Курсор простоял 50 мс — `viewer.quick {side, path}` (S7 §4); шаг автопрокрутки
    /// выделения — событием в очередь handler'а (S7 §3.7).
    fn due(&mut self, s: &AppState, now: Instant) -> Vec<Command> {
        for spot in SPOTS {
            if let Some(d) = doc_at(s, spot).and_then(|id| doc(s, id)) {
                self.text_at(spot).autoscroll(d, now);
            }
        }
        self.quick_due(s, now)
    }

    fn tabs(&self, s: &AppState, theme: &Theme, click: Click<'_>) -> Option<Div> {
        head::tabs(s, theme, click)
    }

    /// Окно текста в колонках и строках — handler'у (`InboxMsg::Viewport`): при открытии,
    /// ресайзе, смене режима и шрифта.
    fn inbox(&mut self, s: &AppState, theme: &Theme, window: &mut Window) -> bool {
        let Some(v) = s.ext.get::<ViewState>() else {
            return false;
        };
        let mut out = self.quick_inbox(s, v, window);
        if !self.text.out.is_empty() {
            let tx = v.inbox.sender();
            for m in self.text.out.drain(..) {
                tx.send(m);
            }
            out = true;
        }
        let Some(id) = shown(s) else {
            self.sent = None;
            return out;
        };
        let Some(d) = doc(s, id) else {
            return out;
        };
        let md = (d.md_mode() && self.md.inbox(d, v)) || out;
        let g = rows::geo(d, theme, self.font_px(s, theme), self.bar_h(s), window);
        let now = (id, g.cols, g.rows);
        if self.sent == Some(now) {
            return md;
        }
        self.sent = Some(now);
        v.inbox.sender().send(InboxMsg::Viewport {
            doc: id,
            cols: g.cols,
            rows: g.rows,
        });
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "экран: шапка, тело, строка, статус, мышь"
    )]
    fn main(
        &mut self,
        f: &Frame<'_>,
        _hidden: bool,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<Main> {
        if self.list.as_ref().is_some_and(|l| l.stale(f.s)) {
            self.list = None;
        }
        let Some((id, d)) = shown(f.s).and_then(|id| Some((id, doc(f.s, id)?))) else {
            if quick::focused(f.s).is_none() {
                self.menu = None;
            }
            self.text.clear();
            self.md.clear();
            // Список экранов или истории над панелями (F12, Alt+F11 из панели).
            let win_w = f32::from(window.viewport_size().width);
            return self.list.as_ref().map(|l| {
                Main::Cover(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .child(l.element(f.s, f.theme, win_w, cx))
                        .into_any_element(),
                )
            });
        };
        if self.menu.is_some_and(|m| m.doc != id) {
            self.menu = None;
        }
        let narrow = f32::from(window.viewport_size().width) < super::chrome::NARROW_W;
        let menu = self.menu;
        let measure = head::Measure::new(window);
        let head_el = head::header(d, f.theme, f.click, &measure);
        let bar = self.bar_of(f.s);
        let keys = match (bar, &self.list) {
            (_, Some(l)) => lists::keys(l.kind),
            (Some(b), None) => find::keys(b.mode),
            (None, None) => head::keys(d, menu.map(|m| m.kind)),
        };
        let bar_el = bar.map(|b| bar_row(b, d, f, cx));
        let (body, zones) = if d.md_mode() {
            // Markdown-документ (S7-22a): список блоков, своя полоса и статус.
            self.text.clear();
            let win_w = f32::from(window.viewport_size().width);
            let body = self
                .md
                .body(f.s, d, f.theme, Spot::Window, win_w, window, cx);
            let mut zones = self.md.zones(d);
            if menu.map(|m| m.kind) == Some(MenuKind::Enc) {
                zones[1] = urfin_i18n::current().get("ui-view-enc-rereads");
            }
            (body, zones)
        } else {
            self.md.clear();
            let font = self.font_px(f.s, f.theme);
            let g = rows::geo(d, f.theme, font, self.bar_h(f.s), window);
            let sel = self.text.sel(d);
            let screen = self
                .text
                .rows
                .build(d, sel.as_ref(), &g, f.theme, &self.text.hit, window);
            let zones = rows::zones(d, &screen, menu.map(|m| m.kind));
            let body = rows::body(screen, &g, d, f.theme, &self.text, Spot::Window, cx)
                .on_scroll_wheel(cx.listener(|h, ev: &gpui_kit::ScrollWheelEvent, w, cx| {
                    let Some(core) = h.core.as_ref() else {
                        return;
                    };
                    let theme = &h.theme;
                    let s = core.state();
                    let Some(v) = h.features.get_mut::<Viewer>() else {
                        return;
                    };
                    let before = v.font;
                    let cmd = v.wheel(ev, s, theme);
                    if v.font != before {
                        h.sync_inbox(w);
                        cx.notify();
                    }
                    if let Some(c) = cmd {
                        h.run(c, w, cx);
                    }
                }));
            (body, zones)
        };
        let mut root = div()
            .id("viewer")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(f.theme.c(crate::tokens::Role::LayerPanel))
            .child(head_el)
            .child(body)
            .children(bar_el)
            .child(super::chrome::status(&zones, narrow, f.theme))
            .child(super::chrome::keybar(
                &[],
                crate::keybar::Mods::None,
                Some(&keys),
                narrow,
                f.theme,
                &f.click,
            ))
            // Выделение и бегунок тянутся и за пределами строк (S7 §3.7).
            .on_mouse_move(cx.listener(|h, ev: &gpui_kit::MouseMoveEvent, w, cx| {
                if with_text(h, Spot::Window, |tv, d| tv.moved(d, ev)).unwrap_or(false) {
                    h.sync_inbox(w);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|h, _: &gpui_kit::MouseUpEvent, _, _| {
                    let _ = with_text(h, Spot::Window, |tv, _| tv.up());
                }),
            )
            // Кнопка мыши «Назад» — `viewer.back` (S7 §3.7).
            .on_mouse_down(
                gpui_kit::MouseButton::Navigate(gpui_kit::NavigationDirection::Back),
                cx.listener(|h, _: &gpui_kit::MouseDownEvent, w, cx| {
                    h.run(Command::new("viewer.back"), w, cx);
                }),
            );
        if let Some(m) = menu {
            root = root.children(head::menu(m, d, f.theme, &measure, cx));
        }
        if let Some(l) = &self.list {
            let win_w = f32::from(window.viewport_size().width);
            root = root.child(l.element(f.s, f.theme, win_w, cx));
        }
        // Поиск идёт: полоса 2 px `accent.fill` под шапкой (S7 §3.6).
        if let Some(x) = d.search.as_ref().filter(|x| x.running) {
            root = root.child(
                div()
                    .absolute()
                    .top(px(rows::H_HEAD))
                    .left_0()
                    .h(px(2.0))
                    .w(gpui_kit::relative(x.progress()))
                    .bg(f.theme.c(crate::tokens::Role::AccentFill)),
            );
        }
        Some(Main::Replace(root.into_any_element()))
    }
}

/// Строка «Найти» или «Перейти» документа `d`.
fn bar_row(b: &FindBar, d: &ViewerDoc, f: &Frame<'_>, cx: &Context<CoreHost>) -> Div {
    match b.mode {
        BarMode::Find => {
            let toggle = |fl: Flag| -> super::overlays::Handler {
                Box::new(cx.listener(move |h, _: &ClickEvent, _, cx| {
                    if let Some(b) = h.features.get_mut::<Viewer>().and_then(|v| v.bar.as_mut()) {
                        b.flags.toggle(fl);
                    }
                    cx.notify();
                }))
            };
            b.find_row(f.theme, counter(d), &toggle)
        }
        BarMode::Goto => {
            let i18n = urfin_i18n::current();
            let label = i18n.get(if d.mode == Mode::Hex {
                "ui-goto-offset"
            } else {
                "ui-goto-line"
            });
            let go: super::overlays::Handler = Box::new(cx.listener(|h, _: &ClickEvent, w, cx| {
                let cmds = match (h.core.as_ref(), h.features.get_mut::<Viewer>()) {
                    (Some(core), Some(v)) => {
                        v.bar_action(core.state(), BarAction::Commit { back: false })
                    }
                    _ => Vec::new(),
                };
                for c in cmds {
                    h.run(c, w, cx);
                }
            }));
            let cancel = (f.click)(Command::new("viewer.search_cancel"));
            b.goto_row(f.theme, label, goto_hint(d, &b.text()), go, cancel)
        }
    }
}

/// Подпись кодировки (имя WHATWG) для шапки и «мешочка»: «UTF-16 LE», «CP866»,
/// «Windows-1251».
pub(super) fn enc_label(name: &str) -> SharedString {
    SharedString::from(match name {
        "UTF-16LE" => "UTF-16 LE".to_owned(),
        "UTF-16BE" => "UTF-16 BE".to_owned(),
        "IBM866" => "CP866".to_owned(),
        "x-mac-cyrillic" => "Mac Cyrillic".to_owned(),
        "macintosh" => "Mac Roman".to_owned(),
        n if n.starts_with("windows-") => format!("Windows-{}", &n["windows-".len()..]),
        n => n.to_owned(),
    })
}

/// Сцены вьювера (S7-07, S7-10, S7-22a): текст с метками поиска, строки «Найти» и
/// «Перейти», «мешочек» кодировок, Hex с курсором, Markdown-документ. Файлы — в
/// демо-дереве (`Projects\urfin`); затем вьювер закрывается.
pub(super) const SCENES: &[Scene] = &[
    // `.md` открывается в «Разметке» — документом; на макете `screen-viewer` — «Текст».
    Scene {
        name: "viewer-changelog-doc",
        steps: &[
            "ui.cancel",
            // Ширина макетов: 32 байта в строке Hex — от 150 колонок.
            "resize 1280x760",
            "panel.focus side=left",
            "viewer.open path=\"Projects\\urfin\\CHANGELOG.md\"",
        ],
    },
    Scene {
        name: "screen-viewer",
        steps: &["viewer.mode mode=text", "viewer.search text=Исправлено"],
    },
    // F7: строка «Найти» с прошлым образцом, «N из M», все совпадения на экране.
    Scene {
        name: "viewer-find",
        steps: &["viewer.search"],
    },
    // Alt+F8: строка «Перейти к строке».
    Scene {
        name: "viewer-goto",
        steps: &["viewer.goto"],
    },
    Scene {
        name: "screen-viewerenc",
        steps: &["viewer.search_cancel", "viewer.codepage_menu"],
    },
    Scene {
        name: "screen-viewerhex",
        steps: &[
            "viewer.close",
            "viewer.open path=\"Projects\\urfin\\urfin.exe\"",
            "viewer.search text=\"54\" kind=hex",
        ],
    },
    // S7-11: выделение в Hex — байты в обеих колонках (совпадение поиска = выделение).
    Scene {
        name: "viewer-select-hex",
        steps: &["viewer.search text=\"4D 5A 90 00\" kind=hex"],
    },
    // S7-11: выделение в Тексте на несколько строк.
    Scene {
        name: "viewer-select",
        steps: &[
            "viewer.close",
            "viewer.open path=\"Projects\\urfin\\CHANGELOG.md\"",
            "viewer.mode mode=text",
            "viewer.search text=\"(?s)Исправлено.{0,90}\" kind=regex",
        ],
    },
    // Markdown-документ (S7-22a): README с логотипом из файла и значком из сети.
    Scene {
        name: "screen-viewer-md",
        steps: &[
            "viewer.close",
            "viewer.open path=\"Projects\\urfin\\README.md\"",
        ],
    },
    // S7-14: два экрана (README и CHANGELOG) — список F12: панели и экраны, текущий ✓.
    Scene {
        name: "viewer-screens",
        steps: &[
            "viewer.open path=\"Projects\\urfin\\CHANGELOG.md\"",
            "viewer.mode mode=text",
            "ui.screens",
        ],
    },
    // Alt+F11: закрытые файлы сцен выше (CHANGELOG, urfin.exe) — новые сверху.
    Scene {
        name: "viewer-history",
        steps: &["viewer.close", "viewer.history"],
    },
    Scene {
        name: "viewer-closed",
        steps: &["viewer.close", "resize 1200x760"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_labels() {
        assert_eq!(enc_label("UTF-16LE").as_ref(), "UTF-16 LE");
        assert_eq!(enc_label("windows-1251").as_ref(), "Windows-1251");
        assert_eq!(enc_label("IBM866").as_ref(), "CP866");
        assert_eq!(enc_label("KOI8-R").as_ref(), "KOI8-R");
    }
}
