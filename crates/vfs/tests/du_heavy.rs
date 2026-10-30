//! Тяжёлая приёмка F01-03 (стенд, `#[ignore]`, release): 1M файлов (`gen_tree`) —
//! итог = `dir /s /a`, отмена ≤ 100 мс, потоков после скана не прибавилось, пик своей
//! кучи ≤ 40 МБ (считающий аллокатор вместо `count-alloc`), первый тик < 100 мс.
//!
//! `cargo nextest run -p urfin-vfs --release --run-ignored only -E 'binary(du_heavy)'`

#![cfg(windows)]
#![allow(unsafe_code, reason = "тестовый GlobalAlloc — обёртка над System")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use urfin_core::{CancelToken, Location};
use urfin_testdata::{Layout as TreeLayout, TreeSpec, dataset};
use urfin_vfs::LocalFs;
use urfin_vfs::du::{DuSnapshot, DuTree, NodeFlags, ScanCfg, scan};

struct Counting;

static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(n: usize) {
    let now = CUR.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: делегирует System; счётчики — атомики без аллокаций.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        // SAFETY: тот же контракт, что у вызывающего.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CUR.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` выделен `System` с тем же `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        if new > layout.size() {
            grow(new - layout.size());
        } else {
            CUR.fetch_sub(layout.size() - new, Ordering::Relaxed);
        }
        // SAFETY: тот же контракт, что у вызывающего.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const MIB: usize = 1 << 20;

/// 1M записей `gen_tree` в 1000 каталогах (+1% пустых подкаталогов, 0,5% ссылок).
fn dataset_1m() -> PathBuf {
    let spec = TreeSpec {
        n: 1_000_000,
        seed: 7,
        layout: TreeLayout::Nav { dirs: 1000 },
    };
    dataset("urfin", "bench-scan-1m", &spec).unwrap()
}

/// `dir /s /a /-c`: (файлов, байт).
fn dir_s(p: &Path) -> (u64, u64) {
    let out = Command::new("cmd")
        .args(["/C", "dir", "/S", "/A", "/-C"])
        .arg(p)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let nums: Vec<u64> = lines[lines.len() - 2]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap())
        .collect();
    (nums[0], nums[1])
}

/// Потоков процесса (PowerShell: без новых фич windows-sys).
fn threads() -> usize {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command"])
        .arg(format!(
            "(Get-Process -Id {}).Threads.Count",
            std::process::id()
        ))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn run(root: &Path, cancel: &CancelToken) -> (urfin_vfs::du::ScanOutcome, usize) {
    let tree = DuTree::new();
    let base = CUR.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let out = scan(
        &Location::local(root),
        &LocalFs::new(),
        &ScanCfg::default(),
        &tree,
        &|| cancel.is_cancelled(),
        None,
        |_| {},
    );
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    (out, peak)
}

#[test]
#[ignore = "стенд: 1M файлов, release"]
fn scan_1m_files() {
    let root = dataset_1m();
    let t0 = Instant::now();
    let (files, bytes) = dir_s(&root);
    let dir_ms = t0.elapsed().as_millis();
    let threads0 = threads();

    // Тёплый проход (метаданные в кэше ОС) — итог, пик памяти, первый тик.
    let (out, peak) = run(&root, &CancelToken::new());
    let s = &out.snapshot;
    let r = s.dirs[0];
    let links = files - u64::from(r.files);
    eprintln!(
        "bench-scan-1m: dir /s /a {dir_ms} ms ({files} files incl. {links} symlinks, {bytes} B); \
         scan {} ms, {} dirs, peak heap {} KiB, first rows {:?}",
        s.took_ms,
        s.dirs.len(),
        peak / 1024,
        out.first_rows,
    );
    assert_eq!(r.logical, bytes, "logical total = dir /s /a");
    assert!(r.flags.has(NodeFlags::DONE));
    assert_eq!(s.flags & DuSnapshot::PARTIAL, 0);
    assert!(peak <= 40 * MIB, "peak heap {peak} B");
    let first = out.first_rows.expect("first rows");
    assert!(first < Duration::from_millis(100), "first rows {first:?}");
    drop(out);

    // Отмена посреди скана (тёплый скан — сотни мс: отмена через 100 мс).
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        c2.cancel();
        Instant::now()
    });
    let (out, _) = run(&root, &cancel);
    let lag = canceller.join().unwrap().elapsed();
    eprintln!("bench-scan-1m: cancel lag {lag:?}");
    assert!(lag <= Duration::from_millis(100), "cancel lag {lag:?}");
    assert_ne!(out.snapshot.flags & DuSnapshot::PARTIAL, 0);

    std::thread::sleep(Duration::from_millis(200));
    let threads1 = threads();
    eprintln!("bench-scan-1m: threads before {threads0}, after {threads1}");
    assert!(threads1 <= threads0, "threads {threads0} -> {threads1}");
}
