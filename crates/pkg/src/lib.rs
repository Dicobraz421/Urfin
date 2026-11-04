//! Urfin — менеджеры пакетов winget, scoop, choco (F02): вкладка `pkg:`, установка,
//! обновление, удаление, фоновая проверка обновлений. Без `unsafe` и UI.
//!
//! Сделано: типы, `[pkg]`, команды `pkg.*` (F02-01); процессы и декодирование вывода
//! (F02-02); обнаружение менеджеров и ARP (F02-03); адаптеры winget (F02-04), scoop
//! (F02-05) и choco с запуском через UAC (F02-06); объединение списков менеджеров и
//! сравнение версий (F02-07); вкладка `pkg:` — «Установленные» и «Обновления», кэш,
//! Ctrl+R (F02-08); поиск (F02-09); установка, обновление и удаление как job (F02-10);
//! проверка по расписанию `--pkg-check`, задача Планировщика и баннер обновлений
//! (F02-11). Остальные команды — «ещё не сделано» тостом до F02-12
//! (`docs/specs/F02.md` §5).

pub mod api;
pub mod arp;
pub mod book;
pub mod check;
pub mod choco;
pub mod config;
pub mod decode;
pub mod detect;
pub mod elevated;
#[cfg(windows)]
mod found;
mod handler;
pub mod jobs;
pub mod merge;
#[cfg(windows)]
pub mod proc;
pub mod schedule;
pub mod scoop;
pub mod search;
pub mod table;
#[cfg(all(test, windows))]
mod testutil;
pub mod version;
pub mod vfs;
pub mod winget;

pub use api::{
    ActionDone, ActionPlan, BadPkgId, CheckedId, Detected, Elevation, Hit, LineSink, MatchKind,
    Mgr, MgrCaps, Package, PackageManager, PkgError, PkgId, PkgOp, RunCx, Update, valid_id,
};
pub use arp::{Arp, ArpEntry, ArpScope};
pub use book::{
    Busy, CACHE_FILE, Filter, MgrLine, PkgBook, PkgCache, PkgFeed, PkgInfo, PkgRow, Rows, Run, View,
};
pub use check::{CHECK_FILE, CheckError, CheckLock, CheckReport, LOCK_FILE, MgrStatus};
#[cfg(windows)]
pub use choco::ChocoAdapter;
pub use config::{PkgConfig, Schedule, SearchIn, Timeouts};
pub use detect::{MANAGERS_FILE, Managers, detect_all, find_powershell};
pub use handler::{PkgHandler, PkgSource, SystemSource};
pub use merge::{Group, InstalledMerge, InstalledRow, SearchMerge, Variant};
#[cfg(windows)]
pub use scoop::ScoopAdapter;
pub use search::{Answer, SearchInfo, SearchKind, SearchRow, SearchView};
pub use version::Version;
pub use vfs::PkgVfs;
#[cfg(windows)]
pub use winget::WingetAdapter;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use urfin_core::{AppState, CmdError, Command, Core};

/// Состояние пакетов в `AppState.ext` (S0 v6 №3): конфиг и книга вкладки `pkg:`. UI
/// читает `urfin_core::pkg::PkgModel` (F02-08a), его ставит handler рядом.
#[derive(Debug)]
pub struct PkgState {
    pub config: PkgConfig,
    pub book: Arc<PkgBook>,
}

/// Модель вкладки `pkg:` из состояния (после первого pump handler `pkg`).
pub fn book(s: &AppState) -> Option<&Arc<PkgBook>> {
    s.ext.get::<PkgState>().map(|p| &p.book)
}

pub(crate) fn not_yet(cmd: &Command) -> CmdError {
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("what", cmd.id.as_str().to_owned());
    CmdError::Rejected(urfin_i18n::current().fmt("pkg-not-yet", &args).into())
}

/// Регистрирует handler `pkg` со своей моделью, без кэша. Конфиг — `config.section("pkg")`.
pub fn register(core: &mut Core, config: PkgConfig) {
    core.register(Box::new(PkgHandler::new(config)));
}

/// Звонок «проверка по расписанию закончилась»: `urfin.exe --pkg-check` сообщает
/// открытому Урфину по IPC единственного экземпляра, handler `pkg` в следующем pump
/// читает `check.json` и показывает баннер (решение владельца F02 §8).
#[derive(Debug, Default)]
pub struct CheckBell(AtomicBool);

impl CheckBell {
    /// Итог готов (из потока IPC; Core будит его же waker).
    pub fn ring(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

/// Регистрирует handler `pkg`: `book` — общая с backend [`PkgVfs`] в реестре VFS;
/// `dir` — `state_dir/pkg` для `cache.json`, `managers.toml` и `check.json`; `bell` —
/// звонок от `--pkg-check`. Без каталога состояния (`--smoke`, `--screens`) — без кэша,
/// менеджеров и Планировщика: процессы не запускаются.
pub fn register_with(
    core: &mut Core,
    config: PkgConfig,
    book: Arc<PkgBook>,
    dir: Option<PathBuf>,
    bell: Arc<CheckBell>,
) {
    let (source, tasks): (Arc<dyn PkgSource>, Arc<dyn schedule::Tasks>) = match &dir {
        #[cfg(windows)]
        Some(d) => (Arc::new(SystemSource), Arc::new(schedule::Schtasks::new(d.clone()))),
        #[cfg(not(windows))]
        Some(_) => (Arc::new(SystemSource), Arc::new(schedule::NoTasks)),
        None => (Arc::new(handler::NoSource), Arc::new(schedule::NoTasks)),
    };
    core.register(Box::new(PkgHandler::with_tasks(
        config, book, source, dir, tasks, bell,
    )));
}

/// `urfin.exe --pkg-check` (F02 §4): проверка менеджеров системы в `dir`
/// (`state_dir/pkg`) без окна и GPUI. Код выхода — 0, если хоть один менеджер ответил
/// или проверка уже идёт (второй запуск выходит сразу); второе — итог записан.
pub fn check_main(dir: &std::path::Path, config: &PkgConfig) -> (u8, bool) {
    match check::run(dir, config, &SystemSource, &urfin_core::CancelToken::new(), false) {
        Ok(r) => {
            tracing::info!(target: "pkg", updates = r.updates.len(), "pkg-check: done");
            (0, true)
        }
        Err(CheckError::Busy) => {
            tracing::info!(target: "pkg", "pkg-check: already running");
            (0, false)
        }
        Err(e) => {
            tracing::warn!(target: "pkg", error = %e, "pkg-check: failed");
            (1, false)
        }
    }
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
        core.dispatch(Command::parse("pkg.important.toggle").unwrap());
        let toasts: Vec<String> = core
            .take_ui_events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Toast { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(toasts, ["Not available yet: pkg.important.toggle"]);
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
