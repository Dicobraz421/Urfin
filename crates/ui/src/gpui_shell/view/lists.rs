//! Список экранов (F12, `ui.screens`, S7 §3.9) и история просмотра и правки (Alt+F11,
//! `viewer.history`) — «мешочки» фичи `Viewer` от своих кнопок (решение 2026-11-01
//! «anchored-menus»): экраны — от кнопки «Экраны» в заголовке окна (список 500 вниз,
//! правый угол прямой, строки 32, `screen-screens.png`), история — от кнопки с часами
//! первой слева в шапке вьювера или редактора (список 600, левый угол прямой, поле
//! «Найти файл» сверху, строки 28, `screen-viewerhistory.png`). Клавиша и клик открывают
//! одну фигуру у одной кнопки; на панелях Alt+F11 — раздел «Файлы» мешочка «История»
//! у пути (`gpui_shell::menu`).
//!
//! Фокус Core список не трогает (клавиши забирает `Viewer::key`): Esc возвращает туда,
//! где он открыт. Фокус или набор экранов сменились — список закрыт.

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClickEvent, Context, Div, Keystroke, SharedString, div, px};
use urfin_core::{AppState, Command, Focus, Location};
use urfin_view::history::HistKind;
use urfin_view::{DocId, ViewState};

use super::super::CoreHost;
use super::super::menu::{hl_text, search_box};
use super::super::overlays::Click;
use super::super::pouch::{H_ITEM, Notch, place, pouch};
use super::super::theme::Theme;
use super::head::Measure;
use super::rows::H_HEAD;
use super::{Viewer, shown};
use crate::tokens::{Metric, Role};

const ICON_FONT: &str = "Segoe Fluent Icons";
/// Ширина списка.
const W_SCREENS: f32 = 500.0;
const W_HISTORY: f32 = 600.0;
/// Строка экранов (`screen-screens.png`).
const H_SCREEN: f32 = 32.0;
/// Строк на виду (остальные — прокруткой).
pub(super) const VISIBLE: usize = 12;
/// Колонка имени.
const W_NAME: f32 = 150.0;

/// Кнопка «Экраны» (DESIGN «Метрики окна»): поля 0 10, значок 16, число, шеврон 8.
const SCR_PAD: f32 = 10.0;
const SCR_ICON: f32 = 16.0;
const SCR_GAP: f32 = 6.0;
const CHEVRON: f32 = 8.0;
/// Зазор кнопки «Экраны» до метки контекста и кнопок окна.
const SCR_MR: f32 = 8.0;
/// Кнопка «История» шапки: 26, поля 0 6 0 7, часы 14 + шеврон 8, рамка.
const HIST_H: f32 = 26.0;
const HIST_W: f32 = 7.0 + 14.0 + SCR_GAP + CHEVRON + 6.0 + 2.0;
/// Левый край кнопки «История» в шапке (поле шапки).
pub(super) const HIST_X: f32 = 12.0;

/// Значки Segoe Fluent Icons.
const GLYPH_PANELS: &str = "\u{E8A1}";
/// «Окна стопкой» (Segoe Fluent Icons: Restore).
const GLYPH_SCREENS: &str = "\u{E923}";
/// Просмотр — глаз (`screen-viewerhistory.png`).
const GLYPH_VIEW: &str = "\u{E890}";
const GLYPH_EDIT: &str = "\u{E70F}";
const GLYPH_HISTORY: &str = "\u{E81C}";

/// Какой список.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListKind {
    /// Экраны и панели (F12).
    Screens,
    /// История: просмотры, правки или всё (Alt+F11, `viewer.history {kind?}`).
    History(Option<HistKind>),
}

/// Открытый список.
#[derive(Debug)]
pub(super) struct List {
    pub kind: ListKind,
    /// Выбранная строка; `None` — встать на текущую (экраны) или первую.
    sel: Option<usize>,
    /// Первая видимая строка.
    first: usize,
    /// Набранное для поиска (история).
    query: String,
    /// Где открыт: фокус и экраны. Сменились — список закрыт.
    at: (Focus, Vec<DocId>),
}

/// Строка списка.
#[derive(Clone, Debug)]
pub(super) struct Row {
    icon: &'static str,
    /// Цифра быстрого выбора (экраны: 0 — панели, 1–9 — экраны).
    digit: Option<u8>,
    name: String,
    detail: String,
    /// История: позиция «стр N» / «конец» перед временем.
    pos: String,
    /// Справа `secondary`: вид экрана или время записи.
    note: String,
    /// Там сейчас фокус (✓, акцент у имени).
    current: bool,
    /// Enter, клик.
    run: Vec<Command>,
    /// История: F3 — просмотр, F4 — правка.
    view: Option<Command>,
    edit: Option<Command>,
}

/// Что сделала клавиша.
#[derive(Debug, PartialEq)]
pub(super) enum Key {
    /// Список закрыт; команды — Core (бывает пусто).
    Close(Vec<Command>),
    /// Выбор или поиск сменились.
    Stay,
}

/// Путь для показа: локальный — путь ОС, иначе uri.
fn path_text(l: &Location) -> String {
    if l.is_local() {
        l.path.display().to_string()
    } else {
        l.to_uri()
    }
}

/// Имя файла и его папка.
fn name_dir(l: &Location) -> (String, String) {
    let name = l
        .file_name()
        .map_or_else(|| path_text(l), |n| n.to_string_lossy().into_owned());
    let dir = l.parent().map(|p| path_text(&p)).unwrap_or_default();
    (name, dir)
}

/// Экран с фокусом: вьювер в окне или редактор; `None` — панели (и быстрый просмотр).
fn current_screen(s: &AppState) -> Option<DocId> {
    let v = s.ext.get::<ViewState>()?;
    if shown(s).is_some() || s.focus == Focus::Editor {
        v.active()
    } else {
        None
    }
}

/// Время записи истории (секунды Unix) по шаблону даты окна.
pub(in crate::gpui_shell) fn when(theme: &Theme, ts: u64) -> String {
    if ts == 0 {
        return String::new();
    }
    let mut cell = crate::fmt_cell::Cell::new();
    let ns = i64::try_from(ts)
        .ok()
        .and_then(|t| t.checked_mul(1_000_000_000))
        .unwrap_or(i64::MIN);
    theme.fmt.write_datetime(&mut cell, ns);
    cell.as_str().to_owned()
}

/// Строки экранов: панели, затем экраны от верхнего (активного) вниз.
fn screen_rows(s: &AppState) -> Vec<Row> {
    let i18n = urfin_i18n::current();
    let cur = current_screen(s);
    let mut out = vec![Row {
        icon: GLYPH_PANELS,
        digit: Some(0),
        name: i18n.get("ui-screens-panels"),
        // Пути обеих панелей, активная первой (`screen-screens.png`).
        detail: format!(
            "{} · {}",
            path_text(&s.active_tab().loc),
            path_text(&s.panel(s.active.other()).tab().loc)
        ),
        pos: String::new(),
        note: String::new(),
        current: cur.is_none(),
        run: vec![Command::new("panel.focus").arg("side", crate::side_arg(s.active))],
        view: None,
        edit: None,
    }];
    let Some(v) = s.ext.get::<ViewState>() else {
        return out;
    };
    let list: Vec<_> = v.screen_list().collect();
    for (k, (id, loc, viewer)) in list.into_iter().rev().enumerate() {
        let (name, detail) = name_dir(loc);
        out.push(Row {
            icon: if viewer { GLYPH_VIEW } else { GLYPH_EDIT },
            digit: u8::try_from(k + 1).ok().filter(|d| *d <= 9),
            name,
            detail,
            pos: String::new(),
            note: i18n.get(if viewer {
                "ui-screens-view"
            } else {
                "ui-screens-edit"
            }),
            current: cur == Some(id),
            run: vec![Command::new("viewer.screen").arg("doc", i64::from(id.0))],
            view: None,
            edit: None,
        });
    }
    out
}

/// Строки истории: новые сверху, по виду; набранное — в имени или папке.
fn history_rows(
    s: &AppState,
    kind: Option<HistKind>,
    query: &str,
    theme: Option<&Theme>,
) -> Vec<Row> {
    let Some(v) = s.ext.get::<ViewState>() else {
        return Vec::new();
    };
    let q = query.to_lowercase();
    v.history
        .iter()
        .rev()
        .filter(|m| kind.is_none_or(|k| m.kind == k))
        .filter_map(|m| {
            let (name, detail) = name_dir(&m.loc);
            if !q.is_empty()
                && !name.to_lowercase().contains(&q)
                && !detail.to_lowercase().contains(&q)
            {
                return None;
            }
            let view = Command::new("viewer.open")
                .arg("path", m.loc.clone())
                .arg("internal", true);
            let edit = Command::new("editor.open")
                .arg("path", m.loc.clone())
                .arg("internal", true);
            let edit_kind = m.kind == HistKind::Edit;
            Some(Row {
                icon: if edit_kind { GLYPH_EDIT } else { GLYPH_VIEW },
                digit: None,
                name,
                detail,
                pos: crate::gpui_shell::menu::memo_pos(m, &urfin_i18n::current()),
                note: theme.map(|t| when(t, m.ts)).unwrap_or_default(),
                current: false,
                run: vec![if edit_kind {
                    edit.clone()
                } else {
                    view.clone()
                }],
                view: Some(view),
                edit: Some(edit),
            })
        })
        .collect()
}

/// Первая видимая строка, чтобы выбранная была на виду.
fn keep_visible(first: usize, sel: usize, n: usize) -> usize {
    let max_first = n.saturating_sub(VISIBLE);
    if sel < first {
        sel
    } else if sel >= first + VISIBLE {
        sel + 1 - VISIBLE
    } else {
        first
    }
    .min(max_first)
}

impl List {
    pub(super) fn new(kind: ListKind, s: &AppState) -> List {
        let screens = s
            .ext
            .get::<ViewState>()
            .map(|v| v.screens.clone())
            .unwrap_or_default();
        List {
            kind,
            sel: None,
            first: 0,
            query: String::new(),
            at: (s.focus, screens),
        }
    }

    /// Фокус или экраны сменились с открытия — список больше не о том.
    pub(super) fn stale(&self, s: &AppState) -> bool {
        let screens = s.ext.get::<ViewState>().map_or(&[][..], |v| &v.screens[..]);
        s.focus != self.at.0 || screens != &self.at.1[..]
    }

    fn rows(&self, s: &AppState, theme: Option<&Theme>) -> Vec<Row> {
        match self.kind {
            ListKind::Screens => screen_rows(s),
            ListKind::History(k) => history_rows(s, k, &self.query, theme),
        }
    }

    /// Выбранная строка: заданная или текущая (экраны), первая (история).
    fn selected(&self, rows: &[Row]) -> usize {
        let n = rows.len();
        self.sel
            .unwrap_or_else(|| rows.iter().position(|r| r.current).unwrap_or(0))
            .min(n.saturating_sub(1))
    }

    /// Клавиша при открытом списке. Чужие клавиши список забирает (как меню).
    pub(super) fn key(&mut self, s: &AppState, ks: &Keystroke) -> Key {
        let rows = self.rows(s, None);
        let n = rows.len();
        let sel = self.selected(&rows);
        let history = matches!(self.kind, ListKind::History(_));
        let m = &ks.modifiers;
        let mut next = sel;
        match ks.key.as_str() {
            "escape" | "f10" => return Key::Close(Vec::new()),
            "f12" if !history => return Key::Close(Vec::new()),
            "f11" if history && m.alt => return Key::Close(Vec::new()),
            "enter" => {
                return Key::Close(rows.get(sel).map(|r| r.run.clone()).unwrap_or_default());
            }
            "f3" | "f4" if history => {
                let r = rows.get(sel);
                let c = if ks.key == "f3" {
                    r.and_then(|r| r.view.clone())
                } else {
                    r.and_then(|r| r.edit.clone())
                };
                return Key::Close(c.into_iter().collect());
            }
            "up" if n > 0 => next = (sel + n - 1) % n,
            "down" if n > 0 => next = (sel + 1) % n,
            "home" => next = 0,
            "end" => next = n.saturating_sub(1),
            "pageup" => next = sel.saturating_sub(VISIBLE),
            "pagedown" => next = (sel + VISIBLE).min(n.saturating_sub(1)),
            "backspace" if history => {
                self.query.pop();
                next = 0;
            }
            k if !history && !m.control && !m.alt => {
                // Цифра — экран с этим номером.
                if let Some(r) = k
                    .parse::<u8>()
                    .ok()
                    .and_then(|d| rows.iter().find(|r| r.digit == Some(d)))
                {
                    return Key::Close(r.run.clone());
                }
            }
            k if history && !m.control && !m.alt => {
                let typed = ks
                    .key_char
                    .as_deref()
                    .filter(|c| !c.chars().any(char::is_control))
                    .map(str::to_owned)
                    .or_else(|| (k.chars().count() == 1).then(|| k.to_owned()));
                if let Some(c) = typed {
                    self.query.push_str(&c);
                    next = 0;
                }
            }
            _ => {}
        }
        self.sel = Some(next);
        self.first = keep_visible(self.first, next, n);
        Key::Stay
    }

    /// Колесо: первая видимая строка.
    fn wheel(&mut self, rows: usize, dy: f32) {
        #[allow(clippy::cast_possible_truncation, reason = "строк — сотни")]
        let k = (dy / H_ITEM).round() as isize;
        let max = rows.saturating_sub(VISIBLE).cast_signed();
        self.first = (self.first.cast_signed() - k).clamp(0, max).cast_unsigned();
    }

    /// Над строками истории — поле «Найти файл» (`screen-viewerhistory.png`); у экранов
    /// заголовка нет.
    fn head(&self, theme: &Theme) -> Div {
        let body = div().flex().flex_col().relative();
        if matches!(self.kind, ListKind::History(_)) {
            let i18n = urfin_i18n::current();
            body.child(search_box(
                &self.query,
                &i18n.get("ui-view-history-find"),
                theme,
            ))
        } else {
            body
        }
    }

    /// Якорь списка: левый край и верх фигуры (от верха области под заголовком окна),
    /// ширина и корешок — кнопка-источник по её раскладке.
    fn anchor(&self, tab: f32, win_w: f32, count_w: f32, title_right: f32) -> Anchor {
        match self.kind {
            ListKind::Screens => {
                let (ax, aw) = screens_at(win_w, count_w, title_right);
                let w = W_SCREENS.min(win_w);
                // Список вниз, правый угол прямой: корешок у правого края списка.
                let (left, x) = place(ax, aw, w, win_w, w - aw);
                Anchor {
                    left,
                    // Кнопка — по нижнему краю заголовка окна, как табы.
                    top: -tab,
                    w,
                    notch: (x, aw, tab + 1.0),
                }
            }
            ListKind::History(_) => {
                let w = W_HISTORY.min(win_w);
                let (left, x) = place(HIST_X, HIST_W, w, win_w, 0.0);
                let top = (H_HEAD - HIST_H) / 2.0;
                Anchor {
                    left,
                    top,
                    w,
                    notch: (x, HIST_W, H_HEAD - top),
                }
            }
        }
    }

    /// «Мешочек» от кнопки и щит под ним (клик мимо закрывает) — поверх области под
    /// заголовком окна; `title_right` — занятое справа в заголовке (метка, кнопки окна).
    pub(super) fn element(
        &self,
        s: &AppState,
        theme: &Theme,
        me: &Measure,
        title_right: f32,
        cx: &Context<CoreHost>,
    ) -> AnyElement {
        let i18n = urfin_i18n::current();
        let rows = self.rows(s, Some(theme));
        let n = rows.len();
        let sel = self.selected(&rows);
        let first = keep_visible(self.first, sel, n);
        let row_h = match self.kind {
            ListKind::Screens => H_SCREEN,
            ListKind::History(_) => H_ITEM,
        };
        let body = self.head(theme);
        let mut list = div().flex().flex_col().relative();
        for (i, r) in rows.iter().enumerate().skip(first).take(VISIBLE) {
            list = list.child(row_view(i, r, i == sel, row_h, &self.query, theme, cx));
        }
        if rows.is_empty() {
            list = list.child(
                div()
                    .h(px(H_ITEM))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(i18n.get("ui-places-empty"))),
            );
        }
        if n > VISIBLE {
            // Позиция списка: тонкий бегунок справа.
            #[allow(clippy::cast_precision_loss, reason = "строк — сотни")]
            let (t, h) = (first as f32 / n as f32, VISIBLE as f32 / n as f32);
            list = list.child(
                div()
                    .absolute()
                    .right(px(-2.0))
                    .top(gpui_kit::relative(t))
                    .h(gpui_kit::relative(h))
                    .w(px(3.0))
                    .rounded(px(2.0))
                    .bg(theme.c(Role::TextTertiary)),
            );
        }
        let count_w = me.w(&screen_count(s).to_string(), 13.0);
        let a = self.anchor(theme.m(Metric::HTab), me.win_w, count_w, title_right);
        let content = match self.kind {
            ListKind::Screens => screens_inner(screen_count(s), theme),
            ListKind::History(_) => history_inner(theme),
        };
        let notch = Notch {
            x: a.notch.0,
            w: a.notch.1,
            h: a.notch.2,
            content: content.into_any_element(),
        };
        let fig = pouch(theme, a.left, a.top, a.w, Some(notch), body.child(list))
            .id("viewer-list")
            .occlude()
            .on_scroll_wheel(
                cx.listener(move |h, ev: &gpui_kit::ScrollWheelEvent, _, cx| {
                    let dy = f32::from(ev.delta.pixel_delta(px(H_ITEM)).y);
                    if let Some(l) = h.features.get_mut::<Viewer>().and_then(|v| v.list.as_mut()) {
                        l.first = first;
                        l.wheel(n, dy);
                        cx.notify();
                    }
                }),
            );
        div()
            .id("viewer-list-shield")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_click(cx.listener(|h, _: &ClickEvent, _, cx| {
                if let Some(v) = h.features.get_mut::<Viewer>() {
                    v.list = None;
                }
                cx.notify();
            }))
            .child(fig)
            .into_any_element()
    }
}

/// Положение «мешочка»: левый край, верх, ширина; корешок — смещение, ширина, высота.
#[derive(Debug, PartialEq)]
struct Anchor {
    left: f32,
    top: f32,
    w: f32,
    notch: (f32, f32, f32),
}

/// Экранов (вьювер и редактор) открыто.
pub(in crate::gpui_shell) fn screen_count(s: &AppState) -> usize {
    s.ext
        .get::<ViewState>()
        .map_or(0, |v| v.screen_list().count())
}

/// Ширина кнопки «Экраны» с числом шириной `count_w`.
fn screens_w(count_w: f32) -> f32 {
    2.0 * SCR_PAD + SCR_ICON + SCR_GAP + count_w + SCR_GAP + CHEVRON
}

/// Кнопка «Экраны» в заголовке окна: левый край и ширина. Правый край — перед меткой
/// контекста и кнопками окна (`title_right`), с зазором [`SCR_MR`].
fn screens_at(win_w: f32, count_w: f32, title_right: f32) -> (f32, f32) {
    let w = screens_w(count_w);
    (win_w - title_right - SCR_MR - w, w)
}

/// Содержимое кнопки «Экраны»: «окна стопкой», число экранов, шеврон.
fn screens_inner(n: usize, theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(SCR_GAP))
        .text_size(px(13.0))
        .text_color(theme.c(Role::TextPrimary))
        .child(
            div()
                .w(px(SCR_ICON))
                .font_family(ICON_FONT)
                .text_size(px(14.0))
                .child(SharedString::new_static(GLYPH_SCREENS)),
        )
        .child(SharedString::from(n.to_string()))
        .child(chevron(theme))
}

fn chevron(theme: &Theme) -> Div {
    div()
        .w(px(CHEVRON))
        .font_family(ICON_FONT)
        .text_size(px(CHEVRON))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(crate::icons::CHEVRON_DOWN.to_string()))
}

/// Кнопка «Экраны» в заголовке окна (DESIGN «Метрики окна»): видна, пока открыт хоть
/// один экран; клик — `ui.screens` (F12), подсказка — название команды и клавиша.
pub(super) fn screens_button(
    s: &AppState,
    theme: &Theme,
    count_w: f32,
    click: Click<'_>,
) -> Option<Div> {
    let n = screen_count(s);
    if n == 0 {
        return None;
    }
    let i18n = urfin_i18n::current();
    let el = div()
        .id("title-screens")
        .w(px(screens_w(count_w)))
        .h(px(theme.m(Metric::HTab)))
        .flex()
        .items_center()
        .justify_center()
        .rounded_t(px(7.0))
        .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
        .child(screens_inner(n, theme))
        .on_click(click(Command::new("ui.screens")));
    let el = super::toolbar::tip_lines(
        el,
        theme,
        vec![(
            i18n.get("cmd-ui-screens"),
            super::toolbar::key_of(s, "ui.screens"),
        )],
    );
    Some(div().flex_none().self_end().mr(px(SCR_MR)).child(el))
}

/// Содержимое кнопки «История»: часы 14 и шеврон.
fn history_inner(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(SCR_GAP))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(14.0))
                .text_color(theme.c(Role::TextPrimary))
                .child(SharedString::new_static(GLYPH_HISTORY)),
        )
        .child(chevron(theme))
}

/// Кнопка «История» (Alt+F11) первой слева в шапке вьювера и редактора: фон `panel`,
/// рамка, скругление 5; клик — `viewer.history`.
pub(super) fn history_button(s: &AppState, theme: &Theme, click: Click<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let el = div()
        .id("head-history")
        .w(px(HIST_W))
        .h(px(HIST_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::LayerPanel))
        .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
        .child(history_inner(theme))
        .on_click(click(Command::new("viewer.history")));
    div().flex_none().child(super::toolbar::tip_lines(
        el,
        theme,
        vec![(
            i18n.get("cmd-viewer-history"),
            super::toolbar::key_of(s, "viewer.history"),
        )],
    ))
}

/// Строка `h`: цифра, значок, имя 150, папка `secondary`, справа вид или время.
fn row_view(
    i: usize,
    r: &Row,
    selected: bool,
    h: f32,
    query: &str,
    theme: &Theme,
    cx: &Context<CoreHost>,
) -> AnyElement {
    let secondary = theme.c(Role::TextSecondary);
    let run = r.run.clone();
    div()
        .id(SharedString::from(format!("viewer-list-{i}")))
        .h(px(h))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .rounded(px(4.0))
        .text_size(px(13.0))
        .when(selected, |d| d.bg(theme.c(Role::FillCursor)))
        .when(!selected, |d| {
            d.hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
        })
        .children(r.digit.map(|k| {
            div()
                .w(px(10.0))
                .flex_none()
                .text_color(secondary)
                .child(SharedString::from(k.to_string()))
        }))
        .child(
            div()
                .w(px(16.0))
                .flex_none()
                .font_family(ICON_FONT)
                .text_size(px(12.0))
                .text_color(theme.c(if r.current {
                    Role::AccentText
                } else {
                    Role::TextSecondary
                }))
                .child(SharedString::new_static(r.icon)),
        )
        .child(
            hl_text(&r.name, query, theme)
                .w(px(W_NAME))
                .flex_none()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .when(r.current, |d| d.text_color(theme.c(Role::AccentText))),
        )
        .child(
            hl_text(&r.detail, query, theme)
                .flex_1()
                .min_w_0()
                .text_ellipsis()
                .text_size(px(12.0))
                .text_color(secondary),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(secondary)
                .child(SharedString::from(r.pos.clone())),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(secondary)
                .child(SharedString::from(r.note.clone())),
        )
        .child(
            div()
                .w(px(14.0))
                .flex_none()
                .font_family(ICON_FONT)
                .text_size(px(10.0))
                .children(
                    r.current
                        .then(|| SharedString::from(crate::icons::ACCEPT.to_string())),
                ),
        )
        .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
            if let Some(v) = h.features.get_mut::<Viewer>() {
                v.list = None;
            }
            for c in run.clone() {
                h.run(c, w, cx);
            }
            cx.notify();
        }))
        .into_any_element()
}

/// Клавиши keybar при открытом списке (во вьювере).
pub(super) fn keys(kind: ListKind) -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    match kind {
        ListKind::Screens => vec![
            k("↑ ↓", "ui-keys-select"),
            k("0–9", "ui-keys-choose"),
            k("Enter", "ui-keys-go"),
            k("Esc", "ui-keys-close"),
        ],
        ListKind::History(_) => vec![
            k("↑ ↓", "ui-keys-select"),
            k("Enter", "ui-keys-go"),
            k("F3", "keybar-viewer-open"),
            k("F4", "keybar-editor-open"),
            k("Esc", "ui-keys-close"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use urfin_core::{Panel, Tab, TabId};
    use urfin_view::history::ViewMemo;
    use urfin_view::{EditorCfg, ViewerCfg};

    use super::*;

    fn ks(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    /// Панели в `C:\w`, история: `a.txt` (просмотр), затем `b.txt` (правка).
    fn state() -> AppState {
        let loc = Location::local(r"C:\w");
        let mut st = AppState::new(
            Panel::new(Tab::new(TabId(1), loc.clone())),
            Panel::new(Tab::new(TabId(2), loc)),
        );
        st.focus = Focus::Panel;
        let mut v = ViewState::new(ViewerCfg::default(), EditorCfg::default());
        v.history
            .record(ViewMemo::start(Location::local(r"C:\w\a.txt")));
        let mut b = ViewMemo::start(Location::local(r"C:\docs\b.txt"));
        b.kind = HistKind::Edit;
        v.history.record(b);
        st.ext.insert(v);
        st
    }

    fn ids(cmds: &[Command]) -> Vec<&str> {
        cmds.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn selection_stays_visible() {
        assert_eq!(keep_visible(0, 3, 40), 0);
        assert_eq!(keep_visible(0, 12, 40), 1);
        assert_eq!(keep_visible(10, 2, 40), 2);
        assert_eq!(keep_visible(0, 39, 40), 28);
        assert_eq!(keep_visible(5, 2, 5), 0);
    }

    /// Без экранов — только «Панели», они текущие; Enter и 0 — к панели, Esc — закрыть.
    #[test]
    fn screens_without_documents() {
        let s = state();
        let rows = screen_rows(&s);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].current);
        assert_eq!(rows[0].detail, r"C:\w · C:\w");
        let mut l = List::new(ListKind::Screens, &s);
        assert_eq!(l.key(&s, &ks("down")), Key::Stay);
        let Key::Close(c) = l.key(&s, &ks("0")) else {
            panic!("0 — экран с номером");
        };
        assert_eq!(ids(&c), ["panel.focus"]);
        assert_eq!(l.key(&s, &ks("escape")), Key::Close(Vec::new()));
        assert_eq!(l.key(&s, &ks("f12")), Key::Close(Vec::new()));
    }

    /// История: новые сверху, вид фильтрует, Enter — по виду записи, F3 / F4 — явно.
    #[test]
    fn history_rows_and_keys() {
        let s = state();
        let all = history_rows(&s, None, "", None);
        assert_eq!(
            all.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["b.txt", "a.txt"]
        );
        assert_eq!(all[0].detail, r"C:\docs");
        assert_eq!(history_rows(&s, Some(HistKind::View), "", None).len(), 1);
        let mut l = List::new(ListKind::History(None), &s);
        let Key::Close(c) = l.key(&s, &ks("enter")) else {
            panic!("Enter открывает");
        };
        assert_eq!(ids(&c), ["editor.open"]);
        let mut l = List::new(ListKind::History(None), &s);
        assert_eq!(l.key(&s, &ks("down")), Key::Stay);
        let Key::Close(c) = l.key(&s, &ks("f3")) else {
            panic!("F3 — просмотр");
        };
        assert_eq!(ids(&c), ["viewer.open"]);
        assert_eq!(c[0].args.loc("path"), Some(Location::local(r"C:\w\a.txt")));
    }

    /// Набор ищет по имени и папке, Backspace стирает.
    #[test]
    fn history_search() {
        let s = state();
        let mut l = List::new(ListKind::History(None), &s);
        assert_eq!(l.key(&s, &ks("d")), Key::Stay);
        assert_eq!(l.key(&s, &ks("o")), Key::Stay);
        let rows = l.rows(&s, None);
        assert_eq!(rows.len(), 1, "«do» — только папка docs");
        assert_eq!(rows[0].name, "b.txt");
        assert_eq!(l.key(&s, &ks("backspace")), Key::Stay);
        assert_eq!(l.key(&s, &ks("backspace")), Key::Stay);
        assert_eq!(l.rows(&s, None).len(), 2);
    }

    /// F12 и клик по кнопке — одна команда `ui.screens`: один `ListKind` и один якорь —
    /// кнопка «Экраны» перед кнопками окна; корешок у правого края списка (прямой угол),
    /// от верха заголовка окна до тела списка.
    #[test]
    fn screens_anchor_is_title_button() {
        let s = state();
        let l = List::new(ListKind::Screens, &s);
        let a = l.anchor(32.0, 1280.0, 8.0, 138.0);
        let (bx, bw) = screens_at(1280.0, 8.0, 138.0);
        assert_eq!(bx + bw, 1280.0 - 138.0 - SCR_MR);
        assert_eq!(a.left + a.w, bx + bw, "правый край списка — край кнопки");
        assert_eq!(a.notch, (W_SCREENS - bw, bw, 33.0));
        assert_eq!(a.top, -32.0);
        // Без экранов строка одна — «Панели».
        assert_eq!(l.rows(&s, None).len(), 1);
    }

    /// История — от кнопки с часами в шапке: левый угол прямой, корешок до низа шапки.
    #[test]
    fn history_anchor_is_head_button() {
        let s = state();
        let l = List::new(ListKind::History(None), &s);
        let a = l.anchor(32.0, 1280.0, 8.0, 138.0);
        assert_eq!((a.left, a.w), (HIST_X, W_HISTORY));
        assert_eq!(a.notch.0, 0.0);
        assert_eq!(a.top + a.notch.2, H_HEAD);
    }

    /// Фокус ушёл или экраны сменились — список не о том.
    #[test]
    fn stale_after_focus_change() {
        let mut s = state();
        let l = List::new(ListKind::Screens, &s);
        assert!(!l.stale(&s));
        s.focus = Focus::Cmdline;
        assert!(l.stale(&s));
    }
}
