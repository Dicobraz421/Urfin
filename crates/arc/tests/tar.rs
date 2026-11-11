//! tar и компрессоры (S8-30): разбор, ссылки, PAX/GNU, потоковый корень, чтение,
//! выборка одним проходом.
//!
//! Фикстуры собираются здесь крейтом `tar` (и компрессорами из зависимостей крейта).
//! Ожидаемые значения сверены с `tar -tvf` (GNU tar 1.35 и bsdtar 3.8 из Windows) на
//! том же архиве: имена, типы, размеры, время UTC; у hardlink `tar -tv` пишет размер 0 и
//! «link to», здесь — размер цели (данные ссылки — данные цели).

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use tar::{Builder, EntryType, Header};
use urfin_arc::{ArcCache, ArcVfs, TarFormat, root_of};
use urfin_core::{Attrs, CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{Caps, ListOpts, Vfs, VfsError};

const MIB: u64 = 1 << 20;

/// 2023-11-14T22:13:20Z.
const T1: u64 = 1_700_000_000;
const T2: u64 = T1 + 100;
const T3: u64 = T1 + 200;

fn vfs() -> ArcVfs<TarFormat> {
    ArcVfs::new(TarFormat::new(), ArcCache::new(256 * MIB))
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

/// Всё дерево: (путь, вид, размер, время в секундах, атрибуты).
fn tree(v: &dyn Vfs, root: &Location) -> Vec<(String, EntryKind, u64, i64, u32)> {
    let mut out = Vec::new();
    let mut todo = vec![String::new()];
    while let Some(dir) = todo.pop() {
        let loc = at(root, if dir.is_empty() { "/" } else { &dir });
        for e in list(v, &loc).unwrap() {
            let p = format!("{dir}/{}", e.name);
            if e.kind == EntryKind::Dir {
                todo.push(p.clone());
            }
            let keep = Attrs::EXEC | Attrs::READONLY | Attrs::DOTFILE;
            out.push((
                p,
                e.kind,
                e.size,
                e.mtime.div_euclid(1_000_000_000),
                e.attrs.0 & keep,
            ));
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

fn kind_of(e: &VfsError) -> Option<io::ErrorKind> {
    match e {
        VfsError::Io { source, .. } => Some(source.kind()),
        _ => None,
    }
}

/// Детерминированные сжимаемые данные.
fn data(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| b"abcdefgh"[(i / 7 + usize::from(seed)) % 8])
        .collect()
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("urfin-arc-tar-")
        .tempdir()
        .unwrap()
}

fn header(ty: EntryType, size: u64, mode: u32, mtime: u64) -> Header {
    let mut h = Header::new_gnu();
    h.set_entry_type(ty);
    h.set_size(size);
    h.set_mode(mode);
    h.set_mtime(mtime);
    h
}

fn file(b: &mut Builder<Vec<u8>>, path: &str, body: &[u8], mode: u32, mtime: u64) {
    let mut h = header(EntryType::Regular, body.len() as u64, mode, mtime);
    b.append_data(&mut h, path, body).unwrap();
}

fn link(b: &mut Builder<Vec<u8>>, ty: EntryType, path: &str, target: &str) {
    let mut h = header(ty, 0, 0o777, T2);
    b.append_link(&mut h, path, target).unwrap();
}

/// PAX-заголовок `x` для следующей записи.
fn pax(b: &mut Builder<Vec<u8>>, recs: &[(&str, &str)]) {
    let mut body = Vec::new();
    for (k, v) in recs {
        let rest = format!(" {k}={v}\n");
        let mut n = rest.len() + 1;
        while n.to_string().len() + rest.len() != n {
            n += 1;
        }
        body.extend_from_slice(format!("{n}{rest}").as_bytes());
    }
    let mut h = Header::new_ustar();
    h.set_entry_type(EntryType::XHeader);
    h.set_size(body.len() as u64);
    h.set_path("PaxHeaders/0").unwrap();
    h.set_cksum();
    b.append(&h, &body[..]).unwrap();
}

const PAX_NAME: &str = "Документы/отчёт с очень длинным именем.txt";

/// Архив со всем, что умеет записать крейт `tar`: GNU long name, PAX (имя, доли
/// секунды), каталог, исполняемый, только чтение, dotfile, пустой, symlink, hardlink,
/// hardlink без цели.
fn build_tar() -> Vec<u8> {
    let mut b = Builder::new(Vec::new());
    let mut h = header(EntryType::Directory, 0, 0o755, T1);
    b.append_data(&mut h, "src/", io::empty()).unwrap();
    file(&mut b, "src/main.rs", &data(1, 1234), 0o644, T2);
    file(&mut b, "run.sh", b"#!/bin/sh\necho hi\n", 0o755, T2);
    let long = format!("deep/{}.txt", "a".repeat(120));
    file(&mut b, &long, &data(2, 700), 0o644, T3);
    pax(&mut b, &[("path", PAX_NAME), ("mtime", "1700000300.5")]);
    let body = data(3, 3000);
    let mut h = Header::new_ustar();
    h.set_entry_type(EntryType::Regular);
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(T3);
    b.append_data(&mut h, "placeholder.txt", &body[..]).unwrap();
    link(&mut b, EntryType::Symlink, "link-to-main", "src/main.rs");
    link(&mut b, EntryType::Link, "src/main-hard.rs", "./src/main.rs");
    file(&mut b, ".hidden", b"secret", 0o444, T1);
    file(&mut b, "empty.txt", b"", 0o644, T1);
    link(&mut b, EntryType::Link, "broken-hard", "nowhere");
    b.into_inner().unwrap()
}

/// Ожидаемое дерево [`build_tar`] (сверено с `tar -tvf`, см. шапку).
fn expected() -> Vec<(String, EntryKind, u64, i64, u32)> {
    let t = |s: u64| i64::try_from(s).unwrap();
    let long = format!("/deep/{}.txt", "a".repeat(120));
    let mut v = vec![
        ("/src".into(), EntryKind::Dir, 2468, t(T1), 0),
        ("/src/main.rs".into(), EntryKind::File, 1234, t(T2), 0),
        (
            "/src/main-hard.rs".into(),
            EntryKind::LinkFile,
            1234,
            t(T2),
            0,
        ),
        ("/run.sh".into(), EntryKind::File, 18, t(T2), Attrs::EXEC),
        ("/deep".into(), EntryKind::Dir, 700, t(T3), 0),
        (long, EntryKind::File, 700, t(T3), 0),
        ("/Документы".into(), EntryKind::Dir, 3000, t(T1) + 300, 0),
        (
            format!("/{PAX_NAME}"),
            EntryKind::File,
            3000,
            t(T1) + 300,
            0,
        ),
        ("/link-to-main".into(), EntryKind::LinkFile, 0, t(T2), 0),
        (
            "/.hidden".into(),
            EntryKind::File,
            6,
            t(T1),
            Attrs::READONLY | Attrs::DOTFILE,
        ),
        ("/empty.txt".into(), EntryKind::File, 0, t(T1), 0),
        ("/broken-hard".into(), EntryKind::LinkBroken, 0, t(T2), 0),
    ];
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn gz(raw: &[u8]) -> Vec<u8> {
    let mut w = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    w.write_all(raw).unwrap();
    w.finish().unwrap()
}

/// Два члена gzip подряд (как `cat a.gz b.gz`).
fn gz2(raw: &[u8]) -> Vec<u8> {
    let cut = raw.len() / 3;
    let mut v = gz(&raw[..cut]);
    v.extend(gz(&raw[cut..]));
    v
}

fn bz2(raw: &[u8]) -> Vec<u8> {
    let mut w = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    w.write_all(raw).unwrap();
    w.finish().unwrap()
}

fn xz(raw: &[u8]) -> Vec<u8> {
    let mut w =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    w.write_all(raw).unwrap();
    w.finish().unwrap()
}

fn zst(raw: &[u8]) -> Vec<u8> {
    zstd::encode_all(raw, 3).unwrap()
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> Location {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    Location::local(p)
}

#[test]
fn listing_matches_tar_tv_in_every_compressor() {
    let dir = tmp();
    let raw = build_tar();
    let cases: [(&str, Vec<u8>, &str); 5] = [
        ("all.tar", raw.clone(), "tar"),
        ("all.tar.gz", gz2(&raw), "tar.gz"),
        ("all.tbz2", bz2(&raw), "tar.bz2"),
        ("all.tar.xz", xz(&raw), "tar.xz"),
        ("all.tar.zst", zst(&raw), "tar.zst"),
    ];
    for (name, bytes, label) in cases {
        let v = vfs();
        let root = root_of("tar", put(dir.path(), name, &bytes));
        // Корень потоком: те же имена, что и после разбора (размеры каталогов — потом).
        let mut streamed: Vec<String> = list(&v, &root)
            .unwrap()
            .into_iter()
            .map(|e| format!("/{}", e.name))
            .collect();
        streamed.sort();
        let mut top: Vec<String> = expected()
            .into_iter()
            .map(|e| e.0)
            .filter(|p| p.matches('/').count() == 1)
            .collect();
        top.sort();
        assert_eq!(streamed, top, "{name}");
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert_eq!(tree(&v, &root), expected(), "{name}");
        assert_eq!(idx.format, label, "{name}");
        let s = idx.summary();
        assert_eq!(s.files, 9, "{name}: файлы и ссылки");
        assert_eq!(s.unpacked, 1234 * 2 + 18 + 700 + 3000 + 6, "{name}");
        if label == "tar" {
            assert_eq!(
                s.packed,
                s.unpacked - 1234,
                "plain tar не сжат, hardlink — без данных"
            );
        } else {
            assert!(
                s.packed > 0 && s.packed <= bytes.len() as u64,
                "{name}: {s:?}"
            );
        }
        assert_eq!(
            read(&v, &at(&root, "/src/main.rs")).unwrap(),
            data(1, 1234),
            "{name}"
        );
        assert_eq!(
            read(&v, &at(&root, "/src/main-hard.rs")).unwrap(),
            data(1, 1234),
            "{name}: hardlink — данные цели"
        );
        assert_eq!(
            read(&v, &at(&root, &format!("/{PAX_NAME}"))).unwrap(),
            data(3, 3000),
            "{name}"
        );
        assert_eq!(read(&v, &at(&root, "/link-to-main")).unwrap(), b"");
        assert_eq!(read(&v, &at(&root, "/empty.txt")).unwrap(), b"");
        let pax = at(&root, &format!("/{PAX_NAME}"));
        let m = v.stat(&pax, true).unwrap();
        assert_eq!(
            m.mtime, 1_700_000_300_500_000_000,
            "{name}: доли секунды из PAX"
        );
        let caps = v.caps(&at(&root, "/src/main.rs"));
        let want = if label == "tar" {
            Caps::SEEK
        } else {
            Caps::SEQUENTIAL
        };
        assert!(
            caps.has(want) && caps.has(Caps::READ_ONLY),
            "{name}: {caps:?}"
        );
    }
}

#[test]
fn plain_tar_entry_seeks() {
    let dir = tmp();
    let v = vfs();
    let root = root_of("tar", put(dir.path(), "s.tar", &build_tar()));
    assert!(
        v.open_read(&at(&root, "/deep")).is_err(),
        "каталог не читается"
    );
    let mut f = v.open_read(&at(&root, "/src/main.rs")).unwrap();
    assert_eq!(f.len(), Some(1234));
    f.seek(SeekFrom::Start(1000)).unwrap();
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, data(1, 1234)[1000..]);

    // Сжатая запись: «где я» — можно, назад — нет.
    let root = root_of("tar", put(dir.path(), "s.tar.gz", &gz(&build_tar())));
    let mut f = v.open_read(&at(&root, "/src/main.rs")).unwrap();
    assert_eq!(f.stream_position().unwrap(), 0);
    let mut head = [0u8; 10];
    f.read_exact(&mut head).unwrap();
    assert!(f.seek(SeekFrom::Start(0)).is_err());
}

#[test]
fn single_compressed_files_are_one_entry() {
    let dir = tmp();
    let body = data(7, 300_000);
    let cases: [(&str, Vec<u8>, &str, &str); 5] = [
        ("dump.sql.gz", gz2(&body), "gz", "dump.sql"),
        ("dump.sql.bz2", bz2(&body), "bz2", "dump.sql"),
        ("dump.sql.xz", xz(&body), "xz", "dump.sql"),
        ("dump.sql.zst", zst(&body), "zst", "dump.sql"),
        ("notes.tgz", gz(&body), "gz", "notes.tar"),
    ];
    for (name, bytes, label, inner) in cases {
        let v = vfs();
        let root = root_of("tar", put(dir.path(), name, &bytes));
        let items = list(&v, &root).unwrap();
        assert_eq!(items.len(), 1, "{name}");
        assert_eq!(items[0].name, inner, "{name}");
        assert_eq!(items[0].size, body.len() as u64, "{name}");
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert_eq!(idx.format, label, "{name}");
        assert_eq!(idx.summary().packed, bytes.len() as u64, "{name}");
        let p = at(&root, &format!("/{inner}"));
        assert_eq!(read(&v, &p).unwrap(), body, "{name}");
        assert!(v.caps(&p).has(Caps::SEQUENTIAL), "{name}");
        assert!(
            v.extract_plan(&[p], 1).is_none(),
            "{name}: чтение — open_read"
        );
    }
}

/// Первые строки корня tar.gz приходят до конца разбора; сводка — неполная.
#[test]
fn tar_gz_root_streams_before_the_end() {
    let dir = tmp();
    let mut b = Builder::new(Vec::new());
    for i in 0..20_000 {
        file(
            &mut b,
            &format!("f{i:05}.txt"),
            b"0123456789abcdef",
            0o644,
            T1,
        );
    }
    let root = root_of(
        "tar",
        put(dir.path(), "many.tar.gz", &gz(&b.into_inner().unwrap())),
    );
    let v = vfs();
    let mut s = v.list(&root, &ListOpts::default()).unwrap();
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    let more = s.next_batch(&mut out, &cancel).unwrap();
    assert!(more, "разбор не окончен");
    assert!(!out.is_empty() && out.len() < 20_000, "{}", out.len());
    assert_eq!(out[0].name, "f00000.txt", "в порядке архива");
    assert!(
        v.cache().latest(root.outer.as_deref().unwrap()).is_none(),
        "индекс ещё строится"
    );
    while s.next_batch(&mut out, &cancel).unwrap() {}
    assert_eq!(out.len(), 20_000);
    let idx = v.cache().latest(root.outer.as_deref().unwrap()).unwrap();
    assert_eq!(idx.summary().files, 20_000);
}

/// `extract_plan` на 1000 файлов из tar.zst — один проход по архиву.
#[test]
fn extract_plan_reads_tar_zst_in_one_pass() {
    let dir = tmp();
    let mut b = Builder::new(Vec::new());
    let body = |i: usize| data((i % 8) as u8, 100 + i * 3);
    for i in 0..1000 {
        file(
            &mut b,
            &format!("d{}/f{i:04}.bin", i % 10),
            &body(i),
            0o644,
            T1,
        );
    }
    let root = root_of(
        "tar",
        put(dir.path(), "k.tar.zst", &zst(&b.into_inner().unwrap())),
    );
    let v = vfs();
    // Запрос в обратном порядке: план всё равно идёт по архиву.
    let files: Vec<Location> = (0..1000)
        .rev()
        .map(|i| at(&root, &format!("/d{}/f{i:04}.bin", i % 10)))
        .collect();
    assert!(
        v.extract_plan(&files, 4).is_none(),
        "индекса нет — плана нет"
    );
    v.index(&root, &CancelToken::new()).unwrap();
    let before = v.format().passes();
    let streams = v.extract_plan(&files, 4).unwrap();
    assert_eq!(streams.len(), 1);
    let cancel = CancelToken::new();
    let mut seen = vec![0u32; files.len()];
    let mut order = Vec::new();
    for mut s in streams {
        while let Some((i, mut r)) = s.next(&cancel).unwrap() {
            let i = i as usize;
            let n = 999 - i;
            // Каждый десятый читаем не до конца: остаток пропускает сам backend.
            if n % 10 == 3 {
                let mut half = vec![0u8; 10];
                r.read_exact(&mut half).unwrap();
                assert_eq!(half, body(n)[..10]);
            } else {
                let mut got = Vec::new();
                r.read_to_end(&mut got).unwrap();
                assert_eq!(got, body(n), "{n}");
            }
            seen[i] += 1;
            order.push(n);
        }
    }
    assert_eq!(v.format().passes() - before, 1, "один проход");
    assert!(seen.iter().all(|&n| n == 1), "{seen:?}");
    assert!(order.windows(2).all(|w| w[0] < w[1]), "порядок архива");
}

#[test]
fn extract_plan_hardlink_with_target_and_plain_tar() {
    let dir = tmp();
    let raw = build_tar();
    let names = [
        "/src/main-hard.rs",
        "/src/main.rs",
        "/run.sh",
        "/link-to-main",
    ];
    for (name, bytes, streams) in [("p.tar", raw.clone(), 1), ("p.tar.gz", gz(&raw), 2)] {
        let v = vfs();
        let root = root_of("tar", put(dir.path(), name, &bytes));
        v.index(&root, &CancelToken::new()).unwrap();
        let files: Vec<Location> = names.iter().map(|p| at(&root, p)).collect();
        let plan = v.extract_plan(&files, 2).unwrap();
        // Сжатый: hardlink и цель — одна запись, второй раз — свой проход.
        assert_eq!(plan.len(), streams, "{name}");
        let cancel = CancelToken::new();
        let mut seen = vec![0u32; files.len()];
        for mut s in plan {
            while let Some((i, mut r)) = s.next(&cancel).unwrap() {
                let mut got = Vec::new();
                r.read_to_end(&mut got).unwrap();
                drop(r);
                let i = i as usize;
                seen[i] += 1;
                assert_eq!(got, read(&v, &files[i]).unwrap(), "{name}: {}", names[i]);
            }
        }
        assert!(seen.iter().all(|&n| n == 1), "{name}: {seen:?}");
    }
}

#[test]
fn junk_truncated_and_old_tar() {
    let dir = tmp();
    let v = vfs();
    let junk = put(dir.path(), "junk.tar", b"definitely not a tar archive");
    let e = list(&v, &root_of("tar", junk)).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::InvalidData), "{e}");

    let full = gz(&build_tar());
    let cut = put(dir.path(), "cut.tar.gz", &full[..full.len() / 2]);
    let e = list(&v, &root_of("tar", cut)).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::InvalidData), "{e}");

    // v7: без `ustar` — по сумме заголовка.
    let mut b = Builder::new(Vec::new());
    let mut h = Header::new_old();
    h.set_size(3);
    h.set_mode(0o644);
    h.set_mtime(T1);
    b.append_data(&mut h, "old.txt", &b"v7!"[..]).unwrap();
    let old = put(dir.path(), "old.tar", &b.into_inner().unwrap());
    let root = root_of("tar", old);
    let items = list(&v, &root).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(read(&v, &at(&root, "/old.txt")).unwrap(), b"v7!");

    // Пустой tar: два нулевых блока.
    let empty = put(dir.path(), "empty.tar", &[0u8; 1024]);
    assert_eq!(list(&v, &root_of("tar", empty)).unwrap().len(), 0);
}
