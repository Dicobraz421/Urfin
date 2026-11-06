//! S8-05: поиск по содержимому в памяти: кодировки, регистр, бинарники, фрагменты.

use std::io::{self, Cursor, Read};
use std::sync::Arc;

use encoding_rs::{Encoding, IBM866, WINDOWS_1251};
use urfin_search::content::{is_ui_thread, mark_ui_thread};
use urfin_search::{CompiledText, ContentSearcher, EncodingMode, FileHit, TextMode, TextQuery};

fn literal(pattern: &str) -> TextQuery {
    TextQuery {
        pattern: pattern.to_owned(),
        ..TextQuery::default()
    }
}

fn run(q: &TextQuery, bytes: &[u8]) -> (bool, Vec<FileHit>) {
    let text = Arc::new(CompiledText::new(q).unwrap());
    let mut s = ContentSearcher::new(text, 1000);
    let mut out = Vec::new();
    let text_file = s.search(Cursor::new(bytes), &|| false, &mut out).unwrap();
    (text_file, out)
}

fn hits(q: &TextQuery, bytes: &[u8]) -> Vec<FileHit> {
    run(q, bytes).1
}

/// Подсвеченная часть фрагмента.
fn marked(h: &FileHit) -> &str {
    let p = h.preview.as_deref().unwrap();
    &p[h.hl.0 as usize..h.hl.1 as usize]
}

fn utf16le_bom(s: &str) -> Vec<u8> {
    let mut b = vec![0xFF, 0xFE];
    b.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
    b
}

fn single_byte(enc: &'static Encoding, s: &str) -> Vec<u8> {
    let (b, _, bad) = enc.encode(s);
    assert!(!bad);
    b.into_owned()
}

const TEXT: &str = "первая\r\nвторая строка\r\n  Привет, мир\r\nчетвёртая\r\n";

/// Один текст в четырёх кодировках режима All.
fn all_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("utf-8", TEXT.as_bytes().to_vec()),
        ("utf-16le", utf16le_bom(TEXT)),
        ("cp1251", single_byte(WINDOWS_1251, TEXT)),
        ("cp866", single_byte(IBM866, TEXT)),
    ]
}

#[test]
fn hello_is_found_in_every_encoding_and_case() {
    for (name, bytes) in all_files() {
        for needle in ["Привет", "привет", "ПРИВЕТ"] {
            let got = hits(&literal(needle), &bytes);
            assert_eq!(got.len(), 1, "{name} {needle}");
            let h = &got[0];
            assert_eq!(h.line, 3, "{name}: номер строки");
            assert_eq!(h.preview.as_deref(), Some("Привет, мир"), "{name}");
            assert_eq!(marked(h), "Привет", "{name}");
        }
    }
}

#[test]
fn case_flag_is_respected_in_every_encoding() {
    for (name, bytes) in all_files() {
        let q = TextQuery {
            case: true,
            ..literal("привет")
        };
        assert!(hits(&q, &bytes).is_empty(), "{name}");
        let q = TextQuery {
            case: true,
            ..literal("Привет")
        };
        assert_eq!(hits(&q, &bytes).len(), 1, "{name}");
    }
}

#[test]
fn line_number_survives_0a_inside_a_utf16_character() {
    // «Њ» — U+040A: байт 0A внутри символа, строка от него не рвётся.
    let bytes = utf16le_bom("Њ Њ Њ\nигла\n");
    let got = hits(&literal("игла"), &bytes);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].line, 2);
}

#[test]
fn two_matches_on_two_lines_are_two_records() {
    let src = "fn scan() {}\n\n// scan again\nother\n";
    let got = hits(&literal("scan"), src.as_bytes());
    assert_eq!(got.iter().map(|h| h.line).collect::<Vec<_>>(), [1, 3]);
    assert_eq!(marked(&got[1]), "scan");
    assert_eq!(got[1].preview.as_deref(), Some("// scan again"));
}

#[test]
fn matches_on_one_line_are_one_record() {
    let got = hits(&literal("ab"), b"ab ab ab\n");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].hl, (0, 2));
}

#[test]
fn per_file_limit_stops_reading() {
    let text = Arc::new(CompiledText::new(&literal("x")).unwrap());
    let mut s = ContentSearcher::new(text, 2);
    let mut out = Vec::new();
    s.search(Cursor::new(b"x\nx\nx\nx\n"), &|| false, &mut out)
        .unwrap();
    assert_eq!(out.len(), 2);
}

#[test]
fn auto_gives_no_false_matches_on_binaries() {
    let auto = TextQuery {
        encoding: EncodingMode::Auto,
        ..literal("needle")
    };
    let mut near = b"needle\0".to_vec();
    near.extend_from_slice(b"needle\n");
    let (text_file, got) = run(&auto, &near);
    assert!(!text_file && got.is_empty());

    // NUL далеко за первым буфером: строки до него уже прочитаны, но файл всё равно бинарный.
    let mut far = b"needle\n".to_vec();
    far.extend(b"padding\n".repeat(40_000));
    far.push(0);
    let (text_file, got) = run(&auto, &far);
    assert!(!text_file && got.is_empty());

    let on = TextQuery {
        binary: true,
        ..auto.clone()
    };
    assert!(!hits(&on, &near).is_empty(), "binary = true ищет и там");
}

#[test]
fn all_skips_binaries_too() {
    let (text_file, got) = run(&literal("Привет"), "Привет\0".as_bytes());
    assert!(!text_file && got.is_empty());
}

#[test]
fn utf16_without_bom_is_binary_unless_asked() {
    let bytes: Vec<u8> = "Привет\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert_eq!(
        hits(&literal("Привет"), &bytes),
        [] as [urfin_search::FileHit; 0]
    );
    let q = TextQuery {
        binary: true,
        ..literal("Привет")
    };
    let got = hits(&q, &bytes);
    assert_eq!(got.len(), 1);
    assert_eq!(marked(&got[0]), "Привет");
}

#[test]
fn one_letter_is_not_a_tail_of_utf8() {
    // Байт 8F — «П» в cp866 и хвост «я» в UTF-8.
    assert_eq!(
        hits(&literal("П"), "заря\n".as_bytes()),
        [] as [urfin_search::FileHit; 0]
    );
    assert_eq!(hits(&literal("П"), &[0x8F, b'\n']).len(), 1);
}

#[test]
fn ascii_needle_in_a_cyrillic_line_reads_right() {
    let line = "привет hello мир\n";
    for bytes in [single_byte(WINDOWS_1251, line), single_byte(IBM866, line)] {
        let got = hits(&literal("hello"), &bytes);
        assert_eq!(got[0].preview.as_deref(), Some("привет hello мир"));
        assert_eq!(marked(&got[0]), "hello");
    }
}

#[test]
fn whole_word_works_in_every_encoding() {
    let q = TextQuery {
        whole_word: true,
        ..literal("привет")
    };
    let word = "ну привет!\n";
    let part = "приветствие\n";
    for enc in [WINDOWS_1251, IBM866] {
        assert_eq!(hits(&q, &single_byte(enc, word)).len(), 1);
        assert_eq!(
            hits(&q, &single_byte(enc, part)),
            [] as [urfin_search::FileHit; 0]
        );
    }
    assert_eq!(hits(&q, word.as_bytes()).len(), 1);
    assert_eq!(hits(&q, part.as_bytes()), [] as [urfin_search::FileHit; 0]);
    assert_eq!(hits(&q, &utf16le_bom(word)).len(), 1);
    assert_eq!(
        hits(&q, &utf16le_bom(part)),
        [] as [urfin_search::FileHit; 0]
    );
}

#[test]
fn regex_is_unicode_aware_and_reads_bom_utf16() {
    let q = TextQuery {
        mode: TextMode::Regex,
        encoding: EncodingMode::Auto,
        ..literal(r"прив\w+")
    };
    let got = hits(&q, "x ПРИВЕТ y\n".as_bytes());
    assert_eq!(marked(&got[0]), "ПРИВЕТ");
    let got = hits(&q, &utf16le_bom("\nПривет\n"));
    assert_eq!(got[0].line, 2);
    assert_eq!(marked(&got[0]), "Привет");
}

#[test]
fn bad_regex_is_an_error() {
    let q = TextQuery {
        mode: TextMode::Regex,
        encoding: EncodingMode::Auto,
        ..literal("(")
    };
    assert!(CompiledText::new(&q).is_err());
}

#[test]
fn named_encoding_decodes_the_file() {
    let q = TextQuery {
        encoding: EncodingMode::Named(IBM866),
        ..literal("Привет")
    };
    let got = hits(&q, &single_byte(IBM866, TEXT));
    assert_eq!(got[0].line, 3);
    assert_eq!(got[0].preview.as_deref(), Some("Привет, мир"));
    assert_eq!(
        hits(&q, &single_byte(WINDOWS_1251, TEXT)),
        [] as [urfin_search::FileHit; 0]
    );
}

#[test]
fn hex_finds_bytes_in_a_binary() {
    let q = TextQuery {
        mode: TextMode::Hex,
        binary: true,
        ..literal("DE AD BE EF")
    };
    let got = hits(&q, &[0, 1, 0xDE, 0xAD, 0xBE, 0xEF, 0, 2, b'\n']);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].line, 1);
}

#[test]
fn invert_gives_one_record_for_a_file_without_the_text() {
    let q = TextQuery {
        invert: true,
        ..literal("нет")
    };
    let got = hits(&q, "тут пусто\nи тут\n".as_bytes());
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].line, got[0].preview.is_none()), (0, true));
    assert_eq!(
        hits(&q, "тут нет\nи тут нет\n".as_bytes()),
        [] as [urfin_search::FileHit; 0]
    );
}

#[test]
fn preview_keeps_the_limit_on_a_long_line() {
    let line = format!("{}игла{}\n", "а".repeat(4000), "б".repeat(4000));
    let got = hits(&literal("игла"), line.as_bytes());
    let p = got[0].preview.as_deref().unwrap();
    assert_eq!(p.chars().count(), 120);
    assert_eq!(marked(&got[0]), "игла");
}

/// Отдаёт кусками и считает чтения.
struct Slow<'a> {
    reads: &'a std::cell::Cell<u32>,
}

impl Read for Slow<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads.set(self.reads.get() + 1);
        buf.fill(b'x');
        Ok(buf.len().min(4096))
    }
}

#[test]
fn stop_is_checked_between_chunks() {
    let reads = std::cell::Cell::new(0);
    let text = Arc::new(CompiledText::new(&literal("needle")).unwrap());
    let mut s = ContentSearcher::new(text, 10);
    let mut out = Vec::new();
    let stop = || reads.get() > 8;
    let r = s.search(Slow { reads: &reads }, &stop, &mut out);
    assert!(r.is_err());
    assert!(reads.get() <= 10, "{}", reads.get());
}

#[test]
fn endless_line_stops_at_the_heap_limit() {
    let reads = std::cell::Cell::new(0);
    let text = Arc::new(CompiledText::new(&literal("needle")).unwrap());
    let mut s = ContentSearcher::new(text, 10);
    let mut out = Vec::new();
    assert!(
        s.search(Slow { reads: &reads }, &|| false, &mut out)
            .is_err()
    );
}

#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "UI"))]
fn reading_on_the_ui_thread_is_a_bug() {
    mark_ui_thread();
    assert!(is_ui_thread());
    hits(&literal("x"), b"x\n");
}
