//! Итог сравнения папок (D-04, `screen-compare.png`) как фича окна (ADR-0022): строка
//! внизу, метки в панелях, Enter/Shift+Enter — синхронизация, Esc — закрыть.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui_kit::{ClickEvent, Context, Div, Keystroke};
use urfin_core::{AppState, Args, Command, Side};

use super::CoreHost;
use super::features::{Frame, ShellFeature};
use super::panel::Note;

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
            for &k in t.listing.order.iter().take(NOTES_MAX) {
                if !t.selection.contains(k) {
                    continue;
                }
                let Some(e) = t.listing.entries.get(k as usize) else {
                    continue;
                };
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

    fn tick(&mut self, s: &AppState, _now: Instant) -> Option<Duration> {
        self.life(s);
        None
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
