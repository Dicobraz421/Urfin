//! 7z (S8-32): разбор, чтение всех методов, пароли, выборка, ошибки.
//!
//! Фикстуры (`tests/fixtures/`) собраны 7-Zip 24.07 из одного дерева (`sevenz-rust2` без
//! `compress` писать не умеет):
//! - `lzma-solid.7z` — `-m0=LZMA -ms=on -mf=off`; `lzma2.7z` — `-m0=LZMA2 -ms=off`;
//! - `ppmd.7z` — `-m0=PPMd -ms=on`; `bcj.7z` — `-m0=BCJ -m1=LZMA2 -ms=off` (`prog.exe dir`);
//! - `bcj2.7z` — `-m0=BCJ2 -m1=LZMA -m2=LZMA -m3=LZMA` (`prog.exe dir/a.txt`);
//! - `aes.7z` — `-m0=LZMA2 -ms=on -psecret`; `aes-names.7z` — то же и `-mhe=on`;
//! - `solid1000.7z` — 1000 файлов одного solid-блока (тест плана — в `formats/sevenz.rs`).
//!
//! Дерево: `dir/` (`dir/sub/` пустой), `dir/a.txt`, `b.bin` (только чтение), `empty.txt`,
//! `prog.exe`, `Отчёт.txt`. Ожидаемые значения сверены с `7z l -slt` (время там местное,
//! здесь — UTC).

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use urfin_arc::{ArcCache, ArcCfg, ArcVfs, PACKED, SevenZFormat, root_of};
use urfin_core::{Attrs, CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{Caps, ExtCell, ListOpts, Vfs, VfsError};

const MIB: u64 = 1 << 20;

fn vfs() -> ArcVfs<SevenZFormat> {
    ArcVfs::new(SevenZFormat::new(), ArcCache::new(256 * MIB))
}

fn at(root: &Location, path: &str) -> Location {
    Location {
        path: PathBuf::from(path),
        ..root.clone()
    }
}

fn list(v: &dyn Vfs, dir: &Location) -> Result<Vec<Entry>, VfsError> {
    let mut s = v.list(dir, &ListOpts::default())?;
    let mut out = Vec::new();
    let cancel = CancelToken::new();
    while s.next_batch(&mut out, &cancel)? {}
    Ok(out)
}

/// Строка дерева: путь, вид, размер, «Сжато» (`None` — `<DIR>`), время UTC, атрибуты.
type Row = (String, EntryKind, u64, Option<u64>, i64, u32);

fn tree(v: &dyn Vfs, root: &Location) -> Vec<Row> {
    let mut out = Vec::new();
    let mut todo = vec![String::new()];
    while let Some(dir) = todo.pop() {
        let loc = at(root, if dir.is_empty() { "/" } else { &dir });
        for e in list(v, &loc).unwrap() {
            let p = format!("{dir}/{}", e.name);
            if e.kind == EntryKind::Dir {
                todo.push(p.clone());
            }
            let packed = match v.ext(&loc, &e, PACKED.id) {
                Some(ExtCell::Size(n)) => Some(n),
                None => None,
                other => panic!("{p}: {other:?}"),
            };
            out.push((p, e.kind, e.size, packed, e.mtime, e.attrs.0));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn read(v: &dyn Vfs, p: &Location) -> Result<Vec<u8>, VfsError> {
    let mut r = v.open_read(p)?;
    let mut out = Vec::new();
    r.read_to_end(&mut out)
        .map_err(|e| VfsError::from_io(p.path.clone(), e))?;
    Ok(out)
}

fn fixture(name: &str) -> Location {
    Location::local(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
}

fn root(name: &str) -> Location {
    root_of("7z", fixture(name))
}

fn kind_of(e: &VfsError) -> Option<io::ErrorKind> {
    match e {
        VfsError::Io { source, .. } => Some(source.kind()),
        VfsError::Denied(_) => Some(io::ErrorKind::PermissionDenied),
        _ => None,
    }
}

fn utc(s: &str) -> i64 {
    let ts: jiff::Timestamp = s.parse().unwrap();
    i64::try_from(ts.as_nanosecond()).unwrap()
}

/// Содержимое исходного дерева.
fn source(name: &str) -> Vec<u8> {
    match name {
        "/dir/a.txt" => (0..10_000).map(|i| b"abcdefgh"[(i / 7 + 1) % 8]).collect(),
        "/b.bin" => {
            let mut x: u32 = 12345;
            (0..3000)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345) & 0x7fff_ffff;
                    (x >> 16) as u8
                })
                .collect()
        }
        "/empty.txt" => Vec::new(),
        "/prog.exe" => b"\x55\x8b\xec\xe8\x10\x00\x00\x00\x5d\xc3".repeat(400),
        "/Отчёт.txt" => "привет, 7z\n".as_bytes().to_vec(),
        other => panic!("нет {other}"),
    }
}

const FILES: [&str; 5] = [
    "/b.bin",
    "/dir/a.txt",
    "/empty.txt",
    "/prog.exe",
    "/Отчёт.txt",
];

const A: u32 = Attrs::ARCHIVE;
const E: u32 = Attrs::ENCRYPTED;

/// Дерево по `7z l -slt`: `packed` — «Packed Size» файлов в порядке `b.bin`, `a.txt`,
/// `prog.exe`, `Отчёт.txt` (у solid — у первого файла блока, у прочих пусто = 0).
fn expected(packed: [u64; 4], enc: u32) -> Vec<Row> {
    let f = EntryKind::File;
    let d = EntryKind::Dir;
    let mut v = vec![
        (
            "/b.bin",
            f,
            3000,
            Some(packed[0]),
            "2021-01-02T03:04:05Z",
            Attrs::READONLY | A | enc,
        ),
        ("/dir", d, 0, None, "2022-08-08T08:08:08Z", 0),
        (
            "/dir/a.txt",
            f,
            10_000,
            Some(packed[1]),
            "2023-06-01T12:34:56Z",
            A | enc,
        ),
        ("/dir/sub", d, 0, None, "2022-07-07T07:07:07Z", 0),
        ("/empty.txt", f, 0, Some(0), "2020-05-05T05:05:05Z", A),
        (
            "/prog.exe",
            f,
            4000,
            Some(packed[2]),
            "2020-05-05T05:05:05Z",
            A | enc,
        ),
        (
            "/Отчёт.txt",
            f,
            17,
            Some(packed[3]),
            "2024-02-29T23:59:58Z",
            A | enc,
        ),
    ];
    v.sort_by(|a, b| a.0.cmp(b.0));
    v.into_iter()
        .map(|(p, k, s, pk, t, a)| (p.to_owned(), k, s, pk, utc(t), a))
        .collect()
}

#[test]
fn listing_and_data_match_7z_l_in_every_method() {
    let cases: [(&str, [u64; 4], bool); 4] = [
        ("lzma-solid.7z", [3141, 0, 0, 0], true),
        ("lzma2.7z", [3004, 69, 44, 21], false),
        ("ppmd.7z", [3607, 0, 0, 0], true),
        ("aes.7z", [3152, 0, 0, 0], true),
    ];
    let v = vfs();
    for (name, packed, solid) in cases {
        let r = root(name);
        let enc = if name.starts_with("aes") { E } else { 0 };
        assert_eq!(tree(&v, &r), expected(packed, enc), "{name}");
        let idx = v.index(&r, &CancelToken::new()).unwrap();
        let s = idx.summary();
        assert_eq!(
            (s.format.as_str(), s.files, s.unpacked, s.packed, s.solid),
            ("7z", 5, 17_017, packed.iter().sum(), solid),
            "{name}: итог `7z l`"
        );
        assert_eq!(s.encrypted, enc != 0, "{name}");
        if enc != 0 {
            assert_eq!(idx.cipher, Some("AES-256"));
            idx.ctx().set_password(Some("secret"));
        }
        for f in FILES {
            assert_eq!(read(&v, &at(&r, f)).unwrap(), source(f), "{name}{f}");
        }
    }
}

#[test]
fn bcj_filters_read_back() {
    let v = vfs();
    let r = root("bcj.7z");
    let rows: Vec<(String, Option<u64>)> = tree(&v, &r).into_iter().map(|r| (r.0, r.3)).collect();
    let want = [
        ("/dir", None),
        ("/dir/a.txt", Some(69)),
        ("/dir/sub", None),
        ("/prog.exe", Some(443)),
    ];
    let want: Vec<(String, Option<u64>)> =
        want.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect();
    assert_eq!(rows, want);
    // BCJ2: четыре pack-потока одного блока — «Сжато» их сумма, как у `7z l`.
    let r2 = root("bcj2.7z");
    let rows: Vec<(String, Option<u64>)> = tree(&v, &r2).into_iter().map(|r| (r.0, r.3)).collect();
    let want = [
        ("/dir", None),
        ("/dir/a.txt", Some(497)),
        ("/prog.exe", Some(0)),
    ];
    let want: Vec<(String, Option<u64>)> =
        want.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect();
    assert_eq!(rows, want);
    for r in [&r, &r2] {
        for f in ["/dir/a.txt", "/prog.exe"] {
            assert_eq!(read(&v, &at(r, f)).unwrap(), source(f), "{f}");
        }
    }
}

#[test]
fn only_empty_files_seek() {
    let v = vfs();
    let r = root("lzma-solid.7z");
    v.index(&r, &CancelToken::new()).unwrap();
    let caps = |p: &str| v.caps(&at(&r, p));
    assert!(caps("/empty.txt").has(Caps::SEEK));
    assert!(caps("/prog.exe").has(Caps::SEQUENTIAL));
    assert!(!caps("/prog.exe").has(Caps::SEEK));
    assert!(caps("/dir").has(Caps::READ_ONLY));

    let mut f = v.open_read(&at(&r, "/prog.exe")).unwrap();
    assert_eq!(f.len(), Some(4000));
    let mut head = [0u8; 10];
    f.read_exact(&mut head).unwrap();
    assert_eq!(f.stream_position().unwrap(), 10);
    assert!(f.seek(SeekFrom::Start(0)).is_err(), "solid — только подряд");
    assert!(v.open_read(&at(&r, "/dir")).is_err(), "каталог не читается");
}

#[test]
fn aes_data_needs_the_password() {
    let v = vfs();
    let r = root("aes.7z");
    let idx = v.index(&r, &CancelToken::new()).unwrap();
    let e = v
        .open_read(&at(&r, "/prog.exe"))
        .err()
        .expect("без пароля — ошибка");
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");
    assert_eq!(read(&v, &at(&r, "/empty.txt")).unwrap(), b"");

    idx.ctx().set_password(Some("wrong"));
    let e = read(&v, &at(&r, "/prog.exe")).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");

    idx.ctx().set_password(Some("secret"));
    assert_eq!(read(&v, &at(&r, "/prog.exe")).unwrap(), source("/prog.exe"));
}

#[test]
fn encrypted_names_need_the_password_before_listing() {
    let v = vfs();
    let r = root("aes-names.7z");
    let outer = fixture("aes-names.7z");
    let e = list(&v, &r).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");

    v.cache().set_password(&outer, Some("wrong"));
    let e = list(&v, &r).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");
    // Неверный пароль забыт: следующий вход снова без него.
    let e = list(&v, &r).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");

    v.cache().set_password(&outer, Some("secret"));
    assert_eq!(tree(&v, &r), expected([3152, 0, 0, 0], E));
    let idx = v.index(&r, &CancelToken::new()).unwrap();
    assert!(idx.summary().encrypted);
    assert_eq!(idx.cipher, Some("AES-256"));
    assert!(idx.ctx().has_password(), "пароль имён — и данным");
    for f in FILES {
        assert_eq!(read(&v, &at(&r, f)).unwrap(), source(f), "{f}");
    }
}

#[test]
fn extract_plan_covers_each_file_once_across_blocks() {
    let v = vfs();
    let r = root("lzma2.7z");
    let files: Vec<Location> = FILES.iter().map(|p| at(&r, p)).collect();
    assert!(
        v.extract_plan(&files, 3).is_none(),
        "индекса нет — плана нет"
    );
    v.index(&r, &CancelToken::new()).unwrap();
    assert!(
        v.extract_plan(&[at(&r, "/dir")], 2).is_none(),
        "каталог — не план"
    );
    let streams = v.extract_plan(&files, 3).unwrap();
    assert!((2..=3).contains(&streams.len()), "{}", streams.len());
    let mut seen = vec![0u32; files.len()];
    let cancel = CancelToken::new();
    for mut s in streams {
        while let Some((i, mut r)) = s.next(&cancel).unwrap() {
            let mut got = Vec::new();
            r.read_to_end(&mut got).unwrap();
            drop(r);
            let i = i as usize;
            seen[i] += 1;
            assert_eq!(got, source(FILES[i]), "{}", FILES[i]);
        }
    }
    assert!(seen.iter().all(|&n| n == 1), "{seen:?}");

    // Solid: файл в середине блока, остальное — в никуда; читатель бросил файл — дальше.
    let r = root("ppmd.7z");
    v.index(&r, &CancelToken::new()).unwrap();
    let files = [at(&r, "/Отчёт.txt"), at(&r, "/dir/a.txt")];
    let mut streams = v.extract_plan(&files, 4).unwrap();
    assert_eq!(streams.len(), 1);
    let s = &mut streams[0];
    let (i, mut part) = s.next(&cancel).unwrap().unwrap();
    assert_eq!(i, 1, "порядок архива");
    let mut head = [0u8; 5];
    part.read_exact(&mut head).unwrap();
    drop(part);
    let (i, mut last) = s.next(&cancel).unwrap().unwrap();
    assert_eq!(i, 0);
    let mut got = Vec::new();
    last.read_to_end(&mut got).unwrap();
    drop(last);
    assert_eq!(got, source("/Отчёт.txt"));
    assert!(s.next(&cancel).unwrap().is_none());
}

#[test]
fn not_a_7z_and_volume_are_clear_errors() {
    let dir = tempfile::tempdir().unwrap();
    let v = vfs();
    let junk = dir.path().join("junk.7z");
    std::fs::write(&junk, b"definitely not a 7z").unwrap();
    let e = list(&v, &root_of("7z", Location::local(junk))).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::InvalidData), "{e}");

    // Первый том: начало архива без заголовка в конце.
    let whole = std::fs::read(fixture("lzma2.7z").path).unwrap();
    let vol = dir.path().join("set.7z.001");
    std::fs::write(&vol, &whole[..whole.len() / 2]).unwrap();
    let e = list(&v, &root_of("7z", Location::local(vol))).unwrap_err();
    urfin_i18n::set_language("en");
    assert!(e.to_string().contains("Multi-volume"), "{e}");
}

#[test]
fn backends_include_7z() {
    let cfg = ArcCfg::default();
    let cache = urfin_arc::cache(&cfg);
    assert!(
        urfin_arc::backends(&cache, &cfg)
            .iter()
            .any(|b| b.scheme() == "7z")
    );
}
