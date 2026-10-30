//! Приёмка S7-09: чанковый поиск = поиск по целому буферу; HIR-перевод 1251/866/KOI8 =
//! поиск по декодированной строке; отмена; ошибка regex с позицией.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "тесты: доля длины"
)]

use std::sync::atomic::AtomicU64;

use encoding_rs::{Encoding, IBM866, KOI8_R, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251};
use proptest::prelude::*;
use regex_automata::meta;
use urfin_core::CancelToken;

use super::scan::{self, Counted, Scan, ScanError, SliceHay};
use super::*;

fn scan_opts<'a>(chunk: u64, cancel: &'a CancelToken, n: &'a AtomicU64) -> Scan<'a> {
    Scan {
        chunk,
        cancel,
        scanned: n,
    }
}

fn encode(s: &str, enc: &'static Encoding) -> Vec<u8> {
    if enc == UTF_16LE {
        return s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    }
    if enc == UTF_16BE {
        return s.encode_utf16().flat_map(u16::to_be_bytes).collect();
    }
    enc.encode(s).0.into_owned()
}

/// Все совпадения без перекрытий по целому буферу (одно окно).
fn whole_count(m: &Matcher, hay: &[u8]) -> Vec<u64> {
    let p = m.prepare(hay);
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(r) = p.find_at(at) {
        out.push(r.start as u64);
        at = r.end.max(r.start + 1);
    }
    out
}

const WORDS: [&str; 8] = [
    "ошибка",
    "Ошибка",
    "error",
    "ERROR",
    "мир",
    "a",
    "ёж",
    "x_1",
];

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            prop::sample::select(WORDS.to_vec()).prop_map(str::to_owned),
            prop::sample::select(vec![" ", "\n", ", ", "-", "\r\n", "日本"])
                .prop_map(str::to_owned),
        ],
        0..120,
    )
    .prop_map(|v| v.concat())
}

fn pattern_strategy() -> impl Strategy<Value = Pattern> {
    prop_oneof![
        (
            prop::sample::select(WORDS.to_vec()),
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(|(s, case, word)| Pattern::Text {
                s: s.to_owned(),
                case,
                word,
            }),
        (
            prop::sample::select(vec![
                "ош[иы]бк[аи]",
                "\\w{2,4}",
                "(?m)^мир",
                "a|ёж",
                "[а-я]+\\s",
                "error\\b",
                "\\d",
            ]),
            any::<bool>()
        )
            .prop_map(|(s, case)| Pattern::Regex {
                s: s.to_owned(),
                case
            }),
    ]
}

fn enc_strategy() -> impl Strategy<Value = &'static Encoding> {
    prop::sample::select(vec![
        UTF_8,
        WINDOWS_1251,
        IBM866,
        KOI8_R,
        UTF_16LE,
        UTF_16BE,
    ])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Вперёд, назад, с переносом и счёт по окнам любого размера = по целому буферу.
    #[test]
    fn chunked_equals_whole(
        text in text_strategy(),
        pat in pattern_strategy(),
        enc in enc_strategy(),
        chunk in 1u64..64,
        from_frac in 0.0f64..1.0,
        back: bool,
    ) {
        let hay = encode(&text, enc);
        let Ok(m) = compile(&pat, Enc::new(enc)) else { return Ok(()); };
        let unit = u64::from(Enc::new(enc).unit());
        let len = hay.len() as u64;
        let from = ((len as f64 * from_frac) as u64) / unit * unit;
        let cancel = CancelToken::new();
        let n = AtomicU64::new(0);
        let o = scan_opts(chunk * unit, &cancel, &n);
        let got = scan::find_next(&mut SliceHay(&hay), &m, from, back, true, &o).unwrap();

        let p = m.prepare(&hay);
        let to_u64 = |r: std::ops::Range<usize>| r.start as u64..r.end as u64;
        let first = |lo: u64, hi: u64| p.find_at(lo as usize).filter(|r| (r.start as u64) < hi).map(to_u64);
        let last = |lo: u64, hi: u64| p.rfind(lo as usize, hi as usize).map(to_u64);
        let want = if back {
            last(0, from).map(|r| (r, false)).or_else(|| last(from, len).map(|r| (r, true)))
        } else {
            first(from, len).map(|r| (r, false)).or_else(|| first(0, from).map(|r| (r, true)))
        };
        prop_assert_eq!(got.map(|h| (h.range, h.wrapped)), want);

        let o = scan_opts(chunk * unit, &cancel, &n);
        let c = scan::count(&mut SliceHay(&hay), &m, 10_000, &o).unwrap();
        prop_assert_eq!(c, Counted { starts: whole_count(&m, &hay), more: false });
    }

    /// HIR-перевод для 1251/866/KOI8: совпадения по байтам = совпадения по декодированной
    /// строке (символ — байт).
    #[test]
    fn hir_translate_equals_decoded(
        text in text_strategy(),
        pat in prop::sample::select(vec![
            "ош[иы]бк[аи]", "(?i)ОШИБКА", "\\w+", "[а-яё]{2}", "(?i)[А-Я]+р", ".ж", "[^a-z\\s]+",
        ]),
        enc in prop::sample::select(vec![WINDOWS_1251, IBM866, KOI8_R]),
    ) {
        let text = text.replace("日本", "");
        let hay = encode(&text, enc);
        let m = compile(&Pattern::Regex { s: pat.to_owned(), case: true }, Enc::new(enc)).unwrap();
        let got = whole_count(&m, &hay);
        let re = meta::Regex::new(pat).unwrap();
        let idx: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let want: Vec<u64> = re
            .find_iter(&text)
            .map(|mm| idx.partition_point(|&i| i < mm.start()) as u64)
            .collect();
        prop_assert_eq!(got, want);
    }
}

#[test]
fn literal_back_and_hex() {
    let hay = b"aa DE AD \xDE\xAD\xBE\xEF tail \xDE\x00\xBE\xEF";
    let m = compile(
        &Pattern::Hex(hex::parse("DE AD BE EF").unwrap()),
        Enc::utf8(),
    )
    .unwrap();
    assert_eq!(m.find(hay, 100, false), Some(109..113));
    let m = compile(
        &Pattern::Hex(hex::parse("DE ?? BE EF").unwrap()),
        Enc::utf8(),
    )
    .unwrap();
    assert_eq!(m.find(hay, 0, true), Some(19..23));
    let m = compile(
        &Pattern::Text {
            s: "a".into(),
            case: true,
            word: false,
        },
        Enc::utf8(),
    )
    .unwrap();
    assert_eq!(m.find(b"aaa", 0, true), Some(2..3));
}

#[test]
fn word_option_works_for_cyrillic_in_1251() {
    let (hay, _, _) = WINDOWS_1251.encode("суперошибка, ошибка; ошибкам");
    let m = compile(
        &Pattern::Text {
            s: "ошибка".into(),
            case: false,
            word: true,
        },
        Enc::new(WINDOWS_1251),
    )
    .unwrap();
    assert_eq!(whole_count(&m, &hay), [13]);
}

#[test]
fn regex_error_has_position() {
    let e = compile(
        &Pattern::Regex {
            s: "ош(ибка".into(),
            case: true,
        },
        Enc::utf8(),
    )
    .unwrap_err();
    assert!(matches!(e, SearchError::Regex { pos: 2, .. }), "{e:?}");
    let e = compile(
        &Pattern::Regex {
            s: "ab[".into(),
            case: true,
        },
        Enc::new(WINDOWS_1251),
    )
    .unwrap_err();
    assert!(matches!(e, SearchError::Regex { pos: 2, .. }), "{e:?}");
    // Пустое совпадение — «далее» стояло бы на месте.
    let e = compile(
        &Pattern::Regex {
            s: "a*".into(),
            case: true,
        },
        Enc::utf8(),
    )
    .unwrap_err();
    assert_eq!(e, SearchError::Empty);
}

#[test]
fn cancelled_scan_stops() {
    let hay = vec![b'x'; 1 << 20];
    let m = compile(
        &Pattern::Regex {
            s: "y+".into(),
            case: true,
        },
        Enc::utf8(),
    )
    .unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let n = AtomicU64::new(0);
    let o = scan_opts(4096, &cancel, &n);
    let r = scan::find_next(&mut SliceHay(&hay), &m, 0, false, true, &o);
    assert!(matches!(r, Err(ScanError::Cancelled)));
}

#[test]
fn count_limit_reports_more() {
    let hay = b"ab ab ab ab".to_vec();
    let m = compile(
        &Pattern::Text {
            s: "ab".into(),
            case: true,
            word: false,
        },
        Enc::utf8(),
    )
    .unwrap();
    let cancel = CancelToken::new();
    let n = AtomicU64::new(0);
    let o = scan_opts(3, &cancel, &n);
    let c = scan::count(&mut SliceHay(&hay), &m, 3, &o).unwrap();
    assert_eq!(
        c,
        Counted {
            starts: vec![0, 3, 6],
            more: true
        }
    );
}
