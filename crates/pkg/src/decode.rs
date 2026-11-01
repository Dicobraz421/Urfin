//! Строки вывода менеджеров (F02 §2.3, «Кодировка»): каждая строка — UTF-8 строго,
//! иначе OEM-кодовая страница системы; затем снимаются VT-последовательности и
//! `\r`-перезаписи (остаётся последний сегмент). Кодировка решается по строке: в одном
//! потоке бывают и UTF-8 (winget, PowerShell), и OEM (choco, инсталляторы).

use encoding_rs::Encoding;
use oem_cp::code_table::DECODING_TABLE_CP_MAP;
use oem_cp::code_table_type::TableType;

/// Потолок незавершённой строки: прогресс без `\n` (только `\r`) не копится без конца.
const MAX_PENDING: usize = 64 * 1024;

/// Декодер OEM-кодовой страницы.
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    cp: u16,
    oem: Oem,
}

#[derive(Clone, Copy)]
enum Oem {
    /// Однобайтовые OEM (437, 850, 866…).
    Table(&'static TableType),
    /// Многобайтовые и прочие из `encoding_rs` (932, 936, 949, 950, 125x).
    Encoding(&'static Encoding),
    /// Неизвестная страница: UTF-8 с заменой.
    Lossy,
}

impl std::fmt::Debug for Oem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Oem::Table(_) => f.write_str("Table"),
            Oem::Encoding(e) => write!(f, "Encoding({})", e.name()),
            Oem::Lossy => f.write_str("Lossy"),
        }
    }
}

impl Decoder {
    /// Декодер для кодовой страницы `cp` (`OEMCP` из реестра).
    pub fn new(cp: u16) -> Decoder {
        let oem = if let Some(t) = DECODING_TABLE_CP_MAP.get(&cp) {
            Oem::Table(t)
        } else if let Some(e) = encoding(cp) {
            Oem::Encoding(e)
        } else {
            Oem::Lossy
        };
        Decoder { cp, oem }
    }

    pub fn cp(&self) -> u16 {
        self.cp
    }

    /// Байты одной строки (без `\n`) → текст: UTF-8 строго, иначе OEM.
    pub fn decode(&self, bytes: &[u8]) -> String {
        if let Ok(s) = std::str::from_utf8(bytes) {
            return s.to_owned();
        }
        match self.oem {
            Oem::Table(t) => t.decode_string_lossy(bytes),
            Oem::Encoding(e) => e.decode_without_bom_handling(bytes).0.into_owned(),
            Oem::Lossy => String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    /// Строка вывода, готовая к разбору: декодирована, без BOM, VT и перезаписей.
    pub fn line(&self, bytes: &[u8]) -> String {
        clean(&self.decode(bytes))
    }
}

/// Кодовые страницы Windows, которых нет в таблицах `oem_cp`.
fn encoding(cp: u16) -> Option<&'static Encoding> {
    Some(match cp {
        932 => encoding_rs::SHIFT_JIS,
        936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1252 => encoding_rs::WINDOWS_1252,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        20866 => encoding_rs::KOI8_R,
        21866 => encoding_rs::KOI8_U,
        _ => return None,
    })
}

/// Снять BOM, VT-последовательности и управляющие символы; `\r` — перезапись строки
/// с начала: остаётся последний сегмент (хвостовые `\r` от `\r\n` не в счёт).
/// Backspace стирает символ слева.
pub fn clean(s: &str) -> String {
    let s = s.trim_start_matches('\u{feff}');
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.next() {
                Some('[') => skip_csi(&mut it),
                Some(']' | 'P' | 'X' | '^' | '_') => skip_string(&mut it),
                // `ESC ( B` и подобные: промежуточные 0x20–0x2F и финальный.
                Some(' '..='/') => {
                    while it.next_if(|c| matches!(c, ' '..='/')).is_some() {}
                    it.next();
                }
                _ => {}
            },
            '\u{9b}' => skip_csi(&mut it),
            '\u{9d}' => skip_string(&mut it),
            '\u{8}' => {
                out.pop();
            }
            '\r' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.truncate(out.trim_end_matches('\r').len());
    if let Some(i) = out.rfind('\r') {
        out.drain(..=i);
    }
    out
}

/// CSI: параметры и промежуточные до финального 0x40–0x7E.
fn skip_csi(it: &mut impl Iterator<Item = char>) {
    for c in it.by_ref() {
        if ('@'..='~').contains(&c) {
            break;
        }
    }
}

/// OSC, DCS и прочие строки: до BEL или ST (`ESC \`, 0x9C).
fn skip_string(it: &mut std::iter::Peekable<impl Iterator<Item = char>>) {
    while let Some(c) = it.next() {
        match c {
            '\u{7}' | '\u{9c}' => break,
            '\u{1b}' => {
                it.next_if_eq(&'\\');
                break;
            }
            _ => {}
        }
    }
}

/// Разрезка байтового потока на строки по `\n`. Незавершённая строка длиннее
/// `MAX_PENDING` ужимается до последнего `\r`-сегмента (или отдаётся как строка).
#[derive(Debug, Default)]
pub struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    /// Очередной кусок потока; готовые строки (байты без `\n`) — в `out`.
    pub fn push(&mut self, mut bytes: &[u8], out: &mut impl FnMut(&[u8])) {
        while let Some(i) = bytes.iter().position(|&b| b == b'\n') {
            if self.pending.is_empty() {
                out(&bytes[..i]);
            } else {
                self.pending.extend_from_slice(&bytes[..i]);
                out(&self.pending);
                self.pending.clear();
            }
            bytes = &bytes[i + 1..];
        }
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > MAX_PENDING {
            match self.pending.iter().rposition(|&b| b == b'\r') {
                Some(i) if i > 0 => {
                    self.pending.drain(..i);
                }
                _ => {
                    out(&self.pending);
                    self.pending.clear();
                }
            }
        }
    }

    /// Конец потока: хвост без `\n` — последняя строка.
    pub fn finish(&mut self, out: &mut impl FnMut(&[u8])) {
        if !self.pending.is_empty() {
            out(&self.pending);
            self.pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn lines(d: &Decoder, chunks: &[&[u8]]) -> Vec<String> {
        let mut sp = LineSplitter::default();
        let mut out = Vec::new();
        for c in chunks {
            sp.push(c, &mut |l| out.push(d.line(l)));
        }
        sp.finish(&mut |l| out.push(d.line(l)));
        out
    }

    /// Приёмка: CP866 и UTF-8 в одном потоке.
    #[test]
    fn cp866_and_utf8_in_one_stream() {
        let d = Decoder::new(866);
        let (cp866, _, bad) = encoding_rs::IBM866.encode("Установка пакета");
        assert!(!bad);
        let mut stream = Vec::new();
        stream.extend_from_slice("Найдено: Mozilla Firefox\r\n".as_bytes());
        stream.extend_from_slice(&cp866);
        stream.extend_from_slice(b"\r\n");
        stream.extend_from_slice("готово ✓\n".as_bytes());
        // Куски режут и строки, и многобайтовые символы.
        let (a, b) = stream.split_at(7);
        let (b, c) = b.split_at(30);
        assert_eq!(
            lines(&d, &[a, b, c]),
            ["Найдено: Mozilla Firefox", "Установка пакета", "готово ✓"]
        );
    }

    #[test]
    fn oem_code_pages() {
        // CP437: `é` — 0x82, `½` — 0xAB.
        assert_eq!(
            Decoder::new(437).decode(&[b'c', b'a', b'f', 0x82, 0xAB]),
            "café½"
        );
        assert_eq!(Decoder::new(850).decode(&[0x9D]), "Ø");
        assert_eq!(Decoder::new(932).decode(&[0x83, 0x65]), "テ");
        assert_eq!(Decoder::new(1251).decode(&[0xCF]), "П");
        assert_eq!(Decoder::new(1).decode(&[b'a', 0xFF]), "a\u{fffd}");
    }

    #[test]
    fn carriage_return_keeps_last_segment() {
        assert_eq!(clean("10%\r50%\r100%"), "100%");
        assert_eq!(
            clean("  - \r  \\ \r          \rFound Git [Git.Git]\r"),
            "Found Git [Git.Git]"
        );
        assert_eq!(clean("plain\r"), "plain");
        assert_eq!(clean("plain"), "plain");
        assert_eq!(clean("\u{feff}bom"), "bom");
    }

    #[test]
    fn vt_and_controls_removed() {
        assert_eq!(clean("\u{1b}[32mgreen\u{1b}[0m text"), "green text");
        assert_eq!(clean("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(
            clean("\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(clean("\u{1b}(Bascii\u{1b}=x"), "asciix");
        assert_eq!(clean("\u{1b}[?25l\u{1b}[2K\rDone\u{1b}[?25h"), "Done");
        assert_eq!(clean("ab\u{8}c\u{7}\tz"), "ac\tz");
    }

    #[test]
    fn long_progress_without_newline_is_bounded() {
        let d = Decoder::new(866);
        let mut sp = LineSplitter::default();
        let mut out = Vec::new();
        for i in 0..20_000 {
            sp.push(format!("\r{i:>5}%").as_bytes(), &mut |l| {
                out.push(d.line(l));
            });
            assert!(sp.pending.len() <= MAX_PENDING + 8);
        }
        sp.push(b" done\n", &mut |l| out.push(d.line(l)));
        assert_eq!(out, ["19999% done"]);
    }

    proptest! {
        /// Произвольные байты и нарезка — без паники; строк не больше, чем `\n` + 1.
        #[test]
        fn arbitrary_bytes(
            bytes in proptest::collection::vec(any::<u8>(), 0..4096),
            cut in 0usize..4096,
            cp in prop_oneof![Just(437u16), Just(866), Just(932), Just(65001)],
        ) {
            let d = Decoder::new(cp);
            let cut = cut.min(bytes.len());
            let (a, b) = bytes.split_at(cut);
            let n = lines(&d, &[a, b]).len();
            prop_assert!(n <= bytes.split(|&c| c == b'\n').count());
        }
    }
}
