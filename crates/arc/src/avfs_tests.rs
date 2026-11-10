use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use urfin_core::EntryKind;

use super::*;
use crate::cache::{ArcEvent, DEFAULT_LIMIT};
use crate::mock::{self, MockFormat};

/// Временный каталог теста (`tempfile` крейту не подключён): удаляется в `Drop`.
pub(crate) struct Tmp(pub PathBuf);

impl Tmp {
    pub(crate) fn new() -> Tmp {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "urfin-arc-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    pub(crate) fn file(&self, name: &str, text: &str) -> Location {
        let p = self.0.join(name);
        std::fs::write(&p, text).unwrap();
        Location::local(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn at(root: &Location, path: &str) -> Location {
    Location {
        path: PathBuf::from(path),
        ..root.clone()
    }
}

fn list_all(v: &dyn Vfs, dir: &Location) -> Result<Vec<Entry>, VfsError> {
    let mut s = v.list(dir, &ListOpts::default())?;
    let mut out = Vec::new();
    let cancel = CancelToken::new();
    while s.next_batch(&mut out, &cancel)? {}
    Ok(out)
}

fn names(v: &[Entry]) -> Vec<&str> {
    v.iter().map(|e| e.name.as_str()).collect()
}

const FILES: [(&str, u64); 5] = [
    ("src/main.rs", 120),
    ("src/lib.rs", 40),
    ("README.md", 10),
    ("../evil.txt", 3),
    (r"C:\abs\x.txt", 4),
];

#[test]
#[allow(
    clippy::many_single_char_names,
    reason = "длинный сценарий одного архива"
)]
fn lists_stats_and_reads_from_the_index() {
    let tmp = Tmp::new();
    let file = tmp.file("a.mock", &mock::text(&FILES));
    let v = ArcVfs::new(MockFormat::catalog(), ArcCache::new(DEFAULT_LIMIT));
    let root = root_of("mock", file.clone());
    let top = list_all(&v, &root).unwrap();
    assert_eq!(
        names(&top),
        ["README.md", "abs", "evil.txt", "src"],
        "zip-slip — внутрь"
    );
    let src = at(&root, "/src");
    assert_eq!(names(&list_all(&v, &src).unwrap()), ["lib.rs", "main.rs"]);
    let main = at(&root, r"/src\main.rs");
    let m = v.stat(&main, true).unwrap();
    assert_eq!(
        (m.kind, m.size, m.alloc_size),
        (EntryKind::File, 120, Some(60))
    );
    let e = v.stat_entry(&src, "lib.rs", Attrs::default()).unwrap();
    assert_eq!(e.size, 40);
    let mut data = Vec::new();
    v.open_read(&main).unwrap().read_to_end(&mut data).unwrap();
    assert_eq!(data.len(), 120);
    assert!(v.open_read(&src).is_err(), "каталог не читается");
    assert!(matches!(
        v.list(&main, &ListOpts::default())
            .unwrap()
            .next_batch(&mut Vec::new(), &CancelToken::new()),
        Err(VfsError::NotDir(_))
    ));
    assert!(matches!(
        list_all(&v, &at(&root, "/nope")),
        Err(VfsError::NotFound(_))
    ));

    assert!(v.caps(&main).has(Caps::READ_ONLY | Caps::SEEK));
    assert_eq!(v.caps(&src), Caps(Caps::READ_ONLY));
    assert!(matches!(
        v.open_write(&main, None),
        Err(VfsError::NoWrite("mock"))
    ));
    assert_eq!(
        v.columns(&src).iter().map(|c| c.id).collect::<Vec<_>>(),
        ["size", "packed", "mtime"]
    );
    let lib = list_all(&v, &src).unwrap().remove(0);
    assert_eq!(v.ext(&src, &lib, "packed"), Some(ExtCell::Size(20)));
    assert_eq!(v.ext(&src, &lib, "size"), None);
    assert_eq!(v.transfer_channels(), 4);

    let ev = v.cache().take_events();
    assert!(
        matches!(&ev[..], [ArcEvent::Set(l, s)] if *l == file && s.files == 5 && s.complete && s.format == "mock"),
        "{ev:?}"
    );
}

#[test]
fn reentry_reads_nothing_until_the_file_changes() {
    let tmp = Tmp::new();
    let file = tmp.file("a.mock", &mock::text(&FILES));
    let v = ArcVfs::new(MockFormat::catalog(), ArcCache::new(DEFAULT_LIMIT));
    let root = root_of("mock", file.clone());
    list_all(&v, &root).unwrap();
    list_all(&v, &at(&root, "/src")).unwrap();
    v.stat(&at(&root, "/README.md"), true).unwrap();
    assert_eq!(v.format().scans(), 1);
    // Другой размер — другой ключ: индекс строится заново.
    tmp.file("a.mock", &mock::text(&FILES[..2]));
    assert_eq!(names(&list_all(&v, &root).unwrap()), ["src"]);
    assert_eq!(v.format().scans(), 2);
    assert_eq!(v.cache().usage().0, 1, "старый индекс того же файла ушёл");
}

#[test]
fn root_parent_is_the_archive_file() {
    let file = Location::local(r"C:\Dev\urfin-0.4.2.zip");
    let root = root_of("zip", file.clone());
    assert_eq!(root.parent(), Some(file));
    assert_eq!(at(&root, "/a/b").parent(), Some(at(&root, "/a")));
    assert!(is_root(&root) && is_root(&at(&root, r"\")) && !is_root(&at(&root, "/a")));
}

#[test]
fn not_an_archive_is_a_format_error() {
    let tmp = Tmp::new();
    let file = tmp.file("a.mock", "plain text\n");
    let v = ArcVfs::new(MockFormat::catalog(), ArcCache::new(DEFAULT_LIMIT));
    let e = list_all(&v, &root_of("mock", file)).unwrap_err();
    assert!(
        matches!(&e, VfsError::Io { source, .. } if source.kind() == io::ErrorKind::InvalidData),
        "{e:?}"
    );
    let dir = Location::local(&tmp.0);
    assert!(
        list_all(&v, &root_of("mock", dir)).is_err(),
        "каталог — не архив"
    );
    assert_eq!(v.cache().usage().0, 0, "сломанный индекс не кэшируется");
}

#[test]
fn streaming_root_lists_while_indexing() {
    let tmp = Tmp::new();
    let file = tmp.file("a.mocks", &format!("{}\ngen 20000\n", mock::MAGIC));
    let v = ArcVfs::new(MockFormat::streaming(), ArcCache::new(DEFAULT_LIMIT));
    let root = root_of("mocks", file.clone());
    let mut s = v.list(&root, &ListOpts::default()).unwrap();
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    let mut batches = 0;
    while s.next_batch(&mut out, &cancel).unwrap() {
        batches += 1;
    }
    assert!(batches >= 2, "по шагу разбора на батч");
    assert_eq!(out.len(), 1000, "d000…d999 — каждый один раз");
    let ev = v.cache().take_events();
    assert!(
        matches!(ev.first(), Some(ArcEvent::Set(_, s)) if !s.complete),
        "{ev:?}"
    );
    assert!(matches!(ev.last(), Some(ArcEvent::Set(_, s)) if s.complete && s.files == 20_000));
    // Второй вход — из кэша; подкаталог — тоже.
    assert_eq!(list_all(&v, &root).unwrap().len(), 1000);
    assert_eq!(
        list_all(&v, &at(&root, "/d007/s0")).unwrap().len(),
        2,
        "f0000007, f0010007"
    );
    assert_eq!(v.format().scans(), 1);
    assert!(
        v.caps(&at(&root, "/d007/s0/f0000007.bin"))
            .has(Caps::SEQUENTIAL)
    );
}

#[test]
fn abandoned_stream_lets_the_next_listing_build() {
    let tmp = Tmp::new();
    let file = tmp.file("a.mocks", &format!("{}\ngen 20000\n", mock::MAGIC));
    let v = ArcVfs::new(MockFormat::streaming(), ArcCache::new(DEFAULT_LIMIT));
    let root = root_of("mocks", file);
    let mut s = v.list(&root, &ListOpts::default()).unwrap();
    assert!(s.next_batch(&mut Vec::new(), &CancelToken::new()).unwrap());
    drop(s);
    assert_eq!(list_all(&v, &root).unwrap().len(), 1000);
}

#[test]
fn extract_plan_covers_each_file_once() {
    let tmp = Tmp::new();
    let file = tmp.file("a.mock", &mock::text(&FILES));
    let v = ArcVfs::new(MockFormat::catalog(), ArcCache::new(DEFAULT_LIMIT));
    let root = root_of("mock", file);
    let files = [
        at(&root, "/src/main.rs"),
        at(&root, "/README.md"),
        at(&root, "/src/lib.rs"),
    ];
    assert!(
        v.extract_plan(&files, 2).is_none(),
        "без индекса в кэше плана нет"
    );
    list_all(&v, &root).unwrap();
    let streams = v.extract_plan(&files, 2).unwrap();
    assert_eq!(streams.len(), 2);
    let cancel = CancelToken::new();
    let mut got = Vec::new();
    for mut s in streams {
        while let Some((i, mut r)) = s.next(&cancel).unwrap() {
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).unwrap();
            got.push((i, buf.len()));
        }
    }
    got.sort_unstable();
    assert_eq!(got, [(0, 120), (1, 10), (2, 40)]);
    assert!(
        v.extract_plan(&[at(&root, "/src")], 1).is_none(),
        "каталог — не файл плана"
    );
}

/// Приёмка S8-24: индекс 1M записей < 600 мс (release); повторный вход — из кэша.
#[test]
fn million_entries_index_fast_and_reenter_without_io() {
    let tmp = Tmp::new();
    let file = tmp.file("big.mock", &format!("{}\ngen 1000000\n", mock::MAGIC));
    let v = ArcVfs::new(MockFormat::catalog(), ArcCache::new(DEFAULT_LIMIT));
    let root = root_of("mock", file);
    let t = Instant::now();
    let top = list_all(&v, &root).unwrap();
    let cold = t.elapsed();
    assert_eq!(top.len(), 1000);
    let idx = v.cache().latest(root.outer.as_deref().unwrap()).unwrap();
    assert_eq!(idx.summary().files, 1_000_000);
    // Debug-сборка тестов без оптимизаций в разы медленнее; бюджет — для release.
    let budget = if cfg!(debug_assertions) { 6000 } else { 600 };
    assert!(cold.as_millis() < budget, "холодный вход {cold:?}");
    let t = Instant::now();
    assert_eq!(list_all(&v, &root).unwrap().len(), 1000);
    let warm = t.elapsed();
    assert_eq!(v.format().scans(), 1, "повторный вход не читает архив");
    assert!(warm.as_millis() < 50, "повторный вход {warm:?}");
    assert!(
        v.cache().usage().1 < DEFAULT_LIMIT,
        "1M записей укладываются в лимит"
    );
    eprintln!(
        "1M: холодно {cold:?}, повторно {warm:?}, {} МиБ",
        v.cache().usage().1 >> 20
    );
}
