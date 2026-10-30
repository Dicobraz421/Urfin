//! Анализ места (F-01, `docs/specs/F01.md`): модель снимка, секция `[du]`, handler `du`.
//! Подсчёт — только по явной команде (`du.open`, №81/№83), без фона.

mod handler;
mod links;
pub mod lister;
pub mod model;
pub mod scan;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

pub use handler::DuHandler;
pub use lister::{FindLister, ListerKind, NtLister, VfsLister};
pub use model::*;
pub use scan::{OnScan, ScanCfg, ScanEvent, ScanOutcome, scan, start};

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

/// Размер из конфига: `"1 MiB"`, `"512KiB"`, `"2 GB"`, `"4096"`. Единицы — B, K/KB/KiB,
/// M/MB/MiB, G/GB/GiB, T/TB/TiB (все двоичные), регистр не важен.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let shift = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 0,
        "k" | "kb" | "kib" => 10,
        "m" | "mb" | "mib" => 20,
        "g" | "gb" | "gib" => 30,
        "t" | "tb" | "tib" => 40,
        _ => return None,
    };
    if let Ok(n) = num.parse::<u64>() {
        return n.checked_mul(1 << shift);
    }
    let f: f64 = num.parse().ok()?;
    let v = (f * f64::from(1u32 << (shift / 2)) * f64::from(1u32 << (shift - shift / 2))).round();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "проверено: конечное, в диапазоне u64"
    )]
    (v.is_finite() && (0.0..1.8e19).contains(&v)).then_some(v as u64)
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1 MiB"), Some(1 << 20));
        assert_eq!(parse_size("512KiB"), Some(512 << 10));
        assert_eq!(parse_size("2 gb"), Some(2 << 30));
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("1.5 MiB"), Some(3 << 19));
        assert_eq!(parse_size("1 parsec"), None);
        assert_eq!(parse_size(""), None);
    }
}
