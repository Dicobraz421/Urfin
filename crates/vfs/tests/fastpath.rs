//! Windows fast path (S4-07): `NtQueryDirectoryFile` и `FindFirstFileExW` дают те
//! же записи, что `std::fs`-эталон, — побайтно (имя, kind, size, mtime/ctime/atime, attrs).
#![cfg(windows)]

use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use urfin_core::{CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{ListMode, ListOpts, LocalFs, Vfs};

fn list(dir: &Path, mode: ListMode) -> Vec<Entry> {
    let fs = LocalFs::with_mode(mode);
    let mut s = fs
        .list(&Location::local(dir), &ListOpts::default())
        .unwrap_or_else(|e| panic!("{mode:?} {}: {e}", dir.display()));
    let mut out = Vec::new();
    let cancel = CancelToken::new();
    while s.next_batch(&mut out, &cancel).unwrap() {}
    out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    out
}

fn verbatim(p: &Path) -> PathBuf {
    PathBuf::from(format!(r"\\?\{}", p.display()))
}

fn attrib(flag: &str, p: &Path) {
    assert!(
        Command::new("attrib")
            .arg(flag)
            .arg(p)
            .status()
            .unwrap()
            .success()
    );
}

/// Каталог со всеми особыми случаями; возвращает корень и глубокий подкаталог.
fn tree(root: &Path) -> PathBuf {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    for i in 0..300u32 {
        let name = match i % 6 {
            0 => format!("file{i}.txt"),
            1 => format!("Файл-{i}.md"),
            2 => format!("img_{i:04}.PNG"),
            3 => format!("😀{i}"),
            4 => format!("{}{i}", "long-name-".repeat(5)),
            _ => format!(".dot{i}"),
        };
        let p = root.join(&name);
        fs::write(&p, vec![b'x'; (i as usize * 37) % 5000]).unwrap();
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t + Duration::from_secs(u64::from(i)))
            .unwrap();
    }
    for i in 0..20 {
        fs::create_dir(root.join(format!("dir{i}"))).unwrap();
    }
    attrib("+h", &root.join("file0.txt"));
    attrib("+r", &root.join("img_0002.PNG"));
    // Хвостовые точка и пробел — только через verbatim.
    fs::write(verbatim(&root.join("trailing. ")), b"dot").unwrap();
    // Непарный суррогат.
    let lone = OsString::from_wide(&[0x61, 0xD800, 0x62]);
    fs::write(root.join(&lone), b"s").unwrap();
    // Junction — без прав администратора.
    assert!(
        Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("junction"))
            .arg(root.join("dir1"))
            .output()
            .unwrap()
            .status
            .success()
    );
    // Symlink — если разрешено (режим разработчика).
    let _ = std::os::windows::fs::symlink_file(root.join("file1.txt"), root.join("symlink"));
    // Глубже MAX_PATH.
    let mut deep = root.to_path_buf();
    for _ in 0..4 {
        deep.push("d".repeat(80));
    }
    fs::create_dir_all(verbatim(&deep)).unwrap();
    for i in 0..5 {
        fs::write(verbatim(&deep.join(format!("f{i}"))), b"deep").unwrap();
    }
    deep
}

#[test]
fn fast_paths_match_std_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tree(tmp.path());
    assert!(deep.as_os_str().len() > 260);
    for dir in [tmp.path(), deep.as_path()] {
        let std = list(dir, ListMode::Std);
        let nt = list(dir, ListMode::Nt);
        let find = list(dir, ListMode::Find);
        assert_ne!(std.len(), 0);
        assert_eq!(nt, std, "NtQueryDirectoryFile vs std in {}", dir.display());
        assert_eq!(find, std, "FindFirstFileExW vs std in {}", dir.display());
    }
    let root = list(tmp.path(), ListMode::Nt);
    let get = |n: &str| root.iter().find(|e| e.name == n).unwrap();
    assert_eq!(get("junction").kind, EntryKind::LinkDir);
    assert_eq!(get("trailing. ").size, 3);
    assert!(
        root.iter()
            .any(|e| e.attrs.has(urfin_core::Attrs::ESCAPED_NAME))
    );
}

#[test]
fn auto_mode_uses_fast_path_and_maps_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    let fs = LocalFs::with_mode(ListMode::Nt);
    assert!(matches!(
        fs.list(&Location::local(&missing), &ListOpts::default()),
        Err(urfin_vfs::VfsError::NotFound(_))
    ));
    let file = tmp.path().join("f");
    std::fs::write(&file, b"").unwrap();
    // Файл вместо каталога: в авто-режиме — NotDir через запасной путь.
    let auto = LocalFs::with_mode(ListMode::Auto);
    assert!(matches!(
        auto.list(&Location::local(&file), &ListOpts::default()),
        Err(urfin_vfs::VfsError::NotDir(_))
    ));
}
