//! `Enc`, посимвольное декодирование строк, таблицы на 256 значений для однобайтовых
//! кодировок (S7 §0, §3.2). Карточка S7-04.

use std::sync::{Mutex, OnceLock, PoisonError};

use encoding_rs::{DecoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE};

/// Кодировка документа.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Enc {
    pub enc: &'static Encoding,
    /// Выбрана детектом (метка «авто» в «мешочке»).
    pub auto: bool,
    /// Длина BOM в начале файла (0, 2 или 3).
    pub bom_len: u8,
}

impl Default for Enc {
    fn default() -> Self {
        Enc::utf8()
    }
}

impl Enc {
    pub const fn new(enc: &'static Encoding) -> Enc {
        Enc {
            enc,
            auto: false,
            bom_len: 0,
        }
    }

    pub const fn utf8() -> Enc {
        Enc::new(UTF_8)
    }

    pub fn is_utf8(&self) -> bool {
        self.enc == UTF_8
    }

    pub fn is_utf16(&self) -> bool {
        self.enc == UTF_16LE || self.enc == UTF_16BE
    }

    pub fn is_utf16be(&self) -> bool {
        self.enc == UTF_16BE
    }

    /// Байт в единице кода: 2 у UTF-16, иначе 1.
    pub fn unit(&self) -> u8 {
        if self.is_utf16() { 2 } else { 1 }
    }

    /// Однобайтовая кодировка (1251, 866, KOI8, 1252…): символ на байт, таблица.
    pub fn is_single_byte(&self) -> bool {
        self.enc.is_single_byte()
    }

    /// Таблица байт → символ для однобайтовой кодировки.
    pub fn table(&self) -> Option<&'static ByteTable> {
        self.is_single_byte().then(|| ByteTable::of(self.enc))
    }

    /// Посимвольный декодер.
    pub fn decoder(&self) -> CharDecoder {
        CharDecoder::new(*self)
    }
}

/// Байт → символ однобайтовой кодировки; неотображаемые байты — U+FFFD.
#[derive(Debug)]
pub struct ByteTable(pub [char; 256]);

impl ByteTable {
    /// Таблица кодировки (строится один раз на процесс).
    pub fn of(enc: &'static Encoding) -> &'static ByteTable {
        type Tables = Mutex<Vec<(&'static Encoding, &'static ByteTable)>>;
        static TABLES: OnceLock<Tables> = OnceLock::new();
        let tables = TABLES.get_or_init(Mutex::default);
        let mut t = tables.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, tab)) = t.iter().find(|(e, _)| *e == enc) {
            return tab;
        }
        let mut chars = ['\u{FFFD}'; 256];
        for (b, c) in chars.iter_mut().enumerate() {
            let byte = [b as u8];
            let (s, bad) = enc.decode_without_bom_handling(&byte);
            if !bad {
                *c = s.chars().next().unwrap_or('\u{FFFD}');
            }
        }
        let tab: &'static ByteTable = Box::leak(Box::new(ByteTable(chars)));
        t.push((enc, tab));
        tab
    }
}

/// Шаг декодирования.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Символ и его длина в байтах.
    Char(char, u8),
    /// Недекодируемые байты (рисуются U+FFFD).
    Bad(u8),
}

impl Step {
    /// Байт в шаге.
    pub fn bytes(self) -> usize {
        match self {
            Step::Char(_, n) | Step::Bad(n) => n.into(),
        }
    }
}

/// Посимвольный декодер строки: UTF-8, UTF-16 и однобайтовые — без состояния; прочие
/// многобайтовые (`Shift_JIS`, GBK, Big5, EUC-*) — `encoding_rs::Decoder` по байту. Если
/// многобайтовая последовательность даёт два символа (редкие пары Big5-HKSCS), рисуется
/// первый.
pub struct CharDecoder {
    enc: Enc,
    table: Option<&'static ByteTable>,
    legacy: Option<encoding_rs::Decoder>,
}

impl std::fmt::Debug for CharDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CharDecoder")
            .field("enc", &self.enc.enc.name())
            .finish_non_exhaustive()
    }
}

impl CharDecoder {
    pub fn new(enc: Enc) -> CharDecoder {
        CharDecoder {
            enc,
            table: enc.table(),
            legacy: None,
        }
    }

    /// Следующий символ в начале `b` (`b` не пуст). Обрезанная последовательность в конце
    /// `b` — `Bad(1)`: ряды режутся по границам символов, обрезка — порча.
    pub fn next(&mut self, b: &[u8]) -> Step {
        if let Some(t) = self.table {
            return match t.0[b[0] as usize] {
                '\u{FFFD}' => Step::Bad(1),
                c => Step::Char(c, 1),
            };
        }
        if self.enc.is_utf8() {
            return utf8(b);
        }
        if self.enc.is_utf16() {
            return utf16(b, self.enc.is_utf16be());
        }
        self.legacy_next(b)
    }

    fn legacy_next(&mut self, b: &[u8]) -> Step {
        let enc = self.enc.enc;
        let dec = self
            .legacy
            .get_or_insert_with(|| enc.new_decoder_without_bom_handling());
        let mut out = [0u8; 16];
        let mut used = 0usize;
        // Escape ISO-2022-JP + символ укладываются в 8 байт.
        while used < b.len() && used < 8 {
            let (res, read, written) =
                dec.decode_to_utf8_without_replacement(&b[used..=used], &mut out, false);
            used += read.max(1);
            match res {
                DecoderResult::InputEmpty if written > 0 => {
                    let s = std::str::from_utf8(&out[..written]).unwrap_or("\u{FFFD}");
                    let c = s.chars().next().unwrap_or('\u{FFFD}');
                    return Step::Char(c, used as u8);
                }
                DecoderResult::InputEmpty => {}
                DecoderResult::Malformed(..) | DecoderResult::OutputFull => break,
            }
        }
        // Порча или незаконченная последовательность: байт — U+FFFD, декодер заново.
        self.legacy = None;
        Step::Bad(1)
    }
}

/// UTF-8: символ или `Bad(1)`.
fn utf8(b: &[u8]) -> Step {
    let lead = b[0];
    if lead < 0x80 {
        return Step::Char(lead as char, 1);
    }
    let n = match lead {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return Step::Bad(1),
    };
    if b.len() < n {
        return Step::Bad(1);
    }
    match std::str::from_utf8(&b[..n]) {
        Ok(s) => Step::Char(s.chars().next().unwrap_or('\u{FFFD}'), n as u8),
        Err(_) => Step::Bad(1),
    }
}

/// UTF-16: символ (с суррогатной парой) или порча (`Bad(2)`, хвостовой байт — `Bad(1)`).
fn utf16(b: &[u8], be: bool) -> Step {
    let unit = |i: usize| -> Option<u16> {
        let p = b.get(i..i + 2)?;
        Some(if be {
            u16::from_be_bytes([p[0], p[1]])
        } else {
            u16::from_le_bytes([p[0], p[1]])
        })
    };
    let Some(u) = unit(0) else {
        return Step::Bad(1);
    };
    match u {
        0xD800..=0xDBFF => match unit(2) {
            Some(lo @ 0xDC00..=0xDFFF) => {
                let c = 0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(lo) - 0xDC00);
                Step::Char(char::from_u32(c).unwrap_or('\u{FFFD}'), 4)
            }
            _ => Step::Bad(2),
        },
        0xDC00..=0xDFFF => Step::Bad(2),
        u => Step::Char(char::from_u32(u.into()).unwrap_or('\u{FFFD}'), 2),
    }
}

/// Байт — не продолжение UTF-8 (`10xxxxxx`).
pub fn is_utf8_boundary(b: u8) -> bool {
    b & 0xC0 != 0x80
}
