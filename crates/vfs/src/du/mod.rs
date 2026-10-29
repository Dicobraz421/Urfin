//! Анализ места (F-01, `docs/specs/F01.md`): модель снимка, секция `[du]`, handler `du`.
//! Подсчёт — только по явной команде (`du.open`, №81/№83), без фона.

mod handler;
pub mod model;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

pub use handler::DuHandler;
pub use model::*;

/// Секция `[du]` конфига (F01 §2.1). Размеры и возраст — строки как в конфиге
/// (`"1 MiB"`, `"1y"`); разбирают их задачи, которые ими пользуются.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DuCfg {
    /// `disk` (на диске) | `logical`.
    pub size: SizeMode,
    /// 0 — авто: `NVMe` 8, SSD 4, HDD 1, сеть 2.
    pub threads: u8,
    /// Порог файла для Крупные/Старые/Дубликаты.
    pub track_min: CompactString,
    /// Порог «Старые»: `6m` / `1y` / `2y` / `5y`.
    pub old_age: CompactString,
    /// Меньше — в дубликаты не идут.
    pub dup_min: CompactString,
    /// Общий лимит `state\du\`, LRU по времени открытия.
    pub cache_max: CompactString,
    /// Снимков в кэше максимум.
    pub cache_keep: u16,
}

impl Default for DuCfg {
    fn default() -> Self {
        DuCfg {
            size: SizeMode::Disk,
            threads: 0,
            track_min: "1 MiB".into(),
            old_age: "1y".into(),
            dup_min: "1 MiB".into(),
            cache_max: "256 MiB".into(),
            cache_keep: 8,
        }
    }
}

/// Регистрирует handler `du`. Конфиг — `config.section("du")`.
pub fn register(core: &mut urfin_core::Core, cfg: DuCfg) {
    core.register(Box::new(DuHandler::new(cfg)));
}
