//! Приёмка S7-27 (строки): `line_of`/`offset_of_line` = наивный счёт по модели на
//! случайных правках (LF, CRLF, CR, UTF-16), до окончания индекса — `Approx`, после
//! патча — `Exact`; большой файл (V1) без чтения целиком.

#![allow(
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    reason = "тесты: смещения в памяти, короткие имена"
)]

use std::io;
use std::sync::Arc;
use std::time::Instant;

use proptest::prelude::*;
use urfin_core::CancelToken;

use super::*;
use crate::edit::piece::tests::{Kind, doc_of, kind, pos, seps, text};
use crate::edit::piece::{AddBuf, MEM_MAX};
use crate::source::{OwnedSource, SourceKind};

/// Номер строки `off` по модели.
fn line_at(model: &[u8], off: usize, sep: Sep) -> u64 {
    seps(&model[..off], sep)
}

/// Начала строк по модели (строка 0 — с 0).
fn starts(model: &[u8], sep: Sep) -> Vec<u64> {
    let mut v = vec![0];
    scan(model, 0, sep, |s| v.push(s));
    v
}

/// Всё точно и совпадает с моделью.
fn exact_lines(doc: &TextDoc, model: &[u8]) {
    let sep = doc.sep();
    let u = sep.unit() as usize;
    for off in (0..=model.len()).step_by(u) {
        assert_eq!(
            doc.line_of(off as u64),
            LineGuess::Exact(line_at(model, off, sep)),
            "line_of({off})"
        );
    }
    let st = starts(model, sep);
    for (n, s) in st.iter().enumerate() {
        assert_eq!(
            doc.offset_of_line(n as u64),
            Some(LineGuess::Exact(*s)),
            "offset_of_line({n})"
        );
    }
    assert_eq!(doc.offset_of_line(st.len() as u64), None);
}

/// Правки: вставка или удаление в доле длины.
fn edits() -> impl Strategy<Value = Vec<(f64, usize, Vec<u8>)>> {
    proptest::collection::vec((0.0..=1.0f64, 0..12usize, text()), 1..60)
}

fn apply(doc: &mut TextDoc, model: &mut Vec<u8>, k: Kind, (a, n, t): &(f64, usize, Vec<u8>)) {
    let u = k.unit();
    let at = pos(*a, model.len(), u);
    let end = (at + n * u).min(model.len());
    let b = k.encode(t);
    doc.replace(at as u64..end as u64, &b).unwrap();
    model.splice(at..end, b);
}

/// Индекс, который дошёл только до `cut` (как фоновый в пути).
fn partial(full: &LineIndex, model: &[u8], cut: usize, sep: Sep) -> LineIndex {
    LineIndex {
        marks: full
            .marks
            .iter()
            .copied()
            .filter(|m| m.0 <= cut as u64)
            .collect(),
        lines: line_at(model, cut, sep),
        upto: cut as u64,
        done: false,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn lines_match_model(
        k in kind(),
        orig in proptest::collection::vec(prop_oneof![Just(b'x'), Just(b'\n')], 0..300),
        edits in edits(),
    ) {
        let orig = k.encode(&orig);
        let mut doc = doc_of(&orig, k.sep());
        let mut model = orig.clone();
        exact_lines(&doc, &model);
        for e in &edits {
            apply(&mut doc, &mut model, k, e);
        }
        exact_lines(&doc, &model);
    }

    /// До окончания индекса: точные ответы верны, остальные — `Approx`; после индекса и
    /// патча — всё точно.
    #[test]
    fn approx_until_indexed(
        k in kind(),
        orig in proptest::collection::vec(prop_oneof![Just(b'x'), Just(b'y'), Just(b'\n')], 1..300),
        cut in 0.0..1.0f64,
        edits in edits(),
    ) {
        let orig = k.encode(&orig);
        let sep = k.sep();
        let src = Arc::new(OwnedSource::new(orig.clone()));
        let full = index_now(&*src, sep).unwrap();
        let cut = pos(cut, orig.len(), k.unit());
        let add = AddBuf::with_limit(std::env::temp_dir().join("urfin-test"), format!("lines-{}", std::process::id()), MEM_MAX);
        let mut doc = TextDoc::new(src.clone(), 0, sep, add, partial(&full, &orig, cut, sep));
        let mut model = orig.clone();
        for e in &edits {
            apply(&mut doc, &mut model, k, e);
        }
        // Патч по дошедшему индексу.
        let cancel = CancelToken::new();
        let patch = lf_patch(&doc.snap(), doc.index(), &cancel).unwrap();
        doc.apply_lf(&patch);
        let mut approx = 0;
        for off in (0..=model.len()).step_by(k.unit()) {
            match doc.line_of(off as u64) {
                LineGuess::Exact(n) => prop_assert_eq!(n, line_at(&model, off, sep), "line_of({})", off),
                LineGuess::Approx(_) => approx += 1,
            }
        }
        if doc.tree().sums().unknown > 0 {
            prop_assert!(approx > 0);
        }
        // Индекс дошёл: дельта до конца, патч — всё точно.
        doc.on_index(crate::index::IndexDelta {
            marks: full.marks.iter().copied().filter(|m| m.0 > cut as u64).collect(),
            lines: full.lines,
            upto: full.upto,
            done: true,
        });
        let patch = lf_patch(&doc.snap(), doc.index(), &cancel).unwrap();
        doc.apply_lf(&patch);
        prop_assert_eq!(doc.tree().sums().unknown, 0);
        exact_lines(&doc, &model);
    }
}

#[test]
fn many_lines_use_index_marks() {
    // > 1024 строк: метки индекса, досчёт от метки.
    let orig: Vec<u8> = (0..5000u32)
        .flat_map(|i| format!("line {i}\n").into_bytes())
        .collect();
    let mut doc = doc_of(&orig, Sep::Byte(b'\n'));
    assert!(doc.index().marks.len() > 2);
    let mut model = orig.clone();
    for (i, at) in [40_000usize, 3, 20_011, 33_333].into_iter().enumerate() {
        let t = format!("new {i}\nx");
        doc.insert(at as u64, t.as_bytes()).unwrap();
        model.splice(at..at, t.bytes());
    }
    doc.remove(10_000..10_500);
    model.drain(10_000..10_500);
    let sep = Sep::Byte(b'\n');
    for off in (0..model.len()).step_by(97) {
        assert_eq!(
            doc.line_of(off as u64),
            LineGuess::Exact(line_at(&model, off, sep))
        );
    }
    for (n, s) in starts(&model, sep).iter().enumerate().step_by(53) {
        assert_eq!(doc.offset_of_line(n as u64), Some(LineGuess::Exact(*s)));
    }
}

/// Файл, который генерируется на лету: строки разной длины, период 4096 байт.
#[derive(Debug)]
struct Synth {
    len: u64,
    block: Vec<u8>,
}

const PERIOD: u64 = 4096;

impl Synth {
    fn new(len: u64) -> Synth {
        let mut block = Vec::with_capacity(PERIOD as usize);
        let mut i = 0u32;
        while block.len() < PERIOD as usize {
            let n = 10 + (i * 37 % 90) as usize;
            block.extend(std::iter::repeat_n(b'a' + (i % 26) as u8, n));
            block.push(b'\n');
            i += 1;
        }
        block.truncate(PERIOD as usize - 1);
        block.push(b'\n');
        Synth { len, block }
    }

    /// Разделителей в `[0, x)`.
    fn lines(&self, x: u64) -> u64 {
        let per = seps(&self.block, Sep::Byte(b'\n'));
        (x / PERIOD) * per + seps(&self.block[..(x % PERIOD) as usize], Sep::Byte(b'\n'))
    }
}

impl ByteSource for Synth {
    fn len(&self) -> u64 {
        self.len
    }

    fn is_final(&self) -> bool {
        true
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.len.saturating_sub(off) as usize);
        let mut done = 0;
        while done < n {
            let at = ((off + done as u64) % PERIOD) as usize;
            let k = (PERIOD as usize - at).min(n - done);
            buf[done..done + k].copy_from_slice(&self.block[at..at + k]);
            done += k;
        }
        Ok(n)
    }

    fn kind(&self) -> SourceKind {
        SourceKind::File
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len)
    }
}

/// Префикс источника, который ещё «растёт»: индекс доходит только до его конца.
#[derive(Debug)]
struct Prefix(Arc<Synth>, u64);

impl ByteSource for Prefix {
    fn len(&self) -> u64 {
        self.1
    }

    fn is_final(&self) -> bool {
        false
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.1.saturating_sub(off) as usize);
        self.0.read_at(off, &mut buf[..n])
    }

    fn kind(&self) -> SourceKind {
        SourceKind::File
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.1)
    }
}

/// V1: номер строки у курсора — `Approx` до индекса, точный после; правки до окончания
/// индекса счёт не ломают. Файл не читается целиком на UI-потоке: страницы — только у
/// курсора.
fn big_file(size: u64) {
    let t = Instant::now();
    let src = Arc::new(Synth::new(size));
    let sep = Sep::Byte(b'\n');
    let add = AddBuf::with_limit(
        std::env::temp_dir().join("urfin-test"),
        format!("v1-{}", std::process::id()),
        MEM_MAX,
    );
    let mut doc = TextDoc::new(src.clone(), 0, sep, add, LineIndex::default());
    assert_eq!(doc.tree().sums().unknown, size);
    let cur = size / 10 * 7 + 123;
    assert!(matches!(doc.line_of(cur), LineGuess::Approx(_)));
    // Правки до индекса.
    let cut = size / 10..size / 10 + 1000;
    doc.remove(cut.clone());
    let ins = size / 2;
    doc.insert(ins, b"hello\nworld\n").unwrap();
    let src_cur = cur + 1000 - 12;
    let expect = src.lines(src_cur) - (src.lines(cut.end) - src.lines(cut.start)) + 2;
    // Индекс дошёл до трети: всё ещё оценка у курсора.
    let mut ix = Indexer::new(sep);
    let mut publish = |d: IndexDelta| doc.on_index(d);
    ix.run(
        &Prefix(src.clone(), (size / 3) & !1),
        IndexMode::Parallel,
        &CancelToken::new(),
        &mut publish,
    )
    .unwrap();
    assert!(!doc.index().done);
    let cancel = CancelToken::new();
    let patch = lf_patch(&doc.snap(), doc.index(), &cancel).unwrap();
    doc.apply_lf(&patch);
    doc.load_now(cur - (256 << 10)..cur + 4096);
    assert!(matches!(doc.line_of(cur), LineGuess::Approx(_)));
    // Индекс до конца (дочитывание прироста), патч — точно.
    let mut ix = Indexer::resume(sep, doc.index());
    let mut publish = |d: IndexDelta| doc.on_index(d);
    ix.run(&*src, IndexMode::Parallel, &cancel, &mut publish)
        .unwrap();
    assert!(doc.index().done);
    assert_eq!(doc.index().lines, src.lines(size));
    let patch = lf_patch(&doc.snap(), doc.index(), &cancel).unwrap();
    doc.apply_lf(&patch);
    assert_eq!(doc.tree().sums().unknown, 0);
    assert_eq!(doc.line_of(cur), LineGuess::Exact(expect));
    let start = doc.offset_of_line(expect).unwrap();
    assert!(matches!(start, LineGuess::Exact(s) if s <= cur && cur - s < 200));
    assert_eq!(
        doc.line_of(doc.len()),
        LineGuess::Exact(src.lines(size) - (src.lines(cut.end) - src.lines(cut.start)) + 2)
    );
    println!(
        "big file {size}: {:?}, index marks {}",
        t.elapsed(),
        doc.index().marks.len()
    );
}

#[test]
fn big_file_lines_64mb() {
    big_file(64 << 20);
}

/// V1 на 10 ГБ (`--release --run-ignored only`).
#[test]
#[ignore = "10 ГБ, release"]
fn big_file_lines_10gb() {
    big_file(10 << 30);
}
