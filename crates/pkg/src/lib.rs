//! Urfin — менеджеры пакетов winget, scoop, choco (F02): вкладка `pkg:`, установка,
//! обновление, удаление, фоновая проверка обновлений. Без `unsafe` и UI.
//!
//! F02-01 — каркас: типы, `[pkg]`, команды `pkg.*` заглушками («ещё не сделано»
//! тостом); остальное — F02-02…F02-14 (`docs/specs/F02.md` §5).

pub mod api;
pub mod check;
pub mod config;

pub use api::{
    ActionPlan, BadPkgId, Detected, Elevation, Hit, LineSink, MatchKind, Mgr, MgrCaps, Package,
    PackageManager, PkgError, PkgId, PkgOp, RunCx, Update,
};
pub use check::{CHECK_FILE, CheckReport, MgrStatus};
pub use config::{PkgConfig, Schedule, SearchIn, Timeouts};

use urfin_core::{ArgSpec, CmdError, Command, CommandMeta, Core, Cx, Handler, ValueKind};

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "pkg",
        args,
        repeatable: false,
    }
}

/// Отмеченные строки или строка под курсором.
const TARGETS: [ArgSpec; 1] = [arg("targets", ValueKind::List, false)];

/// Команды F02 §2.5.
static PKG_COMMANDS: [CommandMeta; 17] = [
    meta(
        "pkg.open",
        "Packages…",
        &[arg("filter", ValueKind::Str, false)],
    ),
    meta(
        "pkg.filter",
        "Packages: show",
        &[arg("name", ValueKind::Str, true)],
    ),
    meta("pkg.refresh", "Packages: refresh", &[]),
    meta(
        "pkg.search",
        "Find package",
        &[
            arg("q", ValueKind::Str, false),
            arg("in", ValueKind::Str, false),
        ],
    ),
    meta("pkg.search_in", "Search in managers", &[]),
    meta("pkg.pick", "Choose variant", &[]),
    meta(
        "pkg.install",
        "Install package",
        &[arg("id", ValueKind::Str, false)],
    ),
    meta("pkg.upgrade", "Upgrade packages", &TARGETS),
    meta("pkg.upgrade_all", "Upgrade all", &[]),
    meta("pkg.uninstall", "Uninstall packages", &TARGETS),
    meta("pkg.info", "Package info", &[]),
    meta("pkg.output", "Package manager output", &[]),
    meta("pkg.important.toggle", "Mark as important", &[]),
    meta("pkg.check_now", "Check for updates now", &[]),
    meta(
        "pkg.schedule",
        "Scheduled update check",
        &[
            arg("on", ValueKind::Bool, true),
            arg("at", ValueKind::Str, false),
        ],
    ),
    meta("pkg.banner.show", "Show updates", &[]),
    meta("pkg.banner.later", "Remind later", &[]),
];

/// Состояние пакетов в `AppState.ext` (S0 v6 №3): результаты запросов приходят сюда
/// через `MsgSink`.
#[derive(Debug, Default)]
pub struct PkgState {
    pub config: PkgConfig,
}

/// Handler `pkg`.
#[derive(Debug)]
pub struct PkgHandler {
    /// Конфиг до первого pump: тогда `PkgState` встаёт в `AppState.ext`.
    pending: Option<PkgConfig>,
}

impl PkgHandler {
    pub fn new(config: PkgConfig) -> PkgHandler {
        PkgHandler {
            pending: Some(config),
        }
    }
}

impl Handler for PkgHandler {
    fn namespace(&self) -> &'static str {
        "pkg"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &PKG_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        Err(not_yet(cmd))
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some(config) = self.pending.take() {
            cx.state.ext.insert(PkgState { config });
        }
    }
}

fn not_yet(cmd: &Command) -> CmdError {
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("what", cmd.id.as_str().to_owned());
    CmdError::Rejected(urfin_i18n::current().fmt("pkg-not-yet", &args).into())
}

/// Регистрирует handler `pkg`. Конфиг — `config.section("pkg")`.
pub fn register(core: &mut Core, config: PkgConfig) {
    core.register(Box::new(PkgHandler::new(config)));
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use urfin_core::{Boot, Location, Services, UiEvent, Waker};

    use super::*;

    struct NoWake;

    impl Waker for NoWake {
        fn wake(&self) {}
        fn wake_after(&self, _d: Duration) {}
    }

    fn core() -> Core {
        let root = Location::local(std::env::temp_dir().join("urfin-pkg-test"));
        let mut core = Core::new(
            Boot::new(root.clone(), root),
            Services::null(),
            Arc::new(NoWake),
        );
        register(&mut core, PkgConfig::default());
        core
    }

    /// Все команды F02 §2.5 — в реестре, в namespace `pkg`, с заголовком `cmd-pkg-*`.
    #[test]
    fn commands_registered() {
        let core = core();
        let ids: Vec<&str> = core
            .registry()
            .iter()
            .filter(|m| m.category == "pkg")
            .map(|m| m.id)
            .collect();
        assert_eq!(
            ids,
            [
                "pkg.open",
                "pkg.filter",
                "pkg.refresh",
                "pkg.search",
                "pkg.search_in",
                "pkg.pick",
                "pkg.install",
                "pkg.upgrade",
                "pkg.upgrade_all",
                "pkg.uninstall",
                "pkg.info",
                "pkg.output",
                "pkg.important.toggle",
                "pkg.check_now",
                "pkg.schedule",
                "pkg.banner.show",
                "pkg.banner.later",
            ]
        );
        let en = urfin_i18n::Catalog::embedded()
            .build(&"en".parse().unwrap())
            .0;
        for id in ids {
            let key = format!("cmd-{}", id.replace('.', "-"));
            assert!(en.has(&key), "{key}");
        }
    }

    /// `PkgState` — в `AppState.ext` после первого pump.
    #[test]
    fn state_in_ext() {
        let mut core = core();
        assert!(core.state().ext.get::<PkgState>().is_none());
        core.pump(Duration::from_millis(5));
        let st = core.state().ext.get::<PkgState>().unwrap();
        assert_eq!(st.config, PkgConfig::default());
    }

    /// Команды пока отклоняются тостом «ещё не сделано».
    #[test]
    fn commands_not_yet() {
        let mut core = core();
        core.dispatch(Command::parse("pkg.schedule on=true at=\"09:30\"").unwrap());
        let toasts: Vec<String> = core
            .take_ui_events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Toast { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(toasts, ["Not available yet: pkg.schedule"]);
    }

    /// Секция `[pkg]` по умолчанию — снапшот (изменение умолчаний видно в ревью).
    #[test]
    fn default_config_snapshot() {
        insta::assert_snapshot!(toml::to_string(&PkgConfig::default()).unwrap());
    }

    #[test]
    fn config_roundtrip() {
        let cfg = PkgConfig {
            order: vec![Mgr::Scoop, Mgr::Winget],
            search_in: SearchIn::Choco,
            important: vec![
                "winget:Mozilla.Firefox".parse().unwrap(),
                "scoop:main/7zip".parse().unwrap(),
            ],
            winget_include_unknown: true,
            schedule: Schedule {
                enabled: true,
                at: "08:15".into(),
            },
            ..PkgConfig::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(toml::from_str::<PkgConfig>(&text).unwrap(), cfg);
    }

    /// Пустая и частичная секция дают умолчания; плохой id «важного» — ошибка секции.
    #[test]
    fn partial_config() {
        assert_eq!(
            toml::from_str::<PkgConfig>("").unwrap(),
            PkgConfig::default()
        );
        let c: PkgConfig = toml::from_str(
            "important = [\"choco:git\"]\ntimeouts = { list_s = 90 }\n[schedule]\nenabled = true",
        )
        .unwrap();
        assert_eq!(c.important[0].mgr, Mgr::Choco);
        assert_eq!(c.timeouts.list_s, 90);
        assert_eq!(c.timeouts.search_s, 30);
        assert!(c.schedule.enabled);
        assert_eq!(c.schedule.at, "12:00");
        assert!(toml::from_str::<PkgConfig>("important = [\"apt:vim\"]").is_err());
    }
}
