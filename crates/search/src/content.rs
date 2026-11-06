//! Поиск по содержимому (S8 §2.3): `grep-searcher` + `grep-regex` через `search_reader`.
//! Mmap нет: `MmapChoice::auto` — `unsafe`, а обход дерева от него и не выигрывает.
//!
//! Режим «все кодировки» — один байтовый regex на иглы в UTF-8, UTF-16LE, cp1251 и cp866:
//! перекодировать файл в каждую — четыре чтения. Файл с BOM UTF-16 иначе нельзя резать на
//! строки верно (байт `0A` бывает в середине символа), поэтому он перекодируется и ищется
//! обычным матчером.

use std::cell::Cell;
use std::fmt::Write as _;
use std::io::{self, Cursor, Read};
use std::ops::Range;
use std::sync::Arc;

use compact_str::CompactString;
use encoding_rs::{Encoding, IBM866, UTF_8, UTF_16LE, WINDOWS_1251};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};

use crate::query::{EncodingMode, QueryError, TextMode, TextQuery, hex_bytes};

/// Фрагмент строки, символов.
const PREVIEW_CHARS: usize = 120;
/// Слева от совпадения, символов, если справа есть что показать.
const LEAD_CHARS: usize = 40;
/// Окно байт вокруг совпадения: хватает на 120 символов в UTF-8.
const WINDOW: usize = 480;
/// Самая длинная строка: дальше файл не читается (бинарный без `NUL` не съест память).
const HEAP_LIMIT: usize = 64 << 20;

/// Порядок определения кодировки совпадения: UTF-8 первой, ASCII-игла — её.
const ENCODINGS: [&Encoding; 4] = [UTF_8, UTF_16LE, WINDOWS_1251, IBM866];

thread_local! {
    static UI_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Помечает текущий поток как UI: чтение файла из него — паника в debug.
pub fn mark_ui_thread() {
    UI_THREAD.with(|c| c.set(true));
}

/// Текущий поток помечен как UI.
pub fn is_ui_thread() -> bool {
    UI_THREAD.with(Cell::get)
}

/// Символ в простом сворачивании регистра: как в маске, без полного case folding.
fn fold(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

fn folded(s: &str) -> String {
    s.chars().map(fold).collect()
}

/// `c` и другой его регистр.
fn variants(c: char, case: bool) -> Vec<char> {
    let mut out = vec![c];
    if case {
        return out;
    }
    let mut u = c.to_uppercase();
    let upper = match (u.next(), u.next()) {
        (Some(one), None) => one,
        _ => c,
    };
    for v in [fold(c), upper] {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// Байты символа в кодировке; `None` — символа в ней нет.
fn encode(enc: &'static Encoding, c: char) -> Option<Vec<u8>> {
    if enc == UTF_16LE {
        let mut b = [0u16; 2];
        return Some(
            c.encode_utf16(&mut b)
                .iter()
                .flat_map(|u| u.to_le_bytes())
                .collect(),
        );
    }
    let mut buf = [0u8; 4];
    let (bytes, _, unmappable) = enc.encode(c.encode_utf8(&mut buf));
    (!unmappable).then(|| bytes.into_owned())
}

fn escape(out: &mut String, bytes: &[u8]) {
    for b in bytes {
        let _ = write!(out, "\\x{b:02X}");
    }
}

/// Игла в кодировке: регистр — альтернативы байтовых форм символа. `None` — не выражается.
fn needle_in(enc: &'static Encoding, needle: &str, case: bool) -> Option<String> {
    let mut out = String::new();
    for c in needle.chars() {
        let forms: Vec<Vec<u8>> = variants(c, case)
            .into_iter()
            .filter_map(|v| encode(enc, v))
            .collect();
        match forms.as_slice() {
            [] => return None,
            [one] => escape(&mut out, one),
            many => {
                out.push_str("(?:");
                for (i, f) in many.iter().enumerate() {
                    if i > 0 {
                        out.push('|');
                    }
                    escape(&mut out, f);
                }
                out.push(')');
            }
        }
    }
    Some(out)
}

/// Байтовый regex «игла в любой из кодировок»; ASCII-формы схлопываются.
fn all_encodings_pattern(needle: &str, case: bool) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(ENCODINGS.len());
    for enc in ENCODINGS {
        if let Some(p) = needle_in(enc, needle, case)
            && !parts.contains(&p)
        {
            parts.push(p);
        }
    }
    format!("(?-u)(?:{})", parts.join("|"))
}

/// Режим «все кодировки».
#[derive(Clone, Debug)]
struct AllEncodings {
    /// Для файлов с BOM UTF-16: они перекодируются в UTF-8.
    decoded: RegexMatcher,
    /// Игла; без учёта регистра — свёрнутая.
    needle: String,
    case: bool,
    whole_word: bool,
}

impl AllEncodings {
    /// Кодировка совпадения.
    fn classify(&self, bytes: &[u8]) -> Option<&'static Encoding> {
        ENCODINGS.into_iter().find(|enc| {
            let (s, bad) = enc.decode_without_bom_handling(bytes);
            !bad && if self.case {
                s == self.needle
            } else {
                folded(&s) == self.needle
            }
        })
    }

    /// Совпадение настоящее. Байт `8F` — и `П` в cp866, и хвост `я` в UTF-8: в строке, которая
    /// сама валидный UTF-8, однобайтовая кодировка — случайность.
    fn accepts(&self, line: &[u8], r: &Range<usize>, enc: &'static Encoding) -> bool {
        if enc.is_single_byte() && !line.is_ascii() && str::from_utf8(line).is_ok() {
            return false;
        }
        !self.whole_word || word_bounds(line, r, enc)
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Границы слова в кодировке совпадения: `(?-u)\b` кириллицу в cp1251 словом не считает.
fn word_bounds(line: &[u8], r: &Range<usize>, enc: &'static Encoding) -> bool {
    let before = &line[r.start.saturating_sub(4)..r.start];
    let after = &line[r.end..(r.end + 4).min(line.len())];
    let last = enc
        .decode_without_bom_handling(before)
        .0
        .chars()
        .next_back();
    let next = enc.decode_without_bom_handling(after).0.chars().next();
    !last.is_some_and(is_word) && !next.is_some_and(is_word)
}

/// Текстовая часть запроса: матчеры собраны, общие для всех потоков.
#[derive(Clone, Debug)]
pub struct CompiledText {
    matcher: RegexMatcher,
    all: Option<AllEncodings>,
    named: Option<&'static Encoding>,
    binary: bool,
    invert: bool,
}

fn regex_err(e: impl std::fmt::Display) -> QueryError {
    QueryError::BadRegex(e.to_string())
}

impl CompiledText {
    /// Сборка матчера: дорого для большого regex, поэтому в job, а не в UI-потоке.
    pub fn new(t: &TextQuery) -> Result<CompiledText, QueryError> {
        let mut plain = RegexMatcherBuilder::new();
        plain
            .case_insensitive(!t.case)
            .word(t.whole_word)
            .fixed_strings(t.mode == TextMode::Literal);
        let mut all = None;
        let matcher = match (t.mode, t.encoding) {
            (TextMode::Hex, _) => {
                let bytes =
                    hex_bytes(&t.pattern).ok_or_else(|| QueryError::BadRegex(t.pattern.clone()))?;
                let mut p = String::from("(?-u)");
                escape(&mut p, &bytes);
                RegexMatcherBuilder::new().build(&p).map_err(regex_err)?
            }
            (TextMode::Literal, EncodingMode::All) => {
                all = Some(AllEncodings {
                    decoded: plain.build(&t.pattern).map_err(regex_err)?,
                    needle: if t.case {
                        t.pattern.clone()
                    } else {
                        folded(&t.pattern)
                    },
                    case: t.case,
                    whole_word: t.whole_word,
                });
                let p = all_encodings_pattern(&t.pattern, t.case);
                RegexMatcherBuilder::new().build(&p).map_err(regex_err)?
            }
            _ => plain.build(&t.pattern).map_err(regex_err)?,
        };
        Ok(CompiledText {
            matcher,
            all,
            named: match t.encoding {
                EncodingMode::Named(e) => Some(e),
                _ => None,
            },
            binary: t.binary,
            invert: t.invert,
        })
    }
}

/// Запись о совпавшей строке.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHit {
    /// С единицы; 0 — запись о файле целиком (`invert`).
    pub line: u32,
    /// Совпадение в `preview`: байты, от и до.
    pub hl: (u16, u16),
    pub preview: Option<CompactString>,
}

/// Чтение кусками с проверкой отмены.
struct Stoppable<'a, R> {
    inner: R,
    stop: &'a dyn Fn() -> bool,
}

impl<R: Read> Read for Stoppable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if (self.stop)() {
            return Err(io::Error::other("cancelled"));
        }
        self.inner.read(buf)
    }
}

/// Поисковик: на поток, у него свои буферы.
#[derive(Debug)]
pub struct ContentSearcher {
    text: Arc<CompiledText>,
    max: usize,
    /// Без перекодировки: байтовый regex «всех кодировок».
    raw: Searcher,
    /// С BOM-сниффингом, при `Named` — с перекодировкой.
    decoding: Searcher,
}

impl ContentSearcher {
    pub fn new(text: Arc<CompiledText>, max_per_file: u32) -> ContentSearcher {
        let detect = if text.binary {
            BinaryDetection::none()
        } else {
            BinaryDetection::quit(0)
        };
        let mut b = SearcherBuilder::new();
        b.line_number(true)
            .binary_detection(detect)
            .heap_limit(Some(HEAP_LIMIT));
        let raw = b.bom_sniffing(false).build();
        let named = text
            .named
            .and_then(|e| grep_searcher::Encoding::new(e.name()).ok());
        let decoding = b.bom_sniffing(true).encoding(named).build();
        ContentSearcher {
            text,
            max: (max_per_file as usize).max(1),
            raw,
            decoding,
        }
    }

    /// Ищет в потоке `src`, записи — в `out` (`out` очищается). `Ok(false)` — файл бинарный и
    /// пропущен. Ошибка чтения и отмена (`stop`) — `Err`, найденное до неё остаётся в `out`.
    pub fn search(
        &mut self,
        mut src: impl Read,
        stop: &dyn Fn() -> bool,
        out: &mut Vec<FileHit>,
    ) -> io::Result<bool> {
        debug_assert!(!is_ui_thread(), "файл читается из UI-потока");
        out.clear();
        let mut head = [0u8; 2];
        let mut n = 0;
        while n < head.len() {
            match src.read(&mut head[n..])? {
                0 => break,
                got => n += got,
            }
        }
        let bom16 = n == 2 && matches!(head, [0xFF, 0xFE] | [0xFE, 0xFF]);
        let inner = Cursor::new(&head[..n]).chain(src);
        let reader = Stoppable { inner, stop };
        let text = Arc::clone(&self.text);
        let (searcher, matcher, all) = match &text.all {
            Some(a) if bom16 => (&mut self.decoding, &a.decoded, None),
            Some(a) => (&mut self.raw, &text.matcher, Some(a)),
            None => (&mut self.decoding, &text.matcher, None),
        };
        let mut sink = Collect {
            matcher,
            all,
            out,
            // `invert` спрашивает только «есть ли в файле».
            max: if text.invert { 1 } else { self.max },
            binary: false,
        };
        searcher.search_reader(matcher, reader, &mut sink)?;
        if sink.binary {
            out.clear();
            return Ok(false);
        }
        if text.invert {
            let absent = out.is_empty();
            out.clear();
            if absent {
                out.push(FileHit {
                    line: 0,
                    hl: (0, 0),
                    preview: None,
                });
            }
        }
        Ok(true)
    }
}

/// Sink: совпавшие строки файла до лимита.
struct Collect<'a> {
    matcher: &'a RegexMatcher,
    /// Только в байтовом режиме «всех кодировок».
    all: Option<&'a AllEncodings>,
    out: &'a mut Vec<FileHit>,
    max: usize,
    binary: bool,
}

impl Collect<'_> {
    /// Первое настоящее совпадение строки и его кодировка.
    fn locate(&self, line: &[u8]) -> Option<(Range<usize>, &'static Encoding)> {
        let mut at = 0;
        while at <= line.len() {
            let m = self.matcher.find_at(line, at).ok()??;
            let r = m.start()..m.end();
            let Some(all) = self.all else {
                return Some((r, UTF_8));
            };
            let enc = all.classify(&line[r.clone()]).unwrap_or(UTF_8);
            if all.accepts(line, &r, enc) {
                return Some((r, enc));
            }
            at = r.end.max(at + 1);
        }
        None
    }
}

impl Sink for &mut Collect<'_> {
    type Error = io::Error;

    fn matched(&mut self, _s: &Searcher, m: &SinkMatch<'_>) -> Result<bool, io::Error> {
        let line = m.bytes();
        let Some((r, enc)) = self.locate(line) else {
            return Ok(true);
        };
        let (preview, hl) = snippet(line, r, line_encoding(line, enc));
        self.out.push(FileHit {
            line: u32::try_from(m.line_number().unwrap_or(0)).unwrap_or(u32::MAX),
            hl,
            preview: Some(preview),
        });
        Ok(self.out.len() < self.max)
    }

    fn binary_data(&mut self, _s: &Searcher, _offset: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }
}

/// Кодировка строки для показа. ASCII-игла одинакова в UTF-8, cp1251 и cp866: если строка не
/// UTF-8, берём ту однобайтовую, в которой больше строчной кириллицы.
fn line_encoding(line: &[u8], enc: &'static Encoding) -> &'static Encoding {
    if enc != UTF_8 || str::from_utf8(line).is_ok() {
        return enc;
    }
    let score = |e: &'static Encoding| {
        let s = e.decode_without_bom_handling(line).0;
        s.chars().filter(|c| matches!(c, 'а'..='я' | 'ё')).count()
    };
    let (cp1251, cp866) = (score(WINDOWS_1251), score(IBM866));
    if cp1251 == 0 && cp866 == 0 {
        UTF_8
    } else if cp866 > cp1251 {
        IBM866
    } else {
        WINDOWS_1251
    }
}

/// Границы окна вокруг `r` на символах кодировки.
fn window(line: &[u8], r: &Range<usize>, enc: &'static Encoding) -> Range<usize> {
    let mut start = r.start.saturating_sub(WINDOW);
    let mut end = (r.end + WINDOW).min(line.len());
    if enc == UTF_16LE {
        start += (r.start - start) % 2;
        end -= (end - r.end) % 2;
    } else if enc == UTF_8 {
        while start < r.start && line[start] & 0xC0 == 0x80 {
            start += 1;
        }
        while end > r.end && end < line.len() && line[end] & 0xC0 == 0x80 {
            end -= 1;
        }
    }
    start..end
}

/// Текст для строки списка: управляющие — пробелы, BOM и хвост строки убраны.
fn clean(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars()
        .filter(|&c| c != '\u{FEFF}')
        .map(|c| if c.is_control() { ' ' } else { c })
}

/// Фрагмент строки ≤ 120 символов вокруг совпадения и границы совпадения в нём, байты.
fn snippet(line: &[u8], r: Range<usize>, enc: &'static Encoding) -> (CompactString, (u16, u16)) {
    let w = window(line, &r, enc);
    let part = |a: usize, b: usize| {
        let s = enc.decode_without_bom_handling(&line[a..b]).0;
        clean(&s).collect::<Vec<char>>()
    };
    let mut lead = part(w.start, r.start);
    let mid = part(r.start, r.end);
    let mut tail = part(r.end, w.end);
    while tail.last().is_some_and(|c| c.is_whitespace()) {
        tail.pop();
    }
    let lead_skip = lead.iter().take_while(|c| c.is_whitespace()).count();
    lead.drain(..lead_skip);

    let mid_len = mid.len().min(PREVIEW_CHARS);
    let room = PREVIEW_CHARS - mid_len;
    let keep_lead = lead
        .len()
        .min(LEAD_CHARS.max(room.saturating_sub(tail.len())))
        .min(room);
    let keep_tail = tail.len().min(room - keep_lead);

    let mut s = String::with_capacity(PREVIEW_CHARS);
    s.extend(&lead[lead.len() - keep_lead..]);
    let from = s.len();
    s.extend(&mid[..mid_len]);
    let to = s.len();
    s.extend(&tail[..keep_tail]);
    let hl = (
        u16::try_from(from).unwrap_or(u16::MAX),
        u16::try_from(to).unwrap_or(u16::MAX),
    );
    (CompactString::from(s.as_str()), hl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_needle_collapses_to_utf8_and_utf16() {
        let p = all_encodings_pattern("ab", true);
        assert_eq!(p, r"(?-u)(?:\x61\x62|\x61\x00\x62\x00)");
    }

    #[test]
    fn cyrillic_needle_has_four_forms() {
        let p = all_encodings_pattern("я", true);
        assert_eq!(p, r"(?-u)(?:\xD1\x8F|\x4F\x04|\xFF|\xEF)");
    }

    #[test]
    fn missing_character_drops_the_encoding() {
        assert_eq!(needle_in(WINDOWS_1251, "😀", true), None);
        assert!(needle_in(UTF_16LE, "😀", true).is_some());
    }

    #[test]
    fn long_line_is_cut_around_the_match() {
        let line = format!("{}игла{}", "а".repeat(500), "б".repeat(500));
        let start = line.find("игла").unwrap();
        let (s, hl) = snippet(line.as_bytes(), start..start + "игла".len(), UTF_8);
        assert_eq!(s.chars().count(), PREVIEW_CHARS);
        assert_eq!(&s[hl.0 as usize..hl.1 as usize], "игла");
    }

    #[test]
    fn line_end_and_indent_are_trimmed() {
        let (s, hl) = snippet(b"\t  key = 1\r\n", 3..6, UTF_8);
        assert_eq!(s, "key = 1");
        assert_eq!(hl, (0, 3));
    }
}
