//! Запрос поиска (S8 §2.1): типы, serde, разбор плоских аргументов `search.run`.
//!
//! Разбор не ходит в ОС: он идёт в UI-потоке. Regex собирается уже в job ([`crate::content`]).

use std::fmt;
use std::path::Path;

use compact_str::CompactString;
use encoding_rs::Encoding;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use urfin_core::{Args, Location, Mask, Value};

use crate::SearchCfg;

/// Что искать: файлы, каталоги или всё.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KindFilter {
    #[default]
    Files,
    Dirs,
    Both,
}

/// Ускоритель Everything (M5b).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accel {
    #[default]
    Auto,
    Off,
}

/// Как понимать текст запроса.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextMode {
    #[default]
    Literal,
    Regex,
    Hex,
}

/// Кодировка содержимого.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum EncodingMode {
    /// Литерал сразу в UTF-8, UTF-16LE, cp1251, cp866.
    #[default]
    All,
    /// UTF-8; UTF-16 по BOM.
    Auto,
    Named(&'static Encoding),
}

impl EncodingMode {
    /// Разбор имени; `None` — неизвестная кодировка.
    pub fn parse(label: &str) -> Option<EncodingMode> {
        match label {
            "all" => Some(EncodingMode::All),
            "auto" => Some(EncodingMode::Auto),
            _ => Encoding::for_label(label.as_bytes()).map(EncodingMode::Named),
        }
    }

    fn label(self) -> &'static str {
        match self {
            EncodingMode::All => "all",
            EncodingMode::Auto => "auto",
            EncodingMode::Named(e) => e.name(),
        }
    }
}

impl Serialize for EncodingMode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

impl<'de> Deserialize<'de> for EncodingMode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let label = CompactString::deserialize(d)?;
        EncodingMode::parse(&label)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown encoding {label}")))
    }
}

/// Поле «Текст» и его настройки.
#[allow(clippy::struct_excessive_bools, reason = "форма запроса из S8 §2.1")]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextQuery {
    pub pattern: String,
    pub mode: TextMode,
    /// С учётом регистра.
    pub case: bool,
    pub whole_word: bool,
    pub encoding: EncodingMode,
    /// `false` — бинарные файлы пропускаются.
    pub binary: bool,
    /// Файлы, где текста нет.
    pub invert: bool,
}

/// Запрос. UI передаёт поля плоскими аргументами `search.run`.
#[allow(clippy::struct_excessive_bools, reason = "форма запроса из S8 §2.1")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchQuery {
    /// Поле «В».
    pub roots: Vec<Location>,
    /// Поле «Имя»; пусто — всё.
    #[serde(with = "opt_mask", skip_serializing_if = "Option::is_none")]
    pub name: Option<Mask>,
    /// Поле «Текст».
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextQuery>,
    /// Глубина обхода: 0 — только корень («Подкаталоги» выкл.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u16>,
    pub kinds: KindFilter,
    /// Размер, включительно; только у файлов.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<(u64, u64)>,
    /// Время изменения, нс от Unix epoch, включительно.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<(i64, i64)>,
    /// Показывать скрытые и входить в скрытые каталоги.
    pub hidden: bool,
    /// Облачные файлы не открывать ради содержимого: `OneDrive` начнёт скачивание.
    pub skip_offline: bool,
    /// M5b.
    pub in_archives: bool,
    /// M5b.
    pub accel: Accel,
}

impl Default for SearchQuery {
    fn default() -> Self {
        SearchQuery {
            roots: Vec::new(),
            name: None,
            text: None,
            depth: None,
            kinds: KindFilter::default(),
            size: None,
            mtime: None,
            hidden: true,
            skip_offline: true,
            in_archives: false,
            accel: Accel::default(),
        }
    }
}

mod opt_mask {
    use compact_str::CompactString;
    use serde::{Deserialize, Deserializer, Serializer};
    use urfin_core::Mask;

    #[allow(clippy::ref_option, reason = "сигнатура serde with")]
    pub fn serialize<S: Serializer>(m: &Option<Mask>, s: S) -> Result<S::Ok, S::Error> {
        match m {
            Some(m) => s.serialize_some(m.as_str()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Mask>, D::Error> {
        Option::<CompactString>::deserialize(d)?
            .map(|s| Mask::parse(&s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

/// Ошибка запроса: показывается пользователю строкой `ui-search-*`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    #[error("bad mask: {0}")]
    BadMask(String),
    #[error("bad path: {0}")]
    BadWhere(String),
    #[error("bad regex: {0}")]
    BadRegex(String),
}

/// Умолчание кодировки: `All` только у литерала (S8 §2.3).
fn encoding_of(arg: Option<&str>, cfg: &str, mode: TextMode) -> Result<EncodingMode, QueryError> {
    let label = arg.unwrap_or(cfg);
    let parsed = if label.is_empty() {
        EncodingMode::All
    } else {
        EncodingMode::parse(label)
            .ok_or_else(|| QueryError::BadRegex(format!("unknown encoding: {label}")))?
    };
    Ok(match (parsed, mode) {
        (EncodingMode::All, TextMode::Regex | TextMode::Hex) => EncodingMode::Auto,
        (e, _) => e,
    })
}

/// Хекс-строка в байты: пары цифр, пробелы между ними можно.
pub(crate) fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    let digits: Vec<u32> = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_digit(16))
        .collect::<Option<_>>()?;
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    Some(
        digits
            .chunks(2)
            .map(|d| u8::try_from(d[0] * 16 + d[1]).unwrap_or(0))
            .collect(),
    )
}

/// Части поля «В»: разделитель `;`, в кавычках — часть пути.
fn split_places(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out.retain_mut(|p| {
        *p = p.trim().to_owned();
        !p.is_empty()
    });
    out
}

/// Адрес части поля «В»: uri схемы, абсолютный путь или относительно панели.
fn place(s: &str, base: &Location) -> Result<Location, QueryError> {
    let bad = || QueryError::BadWhere(s.to_owned());
    if s.contains("://") {
        return Location::parse_uri(s).map_err(|_| bad());
    }
    if Path::new(s).is_absolute() {
        return Ok(Location::local(s));
    }
    if base.is_local() {
        return Ok(Location::local(base.path.join(s)));
    }
    Err(bad())
}

/// Корни без повторов; при полной глубине вложенный корень покрыт внешним.
fn unique_roots(roots: &[Location], whole_tree: bool) -> Vec<Location> {
    let mut by_len: Vec<&Location> = roots.iter().collect();
    by_len.sort_by_key(|r| r.path.as_os_str().len());
    let mut keep: Vec<&Location> = Vec::with_capacity(roots.len());
    for r in by_len {
        let covered = keep.iter().any(|o| {
            *o == r
                || (whole_tree
                    && o.backend == r.backend
                    && o.authority == r.authority
                    && o.outer == r.outer
                    && r.path.starts_with(&o.path))
        });
        if !covered {
            keep.push(r);
        }
    }
    // Порядок поля «В» сохраняется.
    let mut out: Vec<Location> = Vec::with_capacity(keep.len());
    for r in roots {
        if keep.iter().any(|k| std::ptr::eq(*k, r)) {
            out.push(r.clone());
        }
    }
    out
}

/// Все корни аргумента `where`: список строк (адресов), в строке можно несколько через `;`.
fn roots_of(a: &Args, base: &Location) -> Result<Vec<Location>, QueryError> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = a.str("where") {
        parts.extend(split_places(s));
    } else if let Some(list) = a.list("where") {
        for v in list {
            match v {
                Value::Str(s) => parts.extend(split_places(s)),
                Value::Loc(l) => parts.push(l.to_uri()),
                _ => {}
            }
        }
    }
    if parts.is_empty() {
        return Ok(vec![base.clone()]);
    }
    parts.iter().map(|p| place(p, base)).collect()
}

/// `ГГГГ-ММ-ДД` в нс от Unix epoch: полночь UTC, с `end` — последняя наносекунда суток.
pub(crate) fn date_ns(s: &str, end: bool) -> Option<i64> {
    const DAY: i64 = 86_400_000_000_000;
    let mut it = s.trim().splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Дни от эпохи по григорианскому календарю (алгоритм days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let start = (era * 146_097 + doe - 719_468).checked_mul(DAY)?;
    Some(if end { start + DAY - 1 } else { start })
}

/// Границы размера и времени: из аргументов, иначе из `[search]`.
type Ranges = (Option<(u64, u64)>, Option<(i64, i64)>);

fn ranges_of(a: &Args, cfg: &SearchCfg) -> Result<Ranges, QueryError> {
    let int = |k: &str| a.int(k).and_then(|v| u64::try_from(v).ok());
    let min = int("size_min").or(cfg.size_min);
    let max = int("size_max").or(cfg.size_max);
    let size = (min.is_some() || max.is_some())
        .then(|| (min.unwrap_or(0), max.unwrap_or(i64::MAX.unsigned_abs())));
    let date = |k: &str, c: Option<&str>, end: bool| {
        a.str(k)
            .or(c)
            .map(|s| date_ns(s, end).ok_or_else(|| QueryError::BadWhere(s.to_owned())))
            .transpose()
    };
    let from = date("date_from", cfg.date_from.as_deref(), false)?;
    let to = date("date_to", cfg.date_to.as_deref(), true)?;
    let mtime = (from.is_some() || to.is_some())
        .then(|| (from.unwrap_or(i64::MIN), to.unwrap_or(i64::MAX)));
    Ok((size, mtime))
}

fn text_of(a: &Args, cfg: &SearchCfg) -> Result<Option<TextQuery>, QueryError> {
    let pattern = a.str("text").unwrap_or_default();
    if pattern.is_empty() {
        return Ok(None);
    }
    let on = |k: &str| a.bool(k).unwrap_or(false);
    let mode = if on("hex") {
        if hex_bytes(pattern).is_none() {
            return Err(QueryError::BadRegex(pattern.to_owned()));
        }
        TextMode::Hex
    } else if on("regex") {
        TextMode::Regex
    } else {
        TextMode::Literal
    };
    Ok(Some(TextQuery {
        pattern: pattern.to_owned(),
        mode,
        case: on("case"),
        whole_word: a.bool("word").unwrap_or(cfg.whole_word),
        encoding: encoding_of(a.str("encoding"), &cfg.encoding, mode)?,
        // Хекс — поиск байтов: как раз в бинарных файлах.
        binary: mode == TextMode::Hex || a.bool("binary").unwrap_or(cfg.binary),
        invert: on("invert"),
    }))
}

impl SearchQuery {
    /// Разбор плоских аргументов `search.run`; что не задано — из `[search]`, корень по
    /// умолчанию — `base` (папка панели).
    pub fn from_args(a: &Args, cfg: &SearchCfg, base: &Location) -> Result<Self, QueryError> {
        let name = a.str("name").map(str::trim).unwrap_or_default();
        let name = if name.is_empty() {
            None
        } else {
            Some(Mask::parse(name).map_err(|_| QueryError::BadMask(name.to_owned()))?)
        };
        let depth = match a.int("depth") {
            Some(d) => Some(u16::try_from(d.max(0)).unwrap_or(u16::MAX)),
            None => (!a.bool("recursive").unwrap_or(true)).then_some(0),
        };
        let kinds = match a.str("kinds") {
            None | Some("files") => KindFilter::Files,
            Some("dirs") => KindFilter::Dirs,
            Some("both") => KindFilter::Both,
            Some(other) => return Err(QueryError::BadMask(other.to_owned())),
        };
        let (size, mtime) = ranges_of(a, cfg)?;
        Ok(SearchQuery {
            roots: unique_roots(&roots_of(a, base)?, depth.is_none()),
            name,
            text: text_of(a, cfg)?,
            depth,
            kinds,
            size,
            mtime,
            hidden: a.bool("hidden").unwrap_or(cfg.hidden),
            skip_offline: a.bool("skip_offline").unwrap_or(cfg.skip_offline),
            in_archives: false,
            accel: Accel::default(),
        })
    }
}

impl fmt::Display for SearchQuery {
    /// Заголовок job: текст запроса, иначе маска.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.text, &self.name) {
            (Some(t), _) => f.write_str(&t.pattern),
            (None, Some(m)) => f.write_str(m.as_str()),
            (None, None) => f.write_str("*"),
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use urfin_core::{Args, Value};

    use super::*;

    fn base() -> Location {
        Location::local(if cfg!(windows) { r"C:\base" } else { "/base" })
    }

    fn parse(a: &Args) -> Result<SearchQuery, QueryError> {
        SearchQuery::from_args(a, &SearchCfg::default(), &base())
    }

    #[test]
    fn empty_args_search_the_panel_folder() {
        let q = parse(&Args::default()).unwrap();
        assert_eq!(q.roots, [base()]);
        assert!(q.name.is_none() && q.text.is_none() && q.depth.is_none());
        assert_eq!(q.kinds, KindFilter::Files);
    }

    #[test]
    fn mask_of_the_spec_parses() {
        let q = parse(&Args::default().with("name", r"*.rs,*.toml|target\*")).unwrap();
        let m = q.name.unwrap();
        assert!(m.matches("scan.rs") && m.matches("Cargo.TOML"));
        assert!(!m.matches("a.txt") && !m.matches(r"target\a.rs"));
    }

    #[test]
    fn bad_mask_and_place_are_errors() {
        let e = parse(&Args::default().with("name", "a|b|c")).unwrap_err();
        assert_eq!(e, QueryError::BadMask("a|b|c".into()));
        let e = parse(&Args::default().with("where", "bad://%zz")).unwrap_err();
        assert!(matches!(e, QueryError::BadWhere(_)));
    }

    #[test]
    fn where_takes_a_list_and_semicolons() {
        let list = vec![Value::from(r"C:\a;C:\b"), Value::from(r#""C:\c;d""#)];
        let q = parse(&Args::default().with("where", list)).unwrap();
        assert_eq!(
            q.roots,
            [
                Location::local(r"C:\a"),
                Location::local(r"C:\b"),
                Location::local(r"C:\c;d")
            ]
        );
    }

    #[test]
    fn nested_root_is_covered_unless_depth_limited() {
        let two = r"C:\a\b;C:\a";
        let q = parse(&Args::default().with("where", two)).unwrap();
        assert_eq!(q.roots, [Location::local(r"C:\a")]);
        let a = Args::default().with("where", two).with("recursive", false);
        assert_eq!(parse(&a).unwrap().roots.len(), 2);
    }

    #[test]
    fn encoding_defaults_by_mode() {
        let lit = parse(&Args::default().with("text", "x")).unwrap().text;
        assert_eq!(lit.unwrap().encoding, EncodingMode::All);
        let a = Args::default().with("text", "x").with("regex", true);
        assert_eq!(
            parse(&a).unwrap().text.unwrap().encoding,
            EncodingMode::Auto
        );
        let a = Args::default().with("text", "x").with("encoding", "cp866");
        assert_eq!(
            parse(&a).unwrap().text.unwrap().encoding,
            EncodingMode::Named(encoding_rs::IBM866)
        );
    }

    #[test]
    fn config_gives_the_filters_args_override() {
        let cfg = SearchCfg {
            size_min: Some(10),
            hidden: false,
            whole_word: true,
            date_from: Some("1970-01-02".into()),
            ..SearchCfg::default()
        };
        let a = Args::default().with("text", "x").with("size_max", 20);
        let q = SearchQuery::from_args(&a, &cfg, &base()).unwrap();
        assert_eq!(q.size, Some((10, 20)));
        assert!(!q.hidden);
        assert!(q.text.unwrap().whole_word);
        assert_eq!(q.mtime, Some((86_400_000_000_000, i64::MAX)));
        let a = Args::default().with("hidden", true).with("size_min", 1);
        let q = SearchQuery::from_args(&a, &cfg, &base()).unwrap();
        assert!(q.hidden);
        assert_eq!(q.size.map(|s| s.0), Some(1));
    }

    #[test]
    fn dates_and_hex() {
        assert_eq!(date_ns("1970-01-01", false), Some(0));
        assert_eq!(date_ns("2000-03-01", false), Some(951_868_800_000_000_000));
        assert_eq!(date_ns("1970-01-01", true), Some(86_399_999_999_999));
        assert_eq!(date_ns("завтра", false), None);
        assert_eq!(hex_bytes("AB cd"), Some(vec![0xAB, 0xCD]));
        assert_eq!(hex_bytes("ABC"), None);
        assert_eq!(hex_bytes("zz"), None);
    }

    fn mask() -> impl Strategy<Value = Option<&'static str>> {
        prop::sample::select(vec![None, Some("*.rs"), Some(r"*.rs,*.toml|target\*")])
    }

    fn encoding() -> impl Strategy<Value = EncodingMode> {
        prop::sample::select(vec![
            EncodingMode::All,
            EncodingMode::Auto,
            EncodingMode::Named(encoding_rs::WINDOWS_1251),
        ])
    }

    fn text() -> impl Strategy<Value = TextQuery> {
        (
            ".{0,12}",
            prop::sample::select(vec![TextMode::Literal, TextMode::Regex, TextMode::Hex]),
            any::<(bool, bool, bool, bool)>(),
            encoding(),
        )
            .prop_map(|(pattern, mode, f, encoding)| TextQuery {
                pattern,
                mode,
                case: f.0,
                whole_word: f.1,
                encoding,
                binary: f.2,
                invert: f.3,
            })
    }

    proptest! {
        #[test]
        fn serde_roundtrip(
            name in mask(),
            text in prop::option::of(text()),
            depth in prop::option::of(any::<u16>()),
            kinds in prop::sample::select(vec![KindFilter::Files, KindFilter::Dirs, KindFilter::Both]),
            size in prop::option::of((0..i64::MAX.unsigned_abs(), 0..i64::MAX.unsigned_abs())),
            mtime in prop::option::of(any::<(i64, i64)>()),
            flags in any::<(bool, bool)>(),
        ) {
            let q = SearchQuery {
                roots: vec![base()],
                name: name.map(|m| Mask::parse(m).unwrap()),
                text,
                depth,
                kinds,
                size,
                mtime,
                hidden: flags.0,
                skip_offline: flags.1,
                ..SearchQuery::default()
            };
            let s = toml::to_string(&q).unwrap();
            prop_assert_eq!(toml::from_str::<SearchQuery>(&s).unwrap(), q);
        }
    }
}
