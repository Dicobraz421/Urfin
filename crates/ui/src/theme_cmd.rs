//! Команды тем (T-01-4): `ui.theme.set/next/follow_os/reload`, `ui.density.set`,
//! `ui.icons.set`. Выбор — в [`UiState`], запись — в `config.toml`; свои темы
//! (`<config_dir>/themes/*.toml`) читает задача Interactive, ответ — `Msg`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use urfin_core::{CmdError, Command, CommandMeta, Cx, Lane, Level, Msg, ValueKind, settings};

use crate::config::{Density, IconMode};
use crate::handler::{Part, arg, meta, ui_state};
use crate::state::{UiState, UserThemes};
use crate::theme::{self, AUTO, Themes};

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

/// Темы (T-01-4, №59): id — встроенная, своя или `auto`.
const COMMANDS: &[CommandMeta] = &[
    meta("ui.theme.set", "Theme", &[arg("id", ValueKind::Str, true)]),
    meta("ui.theme.next", "Next theme", &[]),
    meta("ui.theme.follow_os", "Theme as in Windows", &[]),
    meta(
        "ui.theme.reload",
        "Reload themes",
        &[arg("if_changed", ValueKind::Bool, false)],
    ),
    meta(
        "ui.density.set",
        "Row height",
        &[arg("px", ValueKind::Int, true)],
    ),
    meta(
        "ui.icons.set",
        "File icons",
        &[arg("mode", ValueKind::Str, true)],
    ),
];

/// Окружение тем из приложения: каталог своих тем и политика организации.
#[derive(Clone, Debug, Default)]
pub struct ThemeEnv {
    pub dir: Option<PathBuf>,
    /// Политики организации (ADR-0020): в `AppState.ext` в первом pump.
    pub policies: urfin_core::PolicySet,
    /// Новые политики от `policy-watch`; без него — только прочитанные при старте.
    pub policy_inbox: Option<crate::sys::policy::PolicyInbox>,
    /// История полей ввода (`<state_dir>/fields.toml`, №70).
    pub fields: Option<PathBuf>,
    /// Приветствие при старте (первый запуск, обновление; D-02).
    pub welcome: Option<crate::state::Welcome>,
    /// Каталоги конфига и журнала («О программе», D-08).
    pub config_dir: Option<PathBuf>,
    pub log_dir: Option<PathBuf>,
}

fn tr(key: &str, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

/// Все id для выбора: встроенные и свои.
pub fn ids(st: &UiState) -> Vec<String> {
    Themes {
        user: st.user_themes.themes.to_vec(),
    }
    .ids()
}

/// Выбор из конфига с учётом политики: заблокированная — её тема; иначе тема политики
/// служит умолчанием вместо «как в ОС».
pub fn seed(st: &mut UiState, cfg_theme: &str) {
    let choice = theme::normalize_choice(cfg_theme);
    st.theme = match &st.policy.theme {
        Some(p) if st.policy.locked || choice == AUTO => theme::normalize_choice(p).into(),
        _ => choice.into(),
    };
}

/// Команды тем; `None` — не наша.
fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    let a = &cmd.args;
    let r = match cmd.id.as_str() {
        "ui.theme.set" | "ui.theme.next" | "ui.theme.follow_os" => {
            let st = ui_state(cx);
            if st.policy.locked {
                return Some(Err(CmdError::Rejected(tr("ui-theme-locked", &[]).into())));
            }
            let id = match cmd.id.as_str() {
                "ui.theme.follow_os" => AUTO.to_owned(),
                "ui.theme.next" => next(st),
                _ => {
                    let id = theme::normalize_choice(a.str("id").unwrap_or_default());
                    if id != AUTO && !ids(st).contains(&id) {
                        return Some(Err(CmdError::Arg {
                            name: "id".into(),
                            msg: tr("ui-theme-unknown", &[("id", &id)]).into(),
                        }));
                    }
                    id
                }
            };
            st.theme = id.as_str().into();
            settings::set(&["ui", "theme"], id).apply(cx);
            Ok(())
        }
        "ui.theme.reload" => {
            reload(cx, !a.bool("if_changed").unwrap_or(false));
            Ok(())
        }
        "ui.density.set" => {
            let d = Density::new(a.int("px").unwrap_or(24));
            ui_state(cx).density = d.px();
            settings::set(&["ui", "density"], i64::from(d.px())).apply(cx);
            Ok(())
        }
        "ui.icons.set" => {
            let raw = a.str("mode").unwrap_or_default();
            match IconMode::parse(raw) {
                Some(m) => {
                    ui_state(cx).icons = m;
                    settings::set(&["ui", "icons"], m.as_str()).apply(cx);
                    Ok(())
                }
                None => Err(CmdError::Arg {
                    name: "mode".into(),
                    msg: "expected mono | color | none".into(),
                }),
            }
        }
        _ => return None,
    };
    Some(r)
}

/// Следующая тема по кругу; из «как в ОС» — первая.
fn next(st: &UiState) -> String {
    let all = ids(st);
    let at = all.iter().position(|i| *i == st.theme.as_str());
    match at {
        Some(i) => all[(i + 1) % all.len()].clone(),
        None => all.first().cloned().unwrap_or_else(|| AUTO.to_owned()),
    }
}

/// Перечитать свои темы. Без `force` — только если mtime каталога изменился
/// (реактивация окна).
pub fn reload(cx: &mut Cx<'_>, force: bool) {
    let st = ui_state(cx);
    let Some(dir) = st.themes_dir.clone() else {
        return;
    };
    let known = st.user_themes.mtime;
    cx.rt
        .spawn(Lane::Interactive, "ui.themes", cx.rt.token(), move |t| {
            let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
            if !force && mtime == known {
                return;
            }
            let (themes, errors) = theme::load_dir(&dir);
            let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
            let _ = t.sink.send(
                Box::new(Loaded {
                    themes,
                    errors,
                    mtime,
                }),
                &t.cancel,
            );
        });
}

struct Loaded {
    themes: Vec<theme::Theme>,
    errors: Vec<String>,
    mtime: Option<SystemTime>,
}

impl Msg for Loaded {
    fn name(&self) -> &'static str {
        "ui.themes"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let st = ui_state(cx);
        st.user_themes = UserThemes {
            themes: Arc::new(self.themes),
            generation: st.user_themes.generation + 1,
            mtime: self.mtime,
        };
        for e in self.errors {
            cx.toast(Level::Warn, tr("ui-theme-error", &[("error", &e)]));
        }
    }
}
