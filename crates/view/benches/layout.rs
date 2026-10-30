//! Раскладка (S7-06): экран 60 строк из середины строки 50 МБ (V2) и из лога; цель —
//! ≤ 50 мкс на строку экрана.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use urfin_view::layout::{LayoutSpec, RowText, WrapKind, line_start_before, render_row, rows_down};

fn screen(data: &[u8], s: &LayoutSpec, at: u64) -> usize {
    let top = line_start_before(data, at, s).unwrap();
    let rows = rows_down(data, (top, 0), 60, s).unwrap();
    let mut out = RowText::default();
    let mut n = 0;
    for r in &rows {
        render_row(data, r, 0, s, &mut out).unwrap();
        n += out.text.len();
    }
    n
}

fn bench(c: &mut Criterion) {
    let v2: Vec<u8> = b"{\"key\":\"value\",\"n\":12345,\"list\":[1,2,3]},"
        .iter()
        .copied()
        .cycle()
        .take(50 << 20)
        .collect();
    let mut s = LayoutSpec {
        width_cols: 160,
        ..LayoutSpec::default()
    };
    let mid = 25 << 20;
    c.bench_function("layout_v2_screen_nowrap", |b| {
        b.iter(|| black_box(screen(&v2, &s, black_box(mid))));
    });
    s.wrap = Some(WrapKind::Chars);
    c.bench_function("layout_v2_screen_wrap_chars", |b| {
        b.iter(|| black_box(screen(&v2, &s, black_box(mid))));
    });
    s.wrap = Some(WrapKind::Words);
    c.bench_function("layout_v2_screen_wrap_words", |b| {
        b.iter(|| black_box(screen(&v2, &s, black_box(mid))));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
