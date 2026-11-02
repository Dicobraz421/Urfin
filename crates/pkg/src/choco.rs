//! choco (F02 §2.3, F02-06): запросы — `--limit-output` (`|`-разделитель, без
//! заголовков), у v2 ещё `--no-color`; v1 (0.x/1.x, `MgrCaps::CHOCO_V1`) — `list
//! --local-only`, поиск — `list <q>`.
//!
//! Мутирующие команды (`install|upgrade|uninstall <id> -y --no-progress`; цвет в
//! своём окне не мешает, `--no-color` не нужен и v1 его не знает) — с
//! `elevate: true`: choco сам права не повышает. Помощника `--pkg-elevated` нет
//! (решение владельца, F02 §8; S0 v6 №17): перед действием — строка
//! [`UAC_NOTICE`], затем choco запускается через UAC (`runas`) в своём консольном
//! окне ([`crate::elevated`]); ход — по журналу choco (`logs\chocolatey.log`), итог —
//! по коду выхода.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::api::{
    ActionPlan, Hit, MatchKind, Mgr, MgrCaps, Package, PkgError, PkgId, PkgOp, Update, valid_id,
};

/// Ключ Fluent строки-предупреждения перед действием choco.
pub const UAC_NOTICE: &str = "pkg-choco-uac";
/// Ключ Fluent: пользователь отклонил запрос UAC.
pub const UAC_DECLINED: &str = "pkg-uac-declined";
/// Ключ Fluent: отмена — повышенный choco мог продолжить работу.
pub const CANCEL_ELEVATED: &str = "pkg-cancel-elevated";

/// Коды «успешно» у запросов: при `useEnhancedExitCodes` `outdated` даёт 2, если
/// есть обновления, `list`/`search` — если ничего не нашлось.
pub const QUERY_OK: [u32; 2] = [0, 2];
/// «Успешно, нужна перезагрузка» (choco и инсталляторы MSI).
pub const REBOOT: [u32; 2] = [1641, 3010];
/// Флаги каждого действия.
pub const ACTION_FLAGS: [&str; 2] = ["-y", "--no-progress"];
/// Флаги, которые запуск с повышением пропускает (белый список, F02 §3.4):
/// флаги действий и `--noop` (пробный прогон без изменений — ручная проверка UAC).
pub const ALLOWED_FLAGS: [&str; 3] = ["-y", "--no-progress", "--noop"];

fn v1(caps: MgrCaps) -> bool {
    caps.contains(MgrCaps::CHOCO_V1)
}

fn argv(exe: &Path, args: &[&str], caps: MgrCaps) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![exe.into()];
    a.extend(args.iter().map(OsString::from));
    a.push("--limit-output".into());
    if !v1(caps) {
        a.push("--no-color".into());
    }
    a
}

/// Установленные: v2 `choco list`, v1 `choco list --local-only`.
pub fn list_argv(exe: &Path, caps: MgrCaps) -> Vec<OsString> {
    if v1(caps) {
        argv(exe, &["list", "--local-only"], caps)
    } else {
        argv(exe, &["list"], caps)
    }
}

/// Обновления: `choco outdated` (источники — сеть самого choco).
pub fn outdated_argv(exe: &Path, caps: MgrCaps) -> Vec<OsString> {
    argv(exe, &["outdated"], caps)
}

/// Запрос поиска: как у winget, и без ведущих `/` (choco понимает `/y` как флаг).
pub fn search_query(q: &str) -> String {
    let q = crate::winget::search_query(q);
    q.trim_start_matches(['/', '-']).trim().to_owned()
}

/// Поиск: v2 `choco search <q>`, v1 `choco list <q>`; `q` — после [`search_query`].
pub fn search_argv(exe: &Path, q: &str, caps: MgrCaps) -> Vec<OsString> {
    let verb = if v1(caps) { "list" } else { "search" };
    argv(exe, &[verb, q], caps)
}

/// Поля строки `--limit-output`: первое — id choco, полей не меньше `n`. Прочие
/// строки (предупреждения, «N packages installed») — `None`.
fn fields(line: &str, n: usize) -> Option<Vec<&str>> {
    let f: Vec<&str> = line.trim().split('|').map(str::trim).collect();
    (f.len() >= n && valid_id(Mgr::Choco, f[0]) && !f[1].is_empty()).then_some(f)
}

fn package(id: &str, version: &str) -> Package {
    Package {
        id: PkgId {
            mgr: Mgr::Choco,
            id: id.into(),
        },
        name: id.into(),
        version: version.into(),
        source: None,
        publisher: None,
    }
}

/// `list`: `id|version`.
pub fn parse_list(lines: &[String]) -> Vec<Package> {
    lines
        .iter()
        .filter_map(|l| fields(l, 2))
        .map(|f| package(f[0], f[1]))
        .collect()
}

/// `outdated`: `id|current|available|pinned`.
pub fn parse_outdated(lines: &[String]) -> Vec<Update> {
    lines
        .iter()
        .filter_map(|l| fields(l, 4))
        .filter(|f| !f[2].is_empty())
        .map(|f| Update {
            pkg: package(f[0], f[1]),
            available: f[2].into(),
            pinned: f[3].eq_ignore_ascii_case("true"),
        })
        .collect()
}

/// `search` (v1 — `list <q>`): `id|version`.
pub fn parse_search(q: &str, lines: &[String]) -> Vec<Hit> {
    parse_list(lines)
        .into_iter()
        .map(|pkg| Hit {
            matched: MatchKind::of(q, &pkg.id.id, &pkg.name),
            pkg,
        })
        .collect()
}

/// Действие: `choco <install|upgrade|uninstall> <id> -y --no-progress`,
/// `elevate: true`.
pub fn action_plan(exe: &Path, op: PkgOp, id: &PkgId) -> Result<ActionPlan, PkgError> {
    let checked = id.checked(Mgr::Choco)?;
    let verb = match op {
        PkgOp::Install => "install",
        PkgOp::Upgrade => "upgrade",
        PkgOp::Uninstall => "uninstall",
    };
    let mut argv: Vec<OsString> = vec![exe.into(), verb.into(), checked.into()];
    argv.extend(ACTION_FLAGS.map(OsString::from));
    Ok(ActionPlan {
        argv,
        elevate: true,
        success_codes: vec![0],
        reboot_codes: REBOOT.to_vec(),
    })
}

/// Белый список запуска с повышением (F02 §3.4): `choco.exe` (`exe`, если задан —
/// именно он), `install|upgrade|uninstall`, id по регэкспу choco и только
/// [`ALLOWED_FLAGS`]. Иное с правами администратора не запускается.
pub fn whitelisted(argv: &[OsString], exe: Option<&Path>) -> bool {
    let [prog, verb, id, flags @ ..] = argv else {
        return false;
    };
    let prog = Path::new(prog);
    let is_choco = prog
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("choco.exe"))
        && prog.is_absolute()
        && exe.is_none_or(|e| e == prog);
    let verb_ok = verb
        .to_str()
        .is_some_and(|v| ["install", "upgrade", "uninstall"].contains(&v));
    let id_ok = id.to_str().is_some_and(|i| valid_id(Mgr::Choco, i));
    let flags_ok = flags
        .iter()
        .all(|f| f.to_str().is_some_and(|f| ALLOWED_FLAGS.contains(&f)));
    is_choco && verb_ok && id_ok && flags_ok
}

/// Журнал choco: `<корень>\logs\chocolatey.log`, корень — над `bin\choco.exe`.
pub fn log_path(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?.parent()?;
    Some(root.join("logs").join("chocolatey.log"))
}

/// Настоящий `choco.exe` над шимом `bin\choco.exe` (`<корень>\choco.exe`): его
/// подпись показывает окно UAC. Шим (`ShimGen`) подписи может не иметь.
pub fn real_exe(shim: &Path) -> Option<PathBuf> {
    let bin = shim.parent()?;
    if !bin.file_name()?.eq_ignore_ascii_case("bin") {
        return None;
    }
    Some(bin.parent()?.join("choco.exe"))
}

/// Уровень записи журнала (log4net).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Начало записи `2026-11-01 12:00:00,123 4242 [INFO ] - текст`: уровень и текст.
/// Строка без такого начала — продолжение предыдущей записи (`None`).
pub fn log_record(line: &str) -> Option<(LogLevel, &str)> {
    let date = line.get(..10)?;
    if !date.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return None;
    }
    let open = line.find(" [")?;
    let close = open + line[open..].find("] - ")?;
    let level = match line[open + 2..close].trim() {
        "DEBUG" | "TRACE" | "VERBOSE" => LogLevel::Debug,
        "INFO" => LogLevel::Info,
        "WARN" => LogLevel::Warn,
        "ERROR" | "FATAL" => LogLevel::Error,
        _ => return None,
    };
    Some((level, &line[close + 4..]))
}

/// Фильтр журнала для ленточного сеанса: записи без `DEBUG`, продолжения — с уровнем
/// своей записи.
#[derive(Debug, Default)]
pub struct LogFilter {
    debug: bool,
}

impl LogFilter {
    /// Строка для показа или `None`.
    pub fn line<'a>(&mut self, line: &'a str) -> Option<&'a str> {
        match log_record(line) {
            Some((level, text)) => {
                self.debug = level == LogLevel::Debug;
                (!self.debug).then_some(text)
            }
            None => (!self.debug && !line.trim().is_empty()).then_some(line),
        }
    }
}

#[cfg(windows)]
pub use adapter::ChocoAdapter;

#[cfg(windows)]
mod adapter {
    use std::ffi::OsString;

    use compact_str::CompactString;

    use super::{
        QUERY_OK, action_plan, list_argv, log_path, outdated_argv, parse_list, parse_outdated,
        parse_search, real_exe, search_argv, search_query, whitelisted,
    };
    use crate::api::{
        ActionPlan, Detected, Hit, Mgr, Package, PackageManager, PkgError, PkgId, PkgOp, RunCx,
        Update,
    };
    use crate::detect::query_version;
    use crate::elevated;
    use crate::found::Found;
    use crate::proc::{ActionDone, run_query};

    /// choco через процессы (F02 §2.3); действия — через UAC (F02 §8).
    #[derive(Debug)]
    pub struct ChocoAdapter {
        found: Found,
    }

    impl ChocoAdapter {
        /// `cached` — из `managers.toml`.
        pub fn new(cached: Option<Detected>) -> ChocoAdapter {
            ChocoAdapter {
                found: Found::new(Mgr::Choco, cached),
            }
        }

        /// Запрос: версия (v1/v2) — заранее, коды [`QUERY_OK`] — успех.
        fn query(
            &self,
            argv: impl FnOnce(&std::path::Path, crate::api::MgrCaps) -> Vec<OsString>,
            cx: &RunCx,
        ) -> Result<Vec<String>, PkgError> {
            let exe = self.found.exe()?;
            if let Err(e) = self.version(cx) {
                tracing::warn!(error = %e, "pkg: choco --version");
            }
            let out = run_query(&argv(&exe, self.found.caps()), cx)?;
            if QUERY_OK.contains(&out.code) {
                Ok(out.lines)
            } else {
                out.ok().map(|o| o.lines)
            }
        }

        /// Исполнить план действия: белый список, затем choco через UAC в своём окне
        /// (настоящий `choco.exe`, не шим — подпись в окне UAC); строки журнала choco
        /// — в `cx.lines`. Отказ в UAC —
        /// `PkgError::Elevation(Declined)`; отмена — `Cancelled` (повышенный choco мог
        /// продолжить: [`super::CANCEL_ELEVATED`]).
        pub fn run_action(&self, plan: &ActionPlan, cx: &RunCx) -> Result<ActionDone, PkgError> {
            let exe = self.found.exe()?;
            if !whitelisted(&plan.argv, Some(&exe)) {
                return Err(PkgError::Spawn("argv outside the choco whitelist".into()));
            }
            let mut plan = plan.clone();
            if let Some(real) = real_exe(&exe).filter(|p| p.is_file()) {
                plan.argv[0] = real.into();
            }
            elevated::run(&plan, log_path(&exe).as_deref(), cx)
        }
    }

    impl PackageManager for ChocoAdapter {
        fn mgr(&self) -> Mgr {
            Mgr::Choco
        }

        fn detect(&self) -> Option<Detected> {
            self.found.get()
        }

        /// `choco --version`; заодно v1/v2 (`MgrCaps::CHOCO_V1`).
        fn version(&self, cx: &RunCx) -> Result<CompactString, PkgError> {
            self.found.version(|d| query_version(d, cx))
        }

        fn list_installed(&self, cx: &RunCx) -> Result<Vec<Package>, PkgError> {
            Ok(parse_list(&self.query(list_argv, cx)?))
        }

        fn list_updates(&self, cx: &RunCx) -> Result<Vec<Update>, PkgError> {
            Ok(parse_outdated(&self.query(outdated_argv, cx)?))
        }

        fn search(&self, q: &str, cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
            let q = search_query(q);
            if q.is_empty() {
                return Ok(Vec::new());
            }
            let lines = self.query(|exe, caps| search_argv(exe, &q, caps), cx)?;
            Ok(parse_search(&q, &lines))
        }

        fn action(&self, op: PkgOp, id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
            action_plan(&self.found.exe()?, op, id)
        }

        fn run(&self, plan: &ActionPlan, cx: &RunCx) -> Result<ActionDone, PkgError> {
            ChocoAdapter::run_action(self, plan, cx)
        }
    }
}

#[cfg(test)]
#[path = "choco_tests.rs"]
mod tests;
