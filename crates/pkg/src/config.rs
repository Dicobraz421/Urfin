//! Секция `[pkg]` конфига (F02 §2.6).

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::api::{Mgr, PkgId};

#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые настройки менеджеров"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PkgConfig {
    /// Порядок менеджеров: «рекомендуется» — вариант первого (решение владельца №4).
    pub order: Vec<Mgr>,
    /// Где искать по умолчанию.
    pub search_in: SearchIn,
    /// «Важные» пакеты: `winget:Mozilla.Firefox`, `scoop:main/7zip`.
    pub important: Vec<PkgId>,
    /// `--silent` у winget install/upgrade.
    pub winget_silent: bool,
    /// `--include-unknown` у `winget upgrade`.
    pub winget_include_unknown: bool,
    /// `scoop update` (git pull бакетов) перед `scoop status`.
    pub scoop_update_buckets: bool,
    pub timeouts: Timeouts,
    pub schedule: Schedule,
}

impl Default for PkgConfig {
    fn default() -> Self {
        PkgConfig {
            order: Mgr::ALL.to_vec(),
            search_in: SearchIn::All,
            important: Vec::new(),
            winget_silent: true,
            winget_include_unknown: false,
            scoop_update_buckets: true,
            timeouts: Timeouts::default(),
            schedule: Schedule::default(),
        }
    }
}

/// Область поиска: все найденные менеджеры или один (`pkg:search?in=`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchIn {
    #[default]
    All,
    Winget,
    Scoop,
    Choco,
}

impl SearchIn {
    /// Один менеджер; `None` — все.
    pub const fn mgr(self) -> Option<Mgr> {
        match self {
            SearchIn::All => None,
            SearchIn::Winget => Some(Mgr::Winget),
            SearchIn::Scoop => Some(Mgr::Scoop),
            SearchIn::Choco => Some(Mgr::Choco),
        }
    }
}

/// Таймауты запросов, секунды (F02 §2.3); у действий таймаута нет.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timeouts {
    pub version_s: u32,
    pub list_s: u32,
    /// Обновление источников winget и `scoop update`.
    pub updates_s: u32,
    /// На менеджер.
    pub search_s: u32,
    /// «Нет вывода N мин» в job-баре у действий.
    pub silent_warn_min: u32,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            version_s: 5,
            list_s: 60,
            updates_s: 180,
            search_s: 30,
            silent_warn_min: 10,
        }
    }
}

/// Проверка обновлений Планировщиком (F02 §4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    /// Ежедневно, `ЧЧ:ММ`; пропущенный запуск — при входе.
    pub at: CompactString,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            enabled: false,
            at: "12:00".into(),
        }
    }
}
