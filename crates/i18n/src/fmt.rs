//! Числа, размеры и даты по языку интерфейса (S2 §7).
//!
//! Разделители дробной части и разрядов берутся из [`NumFmt`] по языку; единицы и шаблон
//! даты — из ключей `fmt-*` в `.ftl`, поэтому язык из `config_dir` форматирует сам.

use fluent_bundle::FluentValue;
use fluent_bundle::memoizer::MemoizerKind;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use unic_langid::LanguageIdentifier;

use crate::{I18n, current};

/// Ключи единиц `fmt::bytes`: Б, КБ … ЭБ (основание 1024, как в Explorer).
const UNITS: [&str; 7] = [
    "fmt-unit-b",
    "fmt-unit-kb",
    "fmt-unit-mb",
    "fmt-unit-gb",
    "fmt-unit-tb",
    "fmt-unit-pb",
    "fmt-unit-eb",
];

/// Разделители чисел языка (CLDR, упрощённо).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NumFmt {
    pub decimal: char,
    pub group: char,
}

impl NumFmt {
    /// Разделители для языка; неизвестный язык — как en.
    pub fn of(lang: &LanguageIdentifier) -> Self {
        // Языки с U+00A0 и запятой; с точкой и запятой; остальные — как en.
        const SPACE: [&str; 18] = [
            "ru", "uk", "be", "kk", "bg", "cs", "sk", "pl", "fi", "sv", "nb", "no", "lt", "lv",
            "et", "hu", "fr", "pt",
        ];
        const DOT: [&str; 13] = [
            "de", "es", "it", "nl", "da", "tr", "id", "el", "ro", "hr", "sr", "sl", "vi",
        ];
        let language = lang.language.as_str();
        let region = lang
            .region
            .as_ref()
            .map(unic_langid::subtags::Region::as_str);
        if language == "pt" && region == Some("BR") || DOT.contains(&language) {
            Self {
                decimal: ',',
                group: '.',
            }
        } else if SPACE.contains(&language) {
            Self {
                decimal: ',',
                group: '\u{a0}',
            }
        } else {
            Self {
                decimal: '.',
                group: ',',
            }
        }
    }

    /// Целое с группировкой разрядов: `48 213`, `48,213`.
    pub fn int(self, n: u64) -> String {
        let digits = n.to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3 * 2);
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(self.group);
            }
            out.push(c);
        }
        out
    }

    /// Десятичная запись `as_string` Fluent (`-12345.5`) по правилам языка.
    fn localize(self, plain: &str) -> String {
        let (sign, rest) = plain.strip_prefix('-').map_or(("", plain), |r| ("-", r));
        let (int, frac) = rest
            .split_once('.')
            .map_or((rest, None), |(i, f)| (i, Some(f)));
        let mut out = String::from(sign);
        match int.parse::<u64>() {
            Ok(n) => out.push_str(&self.int(n)),
            Err(_) => out.push_str(int),
        }
        if let Some(frac) = frac {
            out.push(self.decimal);
            out.push_str(frac);
        }
        out
    }
}

/// Форматтер бандлов цепочки: числа в аргументах (`$count`) — с разделителями языка
/// интерфейса, даже в сообщении из en-fallback. Плюрали по-прежнему выбираются по числу.
/// Fluent принимает указатель на функцию без контекста, поэтому разделители — параметры типа.
pub(crate) fn formatter<M: MemoizerKind>(f: NumFmt) -> fn(&FluentValue, &M) -> Option<String> {
    match (f.decimal, f.group) {
        (',', '.') => localized::<',', '.', M>,
        (',', _) => localized::<',', '\u{a0}', M>,
        _ => localized::<'.', ',', M>,
    }
}

fn localized<const DECIMAL: char, const GROUP: char, M>(
    value: &FluentValue,
    _: &M,
) -> Option<String> {
    let FluentValue::Number(number) = value else {
        return None;
    };
    let f = NumFmt {
        decimal: DECIMAL,
        group: GROUP,
    };
    Some(f.localize(&number.as_string()))
}

/// Размер для колонок и подвала: не больше одного знака после запятой (`1,2 КБ`,
/// `47,1 КБ`, `977 МБ`; решение владельца 2026-10-17 — точные байты в свойствах);
/// меньше 1024 — в байтах (`1023 Б`). Основание 1024; единица растёт, когда округлённое
/// значение доходит до 1000 (`999 КБ` → `1,0 МБ`).
pub fn bytes(n: u64) -> String {
    bytes_with(&current(), n)
}

/// [`bytes`] для заданного языка.
pub fn bytes_with(i18n: &I18n, n: u64) -> String {
    let units = UNITS.map(|k| i18n.get(k));
    let mut out = String::new();
    let _ = write_bytes(
        &mut out,
        n,
        i18n.num_fmt().decimal,
        &units.each_ref().map(String::as_str),
    );
    out
}

/// Ключи Fluent единиц [`bytes`] (Б … ЭБ): для заранее подготовленных подписей.
pub const UNIT_KEYS: [&str; 7] = UNITS;

/// [`bytes`] в любой `fmt::Write` без кучи: `units` — подписи [`UNIT_KEYS`] на языке
/// интерфейса, `decimal` — разделитель дробной части.
pub fn write_bytes(
    w: &mut impl std::fmt::Write,
    n: u64,
    decimal: char,
    units: &[&str; 7],
) -> std::fmt::Result {
    let mut digits = Digits::default();
    let unit = bytes_parts(n, &mut digits);
    for c in digits.as_str().chars() {
        w.write_char(if c == '.' { decimal } else { c })?;
    }
    w.write_char(' ')?;
    w.write_str(units[unit])
}

/// Число до 20 знаков на стеке.
#[derive(Default)]
struct Digits {
    buf: [u8; 32],
    len: usize,
}

impl Digits {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or_default()
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

impl std::fmt::Write for Digits {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        let dst = self.buf.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        dst.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Число (в записи с точкой) в `out` и индекс единицы для [`bytes`].
fn bytes_parts(n: u64, out: &mut Digits) -> usize {
    use std::fmt::Write as _;
    out.clear();
    if n < 1024 {
        let _ = write!(out, "{n}");
        return 0;
    }
    // Точности f64 (53 бита) с запасом хватает на 3 значащие цифры.
    #[allow(clippy::cast_precision_loss)]
    let mut value = n as f64;
    for unit in 1..UNITS.len() {
        value /= 1024.0;
        for decimals in [1, 0] {
            out.clear();
            let _ = write!(out, "{value:.decimals$}");
            let limit = [1000.0, 100.0][decimals];
            if out.as_str().parse::<f64>().is_ok_and(|v| v < limit) {
                return unit;
            }
        }
    }
    unreachable!("u64::MAX < 1000 EiB")
}

/// Точный размер с группировкой: `48 213 байт`, `48,213 bytes`.
pub fn bytes_exact(n: u64) -> String {
    bytes_exact_with(&current(), n)
}

/// [`bytes_exact`] для заданного языка.
pub fn bytes_exact_with(i18n: &I18n, n: u64) -> String {
    // Числа Fluent — f64: точная запись отдельно, число только для плюрали.
    let mut args = fluent_bundle::FluentArgs::new();
    args.set("size", i18n.num_fmt().int(n));
    args.set("count", n);
    i18n.fmt("fmt-bytes-exact", &args)
}

/// Количество с группировкой: `12 345`, `12,345`.
pub fn count(n: u64) -> String {
    current().num_fmt().int(n)
}

/// Дата и время в системной зоне по шаблону `fmt-datetime`: ru `14.10.26 09:12`,
/// en `10/14/26 9:12 AM`. `mtime` — наносекунды от Unix epoch (`Entry::mtime`);
/// `i64::MIN` (неизвестно) и время вне диапазона jiff — пустая строка.
pub fn datetime(mtime: i64) -> String {
    datetime_with(&current(), mtime, &TimeZone::system())
}

/// [`datetime`] для заданного языка и зоны.
pub fn datetime_with(i18n: &I18n, mtime: i64, tz: &TimeZone) -> String {
    if mtime == i64::MIN {
        return String::new();
    }
    let Ok(ts) = Timestamp::from_nanosecond(i128::from(mtime)) else {
        return String::new();
    };
    let pattern = i18n.get("fmt-datetime");
    let zoned = ts.to_zoned(tz.clone());
    // Неверный шаблон из пользовательского `.ftl`: один warn и ISO вместо пустой ячейки.
    jiff::fmt::strtime::format(pattern.as_bytes(), &zoned).unwrap_or_else(|err| {
        i18n.warn_broken("fmt-datetime", || err.to_string());
        zoned.strftime("%Y-%m-%d %H:%M").to_string()
    })
}

/// Метка времени для имён файлов в системной зоне: `2026-10-23-1402` (запись сеанса
/// терминала, F03 §3.5). `secs` — секунды от Unix epoch; вне диапазона — UTC.
pub fn file_stamp(secs: i64) -> String {
    file_stamp_in(secs, &TimeZone::system())
}

/// [`file_stamp`] в заданной зоне.
pub fn file_stamp_in(secs: i64, tz: &TimeZone) -> String {
    let ts = Timestamp::from_second(secs).unwrap_or(Timestamp::UNIX_EPOCH);
    ts.to_zoned(tz.clone())
        .strftime("%Y-%m-%d-%H%M")
        .to_string()
}

/// Время в системной зоне: `14:05` (статус группы ввода, F03 §3.4). `secs` — секунды
/// от Unix epoch; вне диапазона — UTC.
pub fn clock(secs: i64) -> String {
    clock_in(secs, &TimeZone::system())
}

/// [`clock`] в заданной зоне.
pub fn clock_in(secs: i64, tz: &TimeZone) -> String {
    let ts = Timestamp::from_second(secs).unwrap_or(Timestamp::UNIX_EPOCH);
    ts.to_zoned(tz.clone()).strftime("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_utc() {
        // 2026-10-23 14:02:31 UTC.
        assert_eq!(clock_in(1_792_764_151, &TimeZone::UTC), "14:02");
    }

    #[test]
    fn file_stamp_utc() {
        // 2026-10-23 14:02:31 UTC.
        assert_eq!(
            file_stamp_in(1_792_764_151, &TimeZone::UTC),
            "2026-10-23-1402"
        );
    }

    #[test]
    fn separators() {
        let ru: LanguageIdentifier = "ru".parse().unwrap();
        let en: LanguageIdentifier = "en-US".parse().unwrap();
        let br: LanguageIdentifier = "pt-BR".parse().unwrap();
        assert_eq!(NumFmt::of(&ru).int(48_213), "48\u{a0}213");
        assert_eq!(NumFmt::of(&en).int(1_234_567), "1,234,567");
        assert_eq!(NumFmt::of(&br).int(1_000), "1.000");
        assert_eq!(NumFmt::of(&en).int(0), "0");
        assert_eq!(NumFmt::of(&en).int(999), "999");
        assert_eq!(NumFmt::of(&ru).localize("-12345.50"), "-12\u{a0}345,50");
    }

    fn bytes_parts(n: u64) -> (String, usize) {
        let mut d = Digits::default();
        let unit = super::bytes_parts(n, &mut d);
        (d.as_str().to_owned(), unit)
    }

    #[test]
    fn bytes_parts_edges() {
        assert_eq!(bytes_parts(0), ("0".into(), 0));
        assert_eq!(bytes_parts(1023), ("1023".into(), 0));
        assert_eq!(bytes_parts(1024), ("1.0".into(), 1));
        assert_eq!(bytes_parts(10_234), ("10.0".into(), 1));
        assert_eq!(bytes_parts(102_348), ("99.9".into(), 1));
        assert_eq!(bytes_parts(102_349), ("100".into(), 1));
        assert_eq!(bytes_parts(1_023_487), ("999".into(), 1));
        assert_eq!(bytes_parts(1_023_488), ("1.0".into(), 2));
        assert_eq!(bytes_parts(u64::MAX), ("16.0".into(), 6));
    }
}
