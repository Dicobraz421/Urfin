//! Секция `[ui]` конфига (S2 §4). Читается через `Config::section::<UiCfg>("ui")`;
//! `theme`, `zoom`, `split`, `columns` пишут команды `ui.*` (S2-22, `urfin_core::settings`);
//! неверная секция целиком — дефолт и предупреждение core, неверные значения токенов
//! и раскраски — предупреждения [`crate::tokens::Tokens::build`] и
//! [`crate::highlight::Highlighter::build`].

use std::collections::BTreeMap;

use serde::Deserialize;

/// Высота строки списка (`ui.density`, №59): 20 / 24 / 28 px. В TOML — число;
/// старые `compact`/`standard` (S2) читаются как 24/28.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Density(u8);

impl Density {
    pub const VALUES: [u8; 3] = [20, 24, 28];

    /// Ближайшая допустимая высота.
    pub fn new(px: i64) -> Density {
        let px = Self::VALUES
            .into_iter()
            .min_by_key(|v| (i64::from(*v) - px).abs())
            .unwrap_or(24);
        Density(px)
    }

    pub fn px(self) -> u8 {
        self.0
    }
}

impl Default for Density {
    fn default() -> Self {
        Density(24)
    }
}

impl<'de> Deserialize<'de> for Density {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Px(i64),
            Name(String),
        }
        match Raw::deserialize(d)? {
            Raw::Px(px) => Ok(Density::new(px)),
            Raw::Name(n) => match n.to_ascii_lowercase().as_str() {
                "compact" => Ok(Density(24)),
                "standard" => Ok(Density(28)),
                other => other
                    .parse::<i64>()
                    .map(Density::new)
                    .map_err(|_| serde::de::Error::custom(format!("density: {other:?}"))),
            },
        }
    }
}

/// Иконки файлов (`ui.icons`, №59): `mono` — Segoe Fluent моно (умолч.), `color` —
/// цветные по ролям типов, `none` — без иконок. `fluent` (S2) — то же, что `mono`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum IconMode {
    #[default]
    Mono,
    Color,
    None,
}

impl IconMode {
    pub const ALL: [IconMode; 3] = [IconMode::Mono, IconMode::Color, IconMode::None];

    pub fn parse(s: &str) -> Option<IconMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mono" | "fluent" => Some(IconMode::Mono),
            "color" => Some(IconMode::Color),
            "none" => Some(IconMode::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            IconMode::Mono => "mono",
            IconMode::Color => "color",
            IconMode::None => "none",
        }
    }
}

impl<'de> Deserialize<'de> for IconMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        IconMode::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("icons: {s:?}")))
    }
}

/// Переопределения цветов `[ui.tokens.light]` / `[ui.tokens.dark]`: роль → `#RRGGBB`.
#[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct TokenOverrides {
    pub light: BTreeMap<String, String>,
    pub dark: BTreeMap<String, String>,
}

/// Правило раскраски `[[ui.colors]]` (S0-decisions №32; в S2 §4 — `[[ui.highlight]]`).
#[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct ColorRule {
    /// Маска имени (`*.rs`, `*.rs;*.toml`).
    pub mask: Option<String>,
    /// Все атрибуты должны быть: `hidden`, `system`, `readonly`, `exec`, `offline`, …
    pub attrs: Vec<String>,
    /// `dir`, `file`, `link`, `broken`.
    pub kind: Option<String>,
    /// `#RRGGBB` или роль `@accent.text`.
    pub fg: String,
}

/// Набор колонок `[ui.view_modes.<name>]` (S2 §2.3, S2-19): `ViewMode::Custom(n)`,
/// `n` — номер набора по порядку имён. Имя — всегда первая колонка; неизвестные
/// колонки пропускаются с предупреждением ([`crate::columns::parse_sets`]).
#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ViewModeCfg {
    /// Подпись в переключателе; нет — имя набора.
    pub title: Option<String>,
    /// Колонки слева направо: `name`, `ext`, `type`, `size`, `mtime`, `ctime`, `atime`,
    /// `attrs`.
    pub columns: Vec<String>,
    /// Ширины колонок, px: `{ type = 140 }`; нет — по виду колонки.
    pub widths: BTreeMap<String, f32>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct UiCfg {
    /// Тема (№59): `auto` — как в ОС, или id (`system-light`, `sepia`, своя из
    /// `themes/*.toml`); `system`/`light`/`dark` (S2) — синонимы auto/system-*.
    pub theme: String,
    /// `system | "#RRGGBB"`.
    pub accent: String,
    /// `auto | dx12 | warp | vulkan` (S0-decisions №50; Windows). Действует при
    /// следующем запуске окна.
    pub renderer: String,
    /// `system | путь`: Segoe UI Variable → Segoe UI.
    pub font_ui: String,
    pub font_ui_size: f32,
    /// `system | путь`: на Windows — Cascadia Mono → Consolas (S0-decisions 2026-10-17).
    pub font_mono: String,
    pub font_mono_size: f32,
    pub fallback_fonts: Vec<String>,
    pub icons: IconMode,
    pub density: Density,
    /// `auto | iso` (S2-08).
    pub date_format: String,
    pub keybar: bool,
    pub animations: bool,
    /// Масштаб, % (`ui.zoom`; 50–300).
    pub zoom: u16,
    /// Доля левой панели (`ui.split`; 0,1–0,9).
    pub split: f32,
    /// Последний выбранный набор колонок (`ui.columns`).
    pub columns: Option<String>,
    /// Наборы колонок по именам (S2-19).
    pub view_modes: BTreeMap<String, ViewModeCfg>,
    pub tokens: TokenOverrides,
    #[serde(alias = "highlight")]
    pub colors: Vec<ColorRule>,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            accent: "system".into(),
            renderer: "auto".into(),
            font_ui: "system".into(),
            font_ui_size: 13.0,
            font_mono: "system".into(),
            font_mono_size: 13.0,
            fallback_fonts: Vec::new(),
            icons: IconMode::Mono,
            density: Density::default(),
            date_format: "auto".into(),
            keybar: true,
            animations: false,
            zoom: 100,
            split: 0.5,
            columns: None,
            view_modes: BTreeMap::new(),
            tokens: TokenOverrides::default(),
            colors: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_example() {
        let text = r##"
theme = "dark"
accent = "#E81123"
density = "standard"
animations = true
[tokens.light]
"accent.fill" = "#0067C0"
[[highlight]]
mask = "*.rs"
fg = "@accent.text"
"##;
        let cfg: UiCfg = toml::from_str(text).unwrap();
        assert_eq!(cfg.theme, "dark");
        assert_eq!(cfg.density.px(), 28, "standard (S2) → 28");
        let d = |t: &str| toml::from_str::<UiCfg>(t).unwrap().density.px();
        assert_eq!((d("density = 20"), d("density = 27"), d("")), (20, 28, 24));
        let i = |t: &str| toml::from_str::<UiCfg>(t).unwrap().icons;
        assert_eq!(i("icons = \"fluent\""), IconMode::Mono);
        assert_eq!(i("icons = \"color\""), IconMode::Color);
        assert!(toml::from_str::<UiCfg>("icons = \"neon\"").is_err());
        assert_eq!(cfg.tokens.light["accent.fill"], "#0067C0");
        assert_eq!(cfg.colors.len(), 1, "`highlight` — синоним `colors`");
        assert_eq!(cfg.font_ui_size, 13.0);
    }
}
