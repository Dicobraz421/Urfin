//! Быстрый просмотр в области панели (S7 §4, карточка S7-13, `screen-quickview.png`):
//! тело пассивной панели при `Tab.mode = qview` и документа `Placement::Panel`
//! (`viewer.dock`) — шапка «Быстрый просмотр» + тип, номера 40, строки 20, Markdown —
//! документом (≤ 4 МБ).
//!
//! Следование за курсором активной панели — здесь (UI-локальное): цель стоит 50 мс —
//! уходит `viewer.quick {side, path}`; документ открывает handler (Interactive lane).
//! Tab из панели — фокус быстрому просмотру (`viewer.quick … focus=true`), Tab/Esc —
//! обратно (`panel.focus`), F3 — тот же документ в окно (`viewer.dock`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Div, Keystroke, SharedString, Window, canvas, div, px};
use urfin_core::{AppState, Command, Focus, Location, OverlayKind, PanelMode, Side};
use urfin_view::{DocId, InboxMsg, QuickNoteKind, QuickTarget, ViewState, ViewerDoc, quick_target};

use super::super::CoreHost;
use super::super::features::Frame;
use super::super::theme::Theme;
use super::{Spot, Viewer, doc, find, rows};
use crate::tokens::{Metric, Role};

/// Пауза курсора до чтения (S7 §4).
pub(super) const DEBOUNCE: Duration = Duration::from_millis(50);
/// Тот же файл повторно не просим чаще (ответ handler'а ещё в пути).
const ASK_AGAIN: Duration = Duration::from_millis(500);

/// Следование за курсором: цель и с какого момента она такая; что уже попросили.
#[derive(Default, Debug)]
pub(super) struct Follow {
    want: Option<(Location, Instant)>,
    asked: Option<(Location, Instant)>,
}

impl Follow {
    /// Цель кадра (`None` — не файл) и что показано. `Some(d)` — разбудить через `d`.
    pub(super) fn observe(
        &mut self,
        target: Option<&Location>,
        shown: Option<&Location>,
        now: Instant,
    ) -> Option<Duration> {
        let Some(t) = target.filter(|t| Some(*t) != shown) else {
            self.want = None;
            return None;
        };
        if self.want.as_ref().is_none_or(|(l, _)| l != t) {
            self.want = Some((t.clone(), now));
        }
        let since = self.want.as_ref().map_or(now, |w| w.1);
        Some(
            DEBOUNCE
                .saturating_sub(now.saturating_duration_since(since))
                .max(Duration::from_millis(1)),
        )
    }

    /// Цель простояла паузу — её путь (один раз).
    pub(super) fn due(&mut self, now: Instant) -> Option<Location> {
        let (l, since) = self.want.as_ref()?;
        if now.saturating_duration_since(*since) < DEBOUNCE {
            return None;
        }
        if self
            .asked
            .as_ref()
            .is_some_and(|(a, t)| a == l && now.saturating_duration_since(*t) < ASK_AGAIN)
        {
            return None;
        }
        let l = l.clone();
        self.asked = Some((l.clone(), now));
        Some(l)
    }
}

/// Область одной стороны.
#[derive(Default)]
pub(super) struct Slot {
    pub follow: Follow,
    pub text: rows::TextView,
    pub md: doc::DocView,
    /// Размер области под шапкой прошлого кадра (px).
    pub area: Rc<Cell<Option<(f32, f32)>>>,
    /// Последний отправленный `Viewport`.
    pub sent: Option<(DocId, u32, u32)>,
}

fn sides() -> [Side; 2] {
    [Side::Left, Side::Right]
}

/// Быстрый просмотр стороны следует за курсором: режим `qview`, панель видна, в области
/// не перенесённый вьювер, экран документа не закрывает панели.
fn following(s: &AppState, side: Side) -> bool {
    let p = s.panel(side);
    let docked = s
        .ext
        .get::<ViewState>()
        .and_then(|v| v.in_panel(side))
        .is_some_and(|d| !d.quick);
    p.visible && p.tab().mode == PanelMode::QView && super::shown(s).is_none() && !docked
}

/// Область панели `side` видна и показывает быстрый просмотр или перенесённый вьювер.
pub(crate) fn shows(s: &AppState, side: Side) -> bool {
    s.panel(side).visible && urfin_view::panel_shows(s, side)
}

/// Что показано в области: документ или заметка (облако, ошибка).
fn shown_loc(v: &ViewState, side: Side) -> Option<&Location> {
    v.in_panel(side)
        .map(|d| &d.loc)
        .or_else(|| v.qnote[side.idx()].as_ref().map(|n| &n.loc))
}

/// Документ области `side` с фокусом (Tab).
pub(super) fn focused(s: &AppState) -> Option<(Side, DocId)> {
    if !matches!(s.focus, Focus::Viewer | Focus::Overlay(OverlayKind::Find)) {
        return None;
    }
    let v = s.ext.get::<ViewState>()?;
    let side = v.panel_focus()?;
    Some((side, v.quick[side.idx()]?))
}

impl Viewer {
    pub(super) fn slot(&mut self, side: Side) -> &mut Slot {
        &mut self.quick[side.idx()]
    }

    /// Курсор активной панели — цели областей; `Some(d)` — разбудить через `d`.
    pub(super) fn quick_tick(&mut self, s: &AppState, now: Instant) -> Option<Duration> {
        let v = s.ext.get::<ViewState>()?;
        let mut wake: Option<Duration> = None;
        for side in sides() {
            let target = match quick_target(s, side) {
                QuickTarget::File { loc, .. } if following(s, side) => Some(loc),
                _ => None,
            };
            let d = self.quick[side.idx()]
                .follow
                .observe(target.as_ref(), shown_loc(v, side), now);
            wake = match (wake, d) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        wake
    }

    /// Цели, простоявшие паузу: `viewer.quick {side, path}`.
    pub(super) fn quick_due(&mut self, s: &AppState, now: Instant) -> Vec<Command> {
        self.quick_tick(s, now);
        sides()
            .into_iter()
            .filter_map(|side| {
                let loc = self.quick[side.idx()].follow.due(now)?;
                Some(
                    Command::new("viewer.quick")
                        .arg("side", side_name(side))
                        .arg("path", loc),
                )
            })
            .collect()
    }

    /// Клавиши быстрого просмотра: Tab из панели — фокус ему; в его фокусе Tab, Esc и
    /// F10 — назад к панели, F3 — документ в окно.
    pub(super) fn quick_key(s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        let m = &ks.modifiers;
        if m.control || m.alt || m.shift || m.platform {
            return None;
        }
        let key = ks.key.as_str();
        let v = s.ext.get::<ViewState>()?;
        if s.focus == Focus::Panel && key == "tab" {
            let side = s.active.other();
            let d = v.in_panel(side)?;
            // Tab в непустой командной строке — дополнение (S6-17).
            let typed = s
                .ext
                .get::<urfin_term::TermState>()
                .is_some_and(|t| !t.line.text().is_empty());
            if typed || !shows(s, side) {
                return None;
            }
            return Some(vec![
                Command::new("viewer.quick")
                    .arg("side", side_name(side))
                    .arg("path", d.loc.clone())
                    .arg("focus", true),
            ]);
        }
        if s.focus != Focus::Viewer {
            return None;
        }
        let (side, id) = focused(s)?;
        let quick = v.viewer(id).is_some_and(|d| d.quick);
        let _ = side;
        match key {
            "tab" => Some(vec![to_panel(s)]),
            "escape" | "f10" if quick => Some(vec![to_panel(s)]),
            "f3" => Some(vec![Command::new("viewer.dock")]),
            _ => None,
        }
    }

    /// `Viewport` документов областей (по размеру области прошлого кадра) и накопленные
    /// события мыши. `true` — что-то отправлено.
    pub(super) fn quick_inbox(&mut self, s: &AppState, v: &ViewState, window: &mut Window) -> bool {
        let mut sent = false;
        let bar = self.bar_h(s);
        let focus = focused(s).map(|f| f.0);
        for side in sides() {
            let tx = v.inbox.sender();
            let slot = &mut self.quick[side.idx()];
            for m in slot.text.out.drain(..) {
                tx.send(m);
                sent = true;
            }
            let Some(d) = v.in_panel(side) else {
                slot.sent = None;
                continue;
            };
            if d.md_mode() {
                sent |= slot.md.inbox(d, v);
            }
            let Some((w, h)) = slot.area.get() else {
                continue;
            };
            let bar = if focus == Some(side) { bar } else { 0.0 };
            let g = rows::geo_panel(d, w, h, bar, window);
            let now = (d.id, g.cols, g.rows);
            if slot.sent == Some(now) {
                continue;
            }
            slot.sent = Some(now);
            tx.send(InboxMsg::Viewport {
                doc: d.id,
                cols: g.cols,
                rows: g.rows,
            });
            sent = true;
        }
        sent
    }

    /// Тело области панели `side`: шапка и документ, заметка или сводка каталога.
    /// `None` — область не быстрого просмотра.
    pub(super) fn panel_body(
        &mut self,
        f: &Frame<'_>,
        side: Side,
        panel_w: f32,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<AnyElement> {
        let s = f.s;
        if !shows(s, side) {
            self.quick[side.idx()].text.clear();
            self.quick[side.idx()].md.clear();
            return None;
        }
        let theme = f.theme;
        let v = s.ext.get::<ViewState>()?;
        let i18n = urfin_i18n::current();
        let target = quick_target(s, side);
        let d = v.in_panel(side);
        let docked = d.is_some_and(|d| !d.quick);
        let has_focus = focused(s).is_some_and(|f| f.0 == side);
        let kind = d.map(|d| &d.loc).or(match &target {
            QuickTarget::File { loc, .. } => Some(loc),
            _ => None,
        });
        let kind = kind
            .and_then(|l| l.path.extension())
            .map(|e| e.to_string_lossy().to_uppercase())
            .unwrap_or_default();
        let head = super::super::views::head(i18n.get("ui-qv-title"), kind, has_focus, theme);
        let note = v.qnote[side.idx()].as_ref();
        let content: AnyElement = match (&target, d, note) {
            (_, Some(d), _) if docked => self.doc_el(f, side, d, has_focus, panel_w, window, cx),
            (QuickTarget::File { loc, .. }, _, Some(n)) if n.loc == *loc => {
                note_el(&n.kind, theme).into_any_element()
            }
            (QuickTarget::Dir { name }, _, _) => dir_el(s, side, name, theme).into_any_element(),
            (QuickTarget::Nothing, _, _) => div().into_any_element(),
            // Файл: документ (пока новый не открыт — прежний).
            (_, Some(d), _) => self.doc_el(f, side, d, has_focus, panel_w, window, cx),
            _ => div().into_any_element(),
        };
        let area = self.quick[side.idx()].area.clone();
        // Размер области под шапкой — для `Viewport` (колонки и строки документа).
        let measure = canvas(
            move |b, window, _| {
                let now = (f32::from(b.size.width), f32::from(b.size.height));
                if area.get() != Some(now) {
                    area.set(Some(now));
                    window.refresh();
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full();
        Some(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(theme.c(Role::LayerPanel))
                .child(head)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .flex()
                        .flex_col()
                        .child(measure)
                        .child(content),
                )
                .into_any_element(),
        )
    }

    /// Документ области: текст и Hex строками, Markdown — документом; строка «Найти»,
    /// если фокус у него.
    #[allow(
        clippy::too_many_arguments,
        reason = "кадр области: документ, место, окно"
    )]
    fn doc_el(
        &mut self,
        f: &Frame<'_>,
        side: Side,
        d: &ViewerDoc,
        has_focus: bool,
        panel_w: f32,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> AnyElement {
        let spot = Spot::Panel(side);
        let bar = if has_focus { self.bar_of(f.s) } else { None };
        let bar_el = bar.map(|b| super::bar_row(b, d, f, cx));
        let bar_h = if bar_el.is_some() { find::H_BAR } else { 0.0 };
        // Меню кодировок и режимов (Shift+F8, Shift+F4) в фокусе области — плашкой.
        let menu = self.menu.filter(|m| has_focus && m.doc == d.id);
        let area = &mut self.quick[side.idx()];
        let (w, h) = area.area.get().unwrap_or((panel_w, 400.0));
        let menu_el = menu
            .map(|m| super::head::menu_in_area(m, d, f.theme, w, cx))
            .unwrap_or_default();
        let body = if d.md_mode() {
            area.text.clear();
            let body = area.md.body(f.s, d, f.theme, spot, w, window, cx);
            if let Some(t) = area.md.clip.take() {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(t));
            }
            body
        } else {
            area.md.clear();
            let g = rows::geo_panel(d, w, h, bar_h, window);
            let sel = area.text.sel(d);
            let screen = area
                .text
                .rows
                .build(d, sel.as_ref(), &g, f.theme, &area.text.hit, window);
            let id = d.id;
            rows::body(screen, &g, d, f.theme, &area.text, spot, cx).on_scroll_wheel(cx.listener(
                move |h, ev: &gpui_kit::ScrollWheelEvent, w, _| {
                    let Some(v) = h.features.get_mut::<Viewer>() else {
                        return;
                    };
                    // Колесо прокручивает документ области и без фокуса (событие — ему).
                    if let Some(scroll) = v.wheel_scroll(ev, rows::QV_ROW) {
                        v.slot(side)
                            .text
                            .out
                            .push(InboxMsg::Scroll { doc: id, scroll });
                        h.sync_inbox(w);
                    }
                },
            ))
        };
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(body)
            .children(bar_el)
            .children(menu_el)
            .on_mouse_move(cx.listener(move |h, ev: &gpui_kit::MouseMoveEvent, w, cx| {
                if super::with_text(h, spot, |tv, d| tv.moved(d, ev)).unwrap_or(false) {
                    h.sync_inbox(w);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(move |h, _: &gpui_kit::MouseUpEvent, _, _| {
                    let _ = super::with_text(h, spot, |tv, _| tv.up());
                }),
            )
            .into_any_element()
    }
}

/// Tab / Esc в быстром просмотре: фокус — активной панели (она не менялась).
fn to_panel(s: &AppState) -> Command {
    Command::new("panel.focus").arg("side", side_name(s.active))
}

pub(super) fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

/// «Файл в облаке — F3 для загрузки» или ошибка открытия.
fn note_el(kind: &QuickNoteKind, theme: &Theme) -> Div {
    let (text, role) = match kind {
        QuickNoteKind::Cloud => (
            urfin_i18n::current().get("ui-qv-cloud"),
            Role::TextSecondary,
        ),
        QuickNoteKind::Error(e) => (e.to_string(), Role::CriticalFg),
    };
    div()
        .px(px(theme.m(Metric::PadRowX)))
        .py(px(rows::QV_TOP + 2.0))
        .text_size(px(12.5))
        .text_color(theme.c(role))
        .child(SharedString::from(text))
}

/// Каталог под курсором: имя, размер (если посчитан, `vfs.dir_size` — F3) и подсказка.
fn dir_el(s: &AppState, side: Side, name: &str, theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    let t = s.panel(side.other()).tab();
    let bytes = match t.current() {
        Some(urfin_core::Row::Entry { entry, .. })
            if entry.attrs.has(urfin_core::Attrs::SIZE_KNOWN) =>
        {
            urfin_i18n::fmt::bytes_with(&i18n, entry.size)
        }
        _ => i18n.get("ui-qv-dir-size"),
    };
    let line = |text: String, role: Role| {
        div()
            .text_color(theme.c(role))
            .child(SharedString::from(text))
    };
    div()
        .px(px(theme.m(Metric::PadRowX)))
        .py(px(rows::QV_TOP + 2.0))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .text_size(px(12.5))
        .child(line(name.to_owned(), Role::TextPrimary))
        .child(line(bytes, Role::TextSecondary))
        .child(line(i18n.get("ui-qview-dir"), Role::TextSecondary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_waits_for_pause_and_asks_once() {
        let t0 = Instant::now();
        let mut f = Follow::default();
        let mut sent = 0;
        // 1000 файлов подряд, по миллисекунде: ни одного запроса до паузы.
        for i in 0..1000u64 {
            let now = t0 + Duration::from_millis(i);
            let loc = Location::local(format!("C:\\f{i}"));
            assert!(f.observe(Some(&loc), None, now).is_some());
            sent += usize::from(f.due(now).is_some());
        }
        assert_eq!(sent, 0);
        let last = Location::local("C:\\f999");
        let now = t0 + Duration::from_millis(999) + DEBOUNCE;
        f.observe(Some(&last), None, now);
        assert_eq!(f.due(now), Some(last.clone()));
        // Та же цель, ответ ещё в пути — не повторяем.
        assert_eq!(f.due(now + Duration::from_millis(10)), None);
        // Показано — следить не за чем.
        assert!(f.observe(Some(&last), Some(&last), now).is_none());
        assert_eq!(f.due(now + ASK_AGAIN * 2), None);
    }
}
