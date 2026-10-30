//! Строка перехода (S7 §3.5): разбор того, что набрано в «Перейти к строке» / «Перейти к
//! смещению». Общий для вьювера (`viewer.goto`) и редактора (`editor.goto`, S7-18):
//! разбор не знает режима — число без суффикса вьювер в Hex понимает как смещение.
//!
//! Синтаксис:
//! - `123` — строка (с 1); `123:7` — строка и колонка (редактор);
//! - `0x1F00`, `1F00h`, `$1F00` — смещение;
//! - `50%`, `12,5%` — доля файла;
//! - `+N` / `-N` — относительно текущей позиции (строки или байты по режиму).
//!
//! Пробелы, `_` и неразрывные пробелы внутри числа пропускаются (`1 000`).

/// Куда перейти.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Goto {
    /// Число: строка (с 1) в тексте, смещение в Hex; `col` — колонка (с 1) после `:`.
    Number { n: u64, col: Option<u64> },
    /// Смещение в байтах: `0x1F00`, `1F00h`, `$1F00`.
    Offset(u64),
    /// Доля файла в сотых долях процента: 0…10 000.
    Percent(u32),
    /// Сдвиг от текущей позиции: строк в тексте, байт в Hex.
    Rel(i64),
}

/// Набранное не разобрано.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GotoError {
    #[error("empty target")]
    Empty,
    #[error("expected a line, offset (0x1F00, 1F00h, $1F00), percent or +N/-N")]
    Invalid,
}

/// Без пробелов и разделителей разрядов.
fn compact(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, ' ' | '_' | '\t' | '\u{A0}' | '\u{202F}' | '\u{2009}'))
        .collect()
}

fn dec(s: &str) -> Result<u64, GotoError> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(GotoError::Invalid);
    }
    s.parse().map_err(|_| GotoError::Invalid)
}

fn hex(s: &str) -> Result<u64, GotoError> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(GotoError::Invalid);
    }
    u64::from_str_radix(s, 16).map_err(|_| GotoError::Invalid)
}

/// `50`, `12.5`, `12,5` → сотые доли процента; больше 100 % — ошибка.
fn percent(s: &str) -> Result<u32, GotoError> {
    let (int, frac) = match s.split_once(['.', ',']) {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int.is_empty() && frac.is_empty() {
        return Err(GotoError::Invalid);
    }
    let int = if int.is_empty() { 0 } else { dec(int)? };
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(GotoError::Invalid);
    }
    // Две цифры дроби: «12,345» → 12,34 %.
    let mut f = 0u64;
    for (i, b) in frac.bytes().take(2).enumerate() {
        f += u64::from(b - b'0') * if i == 0 { 10 } else { 1 };
    }
    let v = int
        .checked_mul(100)
        .and_then(|x| x.checked_add(f))
        .ok_or(GotoError::Invalid)?;
    if v > 10_000 {
        return Err(GotoError::Invalid);
    }
    u32::try_from(v).map_err(|_| GotoError::Invalid)
}

/// Разбор строки перехода.
pub fn parse(text: &str) -> Result<Goto, GotoError> {
    let s = compact(text.trim());
    if s.is_empty() {
        return Err(GotoError::Empty);
    }
    if let Some(p) = s.strip_suffix('%') {
        return percent(p).map(Goto::Percent);
    }
    if let Some(rest) = s.strip_prefix('+') {
        let n = dec(rest)?;
        return i64::try_from(n)
            .map(Goto::Rel)
            .map_err(|_| GotoError::Invalid);
    }
    if let Some(rest) = s.strip_prefix('-') {
        let n = dec(rest)?;
        return i64::try_from(n)
            .map(|n| Goto::Rel(-n))
            .map_err(|_| GotoError::Invalid);
    }
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return hex(h).map(Goto::Offset);
    }
    if let Some(h) = s.strip_prefix('$') {
        return hex(h).map(Goto::Offset);
    }
    if let Some(h) = s.strip_suffix(['h', 'H']) {
        return hex(h).map(Goto::Offset);
    }
    if let Some((line, col)) = s.split_once(':') {
        return Ok(Goto::Number {
            n: dec(line)?,
            col: Some(dec(col)?),
        });
    }
    Ok(Goto::Number {
        n: dec(&s)?,
        col: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(n: u64) -> Goto {
        Goto::Number { n, col: None }
    }

    #[test]
    fn plain_numbers_and_line_col() {
        assert_eq!(parse("123"), Ok(num(123)));
        assert_eq!(parse("  1 000 "), Ok(num(1000)));
        assert_eq!(parse("1_000"), Ok(num(1000)));
        assert_eq!(
            parse("29:7"),
            Ok(Goto::Number {
                n: 29,
                col: Some(7)
            })
        );
        assert_eq!(parse("29:"), Err(GotoError::Invalid));
        assert_eq!(parse(""), Err(GotoError::Empty));
        assert_eq!(parse("   "), Err(GotoError::Empty));
        assert_eq!(parse("12a"), Err(GotoError::Invalid));
        assert_eq!(parse("99999999999999999999999"), Err(GotoError::Invalid));
    }

    #[test]
    fn offsets_in_three_spellings() {
        assert_eq!(parse("0x1F00"), Ok(Goto::Offset(0x1F00)));
        assert_eq!(parse("0X1f00"), Ok(Goto::Offset(0x1F00)));
        assert_eq!(parse("1F00h"), Ok(Goto::Offset(0x1F00)));
        assert_eq!(parse("1f00H"), Ok(Goto::Offset(0x1F00)));
        assert_eq!(parse("$1F00"), Ok(Goto::Offset(0x1F00)));
        assert_eq!(parse("100h"), Ok(Goto::Offset(0x100)));
        assert_eq!(parse("0x"), Err(GotoError::Invalid));
        assert_eq!(parse("0xZZ"), Err(GotoError::Invalid));
        assert_eq!(parse("h"), Err(GotoError::Invalid));
        assert_eq!(parse("$"), Err(GotoError::Invalid));
    }

    #[test]
    fn percents() {
        assert_eq!(parse("50%"), Ok(Goto::Percent(5000)));
        assert_eq!(parse("0%"), Ok(Goto::Percent(0)));
        assert_eq!(parse("100%"), Ok(Goto::Percent(10_000)));
        assert_eq!(parse("12,5%"), Ok(Goto::Percent(1250)));
        assert_eq!(parse("12.345%"), Ok(Goto::Percent(1234)));
        assert_eq!(parse(".5%"), Ok(Goto::Percent(50)));
        assert_eq!(parse("101%"), Err(GotoError::Invalid));
        assert_eq!(parse("%"), Err(GotoError::Invalid));
        assert_eq!(parse("x%"), Err(GotoError::Invalid));
    }

    #[test]
    fn relative() {
        assert_eq!(parse("+10"), Ok(Goto::Rel(10)));
        assert_eq!(parse("-10"), Ok(Goto::Rel(-10)));
        assert_eq!(parse("+ 1 000"), Ok(Goto::Rel(1000)));
        assert_eq!(parse("+"), Err(GotoError::Invalid));
        assert_eq!(parse("-x"), Err(GotoError::Invalid));
        assert_eq!(parse("+0x10"), Err(GotoError::Invalid));
    }
}
