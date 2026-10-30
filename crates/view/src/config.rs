//! Секции `[viewer]` и `[editor]` конфига (S7 §8). Значения по умолчанию сверены с
//! макетами `screen-settings-viewer.png` и `screen-settings-editor.png`.

use compact_str::CompactString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Где открывать по F3 («во весь экран» — №34).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlacementCfg {
    #[default]
    Window,
    Panel,
}

/// Перенос строк во вьювере; F2 — `off` ↔ последний режим.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WrapCfg {
    #[default]
    Off,
    Words,
    Chars,
}

/// Колонок в hex: `"auto"` — по ширине окна, иначе 8, 16 или 32.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HexCols {
    #[default]
    Auto,
    Fixed(u8),
}

impl Serialize for HexCols {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            HexCols::Auto => s.serialize_str("auto"),
            HexCols::Fixed(n) => s.serialize_u8(*n),
        }
    }
}

impl<'de> Deserialize<'de> for HexCols {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u8),
            S(CompactString),
        }
        match Raw::deserialize(d)? {
            Raw::N(n @ (8 | 16 | 32)) => Ok(HexCols::Fixed(n)),
            Raw::S(s) if s == "auto" => Ok(HexCols::Auto),
            _ => Err(serde::de::Error::custom("expected \"auto\", 8, 16 or 32")),
        }
    }
}

/// Внешний просмотрщик по маске (`[[viewer.assoc]]`), шаблон команды с `{path}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assoc {
    pub mask: CompactString,
    pub cmd: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые выключатели настроек «Просмотр»"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerCfg {
    pub placement: PlacementCfg,
    /// Alt+F3; пусто — программа Windows для типа файла; шаблон `{path}`.
    pub external: String,
    /// Позиция, кодировка и режим (№36).
    pub remember_pos: bool,
    pub history_max: u32,
    /// Имя WHATWG или `auto`.
    pub codepage: CompactString,
    /// Кольцо F8.
    pub codepage_ring: Vec<CompactString>,
    pub wrap: WrapCfg,
    pub tab_size: u8,
    pub hex_cols: HexCols,
    /// mmap для файлов от 64 МБ на локальном диске, кроме изменённых за 60 с.
    pub mmap: bool,
    /// Follow: переоткрывать после ротации.
    pub follow_reopen: bool,
    pub follow_interval_ms: u32,
    /// Байт строки до принудительного переноса.
    pub max_line: u32,
    /// Кэш страниц на документ, мегабайты.
    pub cache_mb: u32,
    pub search_wrap: bool,
    /// Предел счётчика совпадений.
    pub count_limit: u32,
    pub copy_limit_mb: u32,
    /// 0 — моноширинный размер окна.
    pub font_size: u16,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assoc: Vec<Assoc>,
}

impl Default for ViewerCfg {
    fn default() -> Self {
        ViewerCfg {
            placement: PlacementCfg::Window,
            external: String::new(),
            remember_pos: true,
            history_max: 500,
            codepage: "auto".into(),
            codepage_ring: ["auto", "utf-8", "windows-1251", "ibm866"]
                .into_iter()
                .map(CompactString::from)
                .collect(),
            wrap: WrapCfg::Off,
            tab_size: 8,
            hex_cols: HexCols::Auto,
            mmap: true,
            follow_reopen: true,
            follow_interval_ms: 250,
            max_line: 65536,
            cache_mb: 8,
            search_wrap: true,
            count_limit: 10_000,
            copy_limit_mb: 64,
            font_size: 0,
            assoc: Vec::new(),
        }
    }
}

/// Что делает F4 (№35); `external` меняет F4 и Alt+Shift+F4 местами.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorDefault {
    #[default]
    Internal,
    External,
}

/// Конец строки при сохранении: как в файле (новый файл — CRLF) или принудительно.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EolCfg {
    Lf,
    Crlf,
    #[default]
    File,
}

/// Автосохранение: выкл, при потере фокуса, по паузе.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Autosave {
    #[default]
    Off,
    Focus,
    Delay,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые выключатели настроек «Редактор»"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorCfg {
    pub default: EditorDefault,
    /// Пусто → `$VISUAL` / `$EDITOR` → программа Windows; шаблон `{path}`, `{line}`.
    pub external: String,
    /// Консольный ли внешний редактор: `auto` — по имени программы (S7 §5.6).
    pub external_console: CompactString,
    /// Предварительно, итог — спайк S7-16.
    pub max_size_mb: u32,
    /// Пусто — моноширинный шрифт окна.
    pub font: CompactString,
    /// 0 — моноширинный размер окна.
    pub font_size: u16,
    pub tab_size: u8,
    /// Пробелы вместо табов.
    pub expand_tabs: bool,
    pub wrap: bool,
    /// Номера строк, свёртка, мини-карта.
    pub gutter: bool,
    pub new_codepage: CompactString,
    pub new_bom: bool,
    pub eol: EolCfg,
    /// Подсветка синтаксиса; файлы > 16 МБ — без неё.
    pub highlight: bool,
    pub autosave: Autosave,
}

impl Default for EditorCfg {
    fn default() -> Self {
        EditorCfg {
            default: EditorDefault::Internal,
            external: String::new(),
            external_console: "auto".into(),
            max_size_mb: 64,
            font: CompactString::default(),
            font_size: 0,
            tab_size: 4,
            expand_tabs: true,
            wrap: false,
            gutter: true,
            new_codepage: "utf-8".into(),
            new_bom: false,
            eol: EolCfg::File,
            highlight: true,
            autosave: Autosave::Off,
        }
    }
}
