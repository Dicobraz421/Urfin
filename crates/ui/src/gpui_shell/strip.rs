//! Встроенные строки (D-04, `docs/design/DESIGN.md`, №63): F5/F6, F8, F7, отметка
//! группы, групповое переименование, вопросы (конфликт) и быстрый поиск. Строка
//! **накрывает** низ панелей — на ширину активной панели или на всю ширину, если
//! касается обеих; список не перестраивается, сетка панели уменьшается на высоту строки,
//! чтобы курсор не прятался под ней. Клавиши — подписями на кнопках, в keybar не
//! повторяются.

use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, Stateful, div, px};
use urfin_core::{Anchor, AppState, Command, PromptDetail, Side};

use super::theme::Theme;
use crate::UiState;
use crate::tokens::Role;

/// Чью ширину накрывает строка.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Full,
    Panel(Side),
}

impl Scope {
    pub(super) fn covers(self, side: Side) -> bool {
        match self {
            Scope::Full => true,
            Scope::Panel(s) => s == side,
        }
    }

    /// Левый край и ширина по ширинам панелей.
    pub(super) fn bounds(self, widths: [Option<f32>; 2], divider: f32) -> (f32, f32) {
        let left = widths[0].unwrap_or(0.0);
        let right = widths[1].unwrap_or(0.0);
        let both = left
            + right
            + if widths[0].is_some() && widths[1].is_some() {
                divider
            } else {
                0.0
            };
        match self {
            Scope::Full => (0.0, both),
            Scope::Panel(Side::Left) => (0.0, if widths[0].is_some() { left } else { both }),
            Scope::Panel(Side::Right) => {
                if widths[0].is_some() && widths[1].is_some() {
                    (left + divider, right)
                } else {
                    (0.0, both)
                }
            }
        }
    }
}

/// Строка в одну линию: поля 10, кнопка 30, черта 2.
pub(super) const H_ONE: f32 = 54.0;
/// Строка в две линии (F5): поля 12, две линии по 30, зазор 10, черта 2.
pub(super) const H_TWO: f32 = 96.0;
/// Строка в три линии (Ctrl+A): флажки, время, кнопки.
pub(super) const H_THREE: f32 = 138.0;
/// Быстрый поиск.
pub(super) const H_QUICK: f32 = 34.0;
/// Вопрос: заголовок, пояснение, кнопки.
pub(super) const H_PROMPT: f32 = 118.0;
/// Вопрос о конфликте: ещё сравнение в три строки.
pub(super) const H_CONFLICT: f32 = 186.0;
/// Строка дерева превью F7.
pub(super) const H_TREE_ROW: f32 = 22.0;

/// Что накрывает панели сейчас: область и высота (для сетки панели).
pub(super) fn cover(s: &AppState, rows: usize, compare: bool) -> Option<(Scope, f32)> {
    // Строки подключения SSH (F03-05): вопрос или «Новое подключение».
    if s.prompts.active().is_none()
        && let Some(c) = super::ssh::cover(s)
    {
        return Some(c);
    }
    if let Some(p) = s.prompts.active() {
        let scope = match p.spec.anchor {
            Anchor::Panel(side) => Scope::Panel(side),
            _ => Scope::Full,
        };
        let h = if matches!(p.spec.detail, Some(PromptDetail::Conflict { .. })) {
            H_CONFLICT
        } else if super::overlays::error_line(p) {
            H_ONE
        } else {
            H_PROMPT
        };
        return Some((scope, h));
    }
    if let Some(d) = s.ext.get::<UiState>().and_then(|u| u.draft.as_ref()) {
        return Some(match d.cmd.as_str() {
            "ops.copy" if compare => (Scope::Full, H_ONE),
            "ops.copy" | "ops.move" | "ops.link" => (Scope::Full, H_TWO),
            "ops.mkdir" => (
                Scope::Panel(s.active),
                H_TWO
                    + if rows > 0 {
                        8.0 + H_TREE_ROW * f32::from(u16::try_from(rows).unwrap_or(0))
                    } else {
                        0.0
                    },
            ),
            "panel.filter" | "ops.touch" => (Scope::Panel(s.active), H_TWO),
            // «Найти пакет» — у вкладки пакетов (`screen-pkgsearch`).
            "pkg.search" => (Scope::Panel(s.active), super::pkg::H_SEARCH),
            "ops.attrs" | super::field::MULTI_RENAME => (Scope::Panel(s.active), H_THREE),
            _ => (Scope::Panel(s.active), H_ONE),
        });
    }
    // Обрыв связи панели `sftp://` (F03-06, `screen-sftplost`) — под быстрым поиском.
    let quick = s
        .ext
        .get::<urfin_keymap::KeymapView>()
        .is_some_and(|k| k.qs.is_some() || k.pending.is_some());
    if !quick && let Some(r) = crate::ssh::lost_row(s) {
        return Some((Scope::Panel(r.side), super::sftp::H_LOST));
    }
    // Приветствие (D-02) — на всю ширину.
    if s.ext.get::<UiState>().is_some_and(|u| u.welcome.is_some()) {
        return Some((Scope::Full, H_ONE));
    }
    // Итог сравнения (D-04, `screen-compare.png`) — на всю ширину.
    if compare {
        return Some((Scope::Full, H_ONE));
    }
    let keys = s.ext.get::<urfin_keymap::KeymapView>()?;
    if let Some(q) = &keys.qs {
        return Some((Scope::Panel(q.side), H_QUICK));
    }
    keys.pending
        .as_ref()
        .map(|_| (Scope::Panel(s.active), H_QUICK))
}

/// Тон строки: фон и черта сверху.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    /// Обычная: фон `header`, черта `accent`.
    Plain,
    /// Удаление: фон предупреждения об опасности, черта `modified`.
    Danger,
    /// Ждёт ответа (конфликт): фон предупреждения, черта `context_remote`.
    Waiting,
}

/// Контейнер строки.
pub(super) fn container(theme: &Theme, tone: Tone, pad_x: f32, pad_y: f32) -> Div {
    let (bg, line) = match tone {
        Tone::Plain => (Role::Header, Role::AccentFill),
        Tone::Danger => (Role::CriticalBg, Role::Modified),
        Tone::Waiting => (Role::CautionBg, Role::ContextRemote),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .px(px(pad_x))
        .py(px(pad_y))
        .bg(theme.c(bg))
        .border_t_2()
        .border_color(theme.c(line))
        .text_size(px(13.0))
}

/// Вид кнопки строки.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Btn {
    Primary,
    Danger,
    Normal,
}

/// Кнопка с подписью клавиши (D-04: клавиши — на кнопках, не в keybar).
pub(super) fn button(
    theme: &Theme,
    id: String,
    label: String,
    key: &str,
    kind: Btn,
) -> Stateful<Div> {
    let (bg, border, fg, key_fg) = match kind {
        Btn::Primary => (
            Role::AccentFill,
            Role::AccentFill,
            Role::AccentOn,
            Role::AccentOn,
        ),
        Btn::Danger => (
            Role::Modified,
            Role::Modified,
            Role::AccentOn,
            Role::AccentOn,
        ),
        Btn::Normal => (
            Role::LayerPanel,
            Role::StrokeControl,
            Role::TextPrimary,
            Role::TextSecondary,
        ),
    };
    let key_kind = kind;
    div()
        .id(SharedString::from(id))
        .flex_none()
        .h(px(30.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(border))
        .bg(theme.c(bg))
        .text_color(theme.c(fg))
        .whitespace_nowrap()
        .child(SharedString::from(label))
        .when(!key.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.c(key_fg))
                    .when(key_kind != Btn::Normal, |d| d.opacity(0.8))
                    .child(SharedString::from(key.to_owned())),
            )
        })
}

/// Флажок: значок и подпись; `enabled == false` — только показ (нет такой опции).
pub(super) fn check(
    theme: &Theme,
    id: String,
    on: bool,
    label: String,
    enabled: bool,
) -> Stateful<Div> {
    let glyph = if on { "\u{E73A}" } else { "\u{E739}" };
    div()
        .id(SharedString::from(id))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(12.5))
        .whitespace_nowrap()
        .when(!enabled, |d| d.opacity(0.6))
        .child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(if on {
                    Role::AccentText
                } else {
                    Role::TextSecondary
                }))
                .child(SharedString::from(glyph)),
        )
        .child(SharedString::from(label))
}

/// Флажок в три состояния (атрибуты Ctrl+A): да, нет, «у целей по-разному» (`None`).
pub(super) fn check3(theme: &Theme, id: String, on: Option<bool>, label: String) -> Stateful<Div> {
    let glyph = match on {
        Some(true) => "\u{E73A}",
        Some(false) => "\u{E739}",
        None => "\u{E73C}",
    };
    div()
        .id(SharedString::from(id))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(12.5))
        .whitespace_nowrap()
        .child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(if on == Some(false) {
                    Role::TextSecondary
                } else {
                    Role::AccentText
                }))
                .child(SharedString::from(glyph)),
        )
        .child(SharedString::from(label))
}

/// Быстрый поиск (D-04, `screen-quick.png`): строка 34 внизу активной панели — значок,
/// набранное с кареткой, справа число совпадений; не принятый символ — черта красная.
/// Набранный аккорд (`Ctrl+K …`) — такой же строкой.
pub(super) fn quick(s: &AppState, theme: &Theme) -> Option<Div> {
    let i18n = urfin_i18n::current();
    let keys = s.ext.get::<urfin_keymap::KeymapView>()?;
    let row = |line: Role| {
        div()
            .h(px(H_QUICK))
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .bg(theme.c(Role::Header))
            .border_t_2()
            .border_color(theme.c(line))
    };
    if let Some(q) = &keys.qs {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("count", u64::from(q.matches));
        let line = if q.miss_at.is_some() {
            Role::CriticalFg
        } else {
            Role::AccentFill
        };
        return Some(
            row(line)
                .child(
                    div()
                        .font_family("Segoe Fluent Icons")
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::new_static("\u{E721}")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .font_family("Cascadia Mono")
                        .text_size(px(12.5))
                        .child(SharedString::from(q.mask.to_string()))
                        .child(
                            div()
                                .ml(px(1.0))
                                .w(px(2.0))
                                .h(px(15.0))
                                .bg(theme.c(Role::AccentFill)),
                        ),
                )
                .child(
                    div()
                        .ml_auto()
                        .text_size(px(12.0))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(i18n.fmt("ui-qs-matches", &a))),
                ),
        );
    }
    let chord = keys.pending.as_deref()?;
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("chord", chord.to_owned());
    Some(
        row(Role::AccentFill)
            .text_size(px(12.5))
            .child(SharedString::from(i18n.fmt("ui-chord-pending", &a))),
    )
}

/// Итог сравнения папок (`screen-compare.png`): «только слева 2 · только справа 1 ·
/// новее слева 1», `secondary` — «одинаковых N»; Синхронизировать → (Enter), ←
/// (Shift+Enter), Закрыть (Esc).
pub(super) fn compare_bar(
    counts: [u32; 6],
    theme: &Theme,
    click: super::overlays::ClickAll<'_>,
    close: super::overlays::Handler,
) -> Div {
    let i18n = urfin_i18n::current();
    let keys = [
        "ui-compare-only-left",
        "ui-compare-only-right",
        "ui-compare-newer-left",
        "ui-compare-newer-right",
        "ui-compare-differs",
    ];
    let parts: Vec<String> = keys
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("n", n);
            i18n.fmt(k, &a)
        })
        .collect();
    let mut eq = urfin_i18n::FluentArgs::new();
    eq.set("n", counts[5]);
    let row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(Role::AccentText))
                .child(SharedString::new_static("\u{E8F4}")),
        )
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(i18n.get("ui-compare-title"))),
        )
        .child(SharedString::from(parts.join(" · ")))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(format!(
                    "· {}",
                    i18n.fmt("ui-compare-equal", &eq)
                ))),
        )
        .child(
            button(
                theme,
                "compare-lr".into(),
                i18n.get("ui-compare-sync-lr"),
                "Enter",
                Btn::Primary,
            )
            .on_click(click(vec![Command::new("ops.sync").arg("dir", "lr")])),
        )
        .child(
            button(
                theme,
                "compare-rl".into(),
                i18n.get("ui-compare-sync-rl"),
                "Shift+Enter",
                Btn::Normal,
            )
            .on_click(click(vec![Command::new("ops.sync").arg("dir", "rl")])),
        )
        .child(
            button(
                theme,
                "compare-close".into(),
                i18n.get("ui-button-close"),
                "Esc",
                Btn::Normal,
            )
            .on_click(close),
        );
    container(theme, Tone::Plain, 16.0, 10.0).child(row)
}

/// Приветствие (`screen-firstrun.png`, `screen-whatsnew.png`): первый запуск — выбор
/// клавиш Far / DN, «Тема как в Windows», Готово / Позже; обновление — «обновлён до X ·
/// с Y», Закрыть.
#[allow(
    clippy::too_many_lines,
    reason = "первый запуск и обновление — две ветки"
)]
pub(super) fn welcome_bar(
    w: &crate::state::Welcome,
    dn: bool,
    follow: bool,
    theme: &Theme,
    click: super::overlays::ClickAll<'_>,
    pick: &dyn Fn(u8) -> super::overlays::Handler,
) -> Div {
    let i18n = urfin_i18n::current();
    let icon = div()
        .font_family("Segoe Fluent Icons")
        .text_color(theme.c(Role::AccentText))
        .child(SharedString::new_static("\u{E8A1}"));
    let note = |t: String| {
        div()
            .whitespace_nowrap()
            .text_size(px(12.5))
            .text_color(theme.c(Role::TextSecondary))
            .child(SharedString::from(t))
    };
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(12.0))
        .child(icon);
    match w {
        crate::state::Welcome::First => {
            let seg = |id: &str, label: &'static str, on: bool, which: u8| {
                div()
                    .id(SharedString::from(id.to_owned()))
                    .h(px(28.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .when(on, |d| d.bg(theme.c(Role::FillCursor)))
                    .child(SharedString::new_static(label))
                    .on_click(pick(which))
            };
            let done = {
                let mut c =
                    Command::new("ui.welcome.done").arg("preset", if dn { "dn" } else { "far" });
                if follow {
                    c = c.arg("follow_os", true);
                }
                c
            };
            row = row
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(SharedString::from(i18n.get("ui-welcome-title"))),
                )
                .child(note(i18n.get("ui-welcome-keys")))
                .child(
                    div()
                        .flex()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.c(Role::StrokeControl))
                        .overflow_hidden()
                        .child(seg("welcome-far", "Far", !dn, 0))
                        .child(
                            seg("welcome-dn", "DN", dn, 1)
                                .border_l_1()
                                .border_color(theme.c(Role::StrokeControl)),
                        ),
                )
                .child(note(i18n.get("ui-welcome-hint")))
                .child(
                    check(
                        theme,
                        "welcome-theme".into(),
                        follow,
                        i18n.get("ui-welcome-theme"),
                        true,
                    )
                    .on_click(pick(2)),
                )
                .child(
                    button(
                        theme,
                        "welcome-done".into(),
                        i18n.get("ui-welcome-done"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(click(vec![done])),
                )
                .child(
                    button(
                        theme,
                        "welcome-later".into(),
                        i18n.get("ui-welcome-later"),
                        "Esc",
                        Btn::Normal,
                    )
                    .on_click(click(vec![Command::new("ui.welcome.done")])),
                );
        }
        crate::state::Welcome::Updated { from, to } => {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("from", from.clone());
            a.set("to", to.clone());
            row = row
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(SharedString::from(i18n.fmt("ui-welcome-updated", &a))),
                )
                .child(
                    button(
                        theme,
                        "welcome-close".into(),
                        i18n.get("ui-button-close"),
                        "Esc",
                        Btn::Primary,
                    )
                    .on_click(click(vec![Command::new("ui.welcome.done")])),
                );
        }
    }
    container(theme, Tone::Plain, 16.0, 10.0).child(row)
}
