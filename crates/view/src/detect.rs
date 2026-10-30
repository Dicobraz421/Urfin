//! Детект кодировки и EOL по первым 64 КБ: BOM, UTF-16 без BOM, бинарный, UTF-8,
//! chardetng + tld, KOI8-R/U (S7 §3.2). Карточка S7-04.

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{
    Encoding, KOI8_R, KOI8_U, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251, WINDOWS_1252,
};

use crate::enc::Enc;

/// Окончания строк.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Eol {
    Lf,
    /// Так же для файла без переводов строк (новый файл — CRLF, S7 §8).
    #[default]
    CrLf,
    Cr,
    /// Разделитель LF, одиночный CR рисуется `␍`.
    Mixed,
}

/// Результат детекта.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub enc: Enc,
    /// Бинарный файл → Hex.
    pub binary: bool,
    pub eol: Eol,
    /// BOM UTF-32: hex и тост «не поддерживается».
    pub utf32: bool,
}

/// Сколько байт выборки отдаётся chardetng.
const CHARDET_MAX: usize = 16 << 10;

/// Порог выборки для UTF-16 без BOM.
const UTF16_MIN: usize = 512;

/// Байты украинских букв KOI8-U (є і ї ґ, строчные и прописные).
const KOI8_U_ONLY: [u8; 8] = [0xA4, 0xA6, 0xA7, 0xAD, 0xB4, 0xB6, 0xB7, 0xBD];

/// `tld` для chardetng по языку интерфейса: ru/uk/be/kk → `b"ru"`.
pub fn tld_for_lang(lang: &str) -> Option<&'static [u8]> {
    let primary = lang.split(['-', '_']).next().unwrap_or("");
    matches!(
        primary.to_ascii_lowercase().as_str(),
        "ru" | "uk" | "be" | "kk"
    )
    .then_some(b"ru".as_slice())
}

/// Детект по выборке `sample` (начало файла). `at_eof` — выборка — весь файл.
pub fn detect(sample: &[u8], at_eof: bool, tld: Option<&[u8]>) -> Detected {
    let auto = |enc: &'static Encoding, bom_len: u8| Enc {
        enc,
        auto: true,
        bom_len,
    };
    // 1. BOM (UTF-32 раньше UTF-16: FF FE 00 00 начинается с FF FE).
    if sample.starts_with(&[0xFF, 0xFE, 0, 0]) || sample.starts_with(&[0, 0, 0xFE, 0xFF]) {
        return Detected {
            enc: auto(fallback(tld), 0),
            binary: true,
            eol: Eol::default(),
            utf32: true,
        };
    }
    let bom = if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some(auto(UTF_8, 3))
    } else if sample.starts_with(&[0xFF, 0xFE]) {
        Some(auto(UTF_16LE, 2))
    } else if sample.starts_with(&[0xFE, 0xFF]) {
        Some(auto(UTF_16BE, 2))
    } else {
        None
    };
    if let Some(enc) = bom {
        let body = &sample[enc.bom_len as usize..];
        return text(enc, detect_eol(body, &enc, at_eof));
    }
    // 2. UTF-16 без BOM: нули на одной чётности.
    if let Some(enc) = utf16_guess(sample) {
        return text(auto(enc, 0), detect_eol(sample, &Enc::new(enc), at_eof));
    }
    // 3. Бинарный: NUL или больше 10 % управляющих.
    if is_binary(sample) {
        return Detected {
            enc: auto(fallback(tld), 0),
            binary: true,
            eol: Eol::default(),
            utf32: false,
        };
    }
    // 4. Валидный UTF-8 (обрезанный хвост выборки допустим), чистый ASCII — тоже.
    let utf8 = match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => !at_eof && e.error_len().is_none(),
    };
    let enc = if utf8 {
        UTF_8
    } else {
        // 5. chardetng; KOI8-U без украинских букв — KOI8-R.
        // chardetng линеен по длине (≈ 35 нс/байт на кириллице): 16 КБ хватает для
        // уверенного ответа и укладывают детект в 1 мс.
        let part = &sample[..sample.len().min(CHARDET_MAX)];
        let mut d = EncodingDetector::new(Iso2022JpDetection::Deny);
        d.feed(part, at_eof && part.len() == sample.len());
        match d.guess(tld, Utf8Detection::Deny) {
            e if e == KOI8_U && !sample.iter().any(|b| KOI8_U_ONLY.contains(b)) => KOI8_R,
            e => e,
        }
    };
    let enc = auto(enc, 0);
    text(enc, detect_eol(sample, &enc, at_eof))
}

fn text(enc: Enc, eol: Eol) -> Detected {
    Detected {
        enc,
        binary: false,
        eol,
        utf32: false,
    }
}

/// Кодировка текста в hex для бинарного файла: ANSI по языку (как Far).
fn fallback(tld: Option<&[u8]>) -> &'static Encoding {
    if tld == Some(b"ru") {
        WINDOWS_1251
    } else {
        WINDOWS_1252
    }
}

/// UTF-16 без BOM: ≥ 30 % нулей на одной чётности и < 5 % на другой, выборка ≥ 512 байт.
fn utf16_guess(s: &[u8]) -> Option<&'static Encoding> {
    if s.len() < UTF16_MIN {
        return None;
    }
    let (mut even, mut odd) = (0usize, 0usize);
    for pair in s.as_chunks::<2>().0 {
        even += usize::from(pair[0] == 0);
        odd += usize::from(pair[1] == 0);
    }
    let half = s.len() / 2;
    let many = |n: usize| n * 100 >= half * 30;
    let few = |n: usize| n * 100 < half * 5;
    if many(odd) && few(even) {
        Some(UTF_16LE)
    } else if many(even) && few(odd) {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// NUL или больше 10 % управляющих байтов, кроме `\t\n\r\f\x1b`.
fn is_binary(s: &[u8]) -> bool {
    if memchr::memchr(0, s).is_some() {
        return true;
    }
    let ctl = s
        .iter()
        .filter(|&&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B)) || b == 0x7F)
        .count();
    ctl * 10 > s.len()
}

/// EOL по выборке: есть LF → `Lf`/`CrLf`; нет LF, есть CR → `Cr`; смесь → `Mixed`.
pub fn detect_eol(s: &[u8], enc: &Enc, at_eof: bool) -> Eol {
    let (mut lf, mut crlf, mut cr) = (0u64, 0u64, 0u64);
    let mut prev_cr = false;
    let mut on = |c: u16| {
        match c {
            0x0A => {
                lf += 1;
                crlf += u64::from(prev_cr);
            }
            _ if prev_cr => cr += 1,
            _ => {}
        }
        prev_cr = c == 0x0D;
    };
    if enc.is_utf16() {
        let be = enc.is_utf16be();
        for p in s.as_chunks::<2>().0 {
            on(if be {
                u16::from_be_bytes([p[0], p[1]])
            } else {
                u16::from_le_bytes([p[0], p[1]])
            });
        }
    } else {
        for &b in s {
            on(b.into());
        }
    }
    // CR в самом конце: одиночный, только если это конец файла.
    if prev_cr && at_eof {
        cr += 1;
    }
    match (lf, cr) {
        (0, 0) => Eol::default(),
        (0, _) => Eol::Cr,
        (_, 0) if crlf == lf => Eol::CrLf,
        (_, 0) if crlf == 0 => Eol::Lf,
        _ => Eol::Mixed,
    }
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod tests;
