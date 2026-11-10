//! Секция `[arc]` (S8 §3.1–§3.6): `Config::section`, как у других крейтов. Ошибка в
//! секции — вся секция по умолчанию и предупреждение в лог.

use serde::{Deserialize, Serialize};
use urfin_core::Config;

use crate::detect;

/// Настройки архивов.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArcCfg {
    /// Расширения, по которым Enter входит в архив (без точки). Документы-архивы (`docx`,
    /// `jar`, …) сюда не входят — они открываются в приложении ОС (№39); войти в них —
    /// Ctrl+Shift+PgDn (`arc.enter`).
    pub enter_exts: Vec<String>,
    /// Лимит кэша индексов в мебибайтах.
    pub cache_mib: u32,
    /// Кодовая страница имён zip без флага UTF-8: `auto` — OEM из реестра, иначе номер
    /// (`866`, `437`, `1251`).
    pub zip_codepage: String,
    /// Больше — вопрос перед спиллом во временный каталог, байты (S8-31).
    pub spill_max: u64,
    /// Предел степени сжатия записи против zip-бомб, `N:1` (S8-31).
    pub ratio_max: u32,
    /// Переносить Mark-of-the-Web архива на извлечённые файлы (S8-28).
    pub propagate_motw: bool,
}

impl Default for ArcCfg {
    fn default() -> Self {
        ArcCfg {
            enter_exts: detect::archive_exts().map(str::to_owned).collect(),
            cache_mib: 256,
            zip_codepage: "auto".into(),
            spill_max: 4 << 30,
            ratio_max: 1000,
            propagate_motw: true,
        }
    }
}

impl ArcCfg {
    /// `[arc]` из конфига; нет секции — по умолчанию. Расширения — без точки и в нижнем
    /// регистре, лимиты — не меньше 1.
    pub fn from_config(c: &Config) -> ArcCfg {
        let (mut cfg, warning) = c.section::<ArcCfg>("arc");
        if let Some(w) = warning {
            tracing::warn!(warning = ?w, "[arc] config section ignored");
        }
        cfg.enter_exts = cfg
            .enter_exts
            .iter()
            .map(|s| s.trim_start_matches('.').to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        cfg.cache_mib = cfg.cache_mib.max(1);
        cfg.ratio_max = cfg.ratio_max.max(1);
        cfg
    }

    /// Лимит кэша, байты.
    pub fn cache_bytes(&self) -> u64 {
        u64::from(self.cache_mib) << 20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config {
            ext: text.parse().unwrap(),
            ..Config::default()
        }
    }

    #[test]
    fn defaults_snapshot() {
        insta::assert_snapshot!(toml::to_string(&ArcCfg::default()).unwrap());
    }

    #[test]
    fn defaults_roundtrip_through_config() {
        let text = format!("[arc]\n{}", toml::to_string(&ArcCfg::default()).unwrap());
        assert_eq!(ArcCfg::from_config(&config(&text)), ArcCfg::default());
        assert_eq!(ArcCfg::from_config(&Config::default()), ArcCfg::default());
    }

    #[test]
    fn section_overrides_only_given_keys() {
        let c = config("[arc]\nenter_exts = [\".ZIP\", \"7z\", \"\"]\ncache_mib = 0\n");
        let cfg = ArcCfg::from_config(&c);
        assert_eq!(cfg.enter_exts, ["zip", "7z"]);
        assert_eq!(cfg.cache_bytes(), 1 << 20, "лимит не меньше 1 МиБ");
        assert_eq!(cfg.ratio_max, 1000);
        assert!(cfg.propagate_motw);
    }

    #[test]
    fn wrong_type_falls_back_to_defaults() {
        let cfg = ArcCfg::from_config(&config("[arc]\nratio_max = \"x\"\ncache_mib = 64\n"));
        assert_eq!(
            cfg,
            ArcCfg::default(),
            "ошибка в секции — вся секция по умолчанию"
        );
    }
}
