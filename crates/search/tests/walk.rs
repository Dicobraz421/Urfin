//! S8-04: обход. Найденное равно рекурсивному обходу `std::fs`, отмена быстрая, потоков после
//! обхода нет.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use urfin_core::search::{SearchModel, SearchStore};
use urfin_core::{Attrs, Location, Mask};
use urfin_search::{
    KindFilter, Outcome, SearchCfg, SearchQuery, TextQuery, TreeSearch, search_tree,
};
use urfin_testdata::{Layout, TreeSpec, gen_tree};
use urfin_vfs::{VfsRegistry, name};

fn store(root: &Path) -> Arc<SearchStore> {
    SearchModel::default().open(vec![Location::local(root)])
}

fn query(root: &Path) -> SearchQuery {
    SearchQuery {
        roots: vec![Location::local(root)],
        ..SearchQuery::default()
    }
}

fn search_with(q: &SearchQuery, cfg: &SearchCfg, s: &SearchStore) -> Outcome {
    let reg = VfsRegistry::with_local();
    let denied = Mutex::new(Vec::new());
    let run = TreeSearch {
        query: q,
        cfg,
        reg: &reg,
        store: s,
        denied: &denied,
        halted: &|| false,
        poke: &|| {},
    };
    search_tree(&run, || {}).unwrap()
}

/// Найденные пути от корня.
fn found(s: &SearchStore) -> BTreeSet<OsString> {
    s.rows(0..usize::MAX)
        .iter()
        .map(|h| {
            let escaped = h.e.attrs.has(Attrs::ESCAPED_NAME);
            name::to_os(&h.e.name, escaped).into_owned()
        })
        .collect()
}

/// Рекурсивный обход `std::fs`: ссылки на каталоги не обходятся, как и у обходчика.
fn naive(dir: &Path, rel: &Path, depth: Option<u16>, out: &mut BTreeSet<OsString>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let t = e.file_type().unwrap();
        let name = rel.join(e.file_name());
        if t.is_dir() {
            if depth != Some(0) {
                naive(&e.path(), &name, depth.map(|d| d - 1), out);
            }
        } else if !t.is_symlink_dir() {
            out.insert(name.into_os_string());
        }
    }
}

trait SymlinkDir {
    fn is_symlink_dir(&self) -> bool;
}

impl SymlinkDir for std::fs::FileType {
    fn is_symlink_dir(&self) -> bool {
        use std::os::windows::fs::FileTypeExt;
        FileTypeExt::is_symlink_dir(self)
    }
}

/// Имя не в UTF-16: одиночный суррогат.
fn bad_name(tail: &str) -> OsString {
    use std::os::windows::ffi::OsStringExt;
    let mut w: Vec<u16> = "плохое".encode_utf16().collect();
    w.push(0xD800);
    w.extend(tail.encode_utf16());
    OsString::from_wide(&w)
}

/// Дерево `gen_tree` плюс не-UTF-8 имена и путь длиннее `MAX_PATH`.
fn tree(root: &Path) {
    let spec = TreeSpec {
        n: 800,
        seed: 42,
        layout: Layout::Nav { dirs: 12 },
    };
    gen_tree(root, &spec).unwrap();
    let bad_dir = root.join(bad_name(" каталог"));
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(bad_dir.join(bad_name(" файл.txt")), "x").unwrap();
    let mut deep = root.to_path_buf();
    for i in 0..8 {
        deep.push(format!("длинный каталог для пути больше max path {i:03}"));
    }
    assert!(deep.as_os_str().len() > 300);
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("на дне.txt"), "x").unwrap();
    std::fs::write(root.join("сверху.txt"), "x").unwrap();
}

#[test]
fn finds_the_same_set_as_std_fs() {
    let d = tempfile::tempdir().unwrap();
    tree(d.path());
    let s = store(d.path());
    let out = search_with(&query(d.path()), &SearchCfg::default(), &s);
    assert_eq!(out, Outcome::default());

    let mut want = BTreeSet::new();
    naive(d.path(), Path::new(""), None, &mut want);
    assert!(want.len() > 700, "{}", want.len());
    assert_eq!(found(&s), want);
    assert_eq!(s.rows_len(), want.len(), "без повторов");
    assert_eq!(s.files_len(), want.len());
    assert_eq!(s.info.files.load(Ordering::Relaxed), want.len() as u64);
}

#[test]
fn subfolders_off_gives_the_top_level_only() {
    let d = tempfile::tempdir().unwrap();
    tree(d.path());
    let s = store(d.path());
    let q = SearchQuery {
        depth: Some(0),
        ..query(d.path())
    };
    search_with(&q, &SearchCfg::default(), &s);
    let mut want = BTreeSet::new();
    naive(d.path(), Path::new(""), Some(0), &mut want);
    assert_eq!(found(&s), want);
    assert_eq!(found(&s).len(), 1);
}

#[test]
fn depth_counts_levels() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(r"a\b\c")).unwrap();
    for f in ["0.txt", r"a\1.txt", r"a\b\2.txt", r"a\b\c\3.txt"] {
        std::fs::write(d.path().join(f), "").unwrap();
    }
    let s = store(d.path());
    let q = SearchQuery {
        depth: Some(1),
        ..query(d.path())
    };
    search_with(&q, &SearchCfg::default(), &s);
    let want: BTreeSet<OsString> = ["0.txt", r"a\1.txt"].map(OsString::from).into();
    assert_eq!(found(&s), want);
}

#[test]
fn dirs_mask_and_skip_set() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(r"src\target")).unwrap();
    std::fs::create_dir_all(d.path().join(r"$RECYCLE.BIN\x")).unwrap();
    std::fs::write(d.path().join(r"src\a.rs"), "").unwrap();
    std::fs::write(d.path().join(r"src\target\b.rs"), "").unwrap();
    std::fs::write(d.path().join(r"$RECYCLE.BIN\x\c.rs"), "").unwrap();

    let s = store(d.path());
    let q = SearchQuery {
        kinds: KindFilter::Dirs,
        ..query(d.path())
    };
    search_with(&q, &SearchCfg::default(), &s);
    let want: BTreeSet<OsString> = ["src", r"src\target"].map(OsString::from).into();
    assert_eq!(found(&s), want);

    let s = store(d.path());
    let q = SearchQuery {
        name: Mask::parse(r"*.rs|src\target\*").ok(),
        ..query(d.path())
    };
    search_with(&q, &SearchCfg::default(), &s);
    let want: BTreeSet<OsString> = [r"src\a.rs"].map(OsString::from).into();
    assert_eq!(found(&s), want);
}

#[test]
fn hits_of_one_file_are_written_together() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..300 {
        let text = "игла\n".repeat(1 + i % 7);
        std::fs::write(d.path().join(format!("{i}.txt")), text).unwrap();
    }
    let s = store(d.path());
    let q = SearchQuery {
        text: Some(TextQuery {
            pattern: "игла".into(),
            ..TextQuery::default()
        }),
        ..query(d.path())
    };
    search_with(&q, &SearchCfg::default(), &s);
    assert_eq!(s.files_len(), 300);
    for &first in s.files(0..300).iter() {
        let row = s.rows(first as usize..first as usize + 1);
        assert_eq!(
            row.iter().next().unwrap().line,
            1,
            "ряд панели — первая строка"
        );
    }
}

#[test]
fn limit_is_exact_and_reported() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..500 {
        std::fs::write(d.path().join(format!("{i}.txt")), "").unwrap();
    }
    let s = store(d.path());
    let cfg = SearchCfg {
        max_hits: 300,
        ..SearchCfg::default()
    };
    let out = search_with(&query(d.path()), &cfg, &s);
    assert!(out.capped);
    assert_eq!(s.rows_len(), 300);
}

#[test]
fn missing_root_is_reported() {
    let d = tempfile::tempdir().unwrap();
    let gone = d.path().join("нет такого");
    let s = store(&gone);
    let out = search_with(&query(&gone), &SearchCfg::default(), &s);
    assert_eq!(out.failed_roots, [Location::local(&gone)]);
}

#[test]
fn second_root_gets_its_index() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(a.path().join("a.txt"), "").unwrap();
    std::fs::write(b.path().join("b.txt"), "").unwrap();
    let s = store(a.path());
    let mut q = query(a.path());
    q.roots.push(Location::local(b.path()));
    search_with(&q, &SearchCfg::default(), &s);
    let rows = s.rows(0..2);
    let got: BTreeSet<_> = rows
        .iter()
        .map(|h| (h.root, h.e.name.to_string()))
        .collect();
    assert_eq!(
        got,
        [(0, "a.txt".to_owned()), (1, "b.txt".to_owned())].into()
    );
}

/// Живых потоков обхода: метка живёт в TLS и снимается при выходе потока.
struct Live(Arc<AtomicUsize>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Отмена во время чтения большого файла: мс от отмены до выхода. Потоков после выхода 0.
fn cancel_once(root: &Path, q: &SearchQuery) -> Duration {
    let reg = VfsRegistry::with_local();
    let denied = Mutex::new(Vec::new());
    let s = store(root);
    let flag = AtomicBool::new(false);
    let live = Arc::new(AtomicUsize::new(0));
    let main: ThreadId = std::thread::current().id();
    let cfg = SearchCfg::default();
    let halted = || {
        if std::thread::current().id() != main {
            LIVE.with(|l| {
                l.borrow_mut().get_or_insert_with(|| {
                    live.fetch_add(1, Ordering::SeqCst);
                    Live(Arc::clone(&live))
                });
            });
        }
        flag.load(Ordering::SeqCst)
    };
    let run = TreeSearch {
        query: q,
        cfg: &cfg,
        reg: &reg,
        store: &s,
        denied: &denied,
        halted: &halted,
        poke: &|| {},
    };
    let t0 = Instant::now();
    let stopped = Mutex::new(None);
    std::thread::scope(|t| {
        t.spawn(|| {
            std::thread::sleep(Duration::from_millis(30));
            *stopped.lock().unwrap() = Some(Instant::now());
            flag.store(true, Ordering::SeqCst);
        });
        search_tree(&run, || {}).unwrap();
    });
    let done = Instant::now();
    // `scope` ждёт конца замыканий, а TLS снимается при выходе потока, чуть позже.
    while live.load(Ordering::SeqCst) != 0 {
        assert!(
            done.elapsed() < Duration::from_secs(5),
            "потоки обхода живы"
        );
        std::thread::yield_now();
    }
    let at = stopped.lock().unwrap().unwrap_or(t0);
    done.saturating_duration_since(at)
}

fn big_file(dir: &Path) -> PathBuf {
    let f = dir.join("большой.txt");
    let line = "строка без искомого текста, но с кириллицей\n".repeat(2000);
    let chunk = line.as_bytes();
    let n = (64 << 20) / chunk.len();
    let mut w = std::io::BufWriter::new(std::fs::File::create(&f).unwrap());
    for _ in 0..n {
        std::io::Write::write_all(&mut w, chunk).unwrap();
    }
    f
}

#[test]
fn cancel_takes_under_100_ms_and_leaves_no_threads() {
    let d = tempfile::tempdir().unwrap();
    big_file(d.path());
    let q = SearchQuery {
        text: Some(TextQuery {
            pattern: "нет такого".into(),
            ..TextQuery::default()
        }),
        ..query(d.path())
    };
    // Лучшая из трёх: на загруженном CI поток могут снять с ядра, это не про отмену.
    let best = (0..3).map(|_| cancel_once(d.path(), &q)).min().unwrap();
    assert!(best <= Duration::from_millis(100), "{best:?}");
}
