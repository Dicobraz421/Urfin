//! Меню на GPUI (S2 §2.6, D-05, `screen-menu.png` и др.): строка меню F9 накрывает
//! пути панелей, открытый пункт и его список — «мешочек» ([`super::pouch`]). Тем же
//! «мешочком» — сортировка (Ctrl+F12, корешок — заголовок колонки), закладки (Ctrl+D) и
//! история папок (Alt+F12, корешок — путь панели); контекстное меню (Shift+F10) — плашка
//! у строки курсора; наборы колонок (S2-19) — от переключателя в строке пути. Пункты
//! F9 — `menu.toml`, подписи — Fluent, подсказки клавиш — [`KeyHints`]. Клавиши
//! разбирает [`Menu::key`]: ↑↓, Enter, Shift+Enter, Esc, ←/→ (F9), буква пункта (F9),
//! Пробел (переключатель), набор — поиск (закладки, история).

use gpui_kit::prelude::*;
use gpui_kit::{Div, Keystroke, SharedString, div, px};
use urfin_core::{AppState, Command, CommandRegistry, Location, Side, SortKey, ViewMode};

use super::chrome::side_name;
use super::overlays::{Click, ClickAll};
use super::pouch::{H_ITEM, Notch, place, pouch};
use super::theme::Theme;
use crate::keybar::KeyHints;
use crate::menu::{Item, MenuDef, label, letter};
use crate::tokens::{Metric, Role};

const W_BAR_LIST: f32 = 330.0;
const W_SORT: f32 = 290.0;
const W_PLACES: f32 = 420.0;
const W_CONTEXT: f32 = 300.0;
/// Список F9 левее своего пункта — корешок не у самого края.
const BAR_SHIFT: f32 = 40.0;
const ICONS: &str = "Segoe Fluent Icons";

/// Что открыто: меню F9 или «мешочек» с якорем.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Bar,
    Sort(Side),
    Bookmarks(Side),
    History(Side),
    Context(Side),
    /// Скрытые табы панели (№67).
    Tabs(Side),
    /// Наборы колонок `[ui.view_modes]` (S2-19), корешок — переключатель набора.
    Columns(Side),
}

impl Kind {
    pub(super) fn of(id: &str, active: Side) -> Kind {
        let side = |s: &str| {
            if s == "right" {
                Side::Right
            } else {
                Side::Left
            }
        };
        match id {
            "bookmarks" => Kind::Bookmarks(active),
            "history" => Kind::History(active),
            "context" => Kind::Context(active),
            s if s.starts_with("sort-") => Kind::Sort(side(&s[5..])),
            s if s.starts_with("tabs-") => Kind::Tabs(side(&s[5..])),
            s if s.starts_with("columns-") => Kind::Columns(side(&s[8..])),
            _ => Kind::Bar,
        }
    }

    /// Поиск по набору (закладки, история).
    fn searchable(self) -> bool {
        matches!(self, Kind::Bookmarks(_) | Kind::History(_))
    }
}

/// Знак слева от подписи.
#[derive(Clone, Debug, PartialEq)]
enum Mark {
    None,
    /// ✓ у текущего варианта (сортировка).
    Check(bool),
    /// Номер закладки.
    Digit(u8),
}

/// Строка списка: подпись, справа деталь и клавиша; Enter — `run`, Shift+Enter — `alt`.
#[derive(Clone, Debug)]
struct Row {
    mark: Mark,
    label: String,
    /// Путь или пояснение `secondary`; `true` — акцентом («текущая папка»).
    detail: Option<(String, bool)>,
    hint: String,
    run: Vec<Command>,
    alt: Vec<Command>,
    /// Разделитель перед строкой.
    sep: bool,
    /// Переключатель: его состояние; меню после него не закрывается.
    toggle: Option<bool>,
}

impl Row {
    fn new(label: String, run: Vec<Command>) -> Row {
        Row {
            mark: Mark::None,
            label,
            detail: None,
            hint: String::new(),
            run,
            alt: Vec::new(),
            sep: false,
            toggle: None,
        }
    }
}

/// Геометрия окна для якорей: ширина и высота окна, верх строки путей, высоты строки
/// путей, шапки колонок и строки списка, левый край и ширина панелей.
pub(super) struct Geo {
    pub win_w: f32,
    pub win_h: f32,
    pub top: f32,
    pub path_h: f32,
    pub head_h: f32,
    pub row_h: f32,
    pub panels: [Option<(f32, f32)>; 2],
    /// Где вызвано контекстное меню мышью.
    pub pointer: Option<(f32, f32)>,
}

/// Ширина текста размером `size`.
pub(super) type Measure<'a> = &'a dyn Fn(&str, f32) -> f32;

/// Меню и выбор в открытом списке.
pub(super) struct Menu {
    pub menus: Vec<MenuDef>,
    /// (id открытого меню, выбранная строка).
    sel: Option<(String, usize)>,
    /// Набранное для поиска (закладки, история).
    query: String,
    /// Пункты контекстного меню, что есть в реестре.
    context: Vec<(Command, &'static str)>,
    /// Ширина наборов табов по сторонам (последний кадр) — для скрытых табов (№67).
    pub tabs_avail: [f32; 2],
}

/// Контекстное меню (`screen-context.png`): команда и ключ подписи; разделитель — пустой
/// ключ. Пункты без команды в реестре не показываются; F3/F4 — с S7.
const CONTEXT: &[(&str, &str)] = &[
    ("panel.enter", "cmd-panel-enter"),
    ("file.open_with", "cmd-file-open_with"),
    ("", ""),
    ("ui.copy_names full=true", "menu-copy-path"),
    ("ui.copy_names full=false", "menu-copy-name"),
    ("", ""),
    ("file.reveal", "cmd-file-reveal"),
    ("", ""),
    ("ops.delete", "cmd-ops-delete"),
];

/// Сортировка (`screen-sort.png`): ключ `panel.sort` и подпись.
const SORT: [(&str, SortKey, &str); 5] = [
    ("name", SortKey::Name, "ui-sort-name"),
    ("ext", SortKey::Ext, "ui-sort-ext"),
    ("size", SortKey::Size, "ui-sort-size"),
    ("mtime", SortKey::Mtime, "ui-sort-mtime"),
    ("unsorted", SortKey::Unsorted, "ui-sort-unsorted"),
];

impl Menu {
    pub(super) fn new(registry: &CommandRegistry, user: Option<&str>) -> Menu {
        let parsed = user
            .and_then(|t| crate::menu::parse(t, registry).ok())
            .or_else(|| crate::menu::parse(crate::menu::MENU_TOML, registry).ok());
        let context = CONTEXT
            .iter()
            .filter_map(|(text, key)| {
                if text.is_empty() {
                    return Some((Command::new("ui.cancel"), ""));
                }
                let c = Command::parse(text).ok()?;
                registry.validate(&c).ok()?;
                Some((c, *key))
            })
            .collect();
        Menu {
            menus: parsed.map(|(m, _)| m).unwrap_or_default(),
            sel: None,
            query: String::new(),
            context,
            tabs_avail: [600.0; 2],
        }
    }

    fn def(&self, open: &str) -> Option<&MenuDef> {
        self.menus.iter().find(|m| m.id == open)
    }

    /// Выбранная строка открытого меню (новое меню — первая строка, поиск сброшен).
    fn selected(&mut self, open: &str, n: usize) -> usize {
        if self.sel.as_ref().is_none_or(|(id, _)| id != open) {
            self.sel = Some((open.to_owned(), 0));
            self.query.clear();
        }
        self.sel
            .as_ref()
            .map_or(0, |(_, i)| *i)
            .min(n.saturating_sub(1))
    }

    /// Строки открытого меню по состоянию.
    #[allow(clippy::too_many_lines, reason = "строки каждого вида подряд")]
    fn rows(&self, open: &str, s: &AppState, hints: &dyn KeyHints) -> Vec<Row> {
        let i18n = urfin_i18n::current();
        let close = || Command::new("ui.cancel");
        let focus = |side: Side| Command::new("panel.focus").arg("side", side_name(side));
        match Kind::of(open, s.active) {
            Kind::Bar => {
                let Some(def) = self.def(open) else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                let mut sep = false;
                for it in &def.items {
                    match it {
                        Item::Sep => sep = true,
                        Item::Cmd { cmd, label: l } => {
                            let mut run = vec![close()];
                            if let Some(side) = def.side {
                                run.push(focus(side));
                            }
                            run.push((**cmd).clone());
                            let mut r = Row::new(label(&i18n, l), run);
                            r.hint = hints.hint(cmd).map(|h| h.to_string()).unwrap_or_default();
                            r.sep = std::mem::take(&mut sep);
                            out.push(r);
                        }
                    }
                }
                out
            }
            Kind::Sort(side) => {
                let spec = s.panel(side).tab().sort;
                let mut out: Vec<Row> = SORT
                    .iter()
                    .map(|(key, k, l)| {
                        let cmd = Command::new("panel.sort")
                            .arg("key", *key)
                            .arg("desc", spec.desc);
                        let mut r = Row::new(i18n.get(l), vec![close(), focus(side), cmd.clone()]);
                        r.mark = Mark::Check(spec.key == *k);
                        r.hint = hints
                            .hint(&Command::new("panel.sort").arg("key", *key))
                            .map(|h| h.to_string())
                            .unwrap_or_default();
                        r
                    })
                    .collect();
                let key = SORT
                    .iter()
                    .find(|(_, k, _)| *k == spec.key)
                    .map_or("name", |(key, _, _)| key);
                let mut desc = Row::new(
                    i18n.get("ui-sort-desc"),
                    vec![
                        focus(side),
                        Command::new("panel.sort")
                            .arg("key", key)
                            .arg("desc", !spec.desc),
                    ],
                );
                desc.toggle = Some(spec.desc);
                desc.sep = true;
                out.push(desc);
                out
            }
            Kind::Bookmarks(side) => {
                let here = &s.panel(side).tab().loc;
                let mut out = Vec::new();
                for n in (1..10).chain(std::iter::once(0)) {
                    let Some(loc) = s.bookmarks[n].as_ref() else {
                        continue;
                    };
                    let mut r = place_row(loc, side, &i18n);
                    r.mark = Mark::Digit(u8::try_from(n).unwrap_or(0));
                    r.run = vec![
                        close(),
                        focus(side),
                        Command::new("panel.bookmark.go").arg("n", i64::try_from(n).unwrap_or(0)),
                    ];
                    if loc == here {
                        r.detail = Some((i18n.get("ui-places-current"), true));
                    }
                    out.push(r);
                }
                self.filtered(out)
            }
            Kind::History(side) => {
                let here = &s.panel(side).tab().loc;
                let out = s
                    .visited
                    .iter()
                    .filter(|l| *l != here)
                    .map(|l| place_row(l, side, &i18n))
                    .collect();
                self.filtered(out)
            }
            Kind::Tabs(side) => {
                let p = s.panel(side);
                let avail = self.tabs_avail[usize::from(side == Side::Right)];
                let titles = super::chrome::tab_titles(s, side);
                super::chrome::tab_layout(s, side, avail)
                    .hidden()
                    .map(|i| {
                        let mut r = Row::new(
                            titles[i].clone(),
                            vec![
                                close(),
                                Command::new("tab.goto")
                                    .arg("side", side_name(side))
                                    .arg("n", i64::try_from(i + 1).unwrap_or(1)),
                            ],
                        );
                        r.mark = Mark::Digit(u8::try_from((i + 1) % 10).unwrap_or(0));
                        r.detail = Some((path_text(&p.tabs[i].loc), false));
                        r
                    })
                    .collect()
            }
            Kind::Columns(side) => {
                let mode = s.panel(side).tab().view_mode;
                let sets = s
                    .ext
                    .get::<crate::UiState>()
                    .map_or(&[][..], |u| &u.view_modes);
                sets.iter()
                    .enumerate()
                    .map(|(i, set)| {
                        let cmd = Command::new("ui.columns").arg("set", set.name.as_str());
                        let mut r =
                            Row::new(set.title.to_string(), vec![close(), focus(side), cmd]);
                        r.mark =
                            Mark::Check(u8::try_from(i).is_ok_and(|n| mode == ViewMode::Custom(n)));
                        r
                    })
                    .collect()
            }
            Kind::Context(_) => {
                let mut out = Vec::new();
                let mut sep = false;
                for (cmd, key) in &self.context {
                    if key.is_empty() {
                        sep = true;
                        continue;
                    }
                    let mut r = Row::new(i18n.get(key), vec![close(), cmd.clone()]);
                    r.hint = hints.hint(cmd).map(|h| h.to_string()).unwrap_or_default();
                    if cmd.id.as_str() == "panel.enter" {
                        "Enter".clone_into(&mut r.hint);
                    }
                    r.sep = std::mem::take(&mut sep);
                    out.push(r);
                }
                out
            }
        }
    }

    /// Строки, в подписи или пути которых есть набранное (без учёта регистра).
    fn filtered(&self, rows: Vec<Row>) -> Vec<Row> {
        let q = self.query.to_lowercase();
        if q.is_empty() {
            return rows;
        }
        rows.into_iter()
            .filter(|r| {
                r.label.to_lowercase().contains(&q)
                    || r.detail
                        .as_ref()
                        .is_some_and(|(d, _)| d.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// Клавиша при открытом меню: команды для Core (пусто — только смена выбора).
    pub(super) fn key(&mut self, open: &str, s: &AppState, ks: &Keystroke) -> Vec<Command> {
        let kind = Kind::of(open, s.active);
        let rows = self.rows(open, s, &crate::keybar::FarStub);
        let n = rows.len();
        let sel = self.selected(open, n);
        let mut next = sel;
        let shift = ks.modifiers.shift;
        match ks.key.as_str() {
            "escape" | "f9" => return vec![Command::new("ui.cancel")],
            "enter" if shift => {
                if let Some(r) = rows.get(sel).filter(|r| !r.alt.is_empty()) {
                    return r.alt.clone();
                }
            }
            "enter" => return rows.get(sel).map(|r| r.run.clone()).unwrap_or_default(),
            "up" if n > 0 => next = (sel + n - 1) % n,
            "down" if n > 0 => next = (sel + 1) % n,
            "home" => next = 0,
            "end" => next = n.saturating_sub(1),
            "space" if !kind.searchable() => {
                if let Some(r) = rows.iter().find(|r| r.toggle.is_some()) {
                    return r.run.clone();
                }
            }
            "left" | "right" if kind == Kind::Bar => {
                let dir = if ks.key == "left" { -1 } else { 1 };
                if let Some(id) = crate::menu::neighbor(&self.menus, open, dir) {
                    return vec![Command::new("ui.menu").arg("menu", id.as_str())];
                }
            }
            "backspace" if kind.searchable() => {
                self.query.pop();
                next = 0;
            }
            k if kind.searchable() => {
                if let Some(c) = ks
                    .key_char
                    .as_deref()
                    .filter(|c| !c.chars().any(char::is_control))
                {
                    self.query.push_str(c);
                    next = 0;
                } else if k.chars().count() == 1 && !ks.modifiers.control && !ks.modifiers.alt {
                    self.query.push_str(k);
                    next = 0;
                }
            }
            k if kind == Kind::Bar => {
                let i18n = urfin_i18n::current();
                let typed = ks.key_char.as_deref().unwrap_or(k).to_lowercase();
                if let Some(def) = self.def(open) {
                    let hit = def
                        .items
                        .iter()
                        .filter_map(|it| match it {
                            Item::Cmd { label: l, .. } => Some(l),
                            Item::Sep => None,
                        })
                        .position(|l| {
                            letter(&i18n, l)
                                .is_some_and(|c| c.to_string() == typed || c.to_string() == k)
                        });
                    if let Some(i) = hit {
                        return rows.get(i).map(|r| r.run.clone()).unwrap_or_default();
                    }
                }
            }
            _ => {}
        }
        self.sel = Some((open.to_owned(), next));
        Vec::new()
    }

    /// Строка меню F9 поверх путей панелей и левые края её пунктов (для корешка).
    pub(super) fn bar(
        &self,
        open: &str,
        theme: &Theme,
        click: Click<'_>,
        measure: Measure<'_>,
    ) -> (Div, Vec<(f32, f32)>) {
        let i18n = urfin_i18n::current();
        let h = theme.m(Metric::HBreadcrumb);
        let mut row = div()
            .absolute()
            .top(px(theme.m(Metric::HTitlebar)))
            .left_0()
            .right_0()
            .h(px(h))
            .flex()
            .items_end()
            .px(px(BAR_PAD))
            .gap(px(BAR_GAP))
            .bg(theme.c(Role::BgBase))
            .border_b_1()
            .border_color(theme.c(Role::StrokeDivider));
        let mut at = Vec::with_capacity(self.menus.len());
        let mut x = BAR_PAD;
        for m in &self.menus {
            let text = label(&i18n, &m.title);
            let w = measure(&text, BAR_TEXT) + 2.0 * BAR_ITEM_PAD;
            at.push((x, w));
            x += w + BAR_GAP;
            let on = m.id == open;
            row = row.child(
                div()
                    .id(SharedString::from(format!("menu-{}", m.id)))
                    .w(px(w))
                    .h(px(h - 4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(BAR_TEXT))
                    .rounded_t(px(theme.m(Metric::RadiusControl)))
                    .when(!on, |d| d.hover(|d| d.bg(theme.c(Role::FillSubtleHover))))
                    .child(SharedString::from(text))
                    .on_click(click(Command::new("ui.menu").arg("menu", m.id.as_str()))),
            );
        }
        (row, at)
    }

    /// Список открытого меню: «мешочек» от якоря или плашка у курсора.
    #[allow(clippy::too_many_lines, reason = "якорь и тело для каждого вида")]
    #[allow(clippy::many_single_char_names, reason = "координаты и состояние")]
    #[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
    pub(super) fn flyout(
        &mut self,
        open: &str,
        s: &AppState,
        hints: &dyn KeyHints,
        theme: &Theme,
        click: ClickAll<'_>,
        geo: &Geo,
        bar_at: &[(f32, f32)],
        measure: Measure<'_>,
    ) -> Option<Div> {
        let kind = Kind::of(open, s.active);
        let rows = self.rows(open, s, hints);
        let sel = self.selected(open, rows.len());
        let i18n = urfin_i18n::current();
        let panel = |side: Side| {
            geo.panels[usize::from(side == Side::Right)]
                .or(geo.panels[usize::from(side == Side::Left)])
                .unwrap_or((0.0, geo.win_w))
        };
        let mut body = div().flex().flex_col();
        if kind.searchable() {
            body = body.child(search_box(
                &self.query,
                &i18n.get(if matches!(kind, Kind::Bookmarks(_)) {
                    "ui-places-find-bookmark"
                } else {
                    "ui-places-find-folder"
                }),
                theme,
            ));
        }
        let detail_x = matches!(kind, Kind::Bookmarks(_) | Kind::History(_) | Kind::Tabs(_));
        for (i, r) in rows.iter().enumerate() {
            if r.sep {
                body = body.child(
                    div()
                        .mx(px(6.0))
                        .my(px(4.0))
                        .h(px(1.0))
                        .bg(theme.c(Role::StrokeDivider)),
                );
            }
            body = body.child(row_view(
                r,
                i == sel,
                detail_x,
                &self.query,
                theme,
                format!("mi-{open}-{i}"),
                click(r.run.clone()),
            ));
        }
        if rows.is_empty() {
            body = body.child(
                div()
                    .h(px(H_ITEM))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(i18n.get("ui-places-empty"))),
            );
        }
        if let Kind::Bookmarks(side) = kind
            && let Some(add) = add_bookmark(s, side)
        {
            body = body
                .child(
                    div()
                        .mx(px(6.0))
                        .my(px(4.0))
                        .h(px(1.0))
                        .bg(theme.c(Role::StrokeDivider)),
                )
                .child(
                    div().p(px(4.0)).flex().child(
                        super::strip::button(
                            theme,
                            "bookmark-add".into(),
                            i18n.get("ui-places-add"),
                            "",
                            super::strip::Btn::Normal,
                        )
                        .on_click(click(vec![add, Command::new("ui.cancel")])),
                    ),
                );
        }
        let text = theme.c(Role::TextPrimary);
        let notch_text = |t: String| {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(BAR_TEXT))
                .text_color(text)
                .child(SharedString::from(t))
        };
        Some(match kind {
            Kind::Bar => {
                let idx = self.menus.iter().position(|m| m.id == open)?;
                let (ax, aw) = *bar_at.get(idx)?;
                let (left, nx) = place(ax, aw, W_BAR_LIST, geo.win_w, BAR_SHIFT);
                let title = label(&i18n, &self.menus[idx].title);
                let nh = geo.path_h - 4.0;
                pouch(
                    theme,
                    left,
                    geo.top + 4.0,
                    W_BAR_LIST,
                    Some(Notch {
                        x: nx,
                        w: aw,
                        h: nh,
                        content: notch_text(title)
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .into_any_element(),
                    }),
                    body,
                )
            }
            Kind::Sort(side) => {
                let (px0, _) = panel(side);
                let spec = s.panel(side).tab().sort;
                let name = SORT
                    .iter()
                    .find(|(_, k, _)| *k == spec.key)
                    .map_or_else(String::new, |(_, _, l)| i18n.get(l));
                let arrow = if spec.desc { "\u{E70D}" } else { "\u{E70E}" };
                let aw = measure(&name, HEAD_TEXT) + 30.0;
                let (left, nx) = place(px0 + 4.0, aw, W_SORT, geo.win_w, 0.0);
                pouch(
                    theme,
                    left,
                    geo.top + geo.path_h + 1.0,
                    W_SORT,
                    Some(Notch {
                        x: nx,
                        w: aw,
                        h: geo.head_h - 1.0,
                        content: notch_text(name)
                            .text_size(px(HEAD_TEXT))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(
                                div()
                                    .font_family(ICONS)
                                    .text_size(px(9.0))
                                    .child(SharedString::new_static(arrow)),
                            )
                            .into_any_element(),
                    }),
                    body,
                )
            }
            Kind::Bookmarks(side) | Kind::History(side) => {
                let (px0, pw) = panel(side);
                let path = path_text(&s.panel(side).tab().loc);
                let glyph = if matches!(kind, Kind::Bookmarks(_)) {
                    "\u{E8A4}"
                } else {
                    "\u{E81C}"
                };
                let aw = (measure(&path, BAR_TEXT) + 50.0)
                    .min(pw - 24.0)
                    .min(W_PLACES);
                let (left, nx) = place(px0 + 12.0 - 6.0, aw, W_PLACES, geo.win_w, 0.0);
                pouch(
                    theme,
                    left,
                    geo.top + 4.0,
                    W_PLACES,
                    Some(Notch {
                        x: nx,
                        w: aw,
                        h: geo.path_h - 4.0,
                        content: div()
                            .w_full()
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(BAR_TEXT))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(ICONS)
                                    .text_color(theme.c(Role::AccentText))
                                    .child(SharedString::new_static(glyph)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(SharedString::from(path)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(9.0))
                                    .text_color(theme.c(Role::TextSecondary))
                                    .child(SharedString::new_static("▾")),
                            )
                            .into_any_element(),
                    }),
                    body,
                )
            }
            Kind::Tabs(side) => {
                let (px0, _) = panel(side);
                let avail = self.tabs_avail[usize::from(side == Side::Right)];
                let more = super::chrome::tab_layout(s, side, avail)
                    .more_x
                    .unwrap_or(0.0);
                let ax = px0 + super::chrome::TITLE_PAD_L + more;
                let (left, nx) = place(ax, 44.0, W_PLACES, geo.win_w, 0.0);
                let hidden = super::chrome::tab_layout(s, side, avail).hidden().count();
                pouch(
                    theme,
                    left,
                    4.0,
                    W_PLACES,
                    Some(Notch {
                        x: nx,
                        w: 44.0,
                        h: geo.top - 4.0,
                        content: notch_text(format!("› {hidden}"))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .into_any_element(),
                    }),
                    body,
                )
            }
            Kind::Columns(side) => {
                let (px0, pw) = panel(side);
                let t = s.panel(side).tab();
                let title = s
                    .ext
                    .get::<crate::UiState>()
                    .and_then(|u| crate::columns::custom(t.view_mode, &u.view_modes))
                    .map_or_else(String::new, |set| set.title.to_string());
                // Переключатель — у правого края строки пути панели.
                let aw = measure(&title, BAR_TEXT) + 26.0;
                let ax = px0 + pw - theme.m(Metric::PadRowX) - aw;
                let (left, nx) = place(ax, aw, W_SORT, geo.win_w, W_SORT - aw);
                pouch(
                    theme,
                    left,
                    geo.top + 4.0,
                    W_SORT,
                    Some(Notch {
                        x: nx,
                        w: aw,
                        h: geo.path_h - 4.0,
                        content: notch_text(title)
                            .w_full()
                            .justify_center()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(
                                div()
                                    .text_size(px(9.0))
                                    .text_color(theme.c(Role::TextSecondary))
                                    .child(SharedString::new_static("▾")),
                            )
                            .into_any_element(),
                    }),
                    body,
                )
            }
            Kind::Context(side) => {
                let (px0, pw) = panel(side);
                let t = s.panel(side).tab();
                #[allow(clippy::cast_precision_loss, reason = "номер строки на странице")]
                let row = t.cursor.row.saturating_sub(t.cursor.top) as f32;
                let list_top = geo.top + geo.path_h + geo.head_h;
                #[allow(clippy::cast_precision_loss, reason = "пунктов десяток")]
                let h = rows.len() as f32 * H_ITEM + 40.0;
                // Мышью — у указателя; Shift+F10 — под строкой курсора. Не влезает
                // вниз — вверх.
                let (x, below, above) = match geo.pointer {
                    Some((mx, my)) => (mx + 2.0, my + 2.0, my - h - 2.0),
                    None => (
                        px0 + (pw * 0.3).min(200.0),
                        list_top + (row + 1.0) * geo.row_h + 4.0,
                        list_top + row * geo.row_h - h - 4.0,
                    ),
                };
                let y = if below + h > geo.win_h {
                    above.max(geo.top)
                } else {
                    below
                };
                let x = x.min(geo.win_w - W_CONTEXT).max(0.0);
                pouch(theme, x, y, W_CONTEXT, None, body)
            }
        })
    }
}

const BAR_PAD: f32 = 8.0;
const BAR_GAP: f32 = 2.0;
const BAR_ITEM_PAD: f32 = 12.0;
const BAR_TEXT: f32 = 13.0;
const HEAD_TEXT: f32 = 11.5;

/// Путь для показа: локальный — путь ОС, иначе uri.
fn path_text(l: &Location) -> String {
    if l.is_local() {
        l.path.display().to_string()
    } else {
        l.to_uri()
    }
}

/// Строка закладки или истории: имя папки, родитель `secondary`; Enter — перейти в этой
/// панели, Shift+Enter — в другой.
fn place_row(loc: &Location, side: Side, i18n: &urfin_i18n::I18n) -> Row {
    let name = loc
        .file_name()
        .map_or_else(|| path_text(loc), |n| n.to_string_lossy().into_owned());
    let parent = loc.parent().map(|p| path_text(&p)).unwrap_or_default();
    let go = |side: Side| {
        vec![
            Command::new("ui.cancel"),
            Command::new("panel.focus").arg("side", side_name(side)),
            Command::new("panel.go")
                .arg("side", side_name(side))
                .arg("path", loc.clone()),
        ]
    };
    let mut r = Row::new(name, go(side));
    r.alt = go(side.other());
    r.detail = Some((
        if parent.is_empty() {
            i18n.get("ui-places-root")
        } else {
            parent
        },
        false,
    ));
    r
}

/// «Добавить текущую»: первая свободная закладка для папки панели, если её ещё нет.
fn add_bookmark(s: &AppState, side: Side) -> Option<Command> {
    let here = &s.panel(side).tab().loc;
    if s.bookmarks.iter().flatten().any(|l| l == here) {
        return None;
    }
    let free = (1..10)
        .chain(std::iter::once(0))
        .find(|&n| s.bookmarks[n].is_none())?;
    Some(Command::new("panel.bookmark.set").arg("n", i64::try_from(free).unwrap_or(0)))
}

/// Поле поиска сверху списка: лупа, набранное с кареткой или подсказка.
fn search_box(query: &str, placeholder: &str, theme: &Theme) -> Div {
    let empty = query.is_empty();
    div()
        .m(px(2.0))
        .mb(px(4.0))
        .h(px(30.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::AccentFill))
        .bg(theme.c(Role::LayerPanel))
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(11.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static("\u{E721}")),
        )
        .child(
            div()
                .flex()
                .items_center()
                .when(!empty, |d| d.child(SharedString::from(query.to_owned())))
                .child(div().w(px(1.0)).h(px(16.0)).bg(theme.c(Role::TextPrimary)))
                .when(empty, |d| {
                    d.child(
                        div()
                            .text_color(theme.c(Role::TextSecondary))
                            .child(SharedString::from(placeholder.to_owned())),
                    )
                }),
        )
}

/// Текст с подсветкой `highlight` первого вхождения набранного (без учёта регистра).
fn hl_text(text: &str, query: &str, theme: &Theme) -> Div {
    let d = div().flex().whitespace_nowrap().overflow_hidden();
    if query.is_empty() {
        return d.child(SharedString::from(text.to_owned()));
    }
    let low = text.to_lowercase();
    let q = query.to_lowercase();
    // Нижний регистр той же длины в байтах — иначе без подсветки.
    match low.find(&q).filter(|_| low.len() == text.len()) {
        Some(i) if text.is_char_boundary(i) && text.is_char_boundary(i + q.len()) => d
            .child(SharedString::from(text[..i].to_owned()))
            .child(
                div()
                    .bg(theme.c(Role::Highlight))
                    .text_color(theme.c(Role::TextPrimary))
                    .child(SharedString::from(text[i..i + q.len()].to_owned())),
            )
            .child(SharedString::from(text[i + q.len()..].to_owned())),
        _ => d.child(SharedString::from(text.to_owned())),
    }
}

/// Строка списка 28: знак, подпись, деталь, клавиша или переключатель.
fn row_view(
    r: &Row,
    selected: bool,
    detail_col: bool,
    query: &str,
    theme: &Theme,
    id: String,
    on_click: super::overlays::Handler,
) -> gpui_kit::Stateful<Div> {
    let secondary = theme.c(Role::TextSecondary);
    let mut d = div()
        .id(SharedString::from(id))
        .h(px(H_ITEM))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .rounded(px(4.0))
        .when(selected, |d| d.bg(theme.c(Role::FillCursor)))
        .when(!selected, |d| {
            d.hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
        });
    match r.mark {
        Mark::None => {}
        Mark::Check(on) => {
            d = d.child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .font_family(ICONS)
                    .text_size(px(11.0))
                    .child(SharedString::new_static(if on { "\u{E73E}" } else { "" })),
            );
        }
        Mark::Digit(n) => {
            d = d.child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .text_color(secondary)
                    .child(SharedString::from(n.to_string())),
            );
        }
    }
    let name = hl_text(&r.label, query, theme);
    d = if detail_col {
        d.child(
            name.w(px(130.0))
                .flex_none()
                .font_weight(gpui_kit::FontWeight::MEDIUM),
        )
    } else {
        d.child(name.flex_1().min_w_0())
    };
    if let Some((detail, accent)) = &r.detail {
        d = d.child(
            hl_text(detail, if *accent { "" } else { query }, theme)
                .flex_1()
                .min_w_0()
                .text_size(px(12.0))
                .text_color(theme.c(if *accent {
                    Role::AccentText
                } else {
                    Role::TextSecondary
                })),
        );
    }
    if let Some(on) = r.toggle {
        d = d.child(switch(on, theme));
    } else if !r.hint.is_empty() {
        d = d.child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(secondary)
                .child(SharedString::from(r.hint.clone())),
        );
    }
    d.on_click(on_click)
}

/// Переключатель (сегмент «вкл/выкл»): дорожка 28×14, ручка.
fn switch(on: bool, theme: &Theme) -> Div {
    div()
        .flex_none()
        .w(px(28.0))
        .h(px(14.0))
        .px(px(2.0))
        .flex()
        .items_center()
        .when(on, Styled::justify_end)
        .rounded(px(7.0))
        .border_1()
        .border_color(theme.c(if on {
            Role::AccentFill
        } else {
            Role::StrokeStrong
        }))
        .bg(theme.c(if on {
            Role::AccentFill
        } else {
            Role::LayerPanel
        }))
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded(px(4.0))
                .bg(theme.c(if on {
                    Role::AccentOn
                } else {
                    Role::TextSecondary
                })),
        )
}
