//! Скан анализа места (F01-03, `du_scan_100k`): 100k файлов в 5k каталогов — тот же
//! датасет `walk-100k`, что у бенча обходчика (сравнение `NtLister` с `walk`).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use urfin_core::{CancelToken, Location};
use urfin_testdata::{Layout, TreeSpec, dataset};
use urfin_vfs::LocalFs;
use urfin_vfs::du::{DuTree, ListerKind, ScanCfg, scan};

fn tree() -> Location {
    let spec = TreeSpec {
        n: 100_000,
        seed: 1,
        layout: Layout::Nav { dirs: 5000 },
    };
    Location::local(dataset("urfin", "walk-100k", &spec).expect("dataset walk-100k"))
}

fn run(root: &Location, lister: ListerKind) -> u64 {
    let cancel = CancelToken::new();
    let tree = DuTree::new();
    let cfg = ScanCfg {
        lister,
        threads: 4,
        ..ScanCfg::default()
    };
    let out = scan(
        root,
        &LocalFs::new(),
        &cfg,
        &tree,
        &|| cancel.is_cancelled(),
        None,
        |_| {},
    );
    out.snapshot.dirs[0].logical
}

fn bench(c: &mut Criterion) {
    let root = tree();
    let mut g = c.benchmark_group("du_scan_100k");
    g.sample_size(20);
    for (name, kind) in [
        ("nt", ListerKind::Nt),
        ("find", ListerKind::Find),
        ("vfs", ListerKind::Vfs),
    ] {
        g.bench_function(name, |b| b.iter(|| black_box(run(&root, kind))));
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
