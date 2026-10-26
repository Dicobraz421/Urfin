//! Job-бар и список задач (D-06, `screen-jobs.png`, `screen-jobslist.png`).
//!
//! Свёрнутый бар **занимает место** над командной строкой (№63): строка 30 на активную
//! задачу — вид, «что → куда», прогресс 4 px, «из · скорость · ещё», Пауза, ✕. Ждёт
//! ответа — полоса и текст `context_remote`. F11 (`ui.jobs.toggle`, №75) раскрывает
//! список вверх на всю ширину: «Задачи N» + Свернуть, «✓ Готово N · Очистить», строки
//! 38 со своей кнопкой (Ответить / Повторить / Без очереди). Скорость — по замерам
//! прогресса в UI (сглаживание), ядро её не считает.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClickEvent, Context, Div, Keystroke, SharedString, div, px, relative};
use urfin_core::{AppState, Command, Job, JobState};
use urfin_i18n::FluentArgs;

use super::CoreHost;
use super::features::{Frame, ShellFeature};
use super::overlays::{Click, ClickAll};
use super::strip::{Btn, button};
use super::theme::Theme;
use crate::tokens::{Metric, Role};

/// Строка свёрнутого бара и раскрытого списка.
const H_ROW: f32 = 30.0;
const H_LIST_ROW: f32 = 38.0;
/// Вес нового замера скорости.
const RATE_ALPHA: f64 = 0.3;
/// Замер не чаще.
const RATE_STEP_MS: u128 = 250;

/// Скорость задач по замерам: (время, байт или штук, сглаженная скорость в секунду).
#[derive(Default)]
pub(super) struct Rates(HashMap<u64, (Instant, u64, f64)>);

impl Rates {
    /// Новый замер по всем идущим задачам; ушедшие забываются.
    pub(super) fn sample(&mut self, s: &AppState, now: Instant) {
        self.0
            .retain(|id, _| s.jobs.iter().any(|j| j.id.0 == *id && j.state.is_active()));
        for j in s
            .jobs
            .iter()
            .filter(|j| matches!(j.state, JobState::Running))
        {
            let p = j.progress.snapshot();
            let done = if p.bytes_total > 0 {
                p.bytes_done
            } else {
                p.items_done
            };
            let e = self.0.entry(j.id.0).or_insert((now, done, 0.0));
            let dt = now.duration_since(e.0);
            if dt.as_millis() < RATE_STEP_MS {
                continue;
            }
            #[allow(clippy::cast_precision_loss, reason = "скорость для показа")]
            let rate = done.saturating_sub(e.1) as f64 / dt.as_secs_f64();
            e.2 = if e.2 == 0.0 {
                rate
            } else {
                RATE_ALPHA * rate + (1.0 - RATE_ALPHA) * e.2
            };
            e.0 = now;
            e.1 = done;
        }
    }

    fn get(&self, j: &Job) -> f64 {
        self.0.get(&j.id.0).map_or(0.0, |e| e.2)
    }
}

/// Список и выбор в нём (раскрыт F11).
#[derive(Default)]
pub(super) struct List {
    pub sel: usize,
    /// Раскрыта группа «Готово».
    pub done_open: bool,
    /// Очищенные из списка завершённые задачи.
    pub cleared: Vec<u64>,
}

/// Активные задачи (бар), по порядку запуска.
fn active(s: &AppState) -> Vec<&Job> {
    s.jobs.iter().filter(|j| j.state.is_active()).collect()
}

/// Завершённые и не очищенные.
fn done<'a>(s: &'a AppState, l: &List) -> Vec<&'a Job> {
    s.jobs
        .iter()
        .filter(|j| !j.state.is_active() && !l.cleared.contains(&j.id.0))
        .collect()
}

/// Доля выполненного 0…1.
fn frac(j: &Job) -> f32 {
    let p = j.progress.snapshot();
    #[allow(clippy::cast_precision_loss, reason = "доля для полосы")]
    let f = if p.bytes_total > 0 {
        p.bytes_done as f64 / p.bytes_total as f64
    } else if p.items_total > 0 {
        p.items_done as f64 / p.items_total as f64
    } else {
        0.0
    };
    #[allow(clippy::cast_possible_truncation, reason = "доля 0…1")]
    let f = f.clamp(0.0, 1.0) as f32;
    if matches!(j.state, JobState::Done(_)) {
        1.0
    } else {
        f
    }
}

/// Подпись вида задачи: «Копирование», «Перемещение»…; неизвестный — пусто.
fn kind_label(j: &Job) -> String {
    let key = format!("ui-job-{}", j.kind.replace('_', "-"));
    let i18n = urfin_i18n::current();
    if i18n.has(&key) {
        i18n.get(&key)
    } else {
        String::new()
    }
}

/// Значок вида (Segoe Fluent Icons).
fn kind_icon(j: &Job) -> &'static str {
    match j.kind {
        "move" => "\u{E8AB}",
        "delete" | "trash" => "\u{E74D}",
        "link" => "\u{E71B}",
        "rename" => "\u{E8AC}",
        _ => "\u{E8C8}",
    }
}

/// «ещё 47 с», «ещё 3 мин», «ещё 1 ч 5 мин».
fn eta(left: f64) -> Option<String> {
    if !left.is_finite() || left <= 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "секунды"
    )]
    let s = left.ceil() as u64;
    let mut a = FluentArgs::new();
    let key = if s < 60 {
        a.set("s", s);
        "ui-job-eta-s"
    } else if s < 3600 {
        a.set("m", s.div_ceil(60));
        "ui-job-eta-m"
    } else {
        a.set("h", s / 3600);
        a.set("m", (s % 3600) / 60);
        "ui-job-eta-h"
    };
    Some(urfin_i18n::current().fmt(key, &a))
}

/// Состояние строки текстом и его цвет: «1,2 из 3,4 ГБ · 48 МБ/с · ещё 47 с»,
/// «ждёт ответа», «пауза», «в очереди», «ошибка: …», «готово».
#[allow(clippy::many_single_char_names, reason = "состояние, задача, прогресс")]
fn status(s: &AppState, j: &Job, rate: f64) -> (String, Role) {
    let i18n = urfin_i18n::current();
    let p = j.progress.snapshot();
    match &j.state {
        JobState::Waiting(id) => {
            // Что спрашивает: «ждёт ответа: нет доступа» (`screen-operror.png`).
            let text = s.prompts.get(*id).map_or_else(
                || i18n.get("ui-job-waiting"),
                |p| {
                    let mut a = FluentArgs::new();
                    a.set("what", p.spec.title.to_lowercase());
                    i18n.fmt("ui-job-waiting-for", &a)
                },
            );
            (text, Role::ContextRemote)
        }
        JobState::Paused => (i18n.get("ui-job-paused"), Role::TextSecondary),
        JobState::Queued => (i18n.get("ui-job-queued"), Role::TextSecondary),
        JobState::Failed(e) => {
            let mut a = FluentArgs::new();
            a.set("error", e.to_string());
            (i18n.fmt("ui-job-failed", &a), Role::Modified)
        }
        JobState::Cancelled => (i18n.get("ui-job-cancelled"), Role::TextSecondary),
        JobState::Done(sum) => {
            let mut a = FluentArgs::new();
            a.set("n", sum.items);
            (i18n.fmt("ui-job-done", &a), Role::TextSecondary)
        }
        JobState::Running => {
            let mut parts = Vec::with_capacity(3);
            let bytes = p.bytes_total > 0;
            let (done, total) = if bytes {
                (p.bytes_done, p.bytes_total)
            } else {
                (p.items_done, p.items_total)
            };
            if total > 0 {
                let mut a = FluentArgs::new();
                if bytes {
                    a.set("done", urfin_i18n::fmt::bytes_with(&i18n, done));
                    a.set("total", urfin_i18n::fmt::bytes_with(&i18n, total));
                } else {
                    a.set("done", group(done));
                    a.set("total", group(total));
                }
                parts.push(i18n.fmt("ui-job-of", &a));
            }
            if bytes && rate > 0.0 {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "байт/с"
                )]
                let r = rate as u64;
                let mut a = FluentArgs::new();
                a.set("rate", urfin_i18n::fmt::bytes_with(&i18n, r));
                parts.push(i18n.fmt("ui-job-rate", &a));
            }
            #[allow(clippy::cast_precision_loss, reason = "оценка времени")]
            let left = (total.saturating_sub(done)) as f64 / rate;
            if rate > 0.0
                && let Some(e) = eta(left)
            {
                parts.push(e);
            }
            (parts.join(" · "), Role::TextSecondary)
        }
    }
}

/// Число с разделителем тысяч (неразрывный пробел): 12 870.
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('\u{202F}');
        }
        out.push(c);
    }
    out
}

/// Полоса прогресса 4 px: `accent`, ждёт ответа — `context_remote`, ошибка — `modified`.
fn progress(j: &Job, theme: &Theme) -> Div {
    let fill = match j.state {
        JobState::Waiting(_) => Role::ContextRemote,
        JobState::Failed(_) => Role::Modified,
        JobState::Queued | JobState::Paused | JobState::Cancelled => Role::TextTertiary,
        _ => Role::AccentFill,
    };
    div()
        .flex_1()
        .min_w(px(60.0))
        .max_w(px(480.0))
        .h(px(4.0))
        .rounded(px(2.0))
        .bg(theme.c(Role::FillControl))
        .child(
            div()
                .h_full()
                .w(relative(frac(j)))
                .rounded(px(2.0))
                .bg(theme.c(fill)),
        )
}

fn id_arg(j: &Job) -> i64 {
    i64::try_from(j.id.0).unwrap_or(i64::MAX)
}

/// Пауза или «Продолжить» для идущей или приостановленной задачи.
fn pause_cmd(j: &Job) -> Option<(Command, &'static str)> {
    match j.state {
        JobState::Running => Some((
            Command::new("job.pause").arg("id", id_arg(j)),
            "ui-job-pause",
        )),
        JobState::Paused => Some((
            Command::new("job.resume").arg("id", id_arg(j)),
            "ui-job-resume",
        )),
        _ => None,
    }
}

/// Квадратная кнопка-значок 24 (✕, пауза в списке).
fn icon_button(theme: &Theme, id: String, glyph: &'static str) -> gpui_kit::Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .flex_none()
        .w(px(26.0))
        .h(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::LayerPanel))
        .font_family("Segoe Fluent Icons")
        .text_size(px(10.0))
        .text_color(theme.c(Role::TextPrimary))
        .child(SharedString::new_static(glyph))
}

/// С ошибками: упала или завершилась с ошибками элементов.
fn has_errors(j: &Job) -> bool {
    match &j.state {
        JobState::Failed(_) => true,
        JobState::Done(sum) => sum.errors > 0,
        _ => false,
    }
}

/// Свёрнутый job-бар: строка 30 на активную задачу. Активных нет, а у завершённых есть
/// ошибки — сводка «Задачи N · ✓ … · ⓘ … с ошибками · Развернуть F11»
/// (`screen-toasterror.png`).
pub(super) fn bar(
    s: &AppState,
    l: &List,
    rates: &Rates,
    theme: &Theme,
    click: Click<'_>,
) -> Option<Div> {
    if s.ext
        .get::<crate::UiState>()
        .is_some_and(|u| u.jobs_expanded)
    {
        return None;
    }
    let jobs = active(s);
    if jobs.is_empty() {
        return summary(s, l, theme, click);
    }
    let i18n = urfin_i18n::current();
    let pad = theme.m(Metric::PadRowX);
    let mut col = div()
        .flex_none()
        .flex()
        .flex_col()
        .py(px(2.0))
        .bg(theme.c(Role::Header))
        .border_t_1()
        .border_color(theme.c(Role::StrokeDivider));
    for j in jobs {
        let (text, role) = status(s, j, rates.get(j));
        let mut row = div()
            .h(px(H_ROW))
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(pad))
            .text_size(px(12.5))
            .child(
                div()
                    .w(px(110.0))
                    .flex_none()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(SharedString::from(kind_label(j))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(px(300.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(j.title.clone())),
            )
            .child(progress(j, theme))
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(theme.c(role))
                    .child(SharedString::from(text)),
            );
        if let Some((cmd, label)) = pause_cmd(j) {
            row = row.child(
                button(
                    theme,
                    format!("job-pause-{}", j.id.0),
                    i18n.get(label),
                    "",
                    Btn::Normal,
                )
                .h(px(24.0))
                .px(px(10.0))
                .on_click(click(cmd)),
            );
        }
        // Отмена посреди файла — тоже (S5-06).
        row = row.child(
            icon_button(theme, format!("job-cancel-{}", j.id.0), "\u{E711}")
                .on_click(click(Command::new("job.cancel").arg("id", id_arg(j)))),
        );
        col = col.child(row);
    }
    Some(col)
}

/// Сводка завершённых задач, если среди них есть с ошибками.
fn summary(s: &AppState, l: &List, theme: &Theme, click: Click<'_>) -> Option<Div> {
    let fin = done(s, l);
    if !fin.iter().any(|j| has_errors(j)) {
        return None;
    }
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    a.set("n", fin.len());
    let sep = || {
        div()
            .text_color(theme.c(Role::TextTertiary))
            .child(SharedString::new_static("·"))
    };
    let mut row = div()
        .flex_none()
        .h(px(H_ROW))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(theme.m(Metric::PadRowX)))
        .bg(theme.c(Role::Header))
        .border_t_1()
        .border_color(theme.c(Role::StrokeDivider))
        .text_size(px(12.5))
        .child(SharedString::from(i18n.fmt("ui-jobs-title", &a)));
    for j in fin.iter().rev().take(3) {
        let bad = has_errors(j);
        row = row.child(sep()).child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .font_family("Segoe Fluent Icons")
                        .text_color(theme.c(if bad { Role::Modified } else { Role::SuccessFg }))
                        .child(SharedString::new_static(if bad {
                            "\u{E783}"
                        } else {
                            "\u{E73E}"
                        })),
                )
                .child(SharedString::from(kind_label(j)))
                .when(bad, |d| {
                    d.child(
                        div()
                            .text_color(theme.c(Role::Modified))
                            .child(SharedString::from(i18n.get("ui-jobs-with-errors"))),
                    )
                }),
        );
    }
    Some(
        row.child(
            button(
                theme,
                "jobs-expand".into(),
                i18n.get("ui-jobs-expand"),
                "F11",
                Btn::Normal,
            )
            .h(px(24.0))
            .on_click(click(Command::new("ui.jobs.toggle"))),
        ),
    )
}

/// Строки раскрытого списка по порядку: активные, затем (если раскрыта) готовые.
fn rows<'a>(s: &'a AppState, l: &List) -> Vec<&'a Job> {
    let mut v = active(s);
    if l.done_open {
        v.extend(done(s, l));
    }
    v
}

/// Клавиша при раскрытом списке: ↑↓ — задача, Пробел — пауза/продолжить, Del —
/// отменить, Esc и F11 — свернуть.
pub(super) fn key(s: &AppState, l: &mut List, ks: &Keystroke) -> Option<Vec<Command>> {
    if !s
        .ext
        .get::<crate::UiState>()
        .is_some_and(|u| u.jobs_expanded)
    {
        return None;
    }
    let list = rows(s, l);
    let n = list.len();
    l.sel = l.sel.min(n.saturating_sub(1));
    let cur = list.get(l.sel);
    Some(match ks.key.as_str() {
        "escape" | "f11" => vec![Command::new("ui.jobs.toggle")],
        "up" if n > 0 => {
            l.sel = (l.sel + n - 1) % n;
            Vec::new()
        }
        "down" if n > 0 => {
            l.sel = (l.sel + 1) % n;
            Vec::new()
        }
        "space" => cur.and_then(|j| pause_cmd(j)).map(|(c, _)| vec![c])?,
        "delete" => cur
            .filter(|j| j.state.is_active())
            .map(|j| vec![Command::new("job.cancel").arg("id", id_arg(j))])?,
        _ => return None,
    })
}

/// Раскрытый список задач: накрывает низ панелей над командной строкой, на всю ширину.
#[allow(clippy::too_many_lines, reason = "шапка, группа «Готово», строки")]
pub(super) fn list(
    s: &AppState,
    l: &List,
    rates: &Rates,
    theme: &Theme,
    click: ClickAll<'_>,
    toggle_done: super::overlays::Handler,
    clear_done: super::overlays::Handler,
) -> Option<Div> {
    if !s
        .ext
        .get::<crate::UiState>()
        .is_some_and(|u| u.jobs_expanded)
    {
        return None;
    }
    let i18n = urfin_i18n::current();
    let pad = theme.m(Metric::PadRowX);
    let act = active(s);
    let fin = done(s, l);
    let mut a = FluentArgs::new();
    a.set("n", act.len());
    let head = div()
        .h(px(H_LIST_ROW))
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(pad))
        .child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static("\u{E700}")),
        )
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(i18n.fmt("ui-jobs-title", &a))),
        )
        .child(
            button(
                theme,
                "jobs-collapse".into(),
                i18n.get("ui-jobs-collapse"),
                "Esc",
                Btn::Normal,
            )
            .h(px(26.0))
            .on_click(click(vec![Command::new("ui.jobs.toggle")])),
        );
    let mut col = div()
        .flex()
        .flex_col()
        .bg(theme.c(Role::Header))
        .border_t_2()
        .border_color(theme.c(Role::AccentFill))
        .child(head);
    if !fin.is_empty() {
        let mut d = FluentArgs::new();
        d.set("n", fin.len());
        col = col.child(
            div()
                .h(px(32.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(pad))
                .border_t_1()
                .border_color(theme.c(Role::StrokeDivider))
                .text_size(px(12.5))
                .child(
                    div()
                        .id("jobs-done-toggle")
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .font_family("Segoe Fluent Icons")
                                .text_size(px(9.0))
                                .text_color(theme.c(Role::TextSecondary))
                                .child(SharedString::new_static(if l.done_open {
                                    "\u{E70D}"
                                } else {
                                    "\u{E76C}"
                                })),
                        )
                        .child(
                            div()
                                .font_family("Segoe Fluent Icons")
                                .text_color(theme.c(Role::SuccessFg))
                                .child(SharedString::new_static("\u{E73E}")),
                        )
                        .child(SharedString::from(i18n.fmt("ui-jobs-done", &d)))
                        .on_click(toggle_done),
                )
                .child(
                    div()
                        .id("jobs-clear")
                        .text_color(theme.c(Role::AccentText))
                        .child(SharedString::from(i18n.get("ui-jobs-clear")))
                        .on_click(clear_done),
                ),
        );
    }
    for (i, j) in rows(s, l).into_iter().enumerate() {
        let (text, role) = status(s, j, rates.get(j));
        let sel = i == l.sel;
        let action: Option<(String, Vec<Command>)> = match j.state {
            JobState::Waiting(_) => Some((
                i18n.get("ui-job-answer"),
                vec![Command::new("ui.jobs.toggle")],
            )),
            JobState::Failed(_) => Some((
                i18n.get("ui-job-retry"),
                vec![Command::new("job.retry").arg("id", id_arg(j))],
            )),
            JobState::Queued => Some((
                i18n.get("ui-job-run-now"),
                vec![
                    Command::new("job.priority")
                        .arg("id", id_arg(j))
                        .arg("p", 2),
                ],
            )),
            _ => None,
        };
        let mut row = div()
            .h(px(H_LIST_ROW))
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(pad))
            .border_t_1()
            .border_color(theme.c(Role::StrokeDivider))
            .text_size(px(12.5))
            .when(sel, |d| {
                d.bg(theme.c(Role::LayerPanel))
                    .border_1()
                    .border_color(theme.c(Role::AccentFill))
            })
            .child(
                div()
                    .flex_none()
                    .font_family("Segoe Fluent Icons")
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::new_static(kind_icon(j))),
            )
            .child(
                div()
                    .w(px(100.0))
                    .flex_none()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(SharedString::from(kind_label(j))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(px(300.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(j.title.clone())),
            )
            .child(progress(j, theme))
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(theme.c(role))
                    .child(SharedString::from(text)),
            );
        if let Some((label, cmds)) = action {
            row = row.child(
                button(
                    theme,
                    format!("job-act-{}", j.id.0),
                    label,
                    "",
                    if matches!(j.state, JobState::Waiting(_)) {
                        Btn::Primary
                    } else {
                        Btn::Normal
                    },
                )
                .h(px(26.0))
                .on_click(click(cmds)),
            );
        }
        if let Some((cmd, _)) = pause_cmd(j) {
            let glyph = if matches!(j.state, JobState::Paused) {
                "\u{E768}"
            } else {
                "\u{E769}"
            };
            row = row.child(
                icon_button(theme, format!("job-lpause-{}", j.id.0), glyph)
                    .on_click(click(vec![cmd])),
            );
        }
        if j.state.is_active() {
            row = row.child(
                icon_button(theme, format!("job-lcancel-{}", j.id.0), "\u{E711}")
                    .on_click(click(vec![Command::new("job.cancel").arg("id", id_arg(j))])),
            );
        }
        col = col.child(row);
    }
    Some(col)
}

/// Задачи как фича окна (ADR-0022): job-бар, раскрытый список F11 и его клавиши,
/// скорость по замерам.
#[derive(Default)]
pub(super) struct Jobs {
    rates: Rates,
    list: List,
}

impl ShellFeature for Jobs {
    /// Пока идут задачи — замер скорости и перерисовка бара ≤ 30 кадров/с.
    fn tick(&mut self, s: &AppState, now: Instant) -> Option<Duration> {
        if !super::overlays::jobs_running(s) {
            return None;
        }
        self.rates.sample(s, now);
        Some(Duration::from_millis(33))
    }

    /// Раскрытый список задач (F11, D-06) разбирает свои клавиши.
    fn key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        key(s, &mut self.list, ks)
    }

    fn overlay(&self, f: &Frame<'_>, cx: &Context<CoreHost>) -> Option<AnyElement> {
        let toggle_done: super::overlays::Handler =
            Box::new(cx.listener(|h: &mut CoreHost, _: &ClickEvent, _, cx| {
                if let Some(j) = h.features.get_mut::<Jobs>() {
                    j.list.done_open = !j.list.done_open;
                }
                cx.notify();
            }));
        let clear_done: super::overlays::Handler =
            Box::new(cx.listener(|h: &mut CoreHost, _: &ClickEvent, _, cx| {
                let done: Vec<u64> = h.core.as_ref().map_or_else(Vec::new, |core| {
                    core.state()
                        .jobs
                        .iter()
                        .filter(|j| !j.state.is_active())
                        .map(|j| j.id.0)
                        .collect()
                });
                if let Some(j) = h.features.get_mut::<Jobs>() {
                    j.list.cleared.extend(done);
                }
                cx.notify();
            }));
        list(
            f.s,
            &self.list,
            &self.rates,
            f.theme,
            f.click_all,
            toggle_done,
            clear_done,
        )
        .map(IntoElement::into_any_element)
    }

    fn bar(&self, f: &Frame<'_>) -> Option<AnyElement> {
        bar(f.s, &self.list, &self.rates, f.theme, f.click).map(IntoElement::into_any_element)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_and_eta() {
        assert_eq!(group(12_870), "12\u{202F}870");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000_000), "1\u{202F}000\u{202F}000");
        assert!(eta(f64::INFINITY).is_none());
        assert!(eta(0.0).is_none());
    }
}
