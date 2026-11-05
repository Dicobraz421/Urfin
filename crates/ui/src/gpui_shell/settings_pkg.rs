//! Раздел настроек «Пакеты» (F02-12, `screen-settings-pkg.png`, `[pkg]` F02 §2.6):
//! порядок менеджеров перетаскиванием, поиск по умолчанию, расписание, важные программы,
//! пояснения о выводе и правах. Логика без окна — `crate::pkg_settings`. Важные и
//! расписание — команды `pkg.*` (применяются сразу), порядок и поиск — `ui.setting`.

use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Div, FocusHandle, Hsla, MouseButton, Render, SharedString, Stateful, Window, div, px,
};
use std::cell::Cell;
use urfin_core::pkg::{Mgr, PkgConfig, PkgId, PkgModel, SearchIn, Task, View};
use urfin_core::{AppState, Command};

use super::CoreHost;
use super::overlays::ClickAll;
use super::settings::{Form, check, group, row, select_w};
use super::settings_ssh::{Field, input_frame};
use super::theme::Theme;
use crate::UiState;
use crate::pkg_settings::{self as logic, MgrChip, TaskLine};
use crate::tokens::Role;

/// Поле времени и поле поиска важных (`screen-settings-pkg.png`, макет: 60).
const W_AT: f32 = 60.0;
const W_ADD: f32 = 220.0;
/// Список «Ежедневно» (макет: 130).
const W_PERIOD: f32 = 130.0;
const H_CHIP: f32 = 28.0;
/// Глиф «крестик» чипа.
const CROSS: &str = "×";

/// Поля раздела.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    At,
    /// Поиск важной программы по установленным («+»).
    Add,
}

fn config(s: &AppState) -> PkgConfig {
    s.ext
        .get::<PkgModel>()
        .map_or_else(PkgConfig::default, |m| m.config.clone())
}

fn view_of(s: &AppState) -> std::sync::Arc<View> {
    s.ext
        .get::<PkgModel>()
        .map(|m| m.feed.view())
        .unwrap_or_default()
}

/// Открыт ли раздел «Пакеты».
fn open(s: &AppState) -> bool {
    s.ext.get::<UiState>().and_then(|u| u.settings.as_deref()) == Some("pkg")
}

/// Закрыть поиск важных: уход из поля.
fn close_add() -> Command {
    Command::new("ui.settings").arg("section", "pkg")
}

/// Уход из поля: время — применить, поиск — закрыть (выбор уже сделан кнопкой или Enter).
fn on_blur(key: Key, text: &str, s: &AppState) -> Vec<Command> {
    match key {
        Key::At => logic::at_cmds(text, &config(s).schedule),
        Key::Add if open(s) => vec![close_add()],
        Key::Add => Vec::new(),
    }
}

/// Поля раздела: живут, пока он открыт.
#[derive(Default)]
pub(super) struct Fields {
    at: Option<Field<Key>>,
    add: Option<Field<Key>>,
    /// Вернуть фокус окну в следующем кадре.
    blur: bool,
    /// Выбранная подсказка поиска важных и запрос, к которому она относится.
    sel: Cell<usize>,
    query: String,
}

impl Fields {
    pub(super) fn editing(&self) -> bool {
        self.at.iter().chain(&self.add).any(|f| f.focused.get())
    }

    fn query(&self, cx: &Context<CoreHost>) -> String {
        self.add.as_ref().map_or_else(String::new, |f| f.text(cx))
    }

    /// Клавиша в поле раздела: `None` — полю.
    pub(super) fn key(
        &mut self,
        s: &AppState,
        ks: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let adding = self.add.as_ref().is_some_and(|f| f.focused.get());
        let mut cmds = Vec::new();
        match ks.key.as_str() {
            "up" | "down" if adding => {
                let n = logic::matches(&view_of(s), &self.query(cx)).len();
                if n > 0 {
                    let step = if ks.key == "up" { n - 1 } else { 1 };
                    self.sel.set((self.sel.get() + step) % n);
                }
                cx.notify();
                return Some(cmds);
            }
            "enter" if adding => {
                let found = logic::matches(&view_of(s), &self.query(cx));
                cmds.extend(found.get(self.sel.get()).map(|m| logic::toggle_cmd(&m.0)));
                self.blur = true;
            }
            "enter" | "tab" => self.blur = true,
            "escape" => {
                if let Some(f) = self.at.as_ref().filter(|f| f.focused.get()) {
                    f.show(config(s).schedule.at.to_string(), window, cx);
                }
                self.blur = true;
            }
            _ => return None,
        }
        cx.notify();
        Some(cmds)
    }

    /// Поля под состояние, в кадре (как у раздела «SSH»).
    pub(super) fn sync(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        if !open(s) {
            if self.editing() {
                window.focus(focus, cx);
            } else {
                self.at = None;
                self.add = None;
                self.blur = false;
            }
            return;
        }
        let at = config(s).schedule.at.to_string();
        let field = self
            .at
            .get_or_insert_with(|| Field::new(Key::At, at.clone(), on_blur, window, cx));
        if !field.focused.get() && field.text(cx) != at {
            field.show(at, window, cx);
        }
        let adding = s.ext.get::<UiState>().is_some_and(|u| u.pkg_add_important);
        match (adding, &self.add) {
            (true, None) => {
                let f = Field::new(Key::Add, String::new(), on_blur, window, cx);
                f.input.update(cx, |st, cx| st.focus(window, cx));
                self.add = Some(f);
                self.sel.set(0);
            }
            (false, Some(_)) => {
                self.add = None;
                self.blur = false;
            }
            _ => {}
        }
        let q = self.query(cx);
        if q != self.query {
            self.query = q;
            self.sel.set(0);
        }
        if self.blur {
            self.blur = false;
            window.focus(focus, cx);
        }
    }
}

fn secondary(theme: &Theme, text: String) -> Div {
    div()
        .flex_none()
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(text))
}

/// Ручка чипа: шесть точек.
fn grip(color: Hsla) -> Div {
    let dot = || div().size(px(2.0)).bg(color);
    let col = || div().flex().flex_col().gap(px(2.0));
    let dots = || col().child(dot()).child(dot()).child(dot());
    div()
        .flex_none()
        .flex()
        .gap(px(2.0))
        .child(dots())
        .child(dots())
}

/// Чип менеджера без поведения: ручка, имя, версия или «не найден» (пунктир).
fn mgr_face(theme: &Theme, chip: &MgrChip, missing: &str) -> Div {
    let tail = if chip.found {
        chip.version.as_deref().unwrap_or_default()
    } else {
        missing
    };
    div()
        .flex_none()
        .h(px(H_CHIP))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(if chip.found {
            Role::StrokeControl
        } else {
            Role::TextSecondary
        }))
        .when(chip.found, |d| d.bg(theme.c(Role::FillControl)))
        .when(!chip.found, gpui_kit::Styled::border_dashed)
        .text_size(px(13.0))
        .child(grip(theme.c(Role::TextSecondary)))
        .child(
            div()
                .font_weight(if chip.found {
                    gpui_kit::FontWeight::MEDIUM
                } else {
                    gpui_kit::FontWeight::NORMAL
                })
                .child(SharedString::new_static(chip.mgr.as_str())),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(tail.to_owned())),
        )
}

/// Перетаскиваемый чип: копия у указателя.
#[derive(Clone)]
struct Dragged {
    mgr: Mgr,
    fill: Hsla,
    border: Hsla,
    text: Hsla,
}

impl Render for Dragged {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(H_CHIP))
            .px(px(10.0))
            .flex()
            .items_center()
            .rounded(px(4.0))
            .border_1()
            .border_color(self.border)
            .bg(self.fill)
            .text_size(px(13.0))
            .text_color(self.text)
            .child(SharedString::new_static(self.mgr.as_str()))
    }
}

/// Чип порядка: тянется и принимает другой чип.
fn mgr_chip(
    theme: &Theme,
    chip: &MgrChip,
    order: &[Mgr],
    missing: &str,
    cx: &Context<CoreHost>,
) -> Stateful<Div> {
    let dragged = Dragged {
        mgr: chip.mgr,
        fill: theme.c(Role::FillCursor),
        border: theme.c(Role::AccentFill),
        text: theme.c(Role::TextPrimary),
    };
    let accent = theme.c(Role::AccentFill);
    let (order, onto) = (order.to_vec(), chip.mgr);
    mgr_face(theme, chip, missing)
        .id(SharedString::from(format!("set-pkg-order-{}", chip.mgr)))
        .cursor_grab()
        .on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
        .drag_over::<Dragged>(move |st, _, _, _| st.border_color(accent))
        .on_drop(cx.listener(move |h: &mut CoreHost, d: &Dragged, w, cx| {
            if let Some(next) = logic::moved(&order, d.mgr, onto) {
                h.run(logic::order_cmd(&next), w, cx);
            }
        }))
}

/// Чип важной программы с крестиком.
fn important_chip(theme: &Theme, id: &PkgId, name: String, click: ClickAll<'_>) -> Div {
    div()
        .flex_none()
        .h(px(H_CHIP))
        .pl(px(10.0))
        .pr(px(6.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::FillControl))
        .text_size(px(13.0))
        .child(SharedString::from(name))
        .child(
            div()
                .id(SharedString::from(format!("set-pkg-unmark-{id}")))
                .px(px(2.0))
                .text_color(theme.c(Role::TextSecondary))
                .hover(|d| d.text_color(theme.c(Role::TextPrimary)))
                .child(SharedString::new_static(CROSS))
                .on_click(click(vec![logic::toggle_cmd(id)])),
        )
}

/// Подсказка поиска по установленным: выбор — по нажатию, раньше ухода фокуса из поля.
fn match_chip(
    theme: &Theme,
    id: &PkgId,
    name: String,
    on: bool,
    cx: &Context<CoreHost>,
) -> Stateful<Div> {
    let cmd = logic::toggle_cmd(id);
    div()
        .id(SharedString::from(format!("set-pkg-mark-{id}")))
        .flex_none()
        .h(px(26.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(if on {
            Role::AccentFill
        } else {
            Role::StrokeControl
        }))
        .when(on, |d| d.bg(theme.c(Role::FillCursor)))
        .text_size(px(12.5))
        .child(SharedString::from(name))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |h, _, w, cx| h.run(cmd.clone(), w, cx)),
        )
}

/// Поле времени выключенного расписания: только показ (макет: `disabled`).
fn time_off(theme: &Theme, text: &str) -> Div {
    input_frame(theme, W_AT, false)
        .pl(px(8.0))
        .text_size(px(13.0))
        .child(SharedString::from(text.to_owned()))
}

/// Строка о задаче Планировщика: текст и, если поправимо, «Обновить» (создать заново).
fn task_row(theme: &Theme, line: &TaskLine, click: ClickAll<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let args = |name: &'static str, value: &str| {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set(name, value.to_owned());
        a
    };
    let text = match line {
        TaskLine::OtherPath(p) => i18n.fmt("pkg-set-task-other", &args("path", p)),
        TaskLine::Absent => i18n.get("pkg-set-task-absent"),
        TaskLine::Failed(e) => i18n.fmt("pkg-schedule-failed", &args("error", e)),
    };
    let fix = (!matches!(line, TaskLine::Failed(_))).then(|| {
        super::strip::button(
            theme,
            "set-pkg-task-fix".into(),
            i18n.get("pkg-set-task-fix"),
            "",
            super::strip::Btn::Normal,
        )
        .h(px(28.0))
        .on_click(click(vec![Command::new("pkg.schedule").arg("on", true)]))
    });
    row(
        theme,
        String::new(),
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .min_w_0()
                    .text_color(theme.c(Role::ContextRemote))
                    .child(SharedString::from(text)),
            )
            .children(fix),
        None,
    )
}

/// Раздел формой сверху вниз.
#[allow(clippy::too_many_lines, reason = "форма раздела сверху вниз")]
pub(super) fn section(
    s: &AppState,
    theme: &Theme,
    click: ClickAll<'_>,
    fields: &Fields,
    cx: &Context<CoreHost>,
) -> Div {
    let i18n = urfin_i18n::current();
    let c = config(s);
    let view = view_of(s);
    let task = s
        .ext
        .get::<PkgModel>()
        .map_or(Task::Unknown, |m| m.task.clone());
    let f = Form { s, theme, click };

    let missing = i18n.get("pkg-set-missing");
    let chips = logic::order_chips(&c.order, &view.info);
    let order: Vec<Mgr> = chips.iter().map(|m| m.mgr).collect();
    let mut managers = div().flex().flex_wrap().items_center().gap(px(6.0));
    for (i, chip) in chips.iter().enumerate() {
        if i > 0 {
            managers = managers.child(secondary(theme, "›".into()));
        }
        managers = managers.child(mgr_chip(theme, chip, &order, &missing, cx));
    }

    let scopes = [
        (SearchIn::All, i18n.get("pkg-set-search-all")),
        (SearchIn::Winget, Mgr::Winget.as_str().into()),
        (SearchIn::Scoop, Mgr::Scoop.as_str().into()),
        (SearchIn::Choco, Mgr::Choco.as_str().into()),
    ]
    .map(|(scope, label)| (crate::pkg_view::scope_arg(scope), label));

    let on = c.schedule.enabled;
    let toggle = check(theme, "schedule-on", on, i18n.get("pkg-set-schedule-on"))
        .when(task != Task::Busy, |d| {
            d.on_click(click(vec![Command::new("pkg.schedule").arg("on", !on)]))
        });
    let at = if on {
        super::settings_ssh::input_box(theme, fields.at.as_ref(), W_AT)
    } else {
        time_off(theme, &c.schedule.at).text_color(theme.c(Role::TextSecondary))
    };
    let daily = [("daily", i18n.get("pkg-set-daily"))];
    let schedule = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.0))
        .child(toggle)
        .child(
            div()
                .ml(px(8.0))
                .when(!on, |d| d.opacity(0.6))
                .child(select_w(
                    theme,
                    "period",
                    &daily,
                    "daily",
                    click,
                    &|_| Vec::new(),
                    W_PERIOD,
                )),
        )
        .child(secondary(theme, i18n.get("pkg-set-at")))
        .child(at)
        .child(
            super::strip::button(
                theme,
                "set-pkg-check-now".into(),
                i18n.get("pkg-set-check-now"),
                "",
                super::strip::Btn::Normal,
            )
            .ml(px(8.0))
            .h(px(28.0))
            .on_click(click(vec![Command::new("pkg.check_now")])),
        );

    let adding = s.ext.get::<UiState>().is_some_and(|u| u.pkg_add_important);
    let mut important = div().flex().flex_wrap().items_center().gap(px(6.0));
    for (id, name) in logic::important_chips(&view, &c.important) {
        important = important.child(important_chip(theme, &id, name, click));
    }
    important = if adding {
        important.child(super::settings_ssh::input_box(
            theme,
            fields.add.as_ref(),
            W_ADD,
        ))
    } else {
        important.child(
            div()
                .id("set-pkg-add")
                .flex_none()
                .size(px(H_CHIP))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .border_1()
                .border_dashed()
                .border_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static("+"))
                .on_click(click(vec![
                    Command::new("ui.settings")
                        .arg("section", "pkg")
                        .arg("add_important", true),
                ])),
        )
    };
    let suggestions = adding.then(|| {
        let found = logic::matches(&view, &fields.query(cx));
        let body = if found.is_empty() {
            div().child(secondary(theme, i18n.get("pkg-set-important-none")))
        } else {
            let mut line = div().flex().flex_wrap().items_center().gap(px(6.0));
            for (i, (id, name)) in found.iter().enumerate() {
                line = line.child(match_chip(
                    theme,
                    id,
                    name.clone(),
                    i == fields.sel.get(),
                    cx,
                ));
            }
            line
        };
        row(theme, String::new(), body, None)
    });

    let output = [("terminal", i18n.get("pkg-set-output-term"))];
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("pkg-set-managers")).pt(px(0.0)))
        .child(row(
            theme,
            i18n.get("pkg-set-order"),
            managers,
            Some(i18n.get("pkg-set-order-note")),
        ))
        .child(f.list(
            "pkg.search_in",
            i18n.get("pkg-set-search"),
            &scopes,
            crate::pkg_view::scope_arg(c.search_in),
            &|v| super::settings::set("pkg.search_in", v),
            Some(i18n.get("pkg-set-search-note")),
        ))
        .child(group(i18n.get("pkg-set-updates")))
        .child(row(
            theme,
            i18n.get("pkg-set-schedule"),
            schedule,
            Some(i18n.get("pkg-set-schedule-note")),
        ))
        .children(logic::task_line(&task, on).map(|l| task_row(theme, &l, click)))
        .child(row(
            theme,
            i18n.get("pkg-set-important"),
            important,
            Some(i18n.get("pkg-set-important-note")),
        ))
        .children(suggestions)
        .child(group(i18n.get("pkg-set-run")))
        .child(f.list(
            "pkg.output",
            i18n.get("pkg-set-output"),
            &output,
            "terminal",
            &|_| Vec::new(),
            Some("Ctrl+O".into()),
        ))
        .child(row(
            theme,
            i18n.get("pkg-set-admin"),
            div().child(SharedString::from(i18n.get("pkg-set-admin-uac"))),
            None,
        ))
}
