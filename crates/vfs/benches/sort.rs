//! Natural sort (S4-04): `natural_cmp` на ASCII-именах — цель ≥ 100 M сравнений/с.
//! Пересортировка, фильтр (S4-05, S4-11): цели S4 §7.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use urfin_core::{ArrangeSpec, Attrs, Entry, EntryKind, SortKey, SortSpec};
use urfin_vfs::sort::{ArrangeOpts, arrange, natural_cmp, prefix_key};

/// Имена как в реальных каталогах: общий префикс, расширения, номера.
fn names(n: u32) -> Vec<String> {
    let exts = ["txt", "rs", "toml", "md", "png", "zip", "exe", "log"];
    (0..n)
        .map(|i| {
            let x = i.wrapping_mul(2_654_435_761);
            format!(
                "{}-{:04}.{}",
                ["file", "report", "IMG", "data", "Notes"][(x % 5) as usize],
                x % 10_000,
                exts[(x >> 8) as usize % exts.len()]
            )
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let v = names(4096);
    let pairs: Vec<(&str, &str)> = v
        .iter()
        .zip(v.iter().rev())
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let mut g = c.benchmark_group("natural");
    g.throughput(Throughput::Elements(pairs.len() as u64));
    g.bench_function("natural_cmp ascii", |b| {
        b.iter(|| {
            for &(x, y) in &pairs {
                black_box(natural_cmp(black_box(x), black_box(y)));
            }
        });
    });
    g.bench_function("prefix_key", |b| {
        b.iter(|| {
            for &(x, _) in &pairs {
                black_box(prefix_key(black_box(x), true));
            }
        });
    });
    g.finish();

    // S4-05: 100k < 10 мс, 1M < 90 мс (стенд).
    let mut g = c.benchmark_group("arrange");
    g.sample_size(20);
    for n in [100_000u32, 1_000_000] {
        let entries: Vec<Entry> = names(n)
            .into_iter()
            .enumerate()
            .map(|(i, name)| Entry {
                ctime: i64::MIN,
                atime: i64::MIN,
                name: format!("{i:x}{name}").into(),
                size: (i as u64).wrapping_mul(7919) % 1_000_000,
                mtime: 0,
                attrs: Attrs::default(),
                kind: if i % 50 == 0 {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
            })
            .collect();
        let spec = ArrangeSpec {
            sort: SortSpec {
                key: SortKey::Name,
                desc: false,
                dirs_first: true,
            },
            filter: None,
            show_hidden: false,
        };
        let opts = ArrangeOpts::default();
        g.bench_function(format!("name {n}"), |b| {
            b.iter(|| black_box(arrange(black_box(&entries), &spec, opts)));
        });
        // S4-11 (§7): расширение и размер, фильтр по маске — 100k < 4 мс, 1M < 35 мс.
        for (label, key) in [("ext", SortKey::Ext), ("size", SortKey::Size)] {
            let spec = ArrangeSpec {
                sort: SortSpec { key, ..spec.sort },
                ..spec.clone()
            };
            g.bench_function(format!("{label} {n}"), |b| {
                b.iter(|| black_box(arrange(black_box(&entries), &spec, opts)));
            });
        }
        let filtered = ArrangeSpec {
            filter: Some(urfin_core::Mask::parse("*.rs,*.toml").unwrap()),
            ..spec.clone()
        };
        g.bench_function(format!("filter {n}"), |b| {
            b.iter(|| black_box(arrange(black_box(&entries), &filtered, opts)));
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
