//! Экран записи сеанса (F03 §3.6, `screen-sshrecview.png`): кадр плеера на месте строк и
//! нижняя панель 46 — ▶, время, шкала с метками совпадений, длительность, скорость,
//! поле поиска. Кадры приходят от потока плеера (`urfin_view::cast`), UI их только
//! красит: подсветка вывода и совпадения поиска поверх снимка.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::{Context, Div, FontWeight, Hsla, Pixels, SharedString, Window, canvas, div, px};
use urfin_core::{AppState, Command};
use urfin_term::vt::grid::Snapshot;
use urfin_view::search::SearchState;
use urfin_view::{DocId, InboxMsg, ViewerDoc};

use super::super::CoreHost;
use super::super::features::Frame;
use super::super::term_view;
use super::super::theme::Theme;
use super::find::FindBar;
use super::rows::MONO;
use super::{Viewer, counter, shown};
use crate::cast_view;
use crate::tokens::Role;

/// Высота панели, кнопки, поля (`stBar`, `stIconBtn`).
pub(super) const H_BAR: f32 = 46.0;
const BTN: f32 = 30.0;
const SEG_H: f32 = 28.0;
const W_FIELD: f32 = 170.0;
const W_SLIDER: f32 = 120.0;
const TEXT: f32 = 12.5;
const ICON_FONT: &str = "Segoe Fluent Icons";
const PLAY: char = '\u{E768}';
const PAUSE: char = '\u{E769}';
const FIND: char = '\u{E721}';
const UP: char = '\u{E70E}';
const DOWN: char = '\u{E70D}';

/// Ключ кадра: снимок, тема, подсветка вывода, поиск.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameKey {
    frame: usize,
    theme: u64,
    hl: Option<urfin_term::hl::Rules>,
    marks: usize,
    hit: Option<u64>,
}

/// Состояние экрана записи.
#[derive(Default)]
pub(super) struct CastView {
    at: term_view::GridBounds,
    slider: term_view::GridBounds,
    /// Ползунок тянут.
    pub(super) drag: bool,
    cache: Option<(FrameKey, Rc<Snapshot>)>,
    hl: Option<urfin_term::hl::Highlighter>,
    /// Статус документа: с кадрами плеера не меняется.
    status: Option<(DocId, String)>,
    /// Шаг Alt+↓ по истории поиска.
    pub(super) hist: usize,
}

pub(super) fn rgb(c: Hsla) -> [u8; 3] {
    let r: gpui_kit::Rgba = c.into();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "канал 0…1"
    )]
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [ch(r.r), ch(r.g), ch(r.b)]
}

/// Текущее совпадение среди видимых: по времени выведенных меток.
fn current(d: &ViewerDoc, x: &SearchState, visible: usize) -> Option<usize> {
    let c = d.cast()?;
    let ix = c.index.as_ref()?;
    let n = x.ordinal()?;
    let shown = x.marks.partition_point(|&m| ix.time_of(m) <= c.ms);
    cast_view::current(visible, shown, n)
}

impl CastView {
    /// Кадр с подсветкой вывода и совпадениями; тот же снимок — из кэша.
    fn snap(&mut self, s: &AppState, d: &ViewerDoc, theme: &Theme) -> Option<Rc<Snapshot>> {
        let frame = d.cast()?.frame.clone()?;
        let search = d.search.as_ref().filter(|x| x.error.is_none());
        let cfg = s
            .ext
            .get::<urfin_term::TermState>()
            .map(|t| &t.config.highlight)
            .filter(|h| h.enabled);
        let key = FrameKey {
            frame: std::sync::Arc::as_ptr(&frame) as usize,
            theme: theme.generation(),
            hl: cfg.map(|h| h.rules),
            marks: search.map_or(0, |x| x.marks.as_ptr() as usize),
            hit: search.and_then(|x| x.hit.as_ref()).map(|h| h.start),
        };
        if let Some((k, snap)) = &self.cache
            && *k == key
        {
            return Some(snap.clone());
        }
        let mut snap = (*frame).clone();
        if let Some(cfg) = cfg {
            let hl = self
                .hl
                .get_or_insert_with(|| urfin_term::hl::Highlighter::new(cfg.rules));
            hl.set_rules(cfg.rules);
            hl.apply(&mut snap, &term_view::palette(theme));
        }
        if let Some(x) = search
            && let Some(m) = &x.matcher
        {
            let list = cast_view::hits(&snap, m);
            let hit = cast_view::Paint {
                bg: rgb(theme.c(Role::Highlight)),
                fg: rgb(theme.c(Role::TextPrimary)),
            };
            let now = cast_view::Paint {
                bg: rgb(theme.c(Role::AccentFill)),
                fg: rgb(theme.c(Role::AccentOn)),
            };
            let cur = current(d, x, list.len());
            cast_view::paint(&mut snap, &list, cur, hit, now);
        }
        let snap = Rc::new(snap);
        self.cache = Some((key, snap.clone()));
        Some(snap)
    }

    /// Тело экрана: кадр; пока идёт проход — пусто (ход — в статусе).
    pub(super) fn body(&mut self, s: &AppState, d: &ViewerDoc, theme: &Theme, font: f32) -> Div {
        let root = div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .pl(px(12.0))
            .pt(px(8.0))
            .bg(theme.c(Role::LayerPanel));
        match self.snap(s, d, theme) {
            Some(snap) => root.child(term_view::view(snap, font, self.at.clone())),
            None => root,
        }
    }

    /// Статус: сведения одной группой, пока идёт проход — его ход.
    pub(super) fn zones(&mut self, d: &ViewerDoc) -> [String; 3] {
        let Some(c) = d.cast() else {
            return Default::default();
        };
        let Some(ix) = c.index.as_ref() else {
            let done = c.scanned.load(std::sync::atomic::Ordering::Relaxed);
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("pct", super::rows::pct(done, c.file_len()));
            return [
                urfin_i18n::current().fmt("ui-view-indexing", &a),
                String::new(),
                String::new(),
            ];
        };
        let text = match &self.status {
            Some((id, t)) if *id == d.id => t.clone(),
            _ => {
                let t = cast_view::status(
                    &c.header,
                    ix.duration_ms,
                    c.file_len(),
                    &crate::sys::Zone::system(),
                );
                self.status = Some((d.id, t.clone()));
                t
            }
        };
        [text, String::new(), String::new()]
    }

    /// Момент под точкой шкалы.
    fn time_at(&self, d: &ViewerDoc, x: Pixels) -> Option<u64> {
        let b = self.slider.get()?;
        let w = f32::from(b.size.width);
        if w <= 0.0 {
            return None;
        }
        let frac = (f32::from(x - b.origin.x) / w).clamp(0.0, 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "миллисекунды шкалы"
        )]
        Some((f64::from(frac) * d.cast()?.duration_ms() as f64) as u64)
    }
}

/// Шкала: нажатие или протяжка в `x` — перемотка событием в очередь handler'а.
pub(super) fn drag_to(
    h: &mut CoreHost,
    x: Pixels,
    window: &mut Window,
    cx: &mut Context<CoreHost>,
) {
    let Some(core) = h.core.as_ref() else {
        return;
    };
    let s = core.state();
    let Some(d) = shown(s).and_then(|id| super::doc(s, id)) else {
        return;
    };
    let Some(v) = h.features.get_mut::<Viewer>() else {
        return;
    };
    if let Some(ms) = v.cast.time_at(d, x) {
        let doc = d.id;
        v.text.out.push(InboxMsg::CastSeek { doc, ms });
        h.sync_inbox(window);
        cx.notify();
    }
}

fn clock(ms: u64) -> String {
    crate::term_rec::clock(Duration::from_millis(ms))
}

fn icon_button(id: &'static str, glyph: char, size: f32, theme: &Theme) -> gpui_kit::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(BTN))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::LayerPanel))
        .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
        .font_family(ICON_FONT)
        .text_size(px(size))
        .child(SharedString::from(glyph.to_string()))
}

/// Шкала: дорожка, пройденное, метки совпадений (`highlight`, текущая — `accent`), ползунок.
fn slider(v: &CastView, d: &ViewerDoc, theme: &Theme, cx: &Context<CoreHost>) -> Div {
    let c = d.cast();
    let frac = c.map_or(0.0, urfin_view::cast::CastState::frac);
    let accent = theme.c(Role::AccentFill);
    let cur = d.search.as_ref().and_then(SearchState::ordinal);
    let at = v.slider.clone();
    let mut track = div()
        .relative()
        .flex_1()
        .min_w(px(W_SLIDER))
        .h(px(BTN))
        .child(
            canvas(move |b, _, _| at.set(Some(b)), |_, (), _, _| {})
                .absolute()
                .size_full(),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(13.0))
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(theme.c(Role::StrokeDivider)),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .w(gpui_kit::relative(frac))
                .top(px(13.0))
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(accent),
        );
    for (i, x) in c.iter().flat_map(|c| c.ticks.iter()).enumerate() {
        let now = cur == Some(i + 1);
        track = track.child(
            div()
                .absolute()
                .left(gpui_kit::relative(*x))
                .ml(px(-1.5))
                .top(px(7.0))
                .w(px(3.0))
                .h(px(16.0))
                .rounded(px(1.0))
                .border_1()
                .border_color(if now {
                    accent
                } else {
                    theme.c(Role::ContextRemote)
                })
                .bg(if now {
                    accent
                } else {
                    theme.c(Role::Highlight)
                }),
        );
    }
    track
        .child(
            div()
                .absolute()
                .left(gpui_kit::relative(frac))
                .ml(px(-7.0))
                .top(px(8.0))
                .size(px(14.0))
                .rounded(px(7.0))
                .border_2()
                .border_color(theme.c(Role::LayerPanel))
                .bg(accent),
        )
        .on_mouse_down(
            gpui_kit::MouseButton::Left,
            cx.listener(|h, ev: &gpui_kit::MouseDownEvent, w, cx| {
                if let Some(v) = h.features.get_mut::<Viewer>() {
                    v.cast.drag = true;
                }
                drag_to(h, ev.position.x, w, cx);
            }),
        )
}

/// Скорость 1×/2×/4×.
fn speed(d: &ViewerDoc, f: &Frame<'_>) -> Div {
    let now = d.cast().map_or(1, |c| c.speed);
    let theme = f.theme;
    let mut row = div()
        .ml(px(8.0))
        .flex()
        .flex_none()
        .h(px(SEG_H + 2.0))
        .overflow_hidden()
        .rounded(px(5.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::LayerPanel));
    for n in [1u8, 2, 4] {
        let on = n == now;
        let mut seg = div()
            .id(SharedString::from(format!("cast-speed-{n}")))
            .h_full()
            .px(px(10.0))
            .flex()
            .items_center()
            .text_size(px(TEXT))
            .child(SharedString::from(format!("{n}×")));
        if on {
            seg = seg
                .bg(theme.c(Role::FillCursor))
                .text_color(theme.c(Role::CursorText))
                .font_weight(FontWeight::SEMIBOLD);
        } else {
            seg = seg
                .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
                .on_click((f.click)(
                    Command::new("viewer.cast.speed").arg("n", i64::from(n)),
                ));
        }
        row = row.child(seg);
    }
    row
}

/// Поле поиска: открыто (F7) — поле строки «Найти», иначе последний образец; клик — F7.
fn field(bar: Option<&FindBar>, d: &ViewerDoc, f: &Frame<'_>) -> Div {
    if let Some(input) = bar.and_then(|b| b.field_of(f.theme, W_FIELD)) {
        return input;
    }
    let text = d
        .search
        .as_ref()
        .map(|x| super::rows::pattern_text(&x.req.pat))
        .unwrap_or_default();
    let theme = f.theme;
    div().w(px(W_FIELD)).flex_none().child(
        div()
            .id("cast-find")
            .h(px(BTN))
            .px(px(8.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.c(Role::StrokeControl))
            .bg(theme.c(Role::LayerPanel))
            .font_family(MONO)
            .text_size(px(TEXT))
            .child(SharedString::from(text))
            .on_click((f.click)(Command::new("viewer.search"))),
    )
}

/// Панель плеера (`stBar`).
pub(super) fn bar(
    v: &CastView,
    d: &ViewerDoc,
    find: Option<&FindBar>,
    f: &Frame<'_>,
    cx: &Context<CoreHost>,
) -> Div {
    let theme = f.theme;
    let (ms, dur, playing) = d
        .cast()
        .map_or((0, 0, false), |c| (c.ms, c.duration_ms(), c.playing));
    let (note, bad) = counter(d);
    let glyph = if playing { PAUSE } else { PLAY };
    div()
        .h(px(H_BAR))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .border_t_1()
        .border_color(theme.c(Role::StrokeDivider))
        .bg(theme.c(Role::Header))
        .text_size(px(TEXT))
        .child(
            icon_button("cast-play", glyph, 12.0, theme)
                .on_click((f.click)(Command::new("viewer.cast.play"))),
        )
        .child(
            div()
                .flex_none()
                .font_family(MONO)
                .child(SharedString::from(clock(ms))),
        )
        .child(slider(v, d, theme, cx))
        .child(
            div()
                .flex_none()
                .font_family(MONO)
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(clock(dur))),
        )
        .child(speed(d, f))
        .child(
            div()
                .flex_none()
                .ml(px(8.0))
                .font_family(ICON_FONT)
                .text_size(px(14.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(FIND.to_string())),
        )
        .child(field(find, d, f))
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_size(px(12.0))
                .text_color(theme.c(if bad {
                    Role::CriticalFg
                } else {
                    Role::TextSecondary
                }))
                .child(SharedString::from(note)),
        )
        .child(
            icon_button("cast-prev", UP, 10.0, theme)
                .on_click((f.click)(Command::new("viewer.search_prev"))),
        )
        .child(
            icon_button("cast-next", DOWN, 10.0, theme)
                .on_click((f.click)(Command::new("viewer.search_next"))),
        )
}
