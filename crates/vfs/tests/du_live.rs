//! Живые изменения анализа места (F01-10, F01 §3.3): наблюдение поддерева, пачки
//! событий, перелистывание и прививка новых поддеревьев; снятие наблюдения.
#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use urfin_core::Location;
use urfin_vfs::LocalFs;
use urfin_vfs::du::fresh::{Live, LiveCfg};
use urfin_vfs::du::{DuRegistry, DuTree, ScanCfg, SizeMode, scan};

fn cfg() -> ScanCfg {
    ScanCfg {
        size_mode: SizeMode::Logical,
        ..ScanCfg::default()
    }
}

fn start(root: &Path) -> (Arc<DuRegistry>, u32, Live, Arc<AtomicUsize>) {
    fs::create_dir_all(root.join("a").join("deep")).unwrap();
    fs::write(root.join("a").join("one.bin"), vec![0u8; 1000]).unwrap();
    let tree = DuTree::new();
    let snap = scan(
        &Location::local(root),
        &LocalFs::new(),
        &cfg(),
        &tree,
        &|| false,
        None,
        |_| {},
    )
    .snapshot;
    let reg = DuRegistry::new();
    let id = reg.open_snapshot(snap, false);
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    let live = Live::start(
        Arc::clone(&reg),
        id,
        root.to_path_buf(),
        Arc::new(LocalFs::new()),
        LiveCfg::new(cfg()),
        Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    // Наблюдение ставится в потоке: дать ему открыть каталог.
    std::thread::sleep(Duration::from_millis(200));
    (reg, id, live, hits)
}

fn total(reg: &DuRegistry, id: u32) -> (u64, u32) {
    let i = reg
        .info(&urfin_vfs::du::dvfs::location(id, Path::new("")))
        .unwrap();
    (i.bytes, u32::try_from(i.files).unwrap())
}

fn wait(what: &str, limit: Duration, ok: impl Fn() -> bool) -> Duration {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < limit, "timeout: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
    t0.elapsed()
}

#[test]
fn sizes_follow_changes_and_watch_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("w");
    fs::create_dir_all(&root).unwrap();
    let (reg, id, live, hits) = start(&root);
    assert_eq!(total(&reg, id), (1000, 1));
    // Файл в глубине: предки обновляются ≤ 1 с (приёмка; с запасом на медленный CI).
    fs::write(
        root.join("a").join("deep").join("big.bin"),
        vec![0u8; 3 << 20],
    )
    .unwrap();
    let took = wait("added file", Duration::from_secs(3), || {
        total(&reg, id) == (1000 + (3 << 20), 2)
    });
    eprintln!("added: {took:?}");
    assert!(hits.load(Ordering::SeqCst) > 0);
    // Новое поддерево с файлами — досчитано и привито.
    let n = root.join("n").join("m");
    fs::create_dir_all(&n).unwrap();
    for i in 0..50 {
        fs::write(n.join(format!("f{i}.txt")), vec![0u8; 100]).unwrap();
    }
    wait("new subtree", Duration::from_secs(3), || {
        total(&reg, id) == (1000 + (3 << 20) + 5000, 52)
    });
    // Удаление.
    fs::remove_file(root.join("a").join("deep").join("big.bin")).unwrap();
    fs::remove_dir_all(root.join("n")).unwrap();
    let took = wait("removed", Duration::from_secs(3), || {
        total(&reg, id) == (1000, 1)
    });
    eprintln!("removed: {took:?}");
    // Снять: поток выходит, каталог можно удалить.
    live.join();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn burst_of_events_settles_to_scan_total() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("w");
    fs::create_dir_all(&root).unwrap();
    let (reg, id, live, _) = start(&root);
    let d = root.join("a").join("deep");
    for i in 0..3000 {
        fs::write(d.join(format!("f{i}.txt")), b"0123456789").unwrap();
    }
    for i in 0..1000 {
        fs::remove_file(d.join(format!("f{i}.txt"))).unwrap();
    }
    wait("burst", Duration::from_secs(10), || {
        total(&reg, id) == (1000 + 2000 * 10, 2001) || !reg.changed(id).is_empty()
    });
    // Переполнение буфера — «●» у всего снимка; иначе суммы как у скана.
    if reg.changed(id).is_empty() {
        assert_eq!(total(&reg, id), (1000 + 2000 * 10, 2001));
    }
    assert!(live.running());
    drop(live);
}
