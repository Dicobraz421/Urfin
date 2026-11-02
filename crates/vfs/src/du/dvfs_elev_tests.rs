//! Досчёт с правами (F01-12): пути недоступных, счёт под каталогом, слияние итогов.

use std::path::{Path, PathBuf};

use crate::du::dvfs::{DuRegistry, SnapId, location};
use crate::du::elevated::RootSum;
use crate::du::model::{DirId, DuTree, DuType, NodeFlags, SizeMode, SnapMeta, VolumeKey};
use crate::volume::FsKind;
use crate::walk::RawName;

const ROOT: &str = r"C:\T";

fn raw(name: &str) -> RawName<'_> {
    RawName::Str {
        name,
        escaped: false,
    }
}

/// `C:\T`: `a\` (100 Б, внутри недоступная `locked`), недоступная `b`, `c\` (10 Б).
fn setup() -> (std::sync::Arc<DuRegistry>, SnapId, [DirId; 4]) {
    let t = DuTree::new();
    let a = t.add_dir(0, &raw("a"), 0, 0).unwrap();
    let locked = t.add_dir(a, &raw("locked"), 0, 0).unwrap();
    let b = t.add_dir(0, &raw("b"), 0, 0).unwrap();
    let c = t.add_dir(0, &raw("c"), 0, 0).unwrap();
    t.add_sizes(a, 100, 100, 2);
    t.add_sizes(c, 10, 10, 1);
    for d in [locked, b] {
        t.deny(d);
    }
    for d in [locked, a, b, c, 0] {
        t.complete(d);
    }
    let snap = t.freeze(SnapMeta {
        root: PathBuf::from(ROOT),
        volume: VolumeKey {
            serial: 0,
            fs: FsKind::Ntfs,
        },
        scanned_at: 1,
        took_ms: 1,
        size_mode: SizeMode::Disk,
        track_min: 1 << 20,
        flags: 0,
    });
    let reg = DuRegistry::new();
    let id = reg.open_snapshot(snap, false);
    reg.fix(id);
    (reg, id, [a, locked, b, c])
}

fn sum(path: &str, disk: u64, denied: u32) -> RootSum {
    let mut types_disk = [0; DuType::COUNT];
    let mut types_files = [0; DuType::COUNT];
    types_disk[DuType::Video as usize] = disk;
    types_files[DuType::Video as usize] = 4;
    RootSum {
        path: path.into(),
        ok: true,
        disk,
        logical: disk / 2,
        files: 4,
        dirs: 1,
        denied,
        types_disk,
        types_files,
    }
}

fn flags(reg: &DuRegistry, id: SnapId, d: DirId) -> NodeFlags {
    reg.frozen(id).unwrap().snap.dirs[d as usize].flags
}

#[test]
fn denied_paths_and_counts() {
    let (reg, id, _) = setup();
    assert_eq!(
        reg.denied_paths(id),
        [PathBuf::from(r"C:\T\a\locked"), PathBuf::from(r"C:\T\b")]
    );
    assert_eq!(reg.denied_under(&location(id, Path::new(""))), 2);
    assert_eq!(reg.denied_under(&location(id, Path::new("a"))), 1);
    assert_eq!(reg.denied_under(&location(id, Path::new("c"))), 0);
}

#[test]
fn merge_moves_sizes_into_snapshot() {
    let (reg, id, [a, locked, b, _]) = setup();
    let root = location(id, Path::new(""));
    let before = reg.info(&root).unwrap();
    assert_eq!(before.denied, 2);
    // `b` не досчитан (`ok == false`) и чужой путь — без изменений.
    let mut not_ok = sum(r"C:\T\b", 7, 0);
    not_ok.ok = false;
    let (n, bytes) = reg.merge_elevated(
        id,
        &[sum(r"C:\T\a\locked", 1000, 0), not_ok, sum(r"C:\T\c", 5, 0)],
    );
    assert_eq!((n, bytes), (1, 1000));
    let after = reg.info(&root).unwrap();
    assert_eq!(after.denied, 1);
    assert_eq!(after.bytes, before.bytes + 1000);
    assert_eq!(after.files, before.files + 4);
    let f = reg.frozen(id).unwrap();
    assert_eq!(f.snap.dirs[a as usize].disk, 1100);
    assert_eq!(f.snap.dirs[a as usize].logical, 600);
    assert_eq!(f.snap.types.disk[DuType::Video as usize], 1000);
    let fl = flags(&reg, id, locked);
    assert!(fl.has(NodeFlags::ELEVATED | NodeFlags::DONE));
    assert!(!fl.has(NodeFlags::DENIED));
    // `a` больше не «частично без доступа», корень — да (`b`).
    assert!(!flags(&reg, id, a).has(NodeFlags::PARTIAL_DENIED));
    assert!(flags(&reg, id, 0).has(NodeFlags::PARTIAL_DENIED));
    assert!(flags(&reg, id, b).has(NodeFlags::DENIED));
    assert_eq!(reg.denied_paths(id), [PathBuf::from(r"C:\T\b")]);
    // Повтор того же ответа — уже не «нет доступа».
    assert_eq!(
        reg.merge_elevated(id, &[sum(r"C:\T\a\locked", 1000, 0)]),
        (0, 0)
    );
    assert!(reg.take_dirty(id).is_some(), "снимок — в кэш");
}

#[test]
fn inner_denied_stays_partial() {
    let (reg, id, [_, _, b, _]) = setup();
    let (n, _) = reg.merge_elevated(id, &[sum(r"C:\T\b", 50, 2), sum(r"C:\T\a\locked", 1, 0)]);
    assert_eq!(n, 2);
    assert_eq!(reg.info(&location(id, Path::new(""))).unwrap().denied, 0);
    assert_eq!(reg.denied_paths(id), Vec::<PathBuf>::new());
    assert!(flags(&reg, id, b).has(NodeFlags::ELEVATED | NodeFlags::PARTIAL_DENIED));
    assert!(flags(&reg, id, 0).has(NodeFlags::PARTIAL_DENIED));
}

#[test]
fn banner_and_job_state() {
    let (reg, id, _) = setup();
    let root = location(id, Path::new(""));
    assert!(!reg.info(&root).unwrap().elev.banner_closed);
    assert!(reg.close_banner(id));
    assert!(!reg.close_banner(id));
    assert!(reg.info(&root).unwrap().elev.banner_closed);
    reg.set_elev_job(id, Some(urfin_core::JobId(7)));
    assert!(reg.info(&root).unwrap().elev.running);
    assert_eq!(reg.elev_job(id), Some(urfin_core::JobId(7)));
}
