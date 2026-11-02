//! Загрузка снимка анализа места из кэша (F01-04, `du_cache_load`): 300k каталогов,
//! 30k крупных файлов (≈ 17 МБ) — чтение файла и разбор; бюджет ≤ 100 мс (F01 §4).

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use urfin_vfs::du::{
    DuCache, DuCfg, DuNode, DuSnapshot, DuType, FileRec, NameRef, NodeFlags, SizeMode, TypeAgg,
    VolumeKey, cache,
};
use urfin_vfs::volume::FsKind;

/// Синтетический снимок: `n` каталогов, у каждого десятого — крупный файл.
fn snapshot(n: u32) -> DuSnapshot {
    let mut names = Vec::new();
    let mut put = |s: String| {
        let off = u32::try_from(names.len()).unwrap();
        names.extend_from_slice(s.as_bytes());
        NameRef::new(off, u16::try_from(s.len()).unwrap(), false)
    };
    let dirs = (0..n)
        .map(|i| DuNode {
            parent: if i == 0 { u32::MAX } else { (i - 1) / 8 },
            name: if i == 0 {
                NameRef::default()
            } else {
                put(format!("dir-{i:06}"))
            },
            flags: NodeFlags(NodeFlags::DONE),
            files: i % 50,
            file_id: u64::from(i) << 3,
            disk: u64::from(i) * 4096,
            logical: u64::from(i) * 4000,
            mtime: i64::from(i),
        })
        .collect();
    let files = (0..n / 10)
        .map(|i| FileRec {
            dir: i * 10,
            name: put(format!("file-{i:06}.bin")),
            ty: DuType::ALL[i as usize % DuType::COUNT],
            attrs: 0,
            disk: u64::from(n - i) << 20,
            logical: u64::from(n - i) << 20,
            mtime: i64::from(i),
            file_id: u64::from(i),
        })
        .collect();
    DuSnapshot {
        root: PathBuf::from(r"C:\Users"),
        volume: VolumeKey {
            serial: 0x1234_5678,
            fs: FsKind::Ntfs,
        },
        scanned_at: 1,
        took_ms: 2,
        generation: 1,
        usn: None,
        dirs,
        names,
        files,
        types: TypeAgg::default(),
        build: TypeAgg::default(),
        denied: Vec::new(),
        size_mode: SizeMode::Disk,
        track_min: 1 << 20,
        flags: 0,
    }
}

fn bench(c: &mut Criterion) {
    let snap = snapshot(300_000);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache = DuCache::in_dir(tmp.path().to_path_buf(), &DuCfg::default());
    cache.store(&snap).expect("store");
    let bytes = cache::encode(&snap).expect("encode");
    let mut g = c.benchmark_group("du_cache_load");
    g.sample_size(20);
    g.bench_function("decode_300k", |b| {
        b.iter(|| black_box(cache::decode(&bytes).expect("decode")));
    });
    g.bench_function("load_300k", |b| {
        b.iter(|| black_box(cache.load(snap.volume.serial, &snap.root).expect("load")));
    });
    g.bench_function("encode_300k", |b| {
        b.iter(|| black_box(cache::encode(&snap).expect("encode")));
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
