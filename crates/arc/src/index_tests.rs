use std::path::Path;

use super::*;

fn outer() -> OuterKey {
    OuterKey {
        loc: Location::local(r"C:\a.zip"),
        size: 1000,
        mtime: 7,
    }
}

fn file(path: &str, size: u64, mtime: i64) -> RawEntry<'_> {
    RawEntry {
        mtime,
        ..RawEntry::file(path, size, size / 2, EntryLoc::Index(size as u32))
    }
}

fn names(idx: &ArchiveIndex, id: NodeId) -> Vec<&str> {
    idx.children(id).iter().map(|&k| idx.name(k)).collect()
}

#[test]
fn implicit_dirs_sorted_children_and_sums() {
    let mut b = IndexBuilder::new();
    b.push(file("src/main.rs", 100, 5));
    b.push(file("src/lib.rs", 40, 9));
    b.push(file("README.md", 10, 1));
    b.push(RawEntry::dir("docs/"));
    b.push(file("docs/a/b/c.txt", 6, 3));
    let idx = b.finish("zip", outer());
    assert_eq!(names(&idx, ROOT), ["README.md", "docs", "src"]);
    let src = idx.lookup(Path::new("/src")).unwrap();
    assert_eq!(names(&idx, src), ["lib.rs", "main.rs"]);
    let e = idx.entry(src);
    assert_eq!(e.kind, EntryKind::Dir);
    assert_eq!(e.size, 0, "у каталога размера нет, как в панели (`<DIR>`)");
    assert!(!e.attrs.has(Attrs::SIZE_KNOWN));
    assert_eq!(
        idx.subtree(src, |_| {}),
        (140, 2),
        "поддерево — для распаковки"
    );
    assert_eq!(e.mtime, 9, "неявный каталог — время свежего ребёнка");
    let docs = idx.lookup(Path::new("/docs")).unwrap();
    assert_eq!(
        idx.node(docs).mtime,
        i64::MIN,
        "каталог из оглавления — своё время"
    );
    let c = idx.lookup(Path::new(r"\docs\a\b\c.txt")).unwrap();
    assert_eq!(idx.path_of(c), "/docs/a/b/c.txt");
    assert_eq!(idx.node(c).loc, EntryLoc::Index(6));
    assert_eq!(idx.path_of(ROOT), "/");
    let s = idx.summary();
    assert_eq!((s.files, s.unpacked, s.packed, s.size), (4, 156, 78, 1000));
    assert!(s.complete);
    assert_eq!(
        idx.lookup(Path::new("/src/main.rs/x")),
        None,
        "файл — не каталог"
    );
    assert_eq!(
        idx.lookup(Path::new("/SRC/Main.RS")),
        Some(idx.child(src, "main.rs").unwrap())
    );
    assert_eq!(idx.lookup(Path::new("/nope")), None);
}

#[test]
fn last_duplicate_wins_and_kinds_settle() {
    let mut b = IndexBuilder::new();
    b.push(file("a.txt", 10, 1));
    b.push(file("a.txt", 20, 2));
    b.push(file("x", 5, 1));
    // `x` был файлом — путь через него делает его каталогом.
    b.push(file("x/y", 3, 1));
    // Файл поверх каталога с детьми каталог не затирает.
    b.push(file("x", 99, 1));
    b.push(file("../../evil", 1, 1));
    b.push(file(r"C:\Windows\evil.dll", 1, 1));
    b.push(file("/abs", 1, 1));
    assert_eq!(b.push(file("..", 1, 1)), None, "пусто после нормализации");
    let idx = b.finish("zip", outer());
    assert_eq!(names(&idx, ROOT), ["Windows", "a.txt", "abs", "evil", "x"]);
    let a = idx.lookup(Path::new("/a.txt")).unwrap();
    assert_eq!((idx.node(a).size, idx.node(a).mtime), (20, 2));
    let x = idx.lookup(Path::new("/x")).unwrap();
    assert!(idx.node(x).kind.is_dir_like());
    assert_eq!(names(&idx, x), ["y"]);
    let s = idx.summary();
    assert_eq!(s.files, 5, "a.txt, y, evil, evil.dll, abs");
    assert_eq!(s.unpacked, 20 + 3 + 1 + 1 + 1);
}

#[test]
fn root_since_streams_new_root_children() {
    let mut b = IndexBuilder::new();
    let mut out = Vec::new();
    let mut at = b.root_since(0, &mut out);
    assert_eq!(out.len(), 0);
    b.push(file("z/1", 1, 1));
    b.push(file("a", 1, 1));
    at = b.root_since(at, &mut out);
    assert_eq!(
        out.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["z", "a"]
    );
    b.push(file("z/2", 1, 1));
    out.clear();
    let at2 = b.root_since(at, &mut out);
    assert_eq!(out.len(), 0, "в корне новых нет");
    assert_eq!(at, at2);
    let s = b.summary("tar.gz", 55, false);
    assert_eq!(
        (s.files, s.complete, s.format.as_str()),
        (3, false, "tar.gz")
    );
}

#[test]
fn many_names_in_one_dir_find_each() {
    let mut b = IndexBuilder::with_capacity(10_000);
    let names: Vec<String> = (0..10_000).map(|i| format!("f{i:05}")).collect();
    for n in &names {
        b.push(file(n, 1, 1));
    }
    // Тот же набор ещё раз — дубли, не новые узлы.
    for n in &names {
        b.push(file(n, 2, 1));
    }
    let idx = b.finish("zip", outer());
    assert_eq!(idx.len(), 10_001);
    for n in &names {
        let id = idx.child(ROOT, n).unwrap();
        assert_eq!(idx.node(id).size, 2);
    }
    assert!(idx.approx_bytes() > 10_000 * size_of::<Node>() as u64);
}
