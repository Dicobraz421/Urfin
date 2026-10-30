//! `PageCache::get` (S7-03): цель ≤ 50 нс на диапазон строки экрана.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use urfin_core::CancelToken;
use urfin_view::cache::{Loader, PAGE, PageCache};
use urfin_view::source::OwnedSource;

fn bench(c: &mut Criterion) {
    let data: Vec<u8> = (0..4u32 << 20).map(|i| (i % 251) as u8).collect();
    let src = Arc::new(OwnedSource::new(data));
    let mut cache = PageCache::with_source(8 << 20, src.clone());
    cache.want(0..4 << 20, urfin_view::Prio::Visible);
    let loader = Loader::new(src);
    while let Some(req) = cache.take_request() {
        let got = loader.load(&req, &CancelToken::new());
        cache.insert(got);
        cache.want(0..4 << 20, urfin_view::Prio::Visible);
    }
    c.bench_function("page_cache_get_row", |b| {
        let mut off = 0u64;
        b.iter(|| {
            off = (off + 4099) % ((4 << 20) - 200);
            black_box(cache.get(black_box(off..off + 120)).map(|b| b.len()))
        });
    });
    c.bench_function("page_cache_get_two_pages", |b| {
        b.iter(|| black_box(cache.get(black_box(PAGE - 60..PAGE + 60)).map(|b| b.len())));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
