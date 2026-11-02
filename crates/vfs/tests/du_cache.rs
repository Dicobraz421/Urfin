//! Кэш снимков анализа места (F01-04): снимок 300k каталогов — побайтный roundtrip
//! через файл кэша; загрузка ≤ 100 мс — release, на стенде:
//!
//! `cargo nextest run -p urfin-vfs --release --run-ignored only -E 'test(load_budget)'`

use std::path::PathBuf;
use std::time::{Duration, Instant};

use urfin_vfs::du::{
    DuCache, DuCfg, DuNode, DuSnapshot, DuType, FileRec, NameRef, NodeFlags, SizeMode, TypeAgg,
    UsnMark, VolumeKey, cache,
};
use urfin_vfs::volume::FsKind;

/// Синтетический снимок: `n` каталогов (по 8 детей), у каждого десятого — крупный файл.
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
                put(format!("каталог-{i:06}"))
            },
            flags: NodeFlags(if i % 97 == 0 {
                NodeFlags::DENIED
            } else {
                NodeFlags::DONE
            }),
            files: i % 50,
            file_id: u64::from(i) << 3,
            disk: u64::from(i) * 4096,
            logical: u64::from(i) * 4000,
            mtime: -i64::from(i),
        })
        .collect();
    let files = (0..n / 10)
        .map(|i| FileRec {
            dir: i * 10,
            name: put(format!("file-{i:06}.bin")),
            ty: DuType::ALL[i as usize % DuType::COUNT],
            attrs: (i % 16) as u8,
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
        scanned_at: 1_761_955_200_000_000_000,
        took_ms: 282,
        generation: 3,
        usn: Some(UsnMark {
            journal_id: 0xABCD,
            next_usn: 1 << 40,
        }),
        dirs,
        names,
        files,
        types: TypeAgg {
            disk: [7; DuType::COUNT],
            files: [3; DuType::COUNT],
        },
        build: TypeAgg {
            disk: [2; DuType::COUNT],
            files: [1; DuType::COUNT],
        },
        denied: (0..n).step_by(97).skip(1).collect(),
        size_mode: SizeMode::Disk,
        track_min: 1 << 20,
        flags: 0,
    }
}

#[test]
fn roundtrip_300k_dirs() {
    let snap = snapshot(300_000);
    let tmp = tempfile::tempdir().unwrap();
    let c = DuCache::in_dir(tmp.path().to_path_buf(), &DuCfg::default());
    let path = c.store(&snap).unwrap();
    let back = c.load(snap.volume.serial, &snap.root).expect("cached");
    assert_eq!(back, snap);
    let bytes = cache::encode(&back).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "byte-identical");
}

#[test]
#[ignore = "release, стенд: бюджет загрузки F01 §4"]
fn load_budget() {
    let snap = snapshot(300_000);
    let tmp = tempfile::tempdir().unwrap();
    let c = DuCache::in_dir(tmp.path().to_path_buf(), &DuCfg::default());
    c.store(&snap).unwrap();
    let best = (0..5)
        .map(|_| {
            let t = Instant::now();
            assert!(c.load(snap.volume.serial, &snap.root).is_some());
            t.elapsed()
        })
        .min()
        .unwrap();
    eprintln!("du cache load 300k: {best:?}");
    assert!(best <= Duration::from_millis(100), "{best:?}");
}
