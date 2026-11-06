//! Анализ места (F-01, `docs/specs/F01.md`): модель снимка, секция `[du]`, handler `du`.
//! Подсчёт — только по явной команде (`du.open`, №81/№83), без фона.

pub mod cache;
pub mod dups;
pub mod dvfs;
pub mod elevated;
pub mod filters;
pub mod fresh;
mod handler;
mod links;
pub mod lister;
pub mod model;
pub mod scan;
mod views;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

pub use cache::{CacheError, DuCache};
pub use dvfs::{DuInfo, DuRegistry, DuState, DuVfs, DupsInfo, ElevInfo, SnapId};
pub use filters::{TypeGroup, View};
pub use handler::{DuHandler, elevate_forbidden};
pub use lister::{FindLister, ListerKind, NtLister, VfsLister};
pub use model::*;
pub use scan::{OnScan, ScanCfg, ScanEvent, ScanOutcome, pick_lister, scan, start};
pub use views::{Hint, MapDir, MapItem, MapKind};

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

/// Регистрирует handler `du` со своим реестром, без кэша. Конфиг — `config.section("du")`.
pub fn register(core: &mut urfin_core::Core, cfg: DuCfg) {
    core.register(Box::new(DuHandler::new(cfg)));
}

/// Регистрирует handler `du`: `reg` — общий с backend [`DuVfs`] в реестрах VFS
/// листинга и вьювера; `cache` — каталог снимков (`None` — без кэша).
pub fn register_with(
    core: &mut urfin_core::Core,
    cfg: DuCfg,
    reg: std::sync::Arc<DuRegistry>,
    cache: Option<DuCache>,
) {
    register_full(core, cfg, reg, cache, None);
}

/// Как [`register_with`], с досчётом с правами администратора (F01-12): `elevation` —
/// запуск `urfin.exe --du-elevated` через UAC и каталог ответов (`None` — команда
/// `du.elevate` недоступна: `--smoke`, `--screens`, тесты).
pub fn register_full(
    core: &mut urfin_core::Core,
    cfg: DuCfg,
    reg: std::sync::Arc<DuRegistry>,
    cache: Option<DuCache>,
    elevation: Option<elevated::Elevation>,
) {
    core.register(Box::new(
        DuHandler::with(cfg, reg, cache).with_elevation(elevation),
    ));
}

/// Enter на строке без файла во вкладке `du:`: группа «Типов» — раскрыть или свернуть
/// (`du.filter kind=types group=…`), «ещё N файлов» и разделы «Временных» — ничего.
/// Регистрирует приложение в `Services::open_resolvers`.
pub fn open_resolver() -> std::sync::Arc<dyn urfin_core::OpenResolver> {
    std::sync::Arc::new(Rows)
}

struct Rows;

impl urfin_core::OpenResolver for Rows {
    fn resolve(
        &self,
        dir: &urfin_core::Location,
        e: &urfin_core::Entry,
        _args: &urfin_core::Args,
    ) -> Option<urfin_core::Command> {
        let f = dvfs::filter_of(dir)?;
        if e.kind != urfin_core::EntryKind::Other {
            return None;
        }
        let cmd = urfin_core::Command::new("du.filter").arg("kind", f.id());
        Some(match (f, filters::TypeGroup::from_label(&e.name)) {
            (DuFilter::Types, Some(g)) => cmd.arg("group", g.id()),
            _ => cmd,
        })
    }
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
