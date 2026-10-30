//! Hex-образец: `DE AD ?? EF` или `DEAD??EF` (S7 §3.6).

use super::SearchError;

/// Байты образца; `None` — `??` (любой байт). Ошибка — позиция символа (с 0).
pub fn parse(s: &str) -> Result<Vec<Option<u8>>, SearchError> {
    let mut out = Vec::new();
    let mut half: Option<(usize, char)> = None;
    for (pos, c) in s.chars().enumerate() {
        if c.is_whitespace() {
            if let Some((p, _)) = half {
                return Err(SearchError::Hex { pos: p });
            }
            continue;
        }
        let ok = c == '?' || c.is_ascii_hexdigit();
        if !ok {
            return Err(SearchError::Hex { pos });
        }
        match half.take() {
            None => half = Some((pos, c)),
            Some((p, a)) => match (a, c) {
                ('?', '?') => out.push(None),
                ('?', _) | (_, '?') => return Err(SearchError::Hex { pos: p }),
                (a, c) => {
                    let hi = a.to_digit(16).unwrap_or(0);
                    let lo = c.to_digit(16).unwrap_or(0);
                    out.push(Some((hi * 16 + lo) as u8));
                }
            },
        }
    }
    if let Some((p, _)) = half {
        return Err(SearchError::Hex { pos: p });
    }
    if out.is_empty() {
        return Err(SearchError::Empty);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        assert_eq!(
            parse("DE AD ?? ef").unwrap(),
            [Some(0xDE), Some(0xAD), None, Some(0xEF)]
        );
        assert_eq!(parse("dead??EF").unwrap(), parse("DE AD ?? EF").unwrap());
        assert_eq!(parse("DE A"), Err(SearchError::Hex { pos: 3 }));
        assert_eq!(parse("DE AX"), Err(SearchError::Hex { pos: 4 }));
        assert_eq!(parse("D?"), Err(SearchError::Hex { pos: 0 }));
        assert_eq!(parse("  "), Err(SearchError::Empty));
    }
}
