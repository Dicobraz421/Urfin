//! Патч (S4-15, S4 §7): 100 изменений в каталоге 100k — цель < 3 мс CPU.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use urfin_core::{ArrangeSpec, Attrs, Entry, EntryKind, Gen, SortSpec};
use urfin_vfs::patch::{Changes, DirIndex, build_patch};
use urfin_vfs::sort::{ArrangeOpts, arrange};

fn entry(name: String, size: u64) -> Entry {
    Entry {
        ctime: i64::MIN,
        atime: i64::MIN,
        name: name.into(),
        size,
        mtime: 0,
        attrs: Attrs(0),
        kind: EntryKind::File,
    }
}

fn dir(n: u32) -> Vec<Entry> {
    let words = ["file", "report", "IMG", "data", "Notes"];
    (0..n)
        .map(|i| {
            let x = i.wrapping_mul(2_654_435_761);
            entry(
                format!("{}-{i}.txt", words[(x % 5) as usize]),
                u64::from(x % 1000),
            )
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let entries = dir(100_000);
    let spec = ArrangeSpec {
        sort: SortSpec::default(),
        filter: None,
        show_hidden: false,
    };
    let opts = ArrangeOpts::default();
    let order = arrange(&entries, &spec, opts);
    let changes = || Changes {
        // 40 изменённых, 30 новых, 30 удалённых.
        upsert: (0..40u32)
            .map(|k| {
                let mut e = entries[(k * 2_477) as usize].clone();
                e.size += 1;
                e
            })
            .chain((0..30).map(|k| entry(format!("new-{k}.txt"), 1)))
            .collect(),
        removed: (0..30u32)
            .map(|k| entries[(k * 3_181 + 7) as usize].name.clone())
            .collect(),
    };
    let mut g = c.benchmark_group("patch");
    g.bench_function("100 changes in 100k", |b| {
        b.iter_batched(
            || (DirIndex::build(&entries, Gen(1)), changes()),
            |(mut idx, ch)| {
                black_box(build_patch(&entries, &order, &mut idx, &ch, &spec, opts, 0));
            },
            BatchSize::LargeInput,
        );
    });
    g.bench_function("DirIndex::build 100k", |b| {
        b.iter(|| black_box(DirIndex::build(&entries, Gen(1))));
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
