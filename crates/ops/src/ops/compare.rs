//! Сравнение каталогов панелей (S5 §4.13, S5-24), как в Far: в каждой панели
//! выделяются файлы, которых нет на другой стороне или которые там старее (и
//! одинакового времени, но другого размера — в обеих). Каталоги не сравниваются.
//!
//! Quick: по листингам — размер и время с допуском по ФС обеих сторон (FAT —
//! 2 с, exFAT — 10 мс). Content: пары одного размера — ещё и xxh3 содержимого
//! (сначала первые 64 КБ, затем целиком).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use compact_str::CompactString;
use urfin_core::Entry;

use crate::names::name_key;

/// Допуск времени изменения по ФС (S5 §4.13): FAT — 2 с, exFAT — 10 мс.
pub(crate) fn time_tolerance(fs: Option<urfin_vfs::volume::FsKind>) -> i64 {
    use urfin_vfs::volume::FsKind;
    match fs {
        Some(FsKind::Fat32) => 2_000_000_000,
        Some(FsKind::ExFat) => 10_000_000,
        _ => 0,
    }
}

/// Итог сравнения.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompareResult {
    pub only_left: u32,
    pub only_right: u32,
    pub newer_left: u32,
    pub newer_right: u32,
    pub differs: u32,
    pub equal: u32,
    /// Что выделить: ключи имён (`name_key`) в левой и правой панели.
    pub select_left: HashSet<CompactString>,
    pub select_right: HashSet<CompactString>,
}

/// Содержимое одинаково (для режима Content): `None` — не прочитать.
pub(crate) type SameContent<'a> = &'a dyn Fn(&str) -> Option<bool>;

/// Сравнение листингов. `same` — сравнение содержимого пары одного размера по
/// имени (Content); `None` — только размер и время (Quick).
pub fn compare(
    left: &[Entry],
    right: &[Entry],
    tol_ns: i64,
    same: Option<SameContent<'_>>,
) -> CompareResult {
    let files = |v: &[Entry]| -> HashMap<CompactString, (CompactString, u64, i64)> {
        v.iter()
            .filter(|e| !e.kind.is_dir_like())
            .map(|e| (name_key(&e.name), (e.name.clone(), e.size, e.mtime)))
            .collect()
    };
    let (l, r) = (files(left), files(right));
    let mut out = CompareResult::default();
    for (k, (name, ls, lt)) in &l {
        let Some((_, rs, rt)) = r.get(k) else {
            out.only_left += 1;
            out.select_left.insert(k.clone());
            continue;
        };
        let dt = i128::from(*lt) - i128::from(*rt);
        let tol = i128::from(tol_ns);
        if dt > tol {
            out.newer_left += 1;
            out.select_left.insert(k.clone());
        } else if dt < -tol {
            out.newer_right += 1;
            out.select_right.insert(k.clone());
        } else if ls != rs || same.is_some_and(|f| f(name) == Some(false)) {
            out.differs += 1;
            out.select_left.insert(k.clone());
            out.select_right.insert(k.clone());
        } else {
            out.equal += 1;
        }
    }
    for k in r.keys().filter(|k| !l.contains_key(*k)) {
        out.only_right += 1;
        out.select_right.insert(k.clone());
    }
    out
}

/// Содержимое двух файлов одинаково: сначала первые 64 КБ, затем целиком (xxh3).
pub(crate) fn same_content(left: &Path, right: &Path) -> Option<bool> {
    use std::io::Read;
    let head = |p: &Path| -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 64 << 10];
        let mut file = std::fs::File::open(crate::names::os_path(p)).ok()?;
        let mut got = 0;
        while got < buf.len() {
            match file.read(&mut buf[got..]).ok()? {
                0 => break,
                more => got += more,
            }
        }
        buf.truncate(got);
        Some(buf)
    };
    if head(left)? != head(right)? {
        return Some(false);
    }
    let full = |p: &Path| -> Option<u128> {
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        crate::sys::fs::read_chunks(p, false, &mut |c| h.update(c)).ok()?;
        Some(h.digest128())
    };
    Some(full(left)? == full(right)?)
}

/// Пара путей для `same_content` по имени файла.
pub(crate) fn content_checker(left: PathBuf, right: PathBuf) -> impl Fn(&str) -> Option<bool> {
    move |name: &str| same_content(&left.join(name), &right.join(name))
}

#[cfg(test)]
mod tests {
    use urfin_core::{Attrs, EntryKind};

    use super::*;

    fn f(name: &str, size: u64, secs: i64) -> Entry {
        Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: name.into(),
            size,
            mtime: secs * 1_000_000_000,
            attrs: Attrs::default(),
            kind: EntryKind::File,
        }
    }

    fn keys(s: &HashSet<CompactString>) -> Vec<&str> {
        let mut v: Vec<&str> = s.iter().map(CompactString::as_str).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn far_style_selection() {
        let left = [
            f("same.txt", 10, 100),
            f("newer-left.txt", 10, 200),
            f("older-left.txt", 10, 100),
            f("size.txt", 10, 100),
            f("only-left.txt", 1, 1),
        ];
        let right = [
            f("SAME.txt", 10, 100),
            f("newer-left.txt", 10, 100),
            f("older-left.txt", 10, 200),
            f("size.txt", 11, 100),
            f("only-right.txt", 1, 1),
        ];
        let r = compare(&left, &right, 0, None);
        assert_eq!(
            (
                r.only_left,
                r.only_right,
                r.newer_left,
                r.newer_right,
                r.differs,
                r.equal
            ),
            (1, 1, 1, 1, 1, 1)
        );
        assert_eq!(
            keys(&r.select_left),
            ["NEWER-LEFT.TXT", "ONLY-LEFT.TXT", "SIZE.TXT"]
        );
        assert_eq!(
            keys(&r.select_right),
            ["OLDER-LEFT.TXT", "ONLY-RIGHT.TXT", "SIZE.TXT"]
        );
    }

    /// Приёмка S5-24: допуски FAT (2 с) и exFAT (10 мс).
    #[test]
    fn fat_and_exfat_tolerances() {
        use urfin_vfs::volume::FsKind;
        let left = [Entry {
            mtime: 100_000_000_000 + 1_500_000_000,
            ..f("a", 1, 0)
        }];
        let right = [f("a", 1, 100)];
        assert_eq!(
            compare(&left, &right, time_tolerance(Some(FsKind::Fat32)), None).equal,
            1
        );
        assert_eq!(
            compare(&left, &right, time_tolerance(Some(FsKind::ExFat)), None).newer_left,
            1
        );
        let ex = [Entry {
            mtime: 100_000_000_000 + 5_000_000,
            ..f("a", 1, 0)
        }];
        assert_eq!(
            compare(&ex, &right, time_tolerance(Some(FsKind::ExFat)), None).equal,
            1
        );
        assert_eq!(
            compare(&ex, &right, time_tolerance(Some(FsKind::Ntfs)), None).newer_left,
            1
        );
    }

    #[test]
    fn content_mode_finds_same_size_differences() {
        let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
        let (l, r) = (d.path().join("l"), d.path().join("r"));
        std::fs::create_dir_all(&l).unwrap();
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(l.join("a.bin"), vec![1u8; 70_000]).unwrap();
        let mut b = vec![1u8; 70_000];
        b[69_999] = 2; // разница после первых 64 КБ
        std::fs::write(r.join("a.bin"), &b).unwrap();
        std::fs::write(l.join("same.bin"), [3u8; 10]).unwrap();
        std::fs::write(r.join("same.bin"), [3u8; 10]).unwrap();
        let es = [f("a.bin", 70_000, 5), f("same.bin", 10, 5)];
        let check = content_checker(l, r);
        let res = compare(&es, &es, 0, Some(&check));
        assert_eq!((res.differs, res.equal), (1, 1));
        assert_eq!(
            compare(&es, &es, 0, None).equal,
            2,
            "quick: same size and time"
        );
    }
}
