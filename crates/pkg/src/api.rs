//! Трейт менеджера пакетов и типы (F02 §2.1). Адаптеры синхронные; асинхронность — в
//! `PkgHandler` (поток на запрос).

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bitflags::bitflags;
use compact_str::CompactString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use urfin_core::CancelToken;

/// Менеджер пакетов. Порядок вариантов — порядок `[pkg].order` по умолчанию.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mgr {
    Winget,
    Scoop,
    Choco,
}

impl Mgr {
    pub const ALL: [Mgr; 3] = [Mgr::Winget, Mgr::Scoop, Mgr::Choco];

    /// Имя в конфиге, `PkgId` и `pkg:search?in=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Mgr::Winget => "winget",
            Mgr::Scoop => "scoop",
            Mgr::Choco => "choco",
        }
    }

    /// Имя без учёта регистра.
    pub fn parse(s: &str) -> Option<Mgr> {
        Mgr::ALL
            .into_iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(s))
    }
}

impl fmt::Display for Mgr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Пакет у менеджера: winget Id, scoop `bucket/name`, choco id. Текст — `mgr:id`
/// (`winget:Mozilla.Firefox`, `scoop:main/7zip`): так он пишется в `[pkg].important`
/// и `check.json`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PkgId {
    pub mgr: Mgr,
    pub id: CompactString,
}

/// Текст не `mgr:id` с известным менеджером и непустым id.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("bad package id {0:?}: expected <winget|scoop|choco>:<id>")]
pub struct BadPkgId(pub CompactString);

impl FromStr for PkgId {
    type Err = BadPkgId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (mgr, id) = s.split_once(':').unwrap_or_default();
        let id = id.trim();
        match Mgr::parse(mgr.trim()) {
            Some(mgr) if !id.is_empty() => Ok(PkgId { mgr, id: id.into() }),
            _ => Err(BadPkgId(s.into())),
        }
    }
}

impl fmt::Display for PkgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.mgr, self.id)
    }
}

impl Serialize for PkgId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PkgId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        CompactString::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub id: PkgId,
    pub name: CompactString,
    pub version: CompactString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<CompactString>,
}

/// Доступное обновление установленного пакета.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub pkg: Package,
    pub available: CompactString,
    #[serde(default)]
    pub pinned: bool,
}

/// Качество совпадения поиска; меньше — лучше (порядок групп, F02 §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MatchKind {
    Exact,
    IdTail,
    Prefix,
    Contains,
}

/// Результат поиска.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub pkg: Package,
    pub matched: MatchKind,
}

/// Мутирующая операция.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PkgOp {
    Install,
    Upgrade,
    Uninstall,
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

/// Приёмник строк вывода: ленточный сеанс и прогресс.
pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Контекст одного запроса к менеджеру.
#[derive(Clone, Default)]
pub struct RunCx {
    pub cancel: CancelToken,
    /// `None` — без таймаута (действия, F02 §2.3).
    pub timeout: Option<Duration>,
    pub lines: Option<LineSink>,
}

impl fmt::Debug for RunCx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunCx")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("timeout", &self.timeout)
            .field("lines", &self.lines.is_some())
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
}
