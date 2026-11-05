#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    reason = "тесты: короткие имена, доли длины, таблицы случаев"
)]

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use proptest::prelude::*;

use super::*;
use crate::source::{OwnedSource, clamp};

/// Наивные начала строк (после каждого разделителя).
fn naive(data: &[u8], sep: Sep) -> Vec<u64> {
    let mut v = Vec::new();
    match sep {
        Sep::Byte(b) => {
            for (i, &x) in data.iter().enumerate() {
                if x == b {
                    v.push(i as u64 + 1);
                }
            }
        }
        Sep::Le(b) | Sep::Be(b) => {
            for i in (0..data.len().saturating_sub(1)).step_by(2) {
                let unit = if matches!(sep, Sep::Le(_)) {
                    [data[i], data[i + 1]]
                } else {
                    [data[i + 1], data[i]]
                };
                if unit == [b, 0] {
                    v.push(i as u64 + 2);
                }
            }
        }
    }
    v
}

/// Индекс = наивный подсчёт: число строк, метки — начала строк с верными номерами,
/// между метками не больше 1024 строк.
fn check(ix: &LineIndex, data: &[u8], sep: Sep) -> Result<(), TestCaseError> {
    let starts = naive(data, sep);
    prop_assert_eq!(ix.lines, starts.len() as u64);
    prop_assert_eq!(ix.upto, data.len() as u64);
    prop_assert_eq!(ix.marks[0], (0, 0));
    let mut prev = (0u64, 0u64);
    for &(off, line) in &ix.marks[1..] {
        prop_assert!(
            line >= 1 && starts[line as usize - 1] == off,
            "метка {off}/{line}"
        );
        prop_assert!(off > prev.0 && line > prev.1);
        prop_assert!(line - prev.1 <= MARK_EVERY, "разрыв {prev:?} → {line}");
        prev = (off, line);
    }
    prop_assert!(ix.lines - prev.1 <= MARK_EVERY, "хвост без меток");
    Ok(())
}

fn run(src: &dyn ByteSource, ix: &mut Indexer, mode: IndexMode, out: &mut LineIndex) {
    ix.run(src, mode, &CancelToken::new(), &mut |d| out.apply(d))
        .unwrap();
}

fn text(seed: &[u8], wide: Option<bool>) -> Vec<u8> {
    let alphabet = b"ab \n\r\nxyz\n";
    let bytes: Vec<u8> = seed
        .iter()
        .map(|&x| alphabet[x as usize % alphabet.len()])
        .collect();
    match wide {
        None => bytes,
        Some(be) => bytes
            .iter()
            .flat_map(|&b| if be { [0, b] } else { [b, 0] })
            .collect(),
    }
}

fn sep_of(wide: Option<bool>) -> Sep {
    match wide {
        None => Sep::Byte(b'\n'),
        Some(false) => Sep::Le(b'\n'),
        Some(true) => Sep::Be(b'\n'),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Приёмка S7-05: число строк и метки = наивный подсчёт; оба режима, любые блоки.
    #[test]
    fn index_matches_naive(
        seed in proptest::collection::vec(any::<u8>(), 0..24_000),
        wide in prop_oneof![Just(None), Just(Some(false)), Just(Some(true))],
        chunk in 1u64..3000,
        slice in 1usize..700,
    ) {
        let data = text(&seed, wide);
        let sep = sep_of(wide);
        let src = OwnedSource::new(data.clone());
        for mode in [IndexMode::Parallel, IndexMode::Sequential] {
            let mut ix = Indexer::new(sep).with_blocks(chunk * 2, slice * 2);
            let mut out = LineIndex::default();
            run(&src, &mut ix, mode, &mut out);
            prop_assert!(out.done);
            check(&out, &data, sep)?;
        }
    }

    /// Follow/Spool: индексатор дочитывает прирост, в том числе с половиной единицы UTF-16.
    #[test]
    fn index_continues_on_growth(
        seed in proptest::collection::vec(any::<u8>(), 1..12_000),
        wide in prop_oneof![Just(None), Just(Some(false))],
        cut in 0.0f64..1.0,
    ) {
        let data = text(&seed, wide);
        let sep = sep_of(wide);
        let src = Grow::new(data.clone());
        let mid = (data.len() as f64 * cut) as u64;
        src.len.store(mid, Ordering::SeqCst);
        let mut ix = Indexer::new(sep).with_blocks(512, 64);
        let mut out = LineIndex::default();
        run(&src, &mut ix, IndexMode::Sequential, &mut out);
        prop_assert!(!out.done);
        src.len.store(data.len() as u64, Ordering::SeqCst);
        run(&src, &mut ix, IndexMode::Sequential, &mut out);
        prop_assert!(out.done);
        check(&out, &data, sep)?;
    }

    /// `line_of_in`, `goto_line` и `mark_before_line` против наивного подсчёта.
    #[test]
    fn line_of_and_goto(seed in proptest::collection::vec(any::<u8>(), 1..30_000), at in 0.0f64..1.0) {
        let data = text(&seed, None);
        let sep = Sep::Byte(b'\n');
        let mut out = LineIndex::default();
        run(&OwnedSource::new(data.clone()), &mut Indexer::new(sep), IndexMode::Sequential, &mut out);
        let starts = naive(&data, sep);
        let off = (data.len() as f64 * at) as u64;
        let line = starts.partition_point(|&s| s <= off) as u64;
        prop_assert_eq!(out.line_of_in(&data[..], off, sep).unwrap(), LineGuess::Exact(line));
        let start = if line == 0 { 0 } else { starts[line as usize - 1] };
        let from = out.mark_before_line(line);
        prop_assert!(from.1 <= line && line - from.1 <= MARK_EVERY);
        prop_assert_eq!(goto_line(&data[..], from, line, sep).unwrap(), Some(start));
        prop_assert_eq!(goto_line(&data[..], from, out.lines + 1, sep).unwrap(), None);
    }
}

/// Растущий источник (follow, Spool).
#[derive(Debug)]
struct Grow {
    data: Vec<u8>,
    len: AtomicU64,
}

impl Grow {
    fn new(data: Vec<u8>) -> Grow {
        let len = AtomicU64::new(data.len() as u64);
        Grow { data, len }
    }
}

impl ByteSource for Grow {
    fn len(&self) -> u64 {
        self.len.load(Ordering::SeqCst)
    }
    fn is_final(&self) -> bool {
        self.len() == self.data.len() as u64
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let n = clamp(off, buf.len(), self.len());
        buf[..n].copy_from_slice(&self.data[off as usize..off as usize + n]);
        Ok(n)
    }
    fn kind(&self) -> SourceKind {
        SourceKind::Spool
    }
    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len())
    }
}

#[test]
fn line_of_estimates_beyond_upto() {
    let ix = LineIndex {
        marks: vec![(0, 0), (100_000, 1024), (200_000, 2048)],
        lines: 2048,
        upto: 200_000,
        done: false,
    };
    assert_eq!(ix.line_of(100_000), LineGuess::Exact(1024));
    assert_eq!(ix.line_of(150_000), LineGuess::Approx(1536));
    assert_eq!(ix.line_of(400_000), LineGuess::Approx(2048 + 200_000 / 97));
    let done = LineIndex { done: true, ..ix };
    assert_eq!(done.line_of(250_000), LineGuess::Exact(2048));
}

fn log(len: usize) -> Vec<u8> {
    let line = b"2026-10-30 12:00:00 INFO test message with some payload\n";
    line.iter().copied().cycle().take(len).collect()
}

/// Дельты — не чаще 10 раз в секунду (плюс последняя).
#[test]
fn deltas_are_rate_limited() {
    let src = OwnedSource::new(log(24 << 20));
    let mut n = 0u32;
    let mut ix = Indexer::new(Sep::Byte(b'\n')).with_blocks(1 << 20, 64 << 10);
    let t = Instant::now();
    ix.run(
        &src,
        IndexMode::Sequential,
        &CancelToken::new(),
        &mut |_| n += 1,
    )
    .unwrap();
    let max = t.elapsed().as_millis() / 100 + 2;
    assert!(u128::from(n) <= max, "{n} дельт за {:?}", t.elapsed());
}

/// Источник, который сам отменяет прогон на чтении номер `trip` и считает чтения после него.
#[derive(Debug)]
struct Tripwire {
    data: Vec<u8>,
    cancel: CancelToken,
    /// Отдавать данные отображением (параллельный режим: срезы без чтений).
    map: bool,
    trip: u64,
    seen: AtomicU64,
    after: AtomicU64,
}

impl Tripwire {
    fn tick(&self) {
        let n = self.seen.fetch_add(1, Ordering::SeqCst);
        if n == self.trip {
            self.cancel.cancel();
        } else if n > self.trip {
            self.after.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl ByteSource for Tripwire {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn is_final(&self) -> bool {
        true
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.tick();
        let n = clamp(off, buf.len(), self.len());
        buf[..n].copy_from_slice(&self.data[off as usize..off as usize + n]);
        Ok(n)
    }

    fn mapped(&self) -> Option<&[u8]> {
        self.map.then(|| {
            self.tick();
            self.data.as_slice()
        })
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Owned
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len())
    }
}

/// Приёмка S7-05: отмена прерывает оба режима посреди файла, после неё — не больше одного
/// среза на поток. Мерим работу, а не часы: под нагрузкой стенда поток стоит сотни мс и без
/// всякой вины отмены. Время ≤ 50 мс — `tests/heavy.rs` в release.
#[test]
fn cancel_stops_both_modes() {
    let data = log(48 << 20);
    for (mode, map) in [
        (IndexMode::Parallel, false),
        (IndexMode::Parallel, true),
        (IndexMode::Sequential, false),
    ] {
        let cancel = CancelToken::new();
        let src = Tripwire {
            data: data.clone(),
            cancel: cancel.clone(),
            map,
            trip: 2,
            seen: AtomicU64::new(0),
            after: AtomicU64::new(0),
        };
        let mut ix = Indexer::new(Sep::Byte(b'\n')).with_blocks(4 << 20, 256 << 10);
        let r = ix.run(&src, mode, &cancel, &mut |_| {});
        assert!(matches!(r, Err(IndexError::Cancelled)), "{mode:?}: {r:?}");
        let after = src.after.load(Ordering::SeqCst);
        // Поток мог пройти проверку до отмены: у него один срез. Отображение без чтений
        // зовёт `mapped` на диапазон, а не на срез: счёт — по диапазонам батча.
        let threads = rayon::current_num_threads() as u64;
        assert!(after <= threads, "{mode:?}, map {map}: {after} чтений");
    }
}

#[test]
fn sep_by_encoding_and_eol() {
    use encoding_rs::{UTF_16BE, UTF_16LE};
    assert_eq!(Sep::of(&Enc::utf8(), Eol::CrLf), Sep::Byte(b'\n'));
    assert_eq!(Sep::of(&Enc::utf8(), Eol::Cr), Sep::Byte(b'\r'));
    assert_eq!(Sep::of(&Enc::new(UTF_16LE), Eol::Lf), Sep::Le(b'\n'));
    assert_eq!(Sep::of(&Enc::new(UTF_16BE), Eol::Mixed), Sep::Be(b'\n'));
    assert_eq!(
        IndexMode::of(SourceKind::Mapped, Some(Media::Unknown)),
        IndexMode::Parallel
    );
    assert_eq!(
        IndexMode::of(SourceKind::File, Some(Media::Unknown)),
        IndexMode::Sequential
    );
}
