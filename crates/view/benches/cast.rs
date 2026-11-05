//! Перемотка записи сеанса (F03-13, F03 §3.6): файл 10 МБ, бюджет ≤50 мс на перемотку
//! назад и через весь файл.

use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use criterion::{Criterion, criterion_group, criterion_main};
use urfin_core::CancelToken;
use urfin_term::vt::grid::Defaults;
use urfin_view::cast::{Replay, header, scan};
use urfin_view::source::{ByteSource, OwnedSource};

const D: Defaults = Defaults {
    fg: [0, 0, 0],
    bg: [255, 255, 255],
    cursor: [0, 0, 0],
};

/// Запись `len` байт: цветные строки журнала, 12 событий в секунду.
fn recording(len: usize) -> Vec<u8> {
    let mut s = String::with_capacity(len + 4096);
    s.push_str("{\"version\":2,\"width\":132,\"height\":30,\"timestamp\":1792764151}\n");
    let mut i = 0u64;
    while s.len() < len {
        let _ = writeln!(
            s,
            "[{}.{:03},\"o\",\"\\u001b[1;3{}m2026-10-23T14:05:51Z\\u001b[0m INFO worker {i}: \
             processed request /api/v1/items?page={i} in 12ms, status 200 OK\\r\\n\"]",
            i / 12,
            i % 12 * 83,
            i % 7 + 1,
        );
        i += 1;
    }
    s.into_bytes()
}

#[allow(
    clippy::many_single_char_names,
    reason = "бенч: группа, индекс, шаги ГПСЧ"
)]
fn bench(c: &mut Criterion) {
    let bytes = recording(10 << 20);
    let h = header(&bytes).expect("заголовок");
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(bytes));
    let mut g = c.benchmark_group("cast");
    g.sample_size(20);
    let scan_once = || {
        scan(
            &src,
            &h,
            &mut |_| Ok(()),
            &CancelToken::new(),
            &AtomicU64::new(0),
        )
        .expect("проход")
        .expect("без отмены")
    };
    g.bench_function("scan_10mb", |b| b.iter(|| black_box(scan_once())));
    let ix = Arc::new(scan_once());
    let dur = ix.duration_ms;
    g.bench_function("seek_10mb", |b| {
        let mut r = Replay::new(src.clone(), &h, ix.clone());
        // Перемотка в случайные места, в том числе назад: каждая — от ключа.
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        b.iter(|| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            r.seek(x % dur).expect("перемотка");
            black_box(r.snapshot(&D));
        });
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
