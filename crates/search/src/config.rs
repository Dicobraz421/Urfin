//! Секция `[search]` (S8 §2.1, вопрос В3 = А): в форме только поля макета, остальные
//! фильтры — здесь и в аргументах `search.run`.

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// Настройки поиска.
#[allow(clippy::struct_excessive_bools, reason = "ключи `[search]`")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchCfg {
    /// Потоков обхода; 0 — авто (`volume::walk_threads`).
    pub threads: u8,
    /// Попаданий всего: дальше поиск останавливается с баннером.
    pub max_hits: u64,
    /// Строк с совпадением на файл.
    pub max_per_file: u32,
    /// Имена каталогов, в которые не входим (без учёта регистра).
    pub skip: Vec<CompactString>,
    /// Искать в скрытых файлах и каталогах.
    pub hidden: bool,
    /// Облачные файлы (`Attrs::OFFLINE`) не открывать ради содержимого.
    pub skip_offline: bool,
    /// Искать текст и в бинарных файлах.
    pub binary: bool,
    /// Текст — слово целиком.
    pub whole_word: bool,
    /// `all`, `auto` или имя кодировки; пусто — `all` для литерала, `auto` для regex.
    pub encoding: CompactString,
    /// Размер файла, байт, включительно.
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    /// Дата изменения `ГГГГ-ММ-ДД` (UTC), включительно.
    pub date_from: Option<CompactString>,
    pub date_to: Option<CompactString>,
}

impl Default for SearchCfg {
    fn default() -> Self {
        SearchCfg {
            threads: 0,
            max_hits: 1_000_000,
            max_per_file: 1000,
            skip: vec!["System Volume Information".into(), "$Recycle.Bin".into()],
            hidden: true,
            skip_offline: true,
            binary: false,
            whole_word: false,
            encoding: CompactString::default(),
            size_min: None,
            size_max: None,
            date_from: None,
            date_to: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SearchCfg;

    #[test]
    fn defaults_snapshot() {
        insta::assert_toml_snapshot!(SearchCfg::default());
    }

    #[test]
    fn section_overrides_only_given_keys() {
        let cfg: SearchCfg = toml::from_str("max_hits = 5\nsize_min = 10").unwrap();
        assert_eq!(cfg.max_hits, 5);
        assert_eq!(cfg.size_min, Some(10));
        assert_eq!(cfg.max_per_file, SearchCfg::default().max_per_file);
    }
}
