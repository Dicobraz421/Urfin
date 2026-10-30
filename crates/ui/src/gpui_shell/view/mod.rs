//! Экран вьювера (S7-07, S7 §6): фича окна `Viewer` (ADR-0022) — `Main::Replace` на месте
//! панелей, вкладки экранов в заголовке, шапка 36, поле номеров и строки одним `canvas`
//! (кэш `ShapedLine`), полоса прокрутки 14 с метками, статус и keybar вьювера.
//!
//! Позиция, прокрутка и страницы — в handler `viewer`: UI шлёт размер окна событием
//! `InboxMsg::Viewport` (решение 2026-10-30 «viewer-viewport»), колесо и клавиши — командой
//! `viewer.scroll`. Кадр читает только кэш страниц: чего нет — скелетон.
#![allow(
    clippy::many_single_char_names,
    reason = "кадр: d — документ, g — геометрия, r — строка, s — состояние"
)]

mod head;
mod rows;

use gpui_kit::prelude::*;
use gpui_kit::{Context, Div, Keystroke, SharedString, Window, div};
use urfin_core::{AppState, Args, Command, Focus, OverlayKind};
use urfin_view::{DocId, InboxMsg, Mode, Placement, ViewState, ViewerDoc};

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

/// Сцена `--screens` готова к снимку: документ открыт, окно известно handler'у, видимые
/// страницы на месте, индекс (для номеров) и поиск закончены.
pub(super) fn settled(s: &AppState) -> bool {
    let Some(d) = shown(s).and_then(|id| doc(s, id)) else {
        return true;
    };
    d.is_ready()
        && d.view_rows > 0
        && !d.realign
        && d.reveal.is_none()
        && d.pages.inflight() == 0
        && (d.mode == Mode::Hex || d.index.done)
        && d.search
            .as_ref()
            .is_none_or(|x| !x.running && (x.count.done || x.hit.is_none()))
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
    rows: rows::Rows,
    /// Дробные строки колеса (тачпад).
    wheel: (f32, f32),
    thumb: rows::Thumb,
}

impl Viewer {
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
        let (dx, dy) = match ev.delta {
            gpui_kit::ScrollDelta::Lines(p) => (p.x * 3.0, p.y * 3.0),
            gpui_kit::ScrollDelta::Pixels(p) => (f32::from(p.x) / row_h, f32::from(p.y) / row_h),
        };
        if ev.modifiers.control {
            if dy.abs() < f32::EPSILON {
                return None;
            }
            self.font = Some((font + dy.signum()).clamp(8.0, 32.0));
            return None;
        }
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
        let (dir, n) = match (x, y) {
            (_, y) if y > 0 => ("up", y),
            (_, y) if y < 0 => ("down", -y),
            (x, _) if x > 0 => ("left", x * 4),
            (x, _) if x < 0 => ("right", -x * 4),
            _ => return None,
        };
        Some(Command::new("viewer.scroll").arg("dir", dir).arg("n", n))
    }

    /// Клавиши открытого меню шапки; остальное — keymap (контекст `viewer`).
    fn menu_key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        let m = self.menu?;
        let Some(d) = shown(s).filter(|id| *id == m.doc).and_then(|id| doc(s, id)) else {
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
        _s: &AppState,
        _cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let kind = match id {
            "viewer.codepage_menu" => MenuKind::Enc,
            "viewer.mode" => MenuKind::Mode,
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
        self.menu_key(s, ks)
    }

    fn tabs(&self, s: &AppState, theme: &Theme, click: Click<'_>) -> Option<Div> {
        head::tabs(s, theme, click)
    }

    /// Окно текста в колонках и строках — handler'у (`InboxMsg::Viewport`): при открытии,
    /// ресайзе, смене режима и шрифта.
    fn inbox(&mut self, s: &AppState, theme: &Theme, window: &mut Window) -> bool {
        let Some(id) = shown(s) else {
            self.sent = None;
            return false;
        };
        let (Some(v), Some(d)) = (s.ext.get::<ViewState>(), doc(s, id)) else {
            return false;
        };
        let g = rows::geo(d, theme, self.font_px(s, theme), window);
        let now = (id, g.cols, g.rows);
        if self.sent == Some(now) {
            return false;
        }
        self.sent = Some(now);
        v.inbox.sender().send(InboxMsg::Viewport {
            doc: id,
            cols: g.cols,
            rows: g.rows,
        });
        true
    }

    fn main(
        &mut self,
        f: &Frame<'_>,
        _hidden: bool,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<Main> {
        let Some((id, d)) = shown(f.s).and_then(|id| Some((id, doc(f.s, id)?))) else {
            self.menu = None;
            self.rows.clear();
            return None;
        };
        if self.menu.is_some_and(|m| m.doc != id) {
            self.menu = None;
        }
        let font = self.font_px(f.s, f.theme);
        let g = rows::geo(d, f.theme, font, window);
        let screen = self.rows.build(d, &g, f.theme, window);
        let narrow = f32::from(window.viewport_size().width) < super::chrome::NARROW_W;
        let menu = self.menu;
        let measure = head::Measure::new(window);
        let head_el = head::header(d, f.theme, f.click, &measure);
        let zones = rows::zones(d, &screen, menu.map(|m| m.kind));
        let keys = head::keys(d, menu.map(|m| m.kind));
        let body = rows::body(screen, &g, d, f.theme, &self.thumb, cx);
        let mut root = div()
            .id("viewer")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(f.theme.c(crate::tokens::Role::LayerPanel))
            .child(head_el)
            .child(body.on_scroll_wheel(cx.listener(
                |h, ev: &gpui_kit::ScrollWheelEvent, w, cx| {
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
                },
            )))
            .child(super::chrome::status(&zones, narrow, f.theme))
            .child(super::chrome::keybar(
                &[],
                crate::keybar::Mods::None,
                Some(&keys),
                narrow,
                f.theme,
                &f.click,
            ));
        if let Some(m) = menu {
            root = root.children(head::menu(m, d, f.theme, &measure, cx));
        }
        Some(Main::Replace(root.into_any_element()))
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

/// Сцены вьювера (S7-07): текст с метками поиска, «мешочек» кодировок, Hex с курсором.
/// Файлы — в демо-дереве (`Projects\urfin`); затем вьювер закрывается.
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "screen-viewer",
        steps: &[
            "ui.cancel",
            // Ширина макетов: 32 байта в строке Hex — от 150 колонок.
            "resize 1280x760",
            "panel.focus side=left",
            "viewer.open path=\"Projects\\urfin\\CHANGELOG.md\"",
            "viewer.search text=Исправлено",
        ],
    },
    Scene {
        name: "screen-viewerenc",
        steps: &["viewer.codepage_menu"],
    },
    Scene {
        name: "screen-viewerhex",
        steps: &[
            "viewer.close",
            "viewer.open path=\"Projects\\urfin\\urfin.exe\"",
            "viewer.search text=\"54\" kind=hex",
        ],
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
