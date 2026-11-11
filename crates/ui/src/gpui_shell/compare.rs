//! Итог сравнения папок (D-04, `screen-compare.png`) как фича окна (ADR-0022): строка
//! внизу, метки в панелях, Enter/Shift+Enter — синхронизация, Esc — закрыть.
//!
//! Рекурсивное сравнение — панель `cmp:` (S8-39, D-12, `screen-cmppanel`): заголовок с
//! чипом «Сравнение», значок и подпись статуса в строке, строка итогов внизу панели.
//! Тексты — `crate::cmp_panel`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{
    Bounds, ClickEvent, Context, Div, Hsla, Keystroke, Pixels, SharedString, Window, div, point,
    px, size as size2,
};
use urfin_core::search::SearchStore;
use urfin_core::{AppState, Args, Command, Side, Tab};

use super::CoreHost;
use super::features::{Frame, ShellFeature};
use super::panel::{Listen, Note};
use super::screens::Scene;
use super::theme::Theme;
use crate::cmp_panel::{self, Half, Tone};
use crate::tokens::{Metric, Role};

/// Кадр, пока сравнение панели идёт: прогресс и счётчики.
const TICK: Duration = Duration::from_millis(33);
/// Строка итогов панели `cmp:` (`screen-cmppanel`).
pub(super) const TOTALS_H: f32 = 28.0;
/// Значок «две панели»: квадрат 12, половины 4,2×8 со скруглением 1.
const GLYPH: f32 = 12.0;

/// Строк панели, просматриваемых для заметок сравнения, не больше.
const NOTES_MAX: usize = 100_000;

/// Итог сравнения: счётчики (только слева, только справа, новее слева, новее справа,
/// различаются, одинаковых), сторона-источник синхронизации, папки панелей.
struct View {
    counts: [u32; 6],
    sync: Option<Side>,
    locs: [urfin_core::Location; 2],
    seen_draft: bool,
}

#[derive(Default)]
pub(super) struct Compare(Option<View>);

impl Compare {
    /// Строка сравнения видна: итог без черновика или черновик синхронизации.
    pub(super) fn mode(&self, s: &AppState) -> bool {
        let Some(c) = &self.0 else {
            return false;
        };
        match s.ext.get::<crate::UiState>().and_then(|u| u.draft.as_ref()) {
            None => s.prompts.active().is_none(),
            Some(d) => d.cmd == "ops.copy" && c.sync.is_some(),
        }
    }

    /// Сторона-источник синхронизации (черновик копирования из сравнения).
    pub(super) fn sync(&self) -> Option<Side> {
        self.0.as_ref().and_then(|c| c.sync)
    }

    /// Метки у выделенного («новее», «только здесь»), при синхронизации — стрелка у
    /// того, что копируется.
    pub(super) fn notes(&self, s: &AppState, out: &mut [HashMap<String, Note>; 2]) {
        let Some(c) = &self.0 else {
            return;
        };
        let draft = s.ext.get::<crate::UiState>().and_then(|u| u.draft.as_ref());
        let i18n = urfin_i18n::current();
        let (newer, only) = (i18n.get("ui-tag-newer"), i18n.get("ui-tag-only-here"));
        let syncing = c
            .sync
            .filter(|_| draft.is_some_and(|d| d.cmd == "ops.copy"));
        for (i, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            let t = s.panel(side).tab();
            let other = &s.panel(side.other()).tab().listing.entries;
            let names: HashSet<String> = other
                .iter()
                .map(|e| e.name.as_str().to_lowercase())
                .collect();
            // Синхронизация с сервером (`screen-sftpsync`): в назначении — «только здесь»
            // у лишних файлов.
            let server = !s.panel(Side::Left).tab().loc.is_local()
                || !s.panel(Side::Right).tab().loc.is_local();
            let dest = server && syncing.is_some_and(|src| src != side);
            for &k in t.listing.order.iter().take(NOTES_MAX) {
                let Some(e) = t.listing.entries.get(k as usize) else {
                    continue;
                };
                if dest {
                    if !e.kind.is_dir_like() && !names.contains(&e.name.as_str().to_lowercase()) {
                        out[i].insert(e.name.to_string(), Note::Tag(only.clone()));
                    }
                    continue;
                }
                if !t.selection.contains(k) {
                    continue;
                }
                let note = match syncing {
                    Some(src) if src == side => {
                        Note::Arrow(if side == Side::Left { "→" } else { "←" })
                    }
                    Some(_) => continue,
                    None if names.contains(&e.name.as_str().to_lowercase()) => {
                        Note::Tag(newer.clone())
                    }
                    None => Note::Tag(only.clone()),
                };
                out[i].insert(e.name.to_string(), note);
            }
        }
    }

    /// Итог живёт, пока панели на тех же папках; черновик синхронизации закрылся
    /// (Enter или Esc) — итог тоже.
    pub(super) fn life(&mut self, s: &AppState) {
        let Some(c) = self.0.as_mut() else {
            return;
        };
        let moved = c.locs[0] != s.panel(Side::Left).tab().loc
            || c.locs[1] != s.panel(Side::Right).tab().loc;
        let draft = s
            .ext
            .get::<crate::UiState>()
            .is_some_and(|u| u.draft.is_some());
        if c.sync.is_some() && draft {
            c.seen_draft = true;
        }
        if moved || (c.seen_draft && !draft) {
            self.0 = None;
        }
    }
}

impl ShellFeature for Compare {
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        s: &AppState,
        _cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        if id != "ops.compare.result" {
            return None;
        }
        // Рекурсивное — итоги панели `cmp:/<id>`, строка плоского сравнения не нужна.
        if args.bool("recursive").unwrap_or(false) {
            let id = args.int("id").and_then(|v| u32::try_from(v).ok())?;
            return Some(vec![cmp_panel::Totals::command(id, args)]);
        }
        let n = |k: &str| u32::try_from(args.int(k).unwrap_or(0)).unwrap_or(0);
        self.0 = Some(View {
            counts: [
                n("only_left"),
                n("only_right"),
                n("newer_left"),
                n("newer_right"),
                n("differs"),
                n("equal"),
            ],
            sync: match args.str("sync") {
                Some("left") => Some(Side::Left),
                Some("right") => Some(Side::Right),
                _ => None,
            },
            locs: [
                s.panel(Side::Left).tab().loc.clone(),
                s.panel(Side::Right).tab().loc.clone(),
            ],
            seen_draft: false,
        });
        // Окно перерисуется: событие забрано с пустым списком команд.
        Some(Vec::new())
    }

    /// Панель `cmp:`, пока сравнение идёт, — кадр 30 раз в секунду.
    fn tick(&mut self, s: &AppState, _now: Instant) -> Option<Duration> {
        self.life(s);
        [Side::Left, Side::Right]
            .into_iter()
            .any(|side| cmp_panel::running(s, s.panel(side).tab()))
            .then_some(TICK)
    }

    /// Enter — синхронизация →, Shift+Enter — ←, Esc — закрыть.
    fn key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        if self.0.is_none()
            || s.prompts.active().is_some()
            || !s
                .ext
                .get::<crate::UiState>()
                .is_some_and(|u| u.draft.is_none() && u.menu.is_none())
        {
            return None;
        }
        match ks.key.as_str() {
            "enter" if ks.modifiers.shift => Some(vec![Command::new("ops.sync").arg("dir", "rl")]),
            "enter" => Some(vec![Command::new("ops.sync").arg("dir", "lr")]),
            "escape" => {
                self.0 = None;
                Some(Vec::new())
            }
            _ => None,
        }
    }

    fn strip(&self, f: &Frame<'_>, cx: &Context<CoreHost>) -> Option<Div> {
        let c = self.0.as_ref()?;
        let close: super::overlays::Handler =
            Box::new(cx.listener(|h: &mut CoreHost, _: &ClickEvent, _, cx| {
                if let Some(me) = h.features.get_mut::<Compare>() {
                    me.0 = None;
                }
                cx.notify();
            }));
        Some(super::strip::compare_bar(
            c.counts,
            f.theme,
            f.click_all,
            close,
        ))
    }
}

/// Цвет статуса по роли (`screen-cmppanel`).
pub(super) fn tone_color(theme: &Theme, code: &str) -> Hsla {
    theme.c(match cmp_panel::tone(code) {
        Tone::Exec => Role::TypeExec,
        Tone::Accent => Role::AccentText,
        Tone::Modified => Role::Modified,
    })
}

/// Половина значка элементом: 4,2×8, рамка 1, заливка или пунктир.
fn half_el(half: Half, color: Hsla) -> Div {
    let d = div()
        .w(px(4.2))
        .h(px(8.0))
        .rounded(px(1.0))
        .border_1()
        .border_color(color);
    match half {
        Half::Filled => d.bg(color),
        Half::Outline => d,
        Half::Dashed => d.border_dashed(),
    }
}

/// Значок «две панели» 12 элементом: чип заголовка и строка итогов.
fn glyph_el(halves: (Half, Half), color: Hsla) -> Div {
    div()
        .flex_none()
        .size(px(GLYPH))
        .flex()
        .items_center()
        .gap(px(1.6))
        .pl(px(0.5))
        .child(half_el(halves.0, color))
        .child(half_el(halves.1, color))
}

/// Значок статуса в строке списка: квадрат 12 с левым краем `left`, по центру строки.
pub(super) fn paint_glyph(
    window: &mut Window,
    left: Pixels,
    row_y: Pixels,
    row_h: Pixels,
    code: &str,
    color: Hsla,
) {
    let top = row_y + (row_h - px(GLYPH)) / 2.0;
    let (lh, rh) = cmp_panel::halves(code);
    for (half, dx) in [(lh, 1.0), (rh, 6.8)] {
        let rect = Bounds::new(point(left + px(dx), top + px(2.0)), size2(px(4.2), px(8.0)));
        paint_half(window, rect, half, color);
    }
}

fn paint_half(window: &mut Window, b: Bounds<Pixels>, half: Half, color: Hsla) {
    let style = if half == Half::Dashed {
        gpui_kit::BorderStyle::Dashed
    } else {
        gpui_kit::BorderStyle::Solid
    };
    let fill: Hsla = if half == Half::Filled {
        color
    } else {
        gpui_kit::transparent_black()
    };
    window.paint_quad(gpui_kit::quad(b, px(1.0), fill, px(1.0), color, style));
}

/// Место под значком и зазор до подписи — сдвиг текста ячейки «Статус».
pub(super) const GLYPH_ROOM: f32 = GLYPH + 5.0;

/// Чип «Сравнение» перед сторонами (как «Поиск» у `search:`).
fn chip(theme: &Theme) -> Div {
    let text = theme.c(Role::TextPrimary);
    div()
        .flex_none()
        .mr(px(8.0))
        .h(px(20.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::FillControl))
        .text_size(px(11.5))
        .text_color(text)
        .child(glyph_el((Half::Outline, Half::Outline), text))
        .child(SharedString::from(urfin_i18n::current().get("ui-cmp-chip")))
}

/// Заголовок панели сравнения таба `t`: чип, «левая ↔ правая», справа «одинаковых N ·
/// по содержимому»; пока идёт — тонкий неопределённый прогресс, как у `search:`.
pub(super) fn path_bar(
    s: &AppState,
    side: Side,
    t: &Tab,
    store: &SearchStore,
    theme: &Theme,
    field: Option<&super::field::Field>,
    cx: &Listen,
) -> Div {
    let active = s.active == side;
    let bar = div()
        .relative()
        .h(px(theme.m(Metric::HBreadcrumb)))
        .flex_none()
        .flex()
        .items_center()
        .px(px(theme.m(Metric::PadRowX)))
        .text_size(px(12.5))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .when(active && theme.vivid(), |d| d.bg(theme.c(Role::AccentSoft)));
    if let Some(input) = super::field::path_input(field, side) {
        return bar.child(input);
    }
    let line = div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(theme.c(if active {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        }))
        .when(active, |d| {
            d.border_b_2()
                .border_color(theme.c(Role::AccentFill))
                .pb(px(3.0))
                .mt(px(5.0))
        })
        .child(SharedString::from(cmp_panel::head_line(store)));
    let note = div()
        .flex_none()
        .ml(px(8.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(cmp_panel::head_note(
            store,
            cmp_panel::totals(s, t),
        )));
    let progress = (!store.is_done()).then(|| {
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(-1.0))
            .h(px(2.0))
            .overflow_hidden()
            .bg(theme.c(Role::StrokeControl))
            .child(super::du::sweep(theme).top_0())
    });
    bar.child(super::panel::head_buttons(s, side, true, theme, cx))
        .child(chip(theme))
        .child(line)
        .child(div().flex_1())
        .child(note)
        .children(progress)
}

/// Отпечаток заголовка: итоги, ход сравнения и место бегущего отрезка.
pub(super) fn print(s: &AppState, t: &Tab) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let store = cmp_panel::store(s, t)?;
    let mut h = std::hash::DefaultHasher::new();
    cmp_panel::totals(s, t).hash(&mut h);
    (
        store.files_len(),
        store.info.files.load(std::sync::atomic::Ordering::Relaxed),
        store.is_done(),
    )
        .hash(&mut h);
    if !store.is_done() {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() / 16)
            .hash(&mut h);
    }
    Some(h.finish())
}

/// Под строку итогов в сетке строк: есть итоги — 28.
pub(super) fn extra_h(s: &AppState, t: &Tab) -> f32 {
    if cmp_panel::totals(s, t).is_some() {
        TOTALS_H
    } else {
        0.0
    }
}

/// Строка итогов внизу панели (`screen-cmppanel`): 28, фон `header`, рамка `border`
/// сверху; по статусу — значок, подпись и число (600) цветом статуса.
pub(super) fn totals_bar(s: &AppState, t: &Tab, theme: &Theme) -> Option<Div> {
    let totals = cmp_panel::totals(s, t)?;
    let mut bar = div()
        .id("cmp-totals")
        .h(px(TOTALS_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(14.0))
        .px(px(12.0))
        .overflow_hidden()
        .whitespace_nowrap()
        .bg(theme.c(Role::Header))
        .border_t_1()
        .border_color(theme.c(Role::StrokeControl))
        .text_size(px(12.0));
    for (code, n) in cmp_panel::STATUSES.iter().zip(totals.counts) {
        let color = tone_color(theme, code);
        bar = bar.child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_color(color)
                .child(glyph_el(cmp_panel::halves(code), color))
                .child(SharedString::from(cmp_panel::label(code)))
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(SharedString::from(n.to_string())),
                ),
        );
    }
    // Внизу панели: список над ней тянется на остаток высоты.
    Some(div().mt_auto().child(bar))
}

/// Сцены сравнения папок (D-04).
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "screen-compare",
        steps: &["ui.cancel", "sel.none", "ops.compare"],
    },
    Scene {
        name: "screen-syncplan",
        steps: &["ops.sync dir=lr"],
    },
    Scene {
        name: "sync-closed",
        steps: &["ui.cancel", "sel.none"],
    },
];

/// Панель сравнения `cmp:` на фикстуре (S8-39, `screen-cmppanel`): итоги готовы, отмечены
/// оба `compare.rs`, курсор — на втором; после панелей результатов поиска.
pub(super) const CMP_SCENES: &[Scene] = &[
    Scene {
        name: "cmppanel-plain",
        steps: &[
            "resize 1280x760",
            "panel.focus side=left",
            "screens.search kind=cmp",
        ],
    },
    Scene {
        name: "screen-cmppanel",
        steps: &[
            r"panel.cursor.to name=crates\ops\src\ops\compare.rs",
            "sel.toggle",
            r"panel.cursor.to name=crates\ui\src\gpui_shell\compare.rs",
            "sel.toggle",
        ],
    },
    Scene {
        name: "cmppanel-closed",
        steps: &["tab.close"],
    },
];
