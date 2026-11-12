//! Тосты (№51, №63), баннер, job-бар и inline-вопрос (S2 §2.7–2.9) на GPUI (G-05).
//! Всё событийное — внизу одной зоной (№63); тосты — справа над ней.

use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{App, ClickEvent, Div, SharedString, Window, div, px};
use urfin_core::{AppState, Command, Entry, JobState, Level, Prompt, PromptDetail, Value};

use super::theme::Theme;
use crate::tokens::{Metric, Role};

/// Стек тостов: не больше трёх, лишние — «+N» (№51).
const TOASTS_MAX: usize = 3;
/// Скругление тоста (D-02).
const TOAST_RADIUS: f32 = 6.0;
/// Ширина тоста (№63).
const W_TOAST: f32 = 340.0;
/// Тосты — над зоной событий, отступ от её верха и от правого края.
const TOAST_GAP: f32 = 12.0;
const TOAST_RIGHT: f32 = 16.0;
/// Строка баннера (№63).
const H_BANNER: f32 = 40.0;

/// Обработчик клика (команда в Core).
pub(super) type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
pub(super) type Click<'a> = &'a dyn Fn(Command) -> Handler;
/// Клик с цепочкой команд (меню: закрыть, панель, пункт).
pub(super) type ClickAll<'a> = &'a dyn Fn(Vec<Command>) -> Handler;

/// Тост с дедлайном.
pub(super) struct Toast {
    pub level: Level,
    pub text: String,
    pub until: Instant,
}

impl Toast {
    pub(super) fn new(level: Level, text: String, now: Instant) -> Toast {
        let ttl = match level {
            Level::Info | Level::Success => 4,
            Level::Warn => 6,
            Level::Error => 8,
        };
        Toast {
            level,
            text,
            until: now + Duration::from_secs(ttl),
        }
    }
}

/// Тосты справа сверху: не забирают фокус и ввод.
pub(super) fn toasts(list: &[Toast], theme: &Theme, raise: f32) -> Option<Div> {
    if list.is_empty() {
        return None;
    }
    // Якорь — верх зоны событий (нулевая высота): тосты над ним справа, новые снизу,
    // стопка вверх (№63). Только сообщение, без кнопок.
    let mut col = div()
        .absolute()
        .bottom(px(TOAST_GAP + raise))
        .right(px(TOAST_RIGHT))
        .w(px(W_TOAST))
        .flex()
        .flex_col()
        .gap_2();
    if list.len() > TOASTS_MAX {
        col = col.child(
            div()
                .text_size(px(theme.m(Metric::TypeCaption)))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(format!("+{}", list.len() - TOASTS_MAX))),
        );
    }
    let skip = list.len().saturating_sub(TOASTS_MAX);
    for t in list.iter().skip(skip) {
        let fg = match t.level {
            Level::Info | Level::Success => Role::TextPrimary,
            Level::Warn => Role::CautionFg,
            Level::Error => Role::CriticalFg,
        };
        col = col.child(
            div()
                .px(px(12.0))
                .py(px(10.0))
                .flex()
                .items_start()
                .gap(px(10.0))
                .rounded(px(TOAST_RADIUS))
                .border_1()
                .border_color(theme.c(Role::StrokeCard))
                .bg(theme.c(Role::LayerPanel))
                .text_size(px(12.5))
                .text_color(theme.c(fg))
                .children(theme.vivid().then(|| toast_icon(t.level, theme)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(t.text.clone())),
                ),
        );
    }
    Some(div().relative().h(px(0.0)).w_full().child(col))
}

/// Значок тоста в круге 18 (палитра B): итог задачи (`Info`) — галочка на `exec`,
/// ошибка — «!» на `modified`, предупреждение — «!» на `caution`. Знак — цветом `panel`.
fn toast_icon(level: Level, theme: &Theme) -> Div {
    let (fill, glyph, icon_font) = match level {
        Level::Info | Level::Success => (Role::TypeExec, "\u{E73E}", true),
        Level::Warn => (Role::CautionFg, "!", false),
        Level::Error => (Role::Modified, "!", false),
    };
    div()
        .flex_none()
        .size(px(18.0))
        .mt(px(-1.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.c(fill))
        .text_color(theme.c(Role::LayerPanel))
        .font_weight(gpui_kit::FontWeight::BOLD)
        .when(icon_font, |d| d.font_family("Segoe Fluent Icons"))
        .text_size(px(12.0))
        .child(SharedString::new_static(glyph))
}

/// Баннер core (№63): строка 40 в зоне событий над job-баром, сдвигает панели, живёт до
/// ответа. Черта сверху цвета уровня, значок, текст и сразу за ним кнопки (первая —
/// основная).
pub(super) fn banner(s: &AppState, theme: &Theme, click: Click<'_>) -> Option<Div> {
    let b = s.banner.as_ref()?;
    let (bg, line, glyph) = match b.level {
        Level::Info | Level::Success => (Role::InfoBg, Role::AccentFill, '\u{E946}'),
        Level::Warn => (Role::CautionBg, Role::ContextRemote, '\u{E823}'),
        Level::Error => (Role::CriticalBg, Role::CriticalFg, '\u{E783}'),
    };
    let mut row = div()
        .h(px(H_BANNER))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(theme.m(Metric::PadRowX)))
        .bg(theme.c(bg))
        .border_t_2()
        .border_color(theme.c(line))
        .text_size(px(12.5))
        .child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(line))
                .child(SharedString::from(glyph.to_string())),
        )
        .children(banner_text(&b.text, theme));
    for (i, (label, cmd)) in b.actions.iter().enumerate() {
        let primary = i == 0;
        row = row.child(
            div()
                .id(SharedString::from(format!("banner-{i}")))
                .flex_none()
                .h(px(28.0))
                .px(px(14.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(if primary {
                    Role::AccentFill
                } else {
                    Role::StrokeControl
                }))
                .bg(theme.c(if primary {
                    Role::AccentFill
                } else {
                    Role::LayerPanel
                }))
                .text_color(theme.c(if primary {
                    Role::AccentOn
                } else {
                    Role::TextPrimary
                }))
                .child(SharedString::from(label.to_string()))
                .on_click(click(cmd.clone())),
        );
    }
    Some(row)
}

/// Текст баннера (`screen-journal.png`): до первого «: » — полужирно, дальше до « · » —
/// обычным, остальное — `secondary`.
fn banner_text(text: &str, theme: &Theme) -> Vec<Div> {
    let (head, body) = match text.split_once(": ") {
        Some((h, b)) => (Some(format!("{h}:")), b),
        None => (None, text),
    };
    let (main, rest) = body.split_once(" · ").unwrap_or((body, ""));
    let mut out = Vec::with_capacity(3);
    if let Some(h) = head {
        out.push(
            div()
                .flex_none()
                .whitespace_nowrap()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(h)),
        );
    }
    out.push(
        div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(SharedString::from(main.to_owned())),
    );
    if !rest.is_empty() {
        out.push(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(rest.to_owned())),
        );
    }
    out
}

/// Идут ли задачи (job-бар виден, перерисовка ≤ 30 раз/с).
pub(super) fn jobs_running(s: &AppState) -> bool {
    s.jobs.iter().any(|j| matches!(j.state, JobState::Running))
}

pub(super) fn prompt_id(p: &Prompt) -> Value {
    Value::Int(i64::try_from(p.id.0).unwrap_or(i64::MAX))
}

/// Выбор в видимом вопросе: вариант и флажок «для всех».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PromptSel {
    pub id: u64,
    pub sel: usize,
    pub all: bool,
}

impl PromptSel {
    pub(super) const NONE: PromptSel = PromptSel {
        id: u64::MAX,
        sel: 0,
        all: false,
    };

    /// Выбор для вопроса `p`: прежний или вариант по умолчанию.
    pub(super) fn of(self, p: &Prompt) -> PromptSel {
        if self.id == p.id.0 {
            return self;
        }
        let n = p.spec.choices.len();
        PromptSel {
            id: p.id.0,
            sel: usize::from(p.spec.default).min(n.saturating_sub(1)),
            all: false,
        }
    }
}

/// Ответ вариантом `i`; `all` — «для всех» (если вопрос это допускает).
pub(super) fn reply(p: &Prompt, i: usize, all: bool) -> Option<Command> {
    let ch = p.spec.choices.get(i)?;
    let c = Command::new("prompt.reply")
        .arg("id", prompt_id(p))
        .arg("choice", ch.id.as_str());
    Some(if all && p.spec.allow_all {
        c.arg("all", true)
    } else {
        c
    })
}

/// Отмена вопроса (Esc).
pub(super) fn cancel(p: &Prompt) -> Command {
    Command::new("prompt.cancel").arg("id", prompt_id(p))
}

/// Вопрос (D-04): встроенная строка внизу. Конфликт — фон ожидания и черта
/// `context_remote`, сравнение «Копируемый / Существующий» с меткой «новее»; кнопки с
/// клавишами (первая — основная), справа «Так же для остальных», в конце «Отмена».
#[allow(clippy::too_many_lines, reason = "строка вопроса сверху вниз")]
/// Вопрос об ошибке операции одной строкой (`screen-operror.png`): задача ждёт ответа
/// (якорь — job-бар), есть «Повторить», сравнения нет.
pub(super) fn error_line(p: &Prompt) -> bool {
    p.spec.detail.is_none()
        && p.spec.anchor == urfin_core::Anchor::JobBar
        && p.spec.choices.iter().any(|c| c.id == "retry")
}

/// Строка ошибки: ⚠, «Нет доступа:», путь, причина и код `secondary`; Повторить,
/// Пропустить, «Пропустить все такие» (A), Отмена — сразу за текстом.
fn error_bar(p: &Prompt, s: PromptSel, theme: &Theme, click: Click<'_>) -> Div {
    use super::strip::{Btn, Tone, button, container};
    let i18n = urfin_i18n::current();
    let (main, rest) = p
        .spec
        .body
        .split_once('\n')
        .unwrap_or((p.spec.body.as_str(), ""));
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(Role::ContextRemote))
                .child(SharedString::new_static("\u{E7BA}")),
        )
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(format!("{}:", p.spec.title))),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(SharedString::from(main.to_owned())),
        )
        .when(!rest.is_empty(), |d| {
            d.child(
                div()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(rest.to_owned())),
            )
        });
    let cancel_at = p.spec.choices.iter().position(|c| c.id == "cancel");
    for (i, ch) in p.spec.choices.iter().enumerate() {
        if Some(i) == cancel_at {
            continue;
        }
        let key = if i == s.sel {
            "Enter".to_owned()
        } else {
            ch.key.to_uppercase().to_string()
        };
        let kind = if i == s.sel {
            Btn::Primary
        } else {
            Btn::Normal
        };
        row = row.child(
            button(
                theme,
                format!("choice-{i}"),
                ch.label.to_string(),
                &key,
                kind,
            )
            .when_some(reply(p, i, false), |d, c| d.on_click(click(c))),
        );
        if ch.id == "skip" && p.spec.allow_all {
            row = row.child(
                button(
                    theme,
                    "choice-skip-all".into(),
                    i18n.get("ui-prompt-skip-all"),
                    "A",
                    Btn::Normal,
                )
                .when_some(reply(p, i, true), |d, c| d.on_click(click(c))),
            );
        }
    }
    if let Some(i) = cancel_at {
        let ch = &p.spec.choices[i];
        row = row.child(
            button(
                theme,
                format!("choice-{i}"),
                ch.label.to_string(),
                "Esc",
                Btn::Normal,
            )
            .when_some(reply(p, i, false), |d, c| d.on_click(click(c))),
        );
    }
    container(theme, Tone::Waiting, 16.0, 10.0).child(row)
}

/// Поле вопроса с вводом (S8-36, [`super::ask`]): поле и кнопка ответа `i` (команда —
/// по клику, с текстом поля).
pub(super) struct AskView<'a> {
    pub input: &'a gpui_kit::Entity<gpui_kit::component::input::InputState>,
    pub answer: &'a dyn Fn(usize) -> Handler,
}

/// Ширина поля вопроса с вводом.
const W_ASK: f32 = 240.0;

#[allow(clippy::too_many_lines, reason = "заголовок, сравнение, кнопки")]
pub(super) fn prompt_bar(
    p: &Prompt,
    s: PromptSel,
    theme: &Theme,
    click: Click<'_>,
    toggle_all: Handler,
    ask: Option<&AskView<'_>>,
) -> Div {
    use super::strip::{Btn, Tone, button, check, container};
    use gpui_kit::component::Sizable as _;
    if error_line(p) {
        return error_bar(p, s, theme, click);
    }
    let i18n = urfin_i18n::current();
    let detail = match &p.spec.detail {
        Some(PromptDetail::Conflict {
            src,
            dst,
            index,
            total,
        }) => Some((src, dst, *index, *total)),
        _ => None,
    };
    let tone = if detail.is_some() {
        Tone::Waiting
    } else {
        Tone::Plain
    };
    let mut head = div().flex().items_center().gap(px(10.0));
    if detail.is_some() {
        head = head.child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(Role::ContextRemote))
                .child(SharedString::new_static("\u{E7BA}")),
        );
    }
    head = head.child(
        div()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .child(SharedString::from(p.spec.title.clone())),
    );
    if let Some((src, _, index, total)) = detail {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("index", index);
        a.set("total", total);
        a.set("name", src.name.to_string());
        head = head.child(
            div()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.fmt("ui-strip-conflict-of", &a))),
        );
    }
    let mut bar = container(theme, tone, 16.0, 12.0).child(head);
    // У конфликта пояснение повторяет заголовок и сравнение — не показываем.
    if !p.spec.body.is_empty() && detail.is_none() {
        bar = bar.child(
            div()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(p.spec.body.clone())),
        );
    }
    if let Some((src, dst, _, _)) = detail {
        bar = bar.child(conflict(src, dst, theme));
    }
    let mut row = div().flex().flex_wrap().items_center().gap(px(8.0));
    // Поле ввода (пароль архива — с маской) — перед ответами, как ряд пароля строки
    // распаковки (`screen-extractpw`).
    if let Some(a) = ask {
        row = row.child(
            div()
                .flex_none()
                .w(px(W_ASK))
                .h(px(28.0))
                .mr(px(4.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .bg(theme.c(Role::FillControlInput))
                .border_1()
                .border_color(theme.c(Role::StrokeControl))
                .child(
                    gpui_kit::component::input::Input::new(a.input)
                        .small()
                        .appearance(false),
                ),
        );
    }
    let cancel_at = p.spec.choices.iter().position(|c| c.id == "cancel");
    for (i, ch) in p.spec.choices.iter().enumerate() {
        if Some(i) == cancel_at {
            continue;
        }
        let key = if i == s.sel {
            "Enter".to_owned()
        } else if ch.key.is_alphanumeric() {
            ch.key.to_uppercase().to_string()
        } else {
            String::new()
        };
        let kind = if i == s.sel {
            Btn::Primary
        } else {
            Btn::Normal
        };
        let b = button(
            theme,
            format!("choice-{i}"),
            ch.label.to_string(),
            &key,
            kind,
        );
        row = row.child(match ask {
            Some(a) => b.on_click((a.answer)(i)),
            None => b.when_some(reply(p, i, s.all), |d, c| d.on_click(click(c))),
        });
    }
    // Кнопки и флажок — сразу за ответами, не у правого края (DESIGN.md).
    if p.spec.allow_all {
        row = row.child(
            check(
                theme,
                "prompt-all".into(),
                s.all,
                i18n.get("ui-prompt-all"),
                true,
            )
            .on_click(toggle_all),
        );
    }
    if let Some(i) = cancel_at {
        let ch = &p.spec.choices[i];
        row = row.child(
            button(
                theme,
                format!("choice-{i}"),
                ch.label.to_string(),
                "Esc",
                Btn::Normal,
            )
            .when_some(reply(p, i, s.all), |d, c| d.on_click(click(c))),
        );
    }
    bar.child(row)
}

/// Сравнение (D-04): колонки 90 / копируемый / существующий; размер и дата; у более
/// нового — метка «новее» акцентом.
fn conflict(src: &Entry, dst: &Entry, theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    let size = |e: &Entry| {
        let mut out = String::new();
        theme.fmt.write_size(&mut out, e.size);
        out
    };
    let date = |e: &Entry| {
        let mut c = crate::fmt_cell::Cell::default();
        theme.fmt.write_datetime(&mut c, e.mtime);
        c.as_str().trim().to_owned()
    };
    let secondary = theme.c(Role::TextSecondary);
    let cell = |t: String| div().flex_1().min_w_0().child(SharedString::from(t));
    let label = |key: &str| {
        div()
            .w(px(90.0))
            .flex_none()
            .text_color(secondary)
            .child(SharedString::from(i18n.get(key)))
    };
    let newer = |e: &Entry, other: &Entry| {
        let d = cell(date(e)).flex().items_center().gap(px(6.0));
        if e.mtime > other.mtime {
            d.child(
                div()
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.c(Role::AccentFill))
                    .text_color(theme.c(Role::AccentText))
                    .text_size(px(11.0))
                    .child(SharedString::from(i18n.get("ui-strip-newer"))),
            )
        } else {
            d
        }
    };
    let line = || div().flex().gap(px(16.0));
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .text_size(px(12.5))
        .child(
            line()
                .child(div().w(px(90.0)).flex_none())
                .child(cell(i18n.get("ui-conflict-new")).text_color(secondary))
                .child(cell(i18n.get("ui-conflict-old")).text_color(secondary)),
        )
        .child(
            line()
                .child(label("ui-conflict-size"))
                .child(cell(size(src)))
                .child(cell(size(dst))),
        )
        .child(
            line()
                .child(label("ui-conflict-mtime"))
                .child(newer(src, dst))
                .child(newer(dst, src)),
        )
}
