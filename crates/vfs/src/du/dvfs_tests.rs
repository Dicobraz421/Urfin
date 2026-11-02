//! `DuVfs` и реестр анализов (F01-05, F01-06): адреса, листинг из снимка, вычитание
//! удалённого без пересчёта.

use std::fs;
use std::path::Path;

use urfin_core::{Attrs, Entry, EntryKind, Location};

use super::{DuRegistry, DuVfs, location, parse};
use crate::api::{ListOpts, Vfs};
use crate::du::model::{DuSnapshot, DuTree, NodeFlags, SizeMode};
use crate::du::scan::{ScanCfg, scan};
use crate::local::LocalFs;

/// Дерево: `a/` (2 файла + `a/deep/` с файлом), `b/` (1 файл), файл в корне.
fn sample(root: &Path) {
    fs::create_dir_all(root.join("a").join("deep")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::write(root.join("a").join("one.bin"), vec![0u8; 1000]).unwrap();
    fs::write(root.join("a").join("two.bin"), vec![0u8; 500]).unwrap();
    fs::write(root.join("a").join("deep").join("x.bin"), vec![0u8; 300]).unwrap();
    fs::write(root.join("b").join("y.bin"), vec![0u8; 70]).unwrap();
    fs::write(root.join("top.txt"), vec![0u8; 9]).unwrap();
}

/// Снимок `root` с логическими размерами (детерминированы, в отличие от кластеров).
fn snapshot(root: &Path) -> DuSnapshot {
    let cfg = ScanCfg {
        size_mode: SizeMode::Logical,
        ..ScanCfg::default()
    };
    let tree = DuTree::new();
    let out = scan(
        &Location::local(root),
        &LocalFs::new(),
        &cfg,
        &tree,
        &|| false,
        None,
        |_| {},
    );
    out.snapshot
}

fn list(v: &DuVfs, loc: &Location) -> Vec<Entry> {
    let mut s = v.list(loc, &ListOpts::default()).unwrap();
    let mut out = Vec::new();
    while s
        .next_batch(&mut out, &urfin_core::CancelToken::new())
        .unwrap()
    {}
    out
}

fn get<'a>(list: &'a [Entry], name: &str) -> &'a Entry {
    list.iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} not in {list:?}"))
}

#[test]
fn location_roundtrip() {
    let loc = location(3, Path::new("target").join("debug").as_path());
    assert_eq!(loc.backend, "du");
    let (id, rel) = parse(&loc).unwrap();
    assert_eq!(id, 3);
    assert_eq!(rel, ["target", "debug"]);
    assert_eq!(parse(&location(0, Path::new(""))).unwrap().1.len(), 0);
    assert!(parse(&Location::local(r"C:\x")).is_none());
    let uri = loc.to_uri();
    assert_eq!(Location::parse_uri(&uri).unwrap(), loc, "{uri}");
}

/// Колонки зависят от вида в адресе (`Vfs::columns(dir)`, решение 2026-11-02): у
/// «Типов» — «Файлов», у «Временных» — без даты, в папке после фильтра — снова все.
#[test]
fn columns_by_view() {
    use super::{View, location_view};
    use crate::du::model::DuFilter;
    let v = DuVfs::new(DuRegistry::new());
    let ids = |loc: &Location| v.columns(loc).iter().map(|c| c.id).collect::<Vec<_>>();
    assert_eq!(ids(&location(0, Path::new(""))), ["share", "size", "mtime"]);
    assert_eq!(
        ids(&location_view(0, &View::new(DuFilter::Types))),
        ["files", "share", "size"]
    );
    assert_eq!(
        ids(&location_view(0, &View::new(DuFilter::Temp))),
        ["share", "size"]
    );
    let inside = Location {
        path: location_view(0, &View::new(DuFilter::Temp))
            .path
            .join("target"),
        ..location(0, Path::new(""))
    };
    assert_eq!(ids(&inside), ["share", "size", "mtime"]);
}

#[test]
fn lists_subdirs_from_snapshot_and_files_live() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let reg = DuRegistry::new();
    let id = reg.open_snapshot(snapshot(tmp.path()), false);
    let v = DuVfs::new(reg.clone());
    let root = list(&v, &location(id, Path::new("")));
    assert_eq!(root.len(), 3);
    let a = get(&root, "a");
    assert_eq!(a.size, 1800);
    assert!(a.attrs.has(Attrs::SIZE_KNOWN));
    assert!(!a.attrs.has(Attrs::SIZE_PARTIAL));
    assert_eq!(get(&root, "b").size, 70);
    assert_eq!(get(&root, "top.txt").size, 9);
    assert_eq!(get(&root, "top.txt").kind, EntryKind::File);
    let deep = list(&v, &location(id, Path::new("a")));
    assert_eq!(get(&deep, "deep").size, 300);
    let info = reg.info(&location(id, Path::new("a"))).unwrap();
    assert_eq!(info.files, 5);
    assert_eq!(info.bytes, 1879);
    assert_eq!(info.dir, tmp.path().join("a"));
    assert!(!info.scanning && !info.partial && !info.from_cache);
    // Настоящий путь — для F3/F4/F8.
    assert_eq!(
        v.local_path(&location(id, Path::new("a").join("one.bin").as_path())),
        Some(tmp.path().join("a").join("one.bin"))
    );
    assert!(v.local_path(&location(id + 1, Path::new(""))).is_none());
}

/// Недосчитанный каталог (скан остановлен) — `SIZE_PARTIAL`, «≥ X».
#[test]
fn unfinished_dir_is_partial() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let mut snap = snapshot(tmp.path());
    let a = (0u32..)
        .zip(&snap.dirs)
        .find(|(id, _)| snap.name(*id) == "a")
        .map(|(id, _)| id)
        .unwrap();
    snap.dirs[a as usize].flags = NodeFlags(0);
    snap.flags |= DuSnapshot::PARTIAL;
    let reg = DuRegistry::new();
    let id = reg.open_snapshot(snap, true);
    let root = list(&DuVfs::new(reg.clone()), &location(id, Path::new("")));
    assert!(get(&root, "a").attrs.has(Attrs::SIZE_PARTIAL));
    assert!(!get(&root, "b").attrs.has(Attrs::SIZE_PARTIAL));
    let info = reg.info(&location(id, Path::new(""))).unwrap();
    assert!(info.partial && info.from_cache);
}

/// F01-06: удалённое после скана вычитается из каталога и предков без пересчёта;
/// снимок помечен для записи в кэш, повторный листинг не вычитает второй раз.
#[test]
fn deleted_items_are_subtracted_from_ancestors() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let reg = DuRegistry::new();
    let id = reg.open_snapshot(snapshot(tmp.path()), false);
    let v = DuVfs::new(reg.clone());
    let at = |rel: &str| location(id, Path::new(rel));
    // Каталог показан (прошлый листинг — база сравнения), затем файл удалён.
    list(&v, &at("a"));
    fs::remove_file(tmp.path().join("a").join("one.bin")).unwrap();
    let a = list(&v, &at("a"));
    assert!(a.iter().all(|e| e.name != "one.bin"));
    assert_eq!(get(&list(&v, &at("")), "a").size, 800);
    assert_eq!(reg.info(&at("")).unwrap().bytes, 879);
    assert_eq!(reg.info(&at("")).unwrap().files, 4);
    assert_eq!(reg.dirty(), [id]);
    assert!(reg.take_dirty(id).is_some());
    assert_eq!(reg.dirty(), [0u32; 0]);
    // Каталог удалён целиком: уходит из списка, суммы предков — без него.
    fs::remove_dir_all(tmp.path().join("a").join("deep")).unwrap();
    let a = list(&v, &at("a"));
    assert!(a.iter().all(|e| e.name != "deep"));
    assert_eq!(reg.info(&at("")).unwrap().bytes, 579);
    list(&v, &at("a"));
    list(&v, &at(""));
    assert_eq!(
        reg.info(&at("")).unwrap().bytes,
        579,
        "no double subtraction"
    );
    // Снимок из кэша с обнулённым узлом не вычитает его снова.
    let snap = reg.take_dirty(id).unwrap();
    let again = DuRegistry::new();
    let id2 = again.open_snapshot(snap, true);
    let v2 = DuVfs::new(again.clone());
    list(&v2, &location(id2, Path::new("a")));
    assert_eq!(
        again.info(&location(id2, Path::new(""))).unwrap().bytes,
        579
    );
}

/// Во время скана: живое дерево, подкаталоги «≥ X».
#[test]
fn live_tree_lists_partial_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let tree = std::sync::Arc::new(DuTree::new());
    let reg = DuRegistry::new();
    let id = reg.open_live(
        tmp.path().to_path_buf(),
        std::sync::Arc::clone(&tree),
        urfin_core::JobId(7),
    );
    let v = DuVfs::new(reg.clone());
    let root = list(&v, &location(id, Path::new("")));
    assert!(get(&root, "a").attrs.has(Attrs::SIZE_PARTIAL));
    let info = reg.info(&location(id, Path::new(""))).unwrap();
    assert!(info.scanning && info.partial);
    assert_eq!(reg.job(id), Some(urfin_core::JobId(7)));
    reg.finish(id, snapshot(tmp.path()));
    assert_eq!(reg.job(id), None);
    let root = list(&v, &location(id, Path::new("")));
    assert_eq!(get(&root, "a").size, 1800);
    assert!(!get(&root, "a").attrs.has(Attrs::SIZE_PARTIAL));
    assert_eq!(reg.find(tmp.path()), Some(id));
}
