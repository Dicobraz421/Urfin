//! Листеры и обход анализа места (F01-02): сумма = `dir /s /a`, жёсткие ссылки,
//! junction-цикл, сжатые файлы, «нет доступа», отмена `walk_with`.

#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use urfin_core::{CancelToken, EntryKind, Location};
use urfin_vfs::du::{DuSnapshot, ListerKind, NodeFlags, ScanCfg, ScanOutcome, scan};
use urfin_vfs::walk::{
    DirLister, DirRef, DirVisitor, Parent, RawEntry, RawName, WalkOpts, walk_with,
};
use urfin_vfs::{LocalFs, VfsError};

/// Дерево: `depth` уровней по `fan` каталогов, в каждом — файлы разных размеров.
fn tree(root: &Path, depth: u32, fan: u32) {
    fs::create_dir_all(root).unwrap();
    for i in 0..5u32 {
        fs::write(
            root.join(format!("f{i}.bin")),
            vec![0u8; ((i + 1) * 37 + depth * 1000) as usize],
        )
        .unwrap();
    }
    if depth > 0 {
        for d in 0..fan {
            tree(&root.join(format!("d{d}")), depth - 1, fan);
        }
    }
}

/// `dir /s /a /-c`: (файлов, байт) из итоговой строки (без разделителей разрядов —
/// не зависит от локали, кроме слов).
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
    // Хвост: «N файлов X байт», «M папок Y байт свободно».
    let nums: Vec<u64> = lines[lines.len() - 2]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap())
        .collect();
    (nums[0], nums[1])
}

fn run(root: &Path, lister: ListerKind) -> ScanOutcome {
    let cfg = ScanCfg {
        lister,
        threads: 4,
        ..ScanCfg::default()
    };
    let tree = urfin_vfs::du::DuTree::new();
    scan(
        &Location::local(root),
        &LocalFs::new(),
        &cfg,
        &tree,
        &|| false,
        None,
        |_| {},
    )
}

fn root_of(s: &DuSnapshot) -> urfin_vfs::du::DuNode {
    s.dirs[0]
}

/// Приёмка: логический итог = `dir /s /a` всеми листерами; дерево досчитано.
#[test]
fn sum_matches_dir_s() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t");
    tree(&root, 3, 3);
    fs::create_dir(root.join("empty")).unwrap();
    let (files, bytes) = dir_s(&root);
    for kind in [
        ListerKind::Auto,
        ListerKind::Nt,
        ListerKind::Find,
        ListerKind::Vfs,
    ] {
        let out = run(&root, kind);
        let r = root_of(&out.snapshot);
        assert_eq!(r.logical, bytes, "{kind:?}");
        assert_eq!(u64::from(r.files), files, "{kind:?}");
        assert!(r.flags.has(NodeFlags::DONE), "{kind:?}");
        assert!(
            out.snapshot
                .dirs
                .iter()
                .all(|d| d.flags.has(NodeFlags::DONE))
        );
        assert_eq!(out.snapshot.dirs.len(), 1 + 3 + 9 + 27 + 1, "{kind:?}");
        assert_eq!(out.errors, 0);
        // Find — округление до кластера, Vfs — логический; у Nt мелкие файлы могут
        // жить в MFT (0 на диске).
        if matches!(kind, ListerKind::Find | ListerKind::Vfs) {
            assert!(r.disk >= r.logical, "{kind:?}");
        }
        let approx = out.snapshot.flags & DuSnapshot::APPROX != 0;
        assert_eq!(approx, kind == ListerKind::Find, "{kind:?}");
    }
    // Подкаталог: агрегат = его `dir /s`.
    let out = run(&root, ListerKind::Nt);
    let s = &out.snapshot;
    let d0 = s
        .dirs
        .iter()
        .find(|d| d.parent == 0 && name_of(s, d) == "d0")
        .unwrap();
    assert_eq!(d0.logical, dir_s(&root.join("d0")).1);
}

fn name_of<'a>(s: &'a DuSnapshot, d: &urfin_vfs::du::DuNode) -> &'a str {
    std::str::from_utf8(&s.names[d.name.range()]).unwrap()
}

/// Приёмка: жёсткая ссылка ×3 — размер учтён один раз (NTFS).
#[test]
fn hard_links_counted_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    let f = root.join("a").join("data.bin");
    fs::write(&f, vec![7u8; 100_000]).unwrap();
    fs::hard_link(&f, root.join("b").join("link1.bin")).unwrap();
    fs::hard_link(&f, root.join("link2.bin")).unwrap();
    let out = run(&root, ListerKind::Nt);
    let r = root_of(&out.snapshot);
    assert_eq!(r.logical, 100_000);
    assert_eq!(r.files, 3, "names are counted");
    // Без FileId (FindFirstFileExW) дедупа нет — это и помечено APPROX.
    let approx = run(&root, ListerKind::Find);
    assert_eq!(root_of(&approx.snapshot).logical, 300_000);
}

/// Приёмка: junction-цикл не зацикливает; у каталога со ссылкой — `LINK_SKIPPED`.
#[test]
fn junction_loop_terminates() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t");
    tree(&root, 1, 2);
    let lp = root.join("d0").join("loop");
    let made = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&lp)
        .arg(&root)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(made, "cannot create a junction");
    let want = dir_s_without_links(&root);
    for kind in [ListerKind::Nt, ListerKind::Find, ListerKind::Vfs] {
        let t0 = Instant::now();
        let out = run(&root, kind);
        assert!(t0.elapsed() < Duration::from_secs(10));
        let s = &out.snapshot;
        assert_eq!(root_of(s).logical, want, "{kind:?}");
        let d0 = s.dirs.iter().find(|d| name_of(s, d) == "d0").unwrap();
        assert!(d0.flags.has(NodeFlags::LINK_SKIPPED), "{kind:?}");
    }
}

/// Эталон без захода в точки повторной обработки.
fn dir_s_without_links(p: &Path) -> u64 {
    use std::os::windows::fs::MetadataExt;
    let mut sum = 0;
    for e in fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        let md = fs::symlink_metadata(e.path()).unwrap();
        if md.file_attributes() & 0x400 != 0 {
            continue;
        }
        if md.is_dir() {
            sum += dir_s_without_links(&e.path());
        } else {
            sum += md.len();
        }
    }
    sum
}

/// Приёмка: сжатый NTFS-файл — на диске меньше логического.
#[test]
fn compressed_file_disk_below_logical() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t");
    fs::create_dir_all(&root).unwrap();
    let f = root.join("zeros.bin");
    fs::write(&f, vec![0u8; 4 << 20]).unwrap();
    let ok = Command::new("compact")
        .args(["/C", "/Q"])
        .arg(&f)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "compact /c failed");
    for kind in [ListerKind::Nt, ListerKind::Find] {
        let out = run(&root, kind);
        let s = &out.snapshot;
        let r = root_of(s);
        assert_eq!(r.logical, 4 << 20, "{kind:?}");
        assert!(
            r.disk < r.logical,
            "{kind:?}: disk {} logical {}",
            r.disk,
            r.logical
        );
        let big = &s.files[0];
        assert_eq!(s.file_name(big), "zeros.bin");
        assert!(big.disk < big.logical);
        assert_ne!(big.attrs & urfin_vfs::du::FileRec::COMPRESSED, 0);
    }
}

/// Каталог без права на листинг: `DENIED`, у предков `PARTIAL_DENIED`, в `denied`.
#[test]
fn denied_directory_is_marked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t");
    tree(&root, 1, 2);
    let locked = root.join("d1");
    let icacls = |args: &[&str]| {
        Command::new("icacls")
            .arg(&locked)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    assert!(icacls(&["/deny", "*S-1-1-0:(RD)"]), "icacls /deny");
    let out = run(&root, ListerKind::Nt);
    assert!(icacls(&["/remove:d", "*S-1-1-0"]), "icacls /remove");
    let s = &out.snapshot;
    let (id, d1) = (0u32..)
        .zip(&s.dirs)
        .find(|(_, d)| name_of(s, d) == "d1")
        .unwrap();
    assert!(d1.flags.has(NodeFlags::DENIED));
    assert_eq!(d1.files, 0);
    assert!(
        root_of(s)
            .flags
            .has(NodeFlags::PARTIAL_DENIED | NodeFlags::DONE)
    );
    assert_eq!(s.denied, vec![id]);
}

/// Бесконечное дерево с медленным листингом.
struct Endless;

impl DirLister for Endless {
    fn list(
        &self,
        _dir: &DirRef,
        out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
    ) -> Result<Option<Parent>, VfsError> {
        std::thread::sleep(Duration::from_millis(5));
        for (name, kind) in [
            ("a", EntryKind::Dir),
            ("b", EntryKind::Dir),
            ("c", EntryKind::Dir),
            ("f", EntryKind::File),
        ] {
            let e = RawEntry {
                name: RawName::Str {
                    name,
                    escaped: false,
                },
                kind,
                logical: 1,
                disk: 1,
                mtime: 0,
                file_id: 0,
                flags: 0,
            };
            if !out(&e) {
                break;
            }
        }
        Ok(None)
    }
}

struct Count(std::sync::atomic::AtomicU64);

impl DirVisitor for Count {
    type Acc = ();
    fn entry(&self, _dir: &DirRef, (): &mut (), e: &RawEntry<'_>) -> Option<u32> {
        self.0.fetch_add(1, Ordering::Relaxed);
        (e.kind == EntryKind::Dir).then_some(0)
    }
    fn done(&self, _dir: &DirRef, (): (), _res: Result<(), VfsError>) {}
}

/// `walk_with`: отмена ≤ 100 мс на бесконечном дереве; тики идут.
#[test]
fn walk_with_cancel_is_quick() {
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let cancelled_at = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c2.cancel();
        Instant::now()
    });
    let ticked = AtomicBool::new(false);
    let v = Count(std::sync::atomic::AtomicU64::new(0));
    walk_with(
        &Endless,
        vec![DirRef::root(Location::local("C:\\endless"), 0)],
        WalkOpts {
            threads: 4,
            first_tick: Duration::from_millis(10),
            period: Duration::from_millis(50),
        },
        &|| cancel.is_cancelled(),
        &v,
        || ticked.store(true, Ordering::Relaxed),
    );
    let lag = cancelled_at.join().unwrap().elapsed();
    assert!(
        lag <= Duration::from_millis(100),
        "walk_with ended {lag:?} after cancel"
    );
    assert!(ticked.load(Ordering::Relaxed));
    assert!(v.0.load(Ordering::Relaxed) > 10);
}

/// Приёмка (стенд с OneDrive): облачные заглушки не гидратируются, на диске — 0.
/// Каталог — `URFIN_TEST_CLOUD_DIR` (файлы «только в сети»).
#[test]
#[ignore = "стенд: OneDrive с файлами «только в сети» (URFIN_TEST_CLOUD_DIR)"]
fn cloud_placeholders_stay_dehydrated() {
    use std::os::windows::fs::MetadataExt;
    const RECALL: u32 = 0x40_0000;
    let Some(dir) = std::env::var_os("URFIN_TEST_CLOUD_DIR") else {
        panic!("set URFIN_TEST_CLOUD_DIR");
    };
    let dir = Path::new(&dir);
    let online: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.metadata()
                .is_ok_and(|m| m.file_attributes() & RECALL != 0)
        })
        .map(|e| e.path())
        .collect();
    assert!(
        !online.is_empty(),
        "no online-only files in {}",
        dir.display()
    );
    let out = run(dir, ListerKind::Auto);
    for p in &online {
        let a = fs::symlink_metadata(p).unwrap().file_attributes();
        assert_ne!(a & RECALL, 0, "hydrated: {}", p.display());
    }
    assert!(root_of(&out.snapshot).disk < root_of(&out.snapshot).logical);
}
