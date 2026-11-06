//! Приёмка S7-26: случайные правки = модель `Vec<u8>` (содержимое, суммы, `ByteView`,
//! `Hay`), в том числе UTF-16; выгрузка `Add` за порог; замеры (`#[ignore]`).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "тесты: доли и замеры, короткие имена правок"
)]

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use urfin_core::CancelToken;

use super::*;
use crate::cache::ByteView;
use crate::enc::Enc;
use crate::index::{Sep, scan};
use crate::search::scan::{Scan, SliceHay, count};
use crate::search::{Hay, Pattern, compile};
use crate::source::OwnedSource;

/// Документ над байтами в памяти (буфер правок — во временном каталоге теста).
pub(crate) fn doc_of(orig: &[u8], sep: Sep) -> TextDoc {
    let add = AddBuf::with_limit(std::env::temp_dir().join("urfin-test"), name(), MEM_MAX);
    let src = Arc::new(OwnedSource::new(orig.to_vec()));
    let index = crate::edit::lines::index_now(&*src, sep).unwrap();
    TextDoc::new(src, 0, sep, add, index)
}

fn name() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("edit-test-{}-{n}", std::process::id())
}

pub(crate) fn seps(b: &[u8], sep: Sep) -> u64 {
    let mut n = 0;
    scan(b, 0, sep, |_| n += 1);
    n
}

/// Все байты через `chunk` вперёд.
pub(crate) fn read_fwd(v: &dyn ByteView) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let c = v.chunk(at).expect("bytes in memory");
        if c.is_empty() {
            break;
        }
        out.extend_from_slice(c);
        at += c.len() as u64;
    }
    out
}

/// Все байты через `chunk_before` назад.
fn read_back(v: &dyn ByteView) -> Vec<u8> {
    let mut parts = Vec::new();
    let mut end = v.len();
    while end > 0 {
        let c = v.chunk_before(end).expect("bytes in memory");
        assert!(!c.is_empty(), "chunk_before is empty at {end}");
        parts.push(c.to_vec());
        end -= c.len() as u64;
    }
    parts.reverse();
    parts.concat()
}

/// Известное число переводов строк у каждого куска = счёт по модели.
pub(crate) fn pieces_lf(doc: &TextDoc, model: &[u8]) {
    for (ps, p) in doc.tree() {
        if let Lf::Known(n) = p.lf {
            let b = &model[ps as usize..(ps + p.len) as usize];
            let mut k = 0;
            scan(b, ps, doc.sep(), |_| k += 1);
            assert_eq!(u64::from(n), k, "lf of {p:?} at {ps}");
        }
    }
}

/// Проверки документа против модели.
pub(crate) fn same(doc: &TextDoc, model: &[u8]) {
    doc.tree().check();
    let s = doc.tree().sums();
    assert_eq!(s.bytes, model.len() as u64, "bytes");
    assert_eq!(s.unknown, 0, "all LF known in memory");
    assert_eq!(s.lf, seps(model, doc.sep()), "lf");
    pieces_lf(doc, model);
    let v = doc.view();
    assert_eq!(read_fwd(&v), model, "chunk");
    assert_eq!(read_back(&v), model, "chunk_before");
    let snap = doc.snap();
    let mut buf = vec![0; model.len()];
    assert_eq!(snap.read_at(0, &mut buf).unwrap(), model.len());
    assert_eq!(buf, model, "snap");
}

#[derive(Clone, Debug)]
enum Edit {
    Insert(f64, Vec<u8>),
    Remove(f64, usize),
    Replace(f64, usize, Vec<u8>),
}

/// Кодировка теста: разделитель и единица.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Lf,
    CrLf,
    Cr,
    Le,
    Be,
}

impl Kind {
    pub(crate) fn sep(self) -> Sep {
        match self {
            Kind::Lf | Kind::CrLf => Sep::Byte(b'\n'),
            Kind::Cr => Sep::Byte(b'\r'),
            Kind::Le => Sep::Le(b'\n'),
            Kind::Be => Sep::Be(b'\n'),
        }
    }

    pub(crate) fn unit(self) -> usize {
        match self {
            Kind::Le | Kind::Be => 2,
            _ => 1,
        }
    }

    /// Текст из символов `chars` в кодировке (концы строк — по виду).
    pub(crate) fn encode(self, chars: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &c in chars {
            let s: &[u8] = match (c, self) {
                (b'\n', Kind::CrLf) => b"\r\n",
                (b'\n', Kind::Cr) => b"\r",
                (b'\n', _) => b"\n",
                _ => std::slice::from_ref(&c),
            };
            for &b in s {
                match self {
                    Kind::Le => out.extend_from_slice(&[b, 0]),
                    Kind::Be => out.extend_from_slice(&[0, b]),
                    _ => out.push(b),
                }
            }
        }
        out
    }
}

pub(crate) fn kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        Just(Kind::Lf),
        Just(Kind::CrLf),
        Just(Kind::Cr),
        Just(Kind::Le),
        Just(Kind::Be),
    ]
}

pub(crate) fn text() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(prop_oneof![Just(b'a'), Just(b'b'), Just(b'\n')], 0..40)
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        (0.0..=1.0f64, text()).prop_map(|(a, t)| Edit::Insert(a, t)),
        (0.0..=1.0f64, 0..60usize).prop_map(|(a, n)| Edit::Remove(a, n)),
        (0.0..=1.0f64, 0..20usize, text()).prop_map(|(a, n, t)| Edit::Replace(a, n, t)),
    ]
}

/// Позиция на границе единицы.
pub(crate) fn pos(frac: f64, len: usize, unit: usize) -> usize {
    let units = len / unit;
    ((frac * units as f64) as usize).min(units) * unit
}

/// Применяет правку к документу и модели.
fn apply(doc: &mut TextDoc, model: &mut Vec<u8>, e: &Edit, k: Kind) {
    let u = k.unit();
    match e {
        Edit::Insert(a, t) => {
            let at = pos(*a, model.len(), u);
            let b = k.encode(t);
            let s = doc.insert(at as u64, &b).unwrap();
            assert_eq!(bytes_of(&s.inserted), b.len() as u64);
            model.splice(at..at, b);
        }
        Edit::Remove(a, n) => {
            let at = pos(*a, model.len(), u);
            let end = (at + n * u).min(model.len());
            let s = doc.remove(at as u64..end as u64);
            assert_eq!(bytes_of(&s.removed), (end - at) as u64);
            model.drain(at..end);
        }
        Edit::Replace(a, n, t) => {
            let at = pos(*a, model.len(), u);
            let end = (at + n * u).min(model.len());
            let b = k.encode(t);
            doc.replace(at as u64..end as u64, &b).unwrap();
            model.splice(at..end, b);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn edits_match_model(
        k in kind(),
        orig in proptest::collection::vec(prop_oneof![Just(b'x'), Just(b'y'), Just(b'\n')], 0..400),
        edits in proptest::collection::vec(edit(), 1..120),
    ) {
        let orig = k.encode(&orig);
        let mut doc = doc_of(&orig, k.sep());
        let mut model = orig.clone();
        same(&doc, &model);
        let mut snaps = vec![(doc.snap(), model.clone())];
        for (i, e) in edits.iter().enumerate() {
            apply(&mut doc, &mut model, e, k);
            same(&doc, &model);
            if i % 8 == 0 {
                snaps.push((doc.snap(), model.clone()));
            }
        }
        // Снимки не меняются от правок после них.
        for (snap, m) in &snaps {
            snap.tree().check();
            let mut buf = vec![0; m.len()];
            prop_assert_eq!(snap.read_at(0, &mut buf).unwrap(), m.len());
            prop_assert_eq!(&buf, m);
        }
        // Окна `Hay` — те же байты, что модель.
        let snap = doc.snap();
        let mut hay = TextHay::new(&snap);
        let len = model.len();
        for (a, b) in [(0, len), (len / 3, len / 2), (len / 2, len), (len, len)] {
            prop_assert_eq!(hay.window(a as u64..b as u64).unwrap(), &model[a..b]);
        }
    }

    /// Много мелких правок: дерево в несколько уровней.
    #[test]
    fn deep_tree_matches_model(
        seed in proptest::collection::vec((0.0..=1.0f64, 0..3u8), 600..1500),
    ) {
        let orig: Vec<u8> = (0..4000u32).map(|i| if i % 37 == 0 { b'\n' } else { b'a' + (i % 26) as u8 }).collect();
        let mut doc = doc_of(&orig, Sep::Byte(b'\n'));
        let mut model = orig.clone();
        for (i, (a, op)) in seed.iter().enumerate() {
            let at = pos(*a, model.len(), 1);
            match op {
                0 | 1 => {
                    let b = [b'0' + (i % 10) as u8, if i % 7 == 0 { b'\n' } else { b'-' }];
                    doc.insert(at as u64, &b).unwrap();
                    model.splice(at..at, b);
                }
                _ => {
                    let end = (at + 3).min(model.len());
                    doc.remove(at as u64..end as u64);
                    model.drain(at..end);
                }
            }
        }
        prop_assert!(doc.tree().depth() >= 2);
        same(&doc, &model);
        // Удалить всё — пустой документ.
        doc.remove(0..model.len() as u64);
        same(&doc, &[]);
    }
}

#[test]
fn typing_appends_last_add_piece() {
    let mut doc = doc_of(b"hello\nworld\n", Sep::Byte(b'\n'));
    let mut at = 6;
    for c in b"brave new " {
        doc.insert(at, &[*c]).unwrap();
        at += 1;
    }
    // Original | Add | Original.
    assert_eq!(doc.tree().sums().pieces, 3);
    same(&doc, b"hello\nbrave new world\n");
    // Стёр набранное — снова один кусок исходника.
    for _ in 0..10 {
        at -= 1;
        doc.remove(at..at + 1);
    }
    assert_eq!(doc.tree().sums().pieces, 1);
    same(&doc, b"hello\nworld\n");
}

#[test]
fn enter_at_line_end_is_a_new_add_line() {
    let mut doc = doc_of(b"abc\ndef\n", Sep::Byte(b'\n'));
    let s = doc.insert(3, b"\nxyz").unwrap();
    // Сдвинуто за "\n" исходника: строка 1 целиком из `Add`.
    assert_eq!(s.at, 4);
    same(&doc, b"abc\nxyz\ndef\n");
    let p: Vec<Piece> = doc.tree().iter().map(|(_, p)| *p).collect();
    assert_eq!(p[0].src, Src::Original);
    assert_eq!(p[0].len, 4);
    assert_eq!(p[1].src, Src::Add);
    // CRLF.
    let mut doc = doc_of(b"ab\r\ncd", Sep::Byte(b'\n'));
    let s = doc.insert(2, b"\r\n").unwrap();
    assert_eq!(s.at, 4);
    same(&doc, b"ab\r\n\r\ncd");
}

#[test]
fn add_pieces_do_not_cross_blocks() {
    let mut doc = doc_of(b"x\ny", Sep::Byte(b'\n'));
    let big: Vec<u8> = (0..200_000u32)
        .map(|i| if i % 50 == 0 { b'\n' } else { b'q' })
        .collect();
    doc.insert(2, &big).unwrap();
    let mut model = b"x\ny".to_vec();
    model.splice(2..2, big.iter().copied());
    for (_, p) in doc.tree() {
        if p.src == Src::Add {
            assert_eq!(p.start / BLOCK, (p.end() - 1) / BLOCK);
        }
    }
    same(&doc, &model);
    doc.remove(70_000..140_000);
    model.drain(70_000..140_000);
    same(&doc, &model);
}

#[test]
fn search_over_hay_matches_slice() {
    let mut doc = doc_of(b"one two three\nfour two\n", Sep::Byte(b'\n'));
    doc.insert(4, b"two ").unwrap();
    doc.insert(0, b"two\n").unwrap();
    doc.remove(10..12);
    let snap = doc.snap();
    let model = {
        let mut b = vec![0; snap.len() as usize];
        snap.read_at(0, &mut b).unwrap();
        b
    };
    let m = compile(
        &Pattern::Text {
            s: "two".into(),
            case: true,
            word: false,
        },
        Enc::utf8(),
    )
    .unwrap();
    let cancel = CancelToken::new();
    let n = AtomicU64::new(0);
    let o = Scan {
        chunk: 5,
        cancel: &cancel,
        scanned: &n,
    };
    let a = count(&mut TextHay::new(&snap), &m, 100, &o).unwrap();
    let b = count(&mut SliceHay(&model), &m, 100, &o).unwrap();
    assert_eq!(a, b);
    assert!(a.starts.len() >= 3);
}

#[test]
fn snap_is_send_sync_and_cheap() {
    fn check<T: Send + Sync + Clone + 'static>() {}
    check::<TextSnap>();
    assert!(size_of::<Piece>() <= 32);
    let doc = doc_of(b"abc", Sep::Byte(b'\n'));
    let a = doc.snap();
    let b = a.clone();
    assert!(a.tree().same(b.tree()));
}

/// Выгрузка `Add` за порог: память в пределах, чтение выгруженного через кэш и снимок
/// верно.
#[test]
fn add_spills_over_limit() {
    let dir = tempfile::tempdir().unwrap();
    let limit = 4 * BLOCK;
    let add = AddBuf::with_limit(dir.path().to_path_buf(), "edit-spill".into(), limit);
    let orig = b"head\ntail\n".to_vec();
    let src = Arc::new(OwnedSource::new(orig.clone()));
    let index = crate::edit::lines::index_now(&*src, Sep::Byte(b'\n')).unwrap();
    let mut doc = TextDoc::new(src, 0, Sep::Byte(b'\n'), add, index);
    let mut model = orig;
    let mut seed = 7u32;
    for i in 0..24u32 {
        let n = 30_000 + (i as usize * 7919) % 40_000;
        let b: Vec<u8> = (0..n)
            .map(|j| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                if j % 61 == 0 {
                    b'\n'
                } else {
                    b'a' + (seed >> 28) as u8
                }
            })
            .collect();
        let at = (seed as usize) % (model.len() + 1);
        doc.insert(at as u64, &b).unwrap();
        model.splice(at..at, b);
        assert!(
            doc.add().mem() <= limit,
            "add in memory {}",
            doc.add().mem()
        );
    }
    assert!(doc.add().spilled() > 0);
    // Снимок читает выгруженное из файла.
    let snap = doc.snap();
    let mut buf = vec![0; model.len()];
    assert_eq!(snap.read_at(0, &mut buf).unwrap(), model.len());
    assert_eq!(buf, model);
    // UI: выгруженного нет в кэше — `Missing`, после подкачки — те же байты.
    let first_spilled = doc
        .tree()
        .iter()
        .find(|(_, p)| p.src == Src::Add && p.start < doc.add().spilled())
        .map(|(s, _)| s)
        .unwrap();
    assert!(doc.view().chunk(first_spilled).is_err());
    doc.load_now(0..u64::MAX);
    assert_eq!(read_fwd(&doc.view()), model);
    // Известные LF кусков верны (разрезанные без страниц — `Unknown`).
    pieces_lf(&doc, &model);
}

/// Документ с `n` кусками: случайные вставки в исходник 64 МБ.
pub(crate) fn million(pieces: u64) -> (TextDoc, u64) {
    let line = b"0123456789 abcdefghij klmnopqrst uvwxyz ABCDEFGHIJ KLMNOPQRST\n";
    let orig: Vec<u8> = line.iter().copied().cycle().take(64 << 20).collect();
    let mut doc = doc_of(&orig, Sep::Byte(b'\n'));
    let mut seed = 1u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    while doc.tree().sums().pieces < pieces {
        let at = rnd() % (doc.len() + 1);
        doc.insert(at, b"ab").unwrap();
    }
    (doc, rnd())
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() - 1) as f64 * p) as usize]
}

/// Замер: правка в случайном месте при 1 млн кусков ≤ 50 мкс, клон снимка O(1).
/// `cargo nextest run --release --run-ignored only -p urfin-view bench_edit_1m --no-capture`
#[test]
#[ignore = "замер, release"]
fn bench_edit_1m() {
    let t = Instant::now();
    let (mut doc, mut seed) = million(1_000_000);
    println!(
        "build: {} pieces, depth {}, {:?}",
        doc.tree().sums().pieces,
        doc.tree().depth(),
        t.elapsed()
    );
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut ins, mut del) = (Vec::new(), Vec::new());
    for _ in 0..20_000 {
        let at = rnd() % (doc.len() + 1);
        let t = Instant::now();
        doc.insert(at, b"xy").unwrap();
        ins.push(t.elapsed());
        let at = rnd() % (doc.len() - 16);
        let t = Instant::now();
        doc.remove(at..at + 1 + rnd() % 16);
        del.push(t.elapsed());
    }
    let snap = doc.snap();
    let t = Instant::now();
    for _ in 0..100_000 {
        std::hint::black_box(snap.clone());
    }
    let clone = t.elapsed() / 100_000;
    let t = Instant::now();
    for _ in 0..100_000 {
        std::hint::black_box(doc.snap());
    }
    let take = t.elapsed() / 100_000;
    let small = doc_of(b"abc", Sep::Byte(b'\n')).snap();
    let t = Instant::now();
    for _ in 0..100_000 {
        std::hint::black_box(small.clone());
    }
    let clone_small = t.elapsed() / 100_000;
    let ins_avg = ins.iter().sum::<Duration>() / ins.len() as u32;
    let del_avg = del.iter().sum::<Duration>() / del.len() as u32;
    println!(
        "insert: avg {ins_avg:?} p50 {:?} p99 {:?}; remove: avg {del_avg:?} p50 {:?} p99 {:?}",
        pct(&mut ins, 0.5),
        pct(&mut ins, 0.99),
        pct(&mut del, 0.5),
        pct(&mut del, 0.99)
    );
    println!("snap clone: {clone:?} (1 piece: {clone_small:?}); doc.snap(): {take:?}");
    assert!(pct(&mut ins, 0.5) <= Duration::from_micros(50));
    assert!(pct(&mut del, 0.5) <= Duration::from_micros(50));
}

/// Проход вперёд с потоком чтения: окна с перекрытием, смена шага, прыжок назад,
/// перекрытие больше запаса — те же байты, что в документе.
#[test]
fn hay_forward_reads_ahead() {
    let orig: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let mut doc = doc_of(&orig, Sep::Byte(b'\n'));
    let mut model = orig;
    for i in 1..40u64 {
        let at = i * 70_001;
        doc.insert(at, b"edit").unwrap();
        let at = usize::try_from(at).unwrap();
        model.splice(at..at, *b"edit");
    }
    let snap = doc.snap();
    let mut hay = TextHay::new(&snap);
    let len = model.len();
    let mut check = |a: usize, b: usize| {
        let got = hay.window(a as u64..b as u64).unwrap();
        assert!(got == &model[a..b.min(len)], "окно {a}..{b}");
    };
    let (step, over) = (100_000, 4_000);
    let mut a = 0;
    while a < len {
        check(a, a + step);
        a += step - over;
    }
    check(10, 200);
    check(100, 300);
    check(250, 90_000);
    check(1_000, 500_000);
    check(450_000, 700_000);
    check(600_000, 2_000_000);
    check(5, 50);
}

/// `no_sep` не обещает лишнего: «нет» — только там, где разделителя нет в байтах.
#[test]
fn no_sep_matches_bytes() {
    let orig: Vec<u8> = (0..20_000u32)
        .map(|i| if i % 7_001 == 7_000 { b'\n' } else { b'a' })
        .collect();
    let mut doc = doc_of(&orig, Sep::Byte(b'\n'));
    let mut model = orig;
    for (i, (at, ins)) in [(5_000, &b"x"[..]), (9_000, b"y\nz"), (15_000, b"w")]
        .into_iter()
        .enumerate()
    {
        doc.insert(at, ins).unwrap();
        model.splice(at as usize..at as usize, ins.iter().copied());
        let v = doc.view();
        let sep = Sep::Byte(b'\n');
        let mut said = 0;
        for a in (0..model.len()).step_by(977) {
            for n in [1, 500, 3_000, 9_000] {
                let b = (a + n).min(model.len());
                let r = a as u64..b as u64;
                if v.no_sep(r.clone(), sep) {
                    said += 1;
                    assert!(!model[a..b].contains(&b'\n'), "шаг {i}, {r:?}");
                }
                assert!(!v.no_sep(r, Sep::Byte(b'\r')), "чужой разделитель");
            }
        }
        assert!(said > 0, "хоть где-то известно");
    }
}
