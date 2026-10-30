//! Индекс строк (S7-05, S7 §3.3): синтетический лог 256 МБ в памяти, параллельно и в
//! один поток. Цели (release, warm): ≥ 2 ГБ/с на 8 потоках, ≥ 1 ГБ/с в один поток;
//! замер V1 на стенде — S7-20.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use urfin_core::CancelToken;
use urfin_view::index::{IndexMode, Indexer, Sep};
use urfin_view::source::OwnedSource;

/// Строки лога 40–160 байт (≈ 85 в среднем, как V1).
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

fn bench(c: &mut Criterion) {
    let len = 256 << 20;
    let src = Arc::new(OwnedSource::new(log(len)));
    let mut g = c.benchmark_group("index");
    g.sample_size(10);
    g.throughput(Throughput::Bytes(len as u64));
    for (name, mode) in [
        ("parallel", IndexMode::Parallel),
        ("sequential", IndexMode::Sequential),
    ] {
        g.bench_function(name, |b| {
            b.iter(|| {
                let mut ix = Indexer::new(Sep::Byte(b'\n'));
                ix.run(&*src, mode, &CancelToken::new(), &mut |d| {
                    black_box(d);
                })
                .unwrap();
                black_box(ix.lines)
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
