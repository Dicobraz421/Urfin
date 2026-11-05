//! Трейт менеджера пакетов и типы (F02 §2.1). Адаптеры синхронные; асинхронность — в
//! `PkgHandler` (поток на запрос).

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bitflags::bitflags;
use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use urfin_core::CancelToken;

pub use urfin_core::pkg::{BadPkgId, Hit, MatchKind, Mgr, Package, PkgId, PkgOp, Update};

bitflags! {
    /// Возможности найденной версии менеджера; выясняются лениво (F02-03, F02-04).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct MgrCaps: u8 {
        /// winget: `list`/`search`/`upgrade` знают `--output json` (сверка по `--help`).
        const JSON_OUTPUT = 1;
        /// winget: установлен модуль PowerShell `Microsoft.WinGet.Client` (решение владельца №2).
        const PS_MODULE = 1 << 1;
        /// choco v1: `list --local-only`, поиск — `list` без флага.
        const CHOCO_V1 = 1 << 2;
    }
}

/// Найденный менеджер (кэш `state/pkg/managers.toml`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detected {
    pub mgr: Mgr,
    pub exe: PathBuf,
    pub version: Option<CompactString>,
    pub caps: MgrCaps,
}

/// Что запустить для операции; исполняет `jobs.rs` (F02 §3), адаптер ничего не запускает.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionPlan {
    /// argv массивом, без shell-обёрток.
    pub argv: Vec<OsString>,
    /// Нужны права администратора (UAC).
    pub elevate: bool,
    /// Коды выхода «успешно».
    pub success_codes: Vec<u32>,
    /// Коды выхода «успешно, нужна перезагрузка».
    pub reboot_codes: Vec<u32>,
}

/// Итог успешного действия.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionDone {
    Ok,
    /// Код из `reboot_codes`: нужна перезагрузка.
    Reboot,
}

/// Приёмник строк вывода: прогресс действия.
pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Лента вывода (F02 §3.1): ленточный сеанс «Пакеты». Пишет `proc.rs` и журнал choco.
pub trait Tape: Send + Sync {
    /// Заголовок операции.
    fn head(&self, text: &str);
    fn line(&self, text: &str);
    /// Конец: код выхода или причина (отмена, таймаут, запуск).
    fn exit(&self, res: Result<u32, &PkgError>);
}

/// Лента операции: куда и под каким заголовком (`winget upgrade Mozilla.Firefox`, argv
/// не годится: у scoop это `-EncodedCommand`).
#[derive(Clone)]
pub struct TapeCx {
    pub tape: Arc<dyn Tape>,
    pub head: CompactString,
}

impl fmt::Debug for TapeCx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TapeCx")
            .field("head", &self.head)
            .finish_non_exhaustive()
    }
}

/// Контекст одного запроса к менеджеру.
#[derive(Clone, Default)]
pub struct RunCx {
    pub cancel: CancelToken,
    /// `None` — без таймаута (действия, F02 §2.3).
    pub timeout: Option<Duration>,
    pub lines: Option<LineSink>,
    /// `None` — без ленты (`--pkg-check`, тесты).
    pub tape: Option<TapeCx>,
}

impl fmt::Debug for RunCx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunCx")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("timeout", &self.timeout)
            .field("lines", &self.lines.is_some())
            .field("tape", &self.tape.as_ref().map(|t| &t.head))
            .finish()
    }
}

/// Отказ повышения прав.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elevation {
    /// Пользователь отклонил запрос UAC.
    Declined,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PkgError {
    #[error("package manager not found")]
    NotFound,
    /// Процесс не запустился (кроме «нет файла» — это `NotFound`).
    #[error("cannot start: {0}")]
    Spawn(CompactString),
    #[error("timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    /// Неуспешный код выхода и хвост вывода.
    #[error("exit code {code}: {tail}")]
    Exit { code: u32, tail: CompactString },
    #[error("cannot parse output line {line:?}")]
    Parse { line: CompactString },
    #[error("elevation: {0:?}")]
    Elevation(Elevation),
    /// Id не прошёл проверку менеджера ([`valid_id`]): в argv не попадёт.
    #[error("bad package id {0:?}")]
    BadId(CompactString),
}

/// Потолок длины id в argv.
const MAX_ID: usize = 256;

/// Id для argv менеджера `mgr` (F02 §2.3: «каждый `PkgId` перед подстановкой
/// проверяется регэкспом своего менеджера»). Регэкспы:
/// - winget: `^[A-Za-z0-9][A-Za-z0-9._+-]*$`, локальные — `^(ARP|MSIX)\\[^"…\x00-\x1f]*[^\\"…\x00-\x1f]$`
///   (ключ ARP бывает с пробелами и не-ASCII);
/// - scoop: `^([A-Za-z0-9][A-Za-z0-9._-]*/)?[A-Za-z0-9][A-Za-z0-9._+-]*$`;
/// - choco: `^[A-Za-z0-9][A-Za-z0-9._-]*$`.
///
/// Ни один не пропускает ведущий `-` (флаг), кавычки, управляющие символы и
/// усечённое `…`.
pub fn valid_id(mgr: Mgr, id: &str) -> bool {
    fn word(s: &str, extra: &[char]) -> bool {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
    }
    if id.is_empty() || id.len() > MAX_ID {
        return false;
    }
    match mgr {
        Mgr::Winget => {
            let local = id
                .strip_prefix("ARP\\")
                .or_else(|| id.strip_prefix("MSIX\\"));
            match local {
                Some(rest) => {
                    !rest.is_empty()
                        && !rest.ends_with('\\')
                        && !rest.chars().any(|c| c.is_control() || c == '"' || c == '…')
                }
                None => word(id, &['.', '_', '+', '-']),
            }
        }
        Mgr::Scoop => {
            let (bucket, name) = id.split_once('/').map_or((None, id), |(b, n)| (Some(b), n));
            bucket.is_none_or(|b| word(b, &['.', '_', '-'])) && word(name, &['.', '_', '+', '-'])
        }
        Mgr::Choco => word(id, &['.', '_', '-']),
    }
}

/// Проверка id перед argv (тип `PkgId` — в core).
pub trait CheckedId {
    /// Id для argv менеджера `mgr`: свой менеджер и [`valid_id`], иначе `BadId`.
    fn checked(&self, mgr: Mgr) -> Result<&str, PkgError>;
}

impl CheckedId for PkgId {
    fn checked(&self, mgr: Mgr) -> Result<&str, PkgError> {
        if self.mgr == mgr && valid_id(mgr, &self.id) {
            Ok(&self.id)
        } else {
            Err(PkgError::BadId(self.to_string().into()))
        }
    }
}

/// Менеджер пакетов. Все методы синхронные и вызываются не на UI-потоке.
pub trait PackageManager: Send + Sync {
    fn mgr(&self) -> Mgr;
    /// Без запуска процесса (F02 §2.2).
    fn detect(&self) -> Option<Detected>;
    /// Лениво, с кэшем.
    fn version(&self, cx: &RunCx) -> Result<CompactString, PkgError>;
    fn list_installed(&self, cx: &RunCx) -> Result<Vec<Package>, PkgError>;
    fn list_updates(&self, cx: &RunCx) -> Result<Vec<Update>, PkgError>;
    fn search(&self, q: &str, cx: &RunCx) -> Result<Vec<Hit>, PkgError>;
    /// argv и признак повышения; ничего не запускает.
    fn action(&self, op: PkgOp, id: &PkgId, cx: &RunCx) -> Result<ActionPlan, PkgError>;
    /// argv сведений о пакете (`pkg.info`, F3); запуска нет.
    fn info_argv(&self, id: &PkgId) -> Result<Vec<OsString>, PkgError> {
        Err(PkgError::BadId(id.to_string().into()))
    }
    /// Сведения о пакете: вывод — в ленту `cx.tape`.
    fn info(&self, id: &PkgId, cx: &RunCx) -> Result<(), PkgError> {
        #[cfg(windows)]
        {
            crate::proc::run_query(&self.info_argv(id)?, cx)?
                .ok()
                .map(drop)
        }
        #[cfg(not(windows))]
        {
            let _ = (id, cx);
            Err(PkgError::NotFound)
        }
    }
    /// Исполнить план (F02 §3): строки — в `cx.lines`, итог — по кодам плана. По
    /// умолчанию — процесс без повышения прав; choco идёт через UAC (F02 §8).
    fn run(&self, plan: &ActionPlan, cx: &RunCx) -> Result<ActionDone, PkgError> {
        #[cfg(windows)]
        {
            crate::proc::run_action(plan, cx)
        }
        #[cfg(not(windows))]
        {
            let _ = (plan, cx);
            Err(PkgError::NotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_id_text() {
        let id: PkgId = "scoop:main/7zip".parse().unwrap();
        assert_eq!(
            id,
            PkgId {
                mgr: Mgr::Scoop,
                id: "main/7zip".into()
            }
        );
        assert_eq!(id.to_string(), "scoop:main/7zip");
        assert_eq!(
            "WinGet:Mozilla.Firefox".parse::<PkgId>().unwrap().mgr,
            Mgr::Winget
        );
        for bad in ["", "winget", "winget:", "apt:vim", ":x"] {
            assert!(bad.parse::<PkgId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn match_kind_order() {
        assert!(MatchKind::Exact < MatchKind::IdTail);
        assert!(MatchKind::Prefix < MatchKind::Contains);
    }

    #[test]
    fn match_kind_of() {
        assert_eq!(MatchKind::of("Git", "git", "Git"), MatchKind::Exact);
        assert_eq!(
            MatchKind::of("firefox", "Mozilla.Firefox", "Mozilla Firefox"),
            MatchKind::IdTail
        );
        assert_eq!(MatchKind::of("7zip", "main/7zip", "7zip"), MatchKind::Exact);
        assert_eq!(
            MatchKind::of("7zip", "main/7zip", "7-Zip"),
            MatchKind::IdTail
        );
        assert_eq!(
            MatchKind::of("fire", "Mozilla.Firefox", "Firefox"),
            MatchKind::Prefix
        );
        assert_eq!(
            MatchKind::of("fox", "Mozilla.Firefox", "Mozilla Firefox"),
            MatchKind::Contains
        );
    }

    #[test]
    fn ids_for_argv() {
        let ok = [
            (Mgr::Winget, "Mozilla.Firefox"),
            (Mgr::Winget, "Microsoft.VCRedist.2015+.x64"),
            (Mgr::Winget, "9NBLGGH4NNS1"),
            (
                Mgr::Winget,
                r"ARP\Machine\X64\{23170F69-40C1-2702-2409-000001000000}",
            ),
            (Mgr::Winget, r"ARP\User\X64\Mozilla Firefox 131.0 (x64 ru)"),
            (
                Mgr::Winget,
                r"MSIX\Microsoft.WindowsTerminal_1.21.2361.0_x64__8wekyb3d8bbwe",
            ),
            (Mgr::Scoop, "7zip"),
            (Mgr::Scoop, "main/7zip"),
            (Mgr::Scoop, "extras/vcredist2022"),
            (Mgr::Choco, "git.install"),
            (Mgr::Choco, "nodejs-lts"),
        ];
        for (mgr, id) in ok {
            assert!(valid_id(mgr, id), "{mgr}:{id}");
        }
        let long = "a".repeat(MAX_ID + 1);
        let bad = [
            (Mgr::Winget, ""),
            (Mgr::Winget, "--silent"),
            (Mgr::Winget, "-e"),
            (Mgr::Winget, "Microsoft.VisualStudio.2022.Commu…"),
            (Mgr::Winget, "Mozilla Firefox"),
            (Mgr::Winget, "a\"b"),
            (Mgr::Winget, r"ARP\Machine\X64\Mozilla Fire…"),
            (Mgr::Winget, r"ARP\"),
            (Mgr::Winget, r"ARP\Machine\"),
            (Mgr::Winget, "ARP\\x\ny"),
            (Mgr::Winget, &long),
            (Mgr::Scoop, "main/"),
            (Mgr::Scoop, "/7zip"),
            (Mgr::Scoop, "a/b/c"),
            (Mgr::Scoop, "7zip'; rm"),
            (Mgr::Choco, "git install"),
            (Mgr::Choco, "-y"),
            (Mgr::Choco, "git&calc"),
        ];
        for (mgr, id) in bad {
            assert!(!valid_id(mgr, id), "{mgr}:{id}");
        }
        let id: PkgId = "choco:git".parse().unwrap();
        assert_eq!(id.checked(Mgr::Choco), Ok("git"));
        assert_eq!(
            id.checked(Mgr::Winget),
            Err(PkgError::BadId("choco:git".into()))
        );
    }
}
