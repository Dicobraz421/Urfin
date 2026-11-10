//! Секция `[arc]` (S8 §3.1–§3.6).
//!
//! Разбор — вручную из `Config.ext`: `serde` и `toml` крейту не подключены (полоса deps,
//! см. отчёт S8-24). Неверный тип ключа — значение по умолчанию.

use std::fmt::Write as _;

use urfin_core::Config;

use crate::detect;

/// Настройки архивов.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArcCfg {
    /// Расширения, по которым Enter входит в архив (без точки). Документы-архивы (`docx`,
    /// `jar`, …) сюда не входят — они открываются в приложении ОС (№39); войти в них —
    /// Ctrl+Shift+PgDn (`arc.enter`).
    pub enter_exts: Vec<String>,
    /// Лимит кэша индексов в мебибайтах.
    pub cache_mib: u32,
    /// Кодовая страница имён zip без флага UTF-8: `auto` — OEM из реестра (S8-25).
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
    /// `[arc]` из конфига; нет секции — по умолчанию.
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "тип значения из `toml`, а `toml` крейту не подключён"
    )]
    pub fn from_config(c: &Config) -> ArcCfg {
        let mut cfg = ArcCfg::default();
        let Some(t) = c.ext.get("arc").and_then(|v| v.as_table()) else {
            return cfg;
        };
        if let Some(list) = t.get("enter_exts").and_then(|v| v.as_array()) {
            cfg.enter_exts = list
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim_start_matches('.').to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
        let int = |k: &str| t.get(k).and_then(|v| v.as_integer());
        if let Some(v) = int("cache_mib").and_then(|v| u32::try_from(v).ok()) {
            cfg.cache_mib = v.max(1);
        }
        if let Some(v) = int("spill_max").and_then(|v| u64::try_from(v).ok()) {
            cfg.spill_max = v;
        }
        if let Some(v) = int("ratio_max").and_then(|v| u32::try_from(v).ok()) {
            cfg.ratio_max = v.max(1);
        }
        if let Some(v) = t.get("zip_codepage").and_then(|v| v.as_str()) {
            v.clone_into(&mut cfg.zip_codepage);
        }
        if let Some(v) = t.get("propagate_motw").and_then(|v| v.as_bool()) {
            cfg.propagate_motw = v;
        }
        cfg
    }

    /// Лимит кэша, байты.
    pub fn cache_bytes(&self) -> u64 {
        u64::from(self.cache_mib) << 20
    }

    /// Секция TOML — для снимка дефолтов и шаблона конфига.
    pub fn to_toml(&self) -> String {
        let exts: Vec<String> = self.enter_exts.iter().map(|e| format!("{e:?}")).collect();
        let mut s = String::new();
        let _ = writeln!(s, "enter_exts = [{}]", exts.join(", "));
        let _ = writeln!(s, "cache_mib = {}", self.cache_mib);
        let _ = writeln!(s, "zip_codepage = {:?}", self.zip_codepage);
        let _ = writeln!(s, "spill_max = {}", self.spill_max);
        let _ = writeln!(s, "ratio_max = {}", self.ratio_max);
        let _ = writeln!(s, "propagate_motw = {}", self.propagate_motw);
        s
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    /// Таблица того же типа, что `Config.ext` (тип `toml` крейту не назвать).
    fn parse_like<T: FromStr>(_: &T, s: &str) -> T
    where
        T::Err: std::fmt::Debug,
    {
        s.parse().unwrap()
    }

    fn config(text: &str) -> Config {
        let mut c = Config::default();
        c.ext = parse_like(&c.ext, text);
        c
    }

    #[test]
    fn defaults_snapshot() {
        assert_eq!(
            ArcCfg::default().to_toml(),
            include_str!("snapshots/arc_defaults.toml").replace("\r\n", "\n")
        );
    }

    #[test]
    fn defaults_roundtrip_through_config() {
        let text = format!("[arc]\n{}", ArcCfg::default().to_toml());
        assert_eq!(ArcCfg::from_config(&config(&text)), ArcCfg::default());
        assert_eq!(ArcCfg::from_config(&Config::default()), ArcCfg::default());
    }

    #[test]
    fn section_overrides_only_given_keys() {
        let c = config(
            "[arc]\nenter_exts = [\".ZIP\", \"7z\", 5]\ncache_mib = 64\nratio_max = \"x\"\n",
        );
        let cfg = ArcCfg::from_config(&c);
        assert_eq!(cfg.enter_exts, ["zip", "7z"]);
        assert_eq!(cfg.cache_bytes(), 64 << 20);
        assert_eq!(cfg.ratio_max, 1000, "неверный тип — по умолчанию");
        assert!(cfg.propagate_motw);
        assert!(!cfg.enter_exts.iter().any(|e| e == "docx"));
    }
}
