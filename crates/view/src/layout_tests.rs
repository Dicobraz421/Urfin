#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    reason = "тесты: короткие имена, доли длины, таблицы случаев"
)]

use std::fmt::Write;

use encoding_rs::{UTF_16LE, WINDOWS_1251};
use proptest::prelude::*;
use unicode_width::UnicodeWidthStr;

use super::*;

const PIECES: [&str; 16] = [
    "a",
    "b",
    "c",
    "word",
    " ",
    "  ",
    "\t",
    "é",
    "Ж",
    "中",
    "😀",
    "\u{1}",
    "aaaaaaaaaaaaaaaa",
    "\n",
    "\r\n",
    "\r",
];

fn spec(wrap: Option<WrapKind>, width: u32, eol: Eol, enc: Enc) -> LayoutSpec {
    LayoutSpec {
        mode: Mode::Text,
        wrap,
        width_cols: width,
        tab: 4,
        max_line: 64,
        hex_cols: 8,
        enc,
        eol,
    }
}

fn bytes_of(text: &str, utf16: bool, bad: bool) -> Vec<u8> {
    if utf16 {
        return text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    }
    let mut b = text.as_bytes().to_vec();
    if bad && b.len() > 3 {
        let mid = b.len() / 2;
        b[mid] = 0xFF;
    }
    b
}

/// Все начала визуальных строк прямым проходом.
fn all_rows(d: &[u8], s: &LayoutSpec) -> Vec<VRow> {
    let mut v = Vec::new();
    let mut pos = (0u64, 0u32);
    loop {
        let rows = rows_down(d, pos, 50, s).unwrap();
        let Some(last) = rows.last().copied() else {
            break;
        };
        v.extend(rows.iter().copied());
        let more = rows_down(d, last.pos(), 2, s).unwrap();
        match more.get(1) {
            Some(r) => pos = r.pos(),
            None => break,
        }
    }
    v
}

fn arb_case() -> impl Strategy<Value = (Vec<u8>, LayoutSpec)> {
    (
        proptest::collection::vec(0..PIECES.len(), 0..300),
        prop_oneof![
            Just(None),
            Just(Some(WrapKind::Chars)),
            Just(Some(WrapKind::Words))
        ],
        3u32..24,
        prop_oneof![
            Just(Eol::Lf),
            Just(Eol::CrLf),
            Just(Eol::Cr),
            Just(Eol::Mixed)
        ],
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(idx, wrap, width, eol, utf16, bad)| {
            let text: String = idx.iter().map(|&i| PIECES[i]).collect();
            let enc = if utf16 {
                Enc::new(UTF_16LE)
            } else {
                Enc::utf8()
            };
            (bytes_of(&text, utf16, bad), spec(wrap, width, eol, enc))
        })
}

/// Строка кусков с запятыми (`utf8`) или в другой кодировке: без разделителей.
fn long_row(parts: &[usize], enc: Enc) -> Vec<u8> {
    let text: String = parts
        .iter()
        .map(|&i| PIECES[i % 13])
        .chain(std::iter::once("z"))
        .collect();
    if enc.is_utf16() {
        return bytes_of(&text, true, false);
    }
    if enc.is_utf8() {
        return bytes_of(&text, false, parts.len().is_multiple_of(3));
    }
    text.chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Окно чтения и пропуск колонок дают то же, что проход по всем байтам ряда с начала.
    #[test]
    fn row_window_is_whole_row(
        parts in prop::collection::vec(0usize..13, 0..400),
        h in 0u32..600,
        width in 1u32..120,
        which in 0usize..3,
        combining in 0usize..2000,
    ) {
        let enc = [Enc::utf8(), Enc::new(UTF_16LE), Enc::new(WINDOWS_1251)][which];
        let mut d = long_row(&parts, enc);
        // Длинная цепочка нулевой ширины: окну не хватает запаса.
        if enc.is_utf8() {
            let k = combining.min(d.len());
            let tail: Vec<u8> = "\u{301}".repeat(combining).into_bytes();
            let at = (0..=k).rev().find(|&i| is_utf8_boundary(*d.get(i).unwrap_or(&b'a')));
            d.splice(at.unwrap_or(0)..at.unwrap_or(0), tail);
        }
        let mut s = spec(None, width, Eol::Lf, enc);
        s.max_line = 1 << 20;
        let r = VRow {
            start: 0,
            end: d.len() as u64,
            next: d.len() as u64,
            cont: false,
            cut: false,
            seg: 0,
            sub: 0,
        };
        let mut got = RowText::default();
        render_row(&d[..], &r, h, &s, &mut got).unwrap();
        let mut want = RowText {
            unit: enc.unit(),
            ..RowText::default()
        };
        let from = after_bom(&d, 0, &s);
        fill_from(&d, &r, (from, 0), h, h + width, &s, &mut want);
        prop_assert_eq!(&got, &want);
        // Колонка конца — как сумма ширин ячеек.
        let mut col = 0u32;
        cells(&d, 0, &s, |c| {
            col += cell_width(&c, col, &s);
            true
        });
        prop_assert_eq!(col_of(&d[..], &r, r.end, &s).unwrap(), col);
    }

    /// Память переноса: тот же сегмент при другой ширине и другие байты по тому же месту
    /// не берут чужой итог.
    #[test]
    fn wrap_memo_follows_content(
        parts in prop::collection::vec(0usize..13, 1..120),
        w0 in 2u32..40,
        w1 in 2u32..40,
    ) {
        let d = long_row(&parts, Enc::utf8());
        let s0 = spec(Some(WrapKind::Words), w0, Eol::Lf, Enc::utf8());
        let s1 = LayoutSpec { width_cols: w1, ..s0 };
        for s in [s0, s1, s0] {
            let cached = rows_down(&d[..], (0, 0), 1 << 16, &s).unwrap();
            let sg = segment(&d[..], 0, &s).unwrap();
            let kind = s.wrap().unwrap();
            let mut b = Vec::new();
            let bytes = row_bytes(&d[..], sg.start..sg.end, &mut b).unwrap();
            let fresh = wrap_bytes(bytes, &sg, &s, kind);
            let got: Vec<_> = cached.iter().filter(|r| r.seg == 0).map(|r| r.start..r.end).collect();
            prop_assert_eq!(got, fresh);
        }
        let mut other = d.clone();
        for x in &mut other {
            if *x == b'a' {
                *x = b' ';
            }
        }
        let a = rows_down(&other[..], (0, 0), 1 << 16, &s0).unwrap();
        let sg = segment(&other[..], 0, &s0).unwrap();
        let mut b = Vec::new();
        let fresh = wrap_bytes(row_bytes(&other[..], sg.start..sg.end, &mut b).unwrap(), &sg, &s0, WrapKind::Words);
        let got: Vec<_> = a.iter().filter(|r| r.seg == 0).map(|r| r.start..r.end).collect();
        prop_assert_eq!(got, fresh);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// Границы слов ASCII — как у `unicode_segmentation`.
    #[test]
    fn ascii_words_match_uax29(
        text in proptest::string::string_regex("[aZ09_:.',; \"\t\r\n\x0b\x0c()/-]{1,48}").unwrap(),
    ) {
        let mut cs = vec![WrapCell { off: 0, width: 1, word: false }; text.len()];
        mark_words(&text, &mut cs);
        let got: Vec<usize> = (0..cs.len()).filter(|&i| cs[i].word).collect();
        let want: Vec<usize> = text.split_word_bound_indices().map(|(i, _)| i).collect();
        prop_assert_eq!(got, want, "{:?}", text);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(160))]

    /// Приёмка S7-06: обратимость вверх/вниз; строки смежны; `line_start_before` = начало
    /// строки прямого прохода; каждое начало логической строки — начало сегмента.
    #[test]
    fn up_down_reversible((d, s) in arb_case(), at in 0.0f64..1.0) {
        let rows = all_rows(&d, &s);
        if d.is_empty() {
            prop_assert!(rows.is_empty());
            return Ok(());
        }
        prop_assert_eq!(rows[0].pos(), (0, 0));
        for w in rows.windows(2) {
            prop_assert_eq!(w[0].next, w[1].start, "смежность {:?}", w);
            prop_assert_eq!(row_up(&d[..], w[1].pos(), &s).unwrap(), w[0].pos(), "вверх от {:?}", w[1]);
        }
        prop_assert_eq!(rows.last().unwrap().next, d.len() as u64);
        prop_assert_eq!(row_up(&d[..], (0, 0), &s).unwrap(), (0, 0));

        // Начала сегментов: прямой проход против поиска назад.
        let segs: Vec<u64> = rows.iter().filter(|r| r.sub == 0).map(|r| r.seg).collect();
        let off = ((d.len() - 1) as f64 * at) as u64;
        let want = segs[segs.partition_point(|&x| x <= off) - 1];
        prop_assert_eq!(line_start_before(&d[..], off, &s).unwrap(), want, "off {}", off);

        // Каждое начало логической строки — начало сегмента; прочие — разрезы длинных строк.
        let sep = s.sep();
        let mut starts = vec![0u64];
        crate::index::scan(&d, 0, sep, |x| if x < d.len() as u64 { starts.push(x) });
        for x in &starts {
            prop_assert!(segs.binary_search(x).is_ok(), "строка {} не сегмент", x);
        }
        for r in rows.iter().filter(|r| r.sub == 0 && starts.binary_search(&r.seg).is_err()) {
            let line = starts[starts.partition_point(|&x| x <= r.seg) - 1];
            prop_assert!(r.cont, "разрез без cont {:?}", r);
            prop_assert!(r.seg % s.ml() <= 3 && r.seg - line >= 1, "разрез не на кратном {:?}", r);
        }

        // Рендер: ширина подстроки переноса не больше окна.
        let mut out = RowText::default();
        for r in &rows {
            render_row(&d[..], r, 0, &s, &mut out).unwrap();
            if s.wrap.is_some() {
                prop_assert!(out.text.width() as u32 <= s.width_cols.max(2), "{:?} {:?}", r, out.text);
            }
            if !out.text.is_empty() {
                let b0 = out.byte_at(0).unwrap();
                prop_assert!(b0 >= r.start && b0 <= r.end, "{:?} → {}", r, b0);
            }
        }
    }
}

/// insta-таблица строк: Text, перенос по символам и словам, Hex, UTF-16, 1251.
#[test]
fn rows_table_snapshot() {
    let text = "first line\twith tab\r\nвторая строка — кириллица\r\n\
                ctl:\u{1}\u{7f} wide:中文 emoji:😀\r\nmixed\rcr\nend";
    let mut out = String::new();
    let cases: [(&str, LayoutSpec, Vec<u8>); 7] = [
        (
            "text",
            spec(None, 30, Eol::CrLf, Enc::utf8()),
            text.as_bytes().to_vec(),
        ),
        (
            "text-mixed-hscroll",
            spec(None, 30, Eol::Mixed, Enc::utf8()),
            text.as_bytes().to_vec(),
        ),
        (
            "wrap-chars",
            spec(Some(WrapKind::Chars), 12, Eol::CrLf, Enc::utf8()),
            text.as_bytes().to_vec(),
        ),
        (
            "wrap-words",
            spec(Some(WrapKind::Words), 12, Eol::CrLf, Enc::utf8()),
            text.as_bytes().to_vec(),
        ),
        (
            "utf16le-bom",
            spec(
                None,
                30,
                Eol::CrLf,
                Enc {
                    bom_len: 2,
                    ..Enc::new(UTF_16LE)
                },
            ),
            [0xFF, 0xFE]
                .into_iter()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect(),
        ),
        (
            "utf8-bad",
            spec(None, 30, Eol::Lf, Enc::utf8()),
            b"ok \xFF\xFE bad \xD0\n".to_vec(),
        ),
        (
            "cp1251-ctl",
            spec(None, 30, Eol::CrLf, Enc::new(WINDOWS_1251)),
            {
                let (b, ..) = WINDOWS_1251.encode("строка\r\nбайт 98:\u{0}");
                let mut b = b.into_owned();
                b.push(0x98);
                b
            },
        ),
    ];
    for (name, s, d) in cases {
        let _ = writeln!(out, "== {name}");
        let h = if name.contains("hscroll") { 3 } else { 0 };
        let mut rt = RowText::default();
        for r in rows_down(&d[..], (0, 0), 40, &s).unwrap() {
            render_row(&d[..], &r, h, &s, &mut rt).unwrap();
            let _ = writeln!(
                out,
                "{:>4}..{:<4} {}{} |{}|{}{}",
                r.start,
                r.end,
                if r.cont { '+' } else { ' ' },
                r.sub,
                rt.text,
                if rt.cut { " ↩" } else { "" },
                if rt.bad.is_empty() {
                    String::new()
                } else {
                    format!(" bad{:?}", rt.bad)
                },
            );
        }
    }
    let hex = spec(None, 80, Eol::CrLf, Enc::utf8());
    let hex = LayoutSpec {
        mode: Mode::Hex,
        hex_cols: 16,
        ..hex
    };
    let _ = writeln!(out, "== hex");
    let d = text.as_bytes();
    let mut rt = RowText::default();
    for r in rows_down(d, (0, 0), 10, &hex).unwrap() {
        render_row(d, &r, 0, &hex, &mut rt).unwrap();
        let _ = writeln!(out, "|{}|", rt.text);
    }
    insta::assert_snapshot!(out);
}

/// Длинная строка режется на сегменты по `max_line` с маркером `↩`.
#[test]
fn long_line_segments() {
    let d = [b'x'; 200];
    let s = spec(None, 300, Eol::Lf, Enc::utf8());
    let rows = rows_down(&d[..], (0, 0), 10, &s).unwrap();
    let ends: Vec<_> = rows
        .iter()
        .map(|r| (r.start, r.end, r.cut, r.cont))
        .collect();
    assert_eq!(
        ends,
        [
            (0, 64, true, false),
            (64, 128, true, true),
            (128, 192, true, true),
            (192, 200, false, true)
        ]
    );
    assert_eq!(line_start_before(&d[..], 150, &s).unwrap(), 128);
    // UTF-8: разрез не рвёт символ.
    let d = "ж".repeat(100);
    let rows = rows_down(d.as_bytes(), (0, 0), 10, &s).unwrap();
    assert!(rows.iter().all(|r| r.start % 2 == 0), "{rows:?}");
}

/// Приёмка S7-06: hex точен для смещений > 4 ГБ.
#[test]
fn hex_offsets_beyond_4gib() {
    struct Big;
    impl ByteView for Big {
        fn len(&self) -> u64 {
            5 << 30
        }
        fn chunk(&self, off: u64) -> Result<&[u8], Missing> {
            static P: [u8; 256] = {
                let mut p = [0u8; 256];
                let mut i = 0;
                while i < 256 {
                    p[i] = i as u8;
                    i += 1;
                }
                p
            };
            Ok(&P[(off % 256) as usize..])
        }
        fn chunk_before(&self, end: u64) -> Result<&[u8], Missing> {
            let k = (end % 256) as usize;
            Ok(&self.chunk(end - k as u64)?[..k])
        }
    }
    let s = LayoutSpec {
        mode: Mode::Hex,
        hex_cols: 16,
        enc: Enc::new(WINDOWS_1251),
        ..LayoutSpec::default()
    };
    let at = 0x1_2345_6780;
    let rows = rows_down(&Big, (at, 0), 2, &s).unwrap();
    assert_eq!(rows[0].start, at);
    assert_eq!(rows[1].start, at + 16);
    let mut out = RowText::default();
    render_row(&Big, &rows[0], 0, &s, &mut out).unwrap();
    assert!(out.text.starts_with("000123456780  80 81"), "{}", out.text);
    let g = hex_geom(16, Big.len());
    assert_eq!(g.off_w, 12);
    assert_eq!(
        out.text.chars().nth(g.text_at as usize),
        Some('Ђ'),
        "{}",
        out.text
    );
    assert_eq!(out.byte_at(g.text_at + 3), Some(at + 3));
    assert_eq!(row_up(&Big, (at + 5, 0), &s).unwrap(), (at - 16, 0));
    assert_eq!(line_start_before(&Big, at + 5, &s).unwrap(), at);
    assert_eq!(hex_geom(16, 100).off_w, 8);
}

#[test]
fn hscroll_window_and_wide_edges() {
    let d = "ab中cd\tx".as_bytes();
    let s = spec(None, 3, Eol::Lf, Enc::utf8());
    let r = rows_down(d, (0, 0), 1, &s).unwrap()[0];
    let mut out = RowText::default();
    render_row(d, &r, 0, &s, &mut out).unwrap();
    assert_eq!(out.text, "ab ", "широкий на краю — пробел");
    render_row(d, &r, 2, &s, &mut out).unwrap();
    assert_eq!(out.text, "中c");
    assert_eq!(out.byte_at(0), Some(2));
    assert_eq!(out.byte_at(1), Some(5));
    render_row(d, &r, 3, &s, &mut out).unwrap();
    assert_eq!(out.text, " cd");
    render_row(d, &r, 6, &s, &mut out).unwrap();
    assert_eq!(out.text, "  x", "таб до колонки 8");
}

#[test]
fn hex_cols_by_width() {
    assert_eq!(hex_cols_auto(150), 32);
    assert_eq!(hex_cols_auto(149), 16);
    assert_eq!(hex_cols_auto(76), 16);
    assert_eq!(hex_cols_auto(75), 8);
}
