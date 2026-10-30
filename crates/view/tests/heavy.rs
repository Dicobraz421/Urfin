//! Тяжёлые приёмочные замеры S7-02…S7-06: `#[ignore]`, прогон один раз в release:
//! `cargo nextest run -p urfin-view --release --run-ignored only --no-capture`.
//! Цифры — в карточки `docs/tasks/S7-0*.md`.

#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    reason = "тесты: короткие имена, доли длины, таблицы случаев"
)]

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use urfin_core::{CancelToken, Location};
use urfin_vfs::VfsRegistry;
use urfin_view::cache::{Loader, PageCache, Prio};
use urfin_view::index::{IndexError, IndexMode, Indexer, Sep};
use urfin_view::layout::{LayoutSpec, RowText, WrapKind, line_start_before, render_row, rows_down};
use urfin_view::source::{OwnedSource, SourceKind, SourceOpts, open_source, read_full};
use urfin_view::{ByteSource, detect};

fn log(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len + 200);
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = 40 + (x % 120) as usize;
        v.extend(std::iter::repeat_n(b'a' + (x % 26) as u8, n));
        v.push(b'\n');
    }
    v.truncate(len);
    v
}

/// S7-02: sparse 4,5 ГБ открывается ≤ 20 мс (`open_source` + первые 64 КБ + хвост).
#[test]
#[ignore = "тяжёлый: 4,5 ГБ sparse, release"]
fn sparse_4_5gb_opens_fast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v3.bin");
    let f = File::create(&path).unwrap();
    drop(f);
    let sparse = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&path)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(sparse, "fsutil sparse setflag не сработал");
    let len: u64 = 4_500_000_000;
    let mut f = File::options().write(true).open(&path).unwrap();
    f.set_len(len).unwrap();
    for at in [0u64, 1 << 32, len - 4096] {
        use std::io::{Seek, SeekFrom};
        f.seek(SeekFrom::Start(at)).unwrap();
        f.write_all(&[0xAB; 4096]).unwrap();
    }
    f.set_modified(SystemTime::now() - Duration::from_secs(300))
        .unwrap();
    drop(f);
    let reg = VfsRegistry::with_local();
    let loc = Location::local(&path);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let o = open_source(&reg, &loc, &SourceOpts::default(), &CancelToken::new()).unwrap();
        let mut head = vec![0u8; 64 << 10];
        assert_eq!(read_full(&*o.src, 0, &mut head).unwrap(), 64 << 10);
        let mut tail = [0u8; 16];
        read_full(&*o.src, (1 << 32) + 8, &mut tail).unwrap();
        let el = t.elapsed();
        assert_eq!(o.src.kind(), SourceKind::Mapped);
        assert_eq!(o.src.len(), len);
        assert_eq!((head[0], tail[0]), (0xAB, 0xAB));
        best = best.min(el);
    }
    println!("S7-02 sparse 4.5 GB open+64KiB: best {best:?}");
    assert!(best <= Duration::from_millis(20), "{best:?}");
}

/// S7-03: `PageCache::get` ≤ 50 нс.
#[test]
#[ignore = "замер, release"]
fn cache_get_ns() {
    let data = log(8 << 20);
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(data));
    let mut c = PageCache::with_source(8 << 20, src.clone());
    let l = Loader::new(src);
    for at in (0..8u64 << 20).step_by(64 << 10) {
        c.want(at..at + 100, Prio::Visible);
        while let Some(r) = c.take_request() {
            c.insert(l.load(&r, &CancelToken::new()));
        }
    }
    let n = 2_000_000u64;
    let t = Instant::now();
    let mut off = 0u64;
    let mut sum = 0usize;
    for _ in 0..n {
        off = (off + 40_009) % ((8 << 20) - 200);
        sum += c.get(black_box(off..off + 160)).map_or(0, |b| b.len());
    }
    let ns = t.elapsed().as_nanos() as f64 / n as f64;
    black_box(sum);
    println!("S7-03 PageCache::get: {ns:.1} ns");
    assert!(ns <= 50.0, "{ns} ns");
}

/// S7-04: детект ≤ 1 мс на 64 КБ (худший — chardetng на однобайтовой кириллице).
#[test]
#[ignore = "замер, release"]
fn detect_64k_under_1ms() {
    let ru = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь. ".repeat(1600);
    let (cp1251, ..) = encoding_rs::WINDOWS_1251.encode(&ru);
    let samples: [(&str, Vec<u8>); 3] = [
        ("cp1251", cp1251[..64 << 10].to_vec()),
        ("utf-8", ru.as_bytes()[..64 << 10].to_vec()),
        ("log", log(64 << 10)),
    ];
    for (name, s) in samples {
        let mut best = Duration::MAX;
        for _ in 0..30 {
            let t = Instant::now();
            black_box(detect(black_box(&s), false, Some(b"ru")));
            best = best.min(t.elapsed());
        }
        println!("S7-04 detect 64 KiB {name}: {best:?}");
        assert!(best <= Duration::from_millis(1), "{name}: {best:?}");
    }
}

/// S7-05: скорость индекса в памяти (warm) и отмена ≤ 50 мс в обоих режимах.
#[test]
#[ignore = "тяжёлый: 1 ГиБ в памяти, release"]
fn index_speed_and_cancel() {
    let len = 1usize << 30;
    let src = Arc::new(OwnedSource::new(log(len)));
    for mode in [IndexMode::Parallel, IndexMode::Sequential] {
        let mut ix = Indexer::new(Sep::Byte(b'\n'));
        let t = Instant::now();
        ix.run(&*src, mode, &CancelToken::new(), &mut |_| {})
            .unwrap();
        let el = t.elapsed();
        let gbs = len as f64 / el.as_secs_f64() / 1e9;
        println!(
            "S7-05 index {mode:?}: {gbs:.2} GB/s ({} lines, {} threads)",
            ix.lines,
            rayon::current_num_threads()
        );
    }
    for mode in [IndexMode::Parallel, IndexMode::Sequential] {
        let cancel = CancelToken::new();
        let (c2, src) = (cancel.clone(), src.clone());
        let h = std::thread::spawn(move || {
            let mut ix = Indexer::new(Sep::Byte(b'\n'));
            let r = ix.run(&*src, mode, &c2, &mut |_| {});
            (matches!(r, Err(IndexError::Cancelled)), Instant::now())
        });
        std::thread::sleep(Duration::from_millis(20));
        let at = Instant::now();
        cancel.cancel();
        let (cancelled, done) = h.join().unwrap();
        let lag = done.saturating_duration_since(at);
        println!("S7-05 cancel {mode:?}: {lag:?}");
        assert!(cancelled, "{mode:?}: успел дойти до конца");
        assert!(lag <= Duration::from_millis(50), "{mode:?}: {lag:?}");
    }
}

/// S7-06: V2 (строка 50 МБ) — строка экрана ≤ 50 мкс без переноса и с переносом.
#[test]
#[ignore = "замер, release"]
fn v2_screen_row_under_50us() {
    let v2: Vec<u8> = b"{\"key\":\"value\",\"n\":12345,\"list\":[1,2,3]},"
        .iter()
        .copied()
        .cycle()
        .take(50 << 20)
        .collect();
    let rows_n = 60;
    for wrap in [None, Some(WrapKind::Chars), Some(WrapKind::Words)] {
        let s = LayoutSpec {
            width_cols: 160,
            wrap,
            ..LayoutSpec::default()
        };
        let mut worst = 0f64;
        for at in [0u64, 7 << 20, 25 << 20, (50 << 20) - 5000] {
            let mut best = f64::MAX;
            for _ in 0..10 {
                let t = Instant::now();
                let top = line_start_before(&v2[..], at, &s).unwrap();
                let rows = rows_down(&v2[..], (top, 0), rows_n, &s).unwrap();
                let mut out = RowText::default();
                for r in &rows {
                    render_row(&v2[..], r, 0, &s, &mut out).unwrap();
                    black_box(&out);
                }
                best = best.min(t.elapsed().as_secs_f64() * 1e6 / rows.len() as f64);
            }
            worst = worst.max(best);
        }
        println!("S7-06 V2 screen row {wrap:?}: {worst:.2} µs");
        assert!(worst <= 50.0, "{wrap:?}: {worst} µs");
    }
}
