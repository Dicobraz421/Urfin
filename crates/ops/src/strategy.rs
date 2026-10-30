//! Таблица стратегий копирования (S5 §0, S5-14): какой движок для какого
//! файла. Источник — `[ops.strategy]` поверх умолчаний, выбранных бенчами.
//!
//! Мелкие файлы (S5-14, `docs/perf/s5-14-small-files.md`): по умолчанию —
//! системная копия (`CopyFileExW`). Свой путь (`small = "own"`) на стенде с
//! Defender не быстрее (в 8 потоков — на 45 % медленнее): системная копия
//! сообщает антивирусу, что это копия уже проверенного файла. `CopyFile2` —
//! в пределах шума от `CopyFileExW`.
//!
//! Крупные файлы (S5-16, `docs/perf/s5-16-large-files.md`): от `[ops] large_file`
//! (256 МБ) — `CopyFileExW` с `COPY_FILE_NO_BUFFERING`: та же скорость, но кэш ОС
//! не вытесняется гигабайтами копии.

use urfin_vfs::volume::Media;

use crate::api::Engine;

/// Выбор движков по размеру файла.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StrategyTable {
    /// Мелкий файл (< `small_file`): `Os` или `Small` (свой путь).
    pub small: Engine,
    /// От стольких байт — мимо кэша ОС.
    pub large: u64,
    /// Job на одно устройство по классу носителя (`[ops.device_jobs]`, S5-19).
    pub jobs: crate::config::DeviceJobs,
}

/// `[ops] large_file` по умолчанию.
pub(crate) const LARGE_FILE: u64 = 256 << 20;

impl Default for StrategyTable {
    fn default() -> Self {
        StrategyTable {
            small: Engine::Os,
            large: LARGE_FILE,
            jobs: crate::config::DeviceJobs::default(),
        }
    }
}

impl StrategyTable {
    /// Из `[ops.strategy]`; неизвестные значения — умолчание и предупреждение
    /// в лог. Ключи вида `"ssd>ssd"` — S5-19, здесь не разбираются.
    pub(crate) fn from_config(cfg: &crate::config::OpsConfig) -> StrategyTable {
        let map = &cfg.strategy;
        let mut t = StrategyTable {
            large: cfg.large_file,
            jobs: cfg.device_jobs,
            ..StrategyTable::default()
        };
        if let Some(v) = map.get("small") {
            match v.as_str() {
                "os" => t.small = Engine::Os,
                "own" => t.small = Engine::Small,
                other => tracing::warn!(value = other, "[ops.strategy] small: os | own"),
            }
        }
        t
    }
}

/// Стартовое число воркеров по носителю (S5 §3.3).
pub(crate) fn workers_for(m: Media) -> usize {
    match m {
        Media::Nvme => 16,
        Media::Ssd | Media::Net => 8,
        Media::Hdd => 1,
        Media::Usb => 2,
        Media::Unknown => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_engine_from_config() {
        let mut cfg = crate::config::OpsConfig {
            large_file: 1,
            ..crate::config::OpsConfig::default()
        };
        assert_eq!(StrategyTable::from_config(&cfg).small, Engine::Os);
        assert_eq!(StrategyTable::from_config(&cfg).large, 1);
        cfg.strategy.insert("small".into(), "own".into());
        assert_eq!(StrategyTable::from_config(&cfg).small, Engine::Small);
        cfg.strategy.insert("small".into(), "turbo".into());
        assert_eq!(StrategyTable::from_config(&cfg).small, Engine::Os);
    }
}
