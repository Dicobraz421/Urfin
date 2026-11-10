//! zip (S8-25): разбор, имена, время, чтение, выборка, F3 через Spool, наблюдение.
//!
//! Фикстуры: собранные здесь крейтом `zip` и заранее — 7-Zip 24.07 (`tests/fixtures/`):
//! - `7z-cp866-upath.zip` — `7z a -tzip -mcp=866`: имена cp866 и extra 0x7075, время NTFS;
//! - `deflate64.zip` — `7z a -tzip -mm=Deflate64`;
//! - `zipcrypto.zip` — `7z a -tzip -mem=ZipCrypto -psecret`.
//!
//! Ожидаемые значения сверены с `7z l -slt` (время там местное, здесь — UTC).

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_arc::{ArcCache, ArcCfg, ArcVfs, ZipFormat, root_of};
use urfin_core::{Attrs, CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{Caps, ListOpts, Vfs, VfsError, VfsRegistry, WatchEvent, WatchId, WatchTx};
use zip::write::{FullFileOptions, SimpleFileOptions};
use zip::{AesMode, CompressionMethod, DateTime, ZipWriter};

const MIB: u64 = 1 << 20;

fn vfs() -> ArcVfs<ZipFormat> {
    ArcVfs::new(ZipFormat::with_codepage(866), ArcCache::new(256 * MIB))
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

fn entry<'a>(v: &'a [Entry], name: &str) -> &'a Entry {
    v.iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("нет {name:?} в {:?}", names(v)))
}

fn names(v: &[Entry]) -> Vec<&str> {
    v.iter().map(|e| e.name.as_str()).collect()
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

fn kind_of(e: &VfsError) -> Option<io::ErrorKind> {
    match e {
        VfsError::Io { source, .. } => Some(source.kind()),
        // Ошибка при чтении данных (поток записи) — через `VfsError::from_io`.
        VfsError::Denied(_) => Some(io::ErrorKind::PermissionDenied),
        _ => None,
    }
}

/// Наносекунды Unix для UTC-времени.
fn utc(s: &str) -> i64 {
    let ts: jiff::Timestamp = s.parse().unwrap();
    i64::try_from(ts.as_nanosecond()).unwrap()
}

/// DOS-время — местное, как его показывает `7z l`.
fn local(y: i16, mo: i8, d: i8, h: i8, mi: i8, s: i8) -> i64 {
    let dt = jiff::civil::DateTime::new(y, mo, d, h, mi, s, 0).unwrap();
    let ts = jiff::tz::TimeZone::system()
        .to_ambiguous_timestamp(dt)
        .compatible()
        .unwrap();
    i64::try_from(ts.as_nanosecond()).unwrap()
}

fn dos() -> DateTime {
    DateTime::from_date_and_time(2023, 6, 1, 12, 34, 56).unwrap()
}

fn opts(m: CompressionMethod) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(m)
        .last_modified_time(dos())
}

/// Детерминированные сжимаемые данные.
fn data(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| b"abcdefgh"[(i / 7 + usize::from(seed)) % 8])
        .collect()
}

/// `Папка/файл.txt` в cp866, без флага UTF-8 и без 0x7075.
const CP866: &[u8] = b"\x8f\xa0\xaf\xaa\xa0/\xe4\xa0\xa9\xab.txt";

/// Архив со всем, что умеет записать крейт `zip`.
fn build(path: &Path) {
    let mut w = ZipWriter::new(std::fs::File::create(path).unwrap());
    w.add_directory("dir/", opts(CompressionMethod::Stored))
        .unwrap();
    w.start_file("dir/a.txt", opts(CompressionMethod::Deflated))
        .unwrap();
    w.write_all(&data(1, 10_000)).unwrap();
    w.start_file("Отчёт.txt", opts(CompressionMethod::Stored))
        .unwrap();
    w.write_all(b"stored utf-8").unwrap();
    w.start_file_with_options(opts(CompressionMethod::Stored).name(CP866))
        .unwrap();
    w.write_all(b"cp866").unwrap();
    // UT (0x5455): mtime 2021-01-02T03:04:05Z.
    let mut ut = FullFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(dos());
    let secs = 1_609_556_645_i32.to_le_bytes();
    ut.add_extra_field(0x5455, [1, secs[0], secs[1], secs[2], secs[3]], false)
        .unwrap();
    w.start_file("ut.txt", ut).unwrap();
    w.write_all(b"ut").unwrap();
    w.start_file("__MACOSX/._ut.txt", opts(CompressionMethod::Stored))
        .unwrap();
    w.write_all(b"\x00\x05\x16\x07").unwrap();
    w.start_file("z.bin", opts(CompressionMethod::Zstd))
        .unwrap();
    w.write_all(&data(2, 20_000)).unwrap();
    w.start_file("b.bin", opts(CompressionMethod::Bzip2))
        .unwrap();
    w.write_all(&data(3, 20_000)).unwrap();
    w.start_file(
        "big64.bin",
        opts(CompressionMethod::Deflated).large_file(true),
    )
    .unwrap();
    w.write_all(&data(4, 50_000)).unwrap();
    w.start_file(
        "secret.txt",
        opts(CompressionMethod::Deflated).with_aes_encryption(AesMode::Aes256, "pw"),
    )
    .unwrap();
    w.write_all(b"AES-256 secret").unwrap();
    w.set_comment("комментарий").unwrap();
    w.finish().unwrap();
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("urfin-arc-zip-")
        .tempdir()
        .unwrap()
}

#[test]
fn generated_names_sizes_times() {
    let dir = tmp();
    let file = dir.path().join("all.zip");
    build(&file);
    let v = vfs();
    let root = root_of("zip", Location::local(file));
    let top = list(&v, &root).unwrap();
    assert_eq!(
        names(&top),
        [
            "__MACOSX",
            "b.bin",
            "big64.bin",
            "dir",
            "secret.txt",
            "ut.txt",
            "z.bin",
            "Отчёт.txt",
            "Папка"
        ]
    );
    let dos_t = local(2023, 6, 1, 12, 34, 56);
    let a = &list(&v, &at(&root, "/dir")).unwrap()[0];
    assert_eq!((a.name.as_str(), a.size, a.mtime), ("a.txt", 10_000, dos_t));
    assert_eq!(entry(&top, "dir").kind, EntryKind::Dir);
    assert_eq!(entry(&top, "Отчёт.txt").size, 12);
    let cp = &list(&v, &at(&root, "/Папка")).unwrap()[0];
    assert_eq!((cp.name.as_str(), cp.size), ("файл.txt", 5));
    assert!(!cp.attrs.has(Attrs::ESCAPED_NAME));
    assert_eq!(entry(&top, "ut.txt").mtime, utc("2021-01-02T03:04:05Z"));
    assert_eq!(entry(&top, "big64.bin").size, 50_000);
    assert_eq!(entry(&top, "z.bin").size, 20_000);
    assert!(entry(&top, "secret.txt").attrs.has(Attrs::ENCRYPTED));
    let mac = &list(&v, &at(&root, "/__MACOSX")).unwrap()[0];
    assert_eq!(mac.name, "._ut.txt");
    assert!(mac.attrs.has(Attrs::DOTFILE));

    let idx = v.index(&root, &CancelToken::new()).unwrap();
    assert_eq!(idx.format, "zip");
    assert_eq!(idx.comment.as_deref(), Some("комментарий"));
    let s = idx.summary();
    assert!(s.encrypted && s.complete);
    assert_eq!(s.files, 9);
    // «Сжато» — из central directory: сжимаемые данные стали меньше.
    let z = idx.lookup(Path::new("/z.bin")).unwrap();
    assert!(idx.node(z).packed < 20_000);
}

#[test]
fn reads_every_method_and_seeks_only_stored() {
    let dir = tmp();
    let file = dir.path().join("all.zip");
    build(&file);
    let v = vfs();
    let root = root_of("zip", Location::local(file));
    assert_eq!(read(&v, &at(&root, "/dir/a.txt")).unwrap(), data(1, 10_000));
    assert_eq!(read(&v, &at(&root, "/z.bin")).unwrap(), data(2, 20_000));
    assert_eq!(read(&v, &at(&root, "/b.bin")).unwrap(), data(3, 20_000));
    assert_eq!(read(&v, &at(&root, "/big64.bin")).unwrap(), data(4, 50_000));
    assert_eq!(read(&v, &at(&root, "/Папка/файл.txt")).unwrap(), b"cp866");

    // Stored: поддиапазон с seek.
    let stored = at(&root, "/Отчёт.txt");
    assert!(v.caps(&stored).has(Caps::SEEK | Caps::READ_ONLY));
    let mut r = v.open_read(&stored).unwrap();
    assert_eq!(r.len(), Some(12));
    r.seek(SeekFrom::Start(7)).unwrap();
    let mut tail = String::new();
    r.read_to_string(&mut tail).unwrap();
    assert_eq!(tail, "utf-8");
    r.seek(SeekFrom::End(-12)).unwrap();
    let mut head = [0u8; 6];
    r.read_exact(&mut head).unwrap();
    assert_eq!(&head, b"stored");

    // Сжатая: поток, `len` известен, `seek` — ошибка.
    let packed = at(&root, "/dir/a.txt");
    let caps = v.caps(&packed);
    assert!(caps.has(Caps::SEQUENTIAL) && !caps.has(Caps::SEEK));
    let mut r = v.open_read(&packed).unwrap();
    assert_eq!(r.len(), Some(10_000));
    assert!(r.seek(SeekFrom::Start(100)).is_err());
    assert_eq!(r.stream_position().unwrap(), 0);
    // Брошенный на середине читатель не держит поток.
    let mut part = [0u8; 10];
    r.read_exact(&mut part).unwrap();
    drop(r);
}

#[test]
fn aes_needs_the_password() {
    let dir = tmp();
    let file = dir.path().join("all.zip");
    build(&file);
    let v = vfs();
    let root = root_of("zip", Location::local(file));
    let secret = at(&root, "/secret.txt");
    let e = read(&v, &secret).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");
    let idx = v.index(&root, &CancelToken::new()).unwrap();
    idx.ctx().set_password(Some("wrong"));
    let e = read(&v, &secret).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");
    idx.ctx().set_password(Some("pw"));
    assert_eq!(read(&v, &secret).unwrap(), b"AES-256 secret");
    assert!(
        !format!("{:?}", idx.ctx()).contains("pw\""),
        "пароль не в Debug"
    );
}

#[test]
fn fixtures_from_7zip() {
    let v = vfs();
    let ntfs = utc("2024-03-15T07:20:30Z");

    // cp866 + 0x7075: имя — из Unicode Path, время — NTFS.
    let root = root_of("zip", fixture("7z-cp866-upath.zip"));
    let top = list(&v, &root).unwrap();
    assert_eq!(names(&top), ["Отчёт.txt", "Папка"]);
    assert_eq!((top[0].size, top[0].mtime), (31, ntfs));
    assert_eq!(top[1].mtime, ntfs);
    let sub = list(&v, &at(&root, "/Папка")).unwrap();
    assert_eq!(names(&sub), ["файл.txt"]);
    assert_eq!((sub[0].size, sub[0].mtime), (28, ntfs));
    assert_eq!(
        String::from_utf8(read(&v, &at(&root, "/Отчёт.txt")).unwrap()).unwrap(),
        "Отчёт за квартал\n"
    );
    assert_eq!(
        String::from_utf8(read(&v, &at(&root, "/Папка/файл.txt")).unwrap()).unwrap(),
        "вложенный файл\n"
    );

    // Deflate64.
    let root = root_of("zip", fixture("deflate64.zip"));
    let top = list(&v, &root).unwrap();
    assert_eq!(names(&top), ["big.txt"]);
    assert_eq!((top[0].size, top[0].mtime), (5000, ntfs));
    let idx = v.index(&root, &CancelToken::new()).unwrap();
    assert_eq!(
        idx.node(idx.lookup(Path::new("/big.txt")).unwrap()).packed,
        53
    );
    assert_eq!(
        read(&v, &at(&root, "/big.txt")).unwrap(),
        b"deflate64 ".repeat(500)
    );

    // ZipCrypto: deflate и stored, пароль `secret`.
    let root = root_of("zip", fixture("zipcrypto.zip"));
    let top = list(&v, &root).unwrap();
    assert_eq!(names(&top), ["big.txt", "Отчёт.txt"]);
    assert!(top.iter().all(|e| e.attrs.has(Attrs::ENCRYPTED)));
    assert_eq!((top[1].size, top[1].mtime), (31, ntfs));
    let stored = at(&root, "/Отчёт.txt");
    assert!(
        !v.caps(&stored).has(Caps::SEEK),
        "зашифрованная stored — поток"
    );
    let e = read(&v, &stored).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::PermissionDenied), "{e}");
    let idx = v.index(&root, &CancelToken::new()).unwrap();
    idx.ctx().set_password(Some("secret"));
    assert_eq!(
        String::from_utf8(read(&v, &stored).unwrap()).unwrap(),
        "Отчёт за квартал\n"
    );
    assert_eq!(
        read(&v, &at(&root, "/big.txt")).unwrap(),
        b"deflate64 ".repeat(500)
    );
}

#[test]
fn extract_plan_covers_each_file_once() {
    let dir = tmp();
    let file = dir.path().join("all.zip");
    build(&file);
    let v = vfs();
    let root = root_of("zip", Location::local(file));
    let paths = [
        "/z.bin",
        "/dir/a.txt",
        "/Отчёт.txt",
        "/b.bin",
        "/big64.bin",
        "/Папка/файл.txt",
    ];
    let files: Vec<Location> = paths.iter().map(|p| at(&root, p)).collect();
    assert!(
        v.extract_plan(&files, 3).is_none(),
        "индекса нет — плана нет"
    );
    v.index(&root, &CancelToken::new()).unwrap();
    assert!(
        v.extract_plan(&[at(&root, "/dir")], 2).is_none(),
        "каталог — не план"
    );
    let streams = v.extract_plan(&files, 3).unwrap();
    assert!((1..=3).contains(&streams.len()));
    let mut seen = vec![0u32; files.len()];
    let cancel = CancelToken::new();
    for mut s in streams {
        while let Some((i, mut r)) = s.next(&cancel).unwrap() {
            let mut got = Vec::new();
            r.read_to_end(&mut got).unwrap();
            drop(r);
            let i = i as usize;
            seen[i] += 1;
            assert_eq!(got, read(&v, &files[i]).unwrap(), "{}", paths[i]);
        }
    }
    assert!(seen.iter().all(|&n| n == 1), "{seen:?}");
}

#[test]
fn not_a_zip_and_multivolume_are_clear_errors() {
    let dir = tmp();
    let v = vfs();
    let junk = dir.path().join("junk.zip");
    std::fs::write(&junk, b"definitely not a zip").unwrap();
    let e = list(&v, &root_of("zip", Location::local(junk))).unwrap_err();
    assert_eq!(kind_of(&e), Some(io::ErrorKind::InvalidData), "{e}");

    // Первый том разрезанного архива.
    let split = dir.path().join("part.zip");
    std::fs::write(&split, b"PK\x07\x08PK\x03\x04 truncated").unwrap();
    let e = list(&v, &root_of("zip", Location::local(split))).unwrap_err();
    urfin_i18n::set_language("en");
    assert!(e.to_string().contains("Multi-volume"), "{e}");
}

#[test]
fn backends_register_zip() {
    let cfg = ArcCfg::default();
    let cache = urfin_arc::cache(&cfg);
    let schemes: Vec<_> = urfin_arc::backends(&cache, &cfg)
        .iter()
        .map(|b| b.scheme())
        .collect();
    assert_eq!(schemes, ["zip"]);
}

/// F3: `open_source` вьювера читает сжатую запись через Spool (не `Owned`: больше 4 мебибайт).
#[test]
fn viewer_reads_compressed_entry_through_spool() {
    let dir = tmp();
    let file = dir.path().join("big.zip");
    let body = data(5, 5 << 20);
    let mut w = ZipWriter::new(std::fs::File::create(&file).unwrap());
    w.start_file("big.log", opts(CompressionMethod::Deflated))
        .unwrap();
    w.write_all(&body).unwrap();
    w.finish().unwrap();

    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(vfs()));
    let loc = at(&root_of("zip", Location::local(file)), "/big.log");
    let spool = dir.path().join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    let o = urfin_view::SourceOpts {
        spool_dir: Some(spool),
        ..urfin_view::SourceOpts::default()
    };
    let opened = urfin_view::open_source(&reg, &loc, &o, &CancelToken::new()).unwrap();
    let pump = opened.pump.expect("сжатая запись — Spool");
    std::thread::spawn(move || pump.run());
    let src = opened.src;
    let until = Instant::now() + Duration::from_secs(20);
    while !src.is_final() {
        assert!(Instant::now() < until, "Spool не дочитан");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(src.len(), body.len() as u64);
    let mut got = vec![0u8; 4096];
    let off = 3 << 20;
    let n = src.read_at(off, &mut got).unwrap();
    assert_eq!(&got[..n], &body[off as usize..off as usize + n]);
}

/// Изменение файла архива — `Overflow`, удаление — `Gone`; соседние файлы — тишина.
#[cfg(windows)]
#[test]
fn watch_reports_changes_of_the_archive_file() {
    let dir = tmp();
    let file = dir.path().join("w.zip");
    build(&file);
    let v = vfs();
    let root = root_of("zip", Location::local(file.clone()));
    let (tx, rx) = crossbeam_channel::unbounded();
    let guard = v.watch(&root, WatchTx { id: WatchId(7), tx }).unwrap();
    std::fs::write(dir.path().join("other.txt"), b"x").unwrap();
    let wait = |want: fn(&WatchEvent) -> bool| {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((id, ev)) => {
                    assert_eq!(id, WatchId(7));
                    if want(&ev) {
                        return ev;
                    }
                    assert!(matches!(ev, WatchEvent::Overflow), "лишнее событие {ev:?}");
                }
                Err(e) => panic!("нет события: {e}"),
            }
        }
    };
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(b"tail")
        .unwrap();
    wait(|e| matches!(e, WatchEvent::Overflow));
    std::fs::remove_file(&file).unwrap();
    wait(|e| matches!(e, WatchEvent::Gone));
    drop(guard);
}
