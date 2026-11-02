//! Правка снимка по перелистыванию (F01-10, F01-11): файлы, новые и исчезнувшие
//! подкаталоги, крупные файлы, «●», повторное перелистывание ничего не меняет.

use std::fs;
use std::path::Path;

use urfin_core::Location;

use super::super::DuRegistry;
use super::{Listing, attribute};
use crate::du::fresh::{self, Relister};
use crate::du::model::{DirId, DuSnapshot, DuTree, DuType, NodeFlags, SizeMode};
use crate::du::scan::{ScanCfg, scan};
use crate::local::LocalFs;

/// `a/` (2 файла + `a/deep/x.bin`), `b/y.bin`, `top.txt`.
fn sample(root: &Path) {
    fs::create_dir_all(root.join("a").join("deep")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::write(root.join("a").join("one.bin"), vec![0u8; 1000]).unwrap();
    fs::write(root.join("a").join("two.bin"), vec![0u8; 500]).unwrap();
    fs::write(root.join("a").join("deep").join("x.bin"), vec![0u8; 300]).unwrap();
    fs::write(root.join("b").join("y.bin"), vec![0u8; 70]).unwrap();
    fs::write(root.join("top.txt"), vec![0u8; 9]).unwrap();
}

fn cfg() -> ScanCfg {
    ScanCfg {
        size_mode: SizeMode::Logical,
        // `one.bin` — крупный.
        track_min: 800,
        ..ScanCfg::default()
    }
}

fn snapshot(root: &Path) -> DuSnapshot {
    let tree = DuTree::new();
    scan(
        &Location::local(root),
        &LocalFs::new(),
        &cfg(),
        &tree,
        &|| false,
        None,
        |_| {},
    )
    .snapshot
}

fn open(root: &Path) -> (std::sync::Arc<DuRegistry>, u32) {
    let reg = DuRegistry::new();
    let id = reg.open_snapshot(snapshot(root), false);
    (reg, id)
}

fn node(reg: &DuRegistry, id: u32, rel: &str) -> crate::du::model::DuNode {
    let f = reg.frozen(id).unwrap();
    let d = f.nearest(Path::new(rel));
    f.snap.dirs[d as usize]
}

fn dir(reg: &DuRegistry, id: u32, rel: &str) -> DirId {
    reg.nearest(id, Path::new(rel)).unwrap()
}

/// Перелистать каталоги `rels` через `fresh::relist` (новые поддеревья — сканом).
fn relist(reg: &DuRegistry, id: u32, root: &Path, rels: &[&str]) -> usize {
    let mut dirs: Vec<(DirId, Vec<DuType>)> =
        rels.iter().map(|r| (dir(reg, id, r), Vec::new())).collect();
    dirs.sort_unstable_by_key(|(d, _)| *d);
    let rl = Relister::for_root(root, cfg().track_min);
    fresh::relist(reg, id, &dirs, &rl, &LocalFs::new(), &cfg(), &|| false)
}

#[test]
fn added_and_removed_files_reach_ancestors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sample(root);
    let (reg, id) = open(root);
    let before = node(&reg, id, "");
    assert_eq!(before.logical, 1879);
    fs::write(root.join("a").join("new.txt"), vec![0u8; 2000]).unwrap();
    fs::remove_file(root.join("a").join("two.bin")).unwrap();
    assert_eq!(relist(&reg, id, root, &["a"]), 1);
    let a = node(&reg, id, "a");
    assert_eq!(a.logical, 1000 + 2000 + 300);
    assert_eq!(a.files, 3);
    let r = node(&reg, id, "");
    assert_eq!(r.logical, 1879 - 500 + 2000);
    assert_eq!(r.files, before.files);
    // Повторно — ничего не меняется.
    relist(&reg, id, root, &["a"]);
    assert_eq!(node(&reg, id, "").logical, 1879 - 500 + 2000);
    // Новый крупный файл — в списке крупных, по размеру ↓.
    let f = reg.frozen(id).unwrap();
    let names: Vec<&str> = f.snap.files.iter().map(|r| f.snap.file_name(r)).collect();
    assert_eq!(names, ["new.txt", "one.bin"]);
    assert!(reg.take_dirty(id).is_some(), "snapshot must be stored");
}

#[test]
fn new_subtree_grafted_and_gone_dir_subtracted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sample(root);
    let (reg, id) = open(root);
    fs::create_dir_all(root.join("a").join("fresh").join("inner")).unwrap();
    fs::write(
        root.join("a").join("fresh").join("inner").join("z.bin"),
        vec![0u8; 900],
    )
    .unwrap();
    fs::remove_dir_all(root.join("b")).unwrap();
    relist(&reg, id, root, &["a", ""]);
    let fresh = node(&reg, id, r"a\fresh\inner");
    assert_eq!(fresh.logical, 900);
    assert_eq!(node(&reg, id, r"a\fresh").logical, 900);
    assert_eq!(node(&reg, id, "a").logical, 1800 + 900);
    let r = node(&reg, id, "");
    assert_eq!(r.logical, 1879 - 70 + 900);
    // `b` нет: ближайший каталог к `b` — корень.
    assert_eq!(dir(&reg, id, "b"), 0);
    // Крупный файл привитого поддерева — в списке.
    let f = reg.frozen(id).unwrap();
    assert!(f.snap.files.iter().any(|r| f.snap.file_name(r) == "z.bin"));
    // Повторный скан того же дерева даёт те же итоги.
    let again = snapshot(root);
    assert_eq!(again.dirs[0].logical, r.logical);
    assert_eq!(again.dirs[0].files, r.files);
}

#[test]
fn changed_marks_and_clears() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sample(root);
    let (reg, id) = open(root);
    let a = dir(&reg, id, "a");
    assert_eq!(reg.mark_changed(id, Some(&[a])), 1);
    assert_eq!(reg.mark_changed(id, Some(&[a])), 0);
    assert_eq!(reg.changed(id), vec![a]);
    relist(&reg, id, root, &["a"]);
    assert_eq!(reg.changed(id), Vec::<u32>::new());
    let n = reg.mark_changed(id, None);
    assert_eq!(n, reg.frozen(id).unwrap().snap.dirs.len());
    assert!(node(&reg, id, "b").flags.has(NodeFlags::CHANGED));
}

#[test]
fn fixture_is_not_watchable() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let (reg, id) = open(tmp.path());
    assert!(reg.watchable(id));
    reg.fix(id);
    assert!(!reg.watchable(id));
}

#[test]
fn types_follow_hint_or_listing() {
    let mut out = [(0i64, 0i64); DuType::COUNT];
    attribute(
        100,
        1,
        &[DuType::Image, DuType::Image],
        &[0; DuType::COUNT],
        &mut out,
    );
    assert_eq!(out[DuType::Image as usize], (100, 1));
    let mut out = [(0i64, 0i64); DuType::COUNT];
    let mut small = [0u64; DuType::COUNT];
    small[DuType::Doc as usize] = 300;
    small[DuType::Code as usize] = 100;
    attribute(-40, -1, &[DuType::Doc, DuType::Code], &small, &mut out);
    assert_eq!(out[DuType::Doc as usize], (-30, -1));
    assert_eq!(out[DuType::Code as usize], (-10, 0));
    let mut out = [(0i64, 0i64); DuType::COUNT];
    attribute(5, 1, &[], &[0; DuType::COUNT], &mut out);
    assert_eq!(out[DuType::Other as usize], (5, 1));
    let _ = Listing::default();
}
