//! RAR (S8-34): листинг, чтение, пароли, тома, ошибки; без feature `rar` — файл открывает ОС.
//!
//! Создавать RAR нам нельзя (лицензия `UnRAR`), фикстуры — готовые файлы
//! (`tests/fixtures/rar/README.md`): тестовые архивы крейта `unrar` 0.5.8. Ожидаемые
//! значения сверены с `7z l -slt` 7-Zip (тот же разбор, что `unrar lt`; время там местное,
//! здесь — UTC).

use std::path::Path;
use std::sync::Arc;

use urfin_arc::{ArcCache, ArcCfg, open_resolver};
use urfin_core::{Args, Attrs, Entry, EntryKind, Location};
use urfin_vfs::VfsRegistry;

fn fixture(name: &str) -> Location {
    Location::local(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rar")
            .join(name),
    )
}

/// Реестр, как у приложения: локальная ФС и [`urfin_arc::backends`].
fn registry(cache: &Arc<ArcCache>) -> Arc<VfsRegistry> {
    let mut reg = VfsRegistry::with_local();
    for b in urfin_arc::backends(cache, &ArcCfg::default()) {
        reg.register(b);
    }
    Arc::new(reg)
}

/// Enter по `.rar`: с feature `rar` — вход в архив, без неё — `None`, файл открывает ОС.
#[test]
fn enter_rar_needs_the_feature() {
    let cache = ArcCache::new(64 << 20);
    let r = open_resolver(&ArcCfg::default(), registry(&cache));
    let f = fixture("version.rar");
    let dir = Location::local(f.path.parent().unwrap());
    let e = Entry {
        name: "version.rar".into(),
        size: 87,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind: EntryKind::File,
    };
    let cmd = r.resolve(&dir, &e, &Args::default());
    if cfg!(feature = "rar") {
        let cmd = cmd.expect("rar входит в архив");
        assert_eq!(cmd.id.as_str(), "panel.go");
        let to = cmd.args.loc("path").unwrap();
        assert_eq!(to.backend, "rar");
        assert_eq!(to.outer.as_deref(), Some(&f));
    } else {
        assert!(cmd.is_none(), "без feature rar — приложение ОС");
    }
}

#[cfg(feature = "rar")]
mod corpus {
    use std::io::{self, Read};
    use std::path::PathBuf;

    use urfin_arc::{ArcCache, ArcVfs, PACKED, RarFormat, root_of};
    use urfin_core::{Attrs, CancelToken, Entry, EntryKind, Location};
    use urfin_vfs::{Caps, ExtCell, ListOpts, Vfs, VfsError};

    use super::fixture;

    fn vfs() -> ArcVfs<RarFormat> {
        ArcVfs::new(RarFormat::new(), ArcCache::new(64 << 20))
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

    /// Строка дерева: путь, вид, размер, «Сжато» (`None` — `<DIR>`), время UTC (с), атрибуты.
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
                let secs = if e.mtime == i64::MIN {
                    i64::MIN
                } else {
                    e.mtime.div_euclid(1_000_000_000)
                };
                out.push((p, e.kind, e.size, packed, secs, e.attrs.0));
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

    fn row(p: &str, size: u64, packed: u64, secs: i64, attrs: u32) -> Row {
        (p.into(), EntryKind::File, size, Some(packed), secs, attrs)
    }

    /// Нужен пароль: при открытии — `arc-error-no-password`, при чтении данных — та же
    /// ошибка через `io::Error` (`PermissionDenied` → `Denied`).
    fn denied(e: &VfsError) -> bool {
        match e {
            VfsError::Io { source, .. } => source.kind() == io::ErrorKind::PermissionDenied,
            VfsError::Denied(_) => true,
            _ => false,
        }
    }

    const GITIGNORE: &[u8] = b"target\nCargo.lock\n";

    /// RAR 2.9 (RAR4): `VERSION` 11 Б, сжато 21, 2015-08-07 17:21:08 (+03).
    #[test]
    fn rar4_lists_and_reads() {
        let v = vfs();
        let root = root_of("rar", fixture("version.rar"));
        assert_eq!(tree(&v, &root), [row("/VERSION", 11, 21, 1_438_957_268, 0)]);
        assert_eq!(read(&v, &at(&root, "/VERSION")).unwrap(), b"unrar-0.4.0");
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert_eq!(idx.format, "rar");
        assert!(!idx.solid && !idx.encrypted);
        let s = idx.summary();
        assert_eq!(
            (s.files, s.unpacked, s.packed, s.complete),
            (1, 11, 21, true)
        );
        assert!(v.caps(&at(&root, "/VERSION")).has(Caps::SEQUENTIAL));
    }

    /// RAR5 с комментарием, stored: `.gitignore` 18/18, режим Unix `-rw-r--r--`.
    #[test]
    fn rar5_comment_and_unix_mode() {
        let v = vfs();
        let root = root_of("rar", fixture("comment.rar"));
        assert_eq!(
            tree(&v, &root),
            [row("/.gitignore", 18, 18, 1_575_907_285, Attrs::DOTFILE)]
        );
        assert_eq!(read(&v, &at(&root, "/.gitignore")).unwrap(), GITIGNORE);
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert_eq!(idx.comment.as_deref().map(str::trim), Some("abcdef12345"));
    }

    #[test]
    fn solid_archive() {
        let v = vfs();
        let root = root_of("rar", fixture("solid.rar"));
        assert_eq!(
            tree(&v, &root),
            [row("/.gitignore", 18, 32, 1_573_595_805, Attrs::DOTFILE)]
        );
        assert!(v.index(&root, &CancelToken::new()).unwrap().solid);
        assert_eq!(read(&v, &at(&root, "/.gitignore")).unwrap(), GITIGNORE);
    }

    /// RAR4, данные зашифрованы (`-p`, пароль `unrar`): имена видны без пароля.
    #[test]
    fn encrypted_entries_need_the_password() {
        let v = vfs();
        let root = root_of("rar", fixture("crypted.rar"));
        let enc = Attrs::DOTFILE | Attrs::ENCRYPTED;
        assert_eq!(
            tree(&v, &root),
            [row("/.gitignore", 18, 32, 1_438_607_366, enc)]
        );
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert!(idx.encrypted);
        assert_eq!(idx.cipher, Some("AES-128"));
        let f = at(&root, "/.gitignore");
        assert!(denied(&read(&v, &f).unwrap_err()), "без пароля");
        idx.ctx().set_password(Some("wrong"));
        let r = read(&v, &f);
        assert!(r.as_ref().is_err_and(denied), "неверный пароль: {r:?}");
        idx.ctx().set_password(Some("unrar"));
        assert_eq!(read(&v, &f).unwrap(), GITIGNORE);
    }

    /// RAR5, заголовки зашифрованы (`-hp`, пароль `password`): пароль до листинга.
    #[test]
    fn encrypted_headers_need_the_password_first() {
        let v = vfs();
        let outer = fixture("comment-hpw-password.rar");
        let root = root_of("rar", outer.clone());
        assert!(denied(&list(&v, &root).unwrap_err()), "без пароля");
        v.cache().set_password(&outer, Some("wrong"));
        assert!(denied(&list(&v, &root).unwrap_err()), "неверный пароль");
        v.cache().set_password(&outer, Some("password"));
        let enc = Attrs::DOTFILE | Attrs::ENCRYPTED;
        assert_eq!(
            tree(&v, &root),
            [row("/.gitignore", 18, 32, 1_575_907_285, enc)]
        );
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert!(idx.encrypted);
        assert_eq!(idx.cipher, Some("AES-256"));
        assert_eq!(read(&v, &at(&root, "/.gitignore")).unwrap(), GITIGNORE);
    }

    /// Первый том RAR4 без второго: листинг — всё, что в томе; файл с продолжением —
    /// ошибка с именем тома. Каталоги — `<DIR>` без «Сжато».
    #[test]
    fn first_volume_without_the_rest() {
        let v = vfs();
        let root = root_of("rar", fixture("archive.part1.rar"));
        let t = 1_438_607_366;
        let dir = |p: &str, secs: i64| (p.to_owned(), EntryKind::Dir, 0, None, secs, 0);
        assert_eq!(
            tree(&v, &root),
            [
                row("/Cargo.toml", 313, 243, t, 0),
                row("/build.rs", 2396, 546, t, 0),
                dir("/examples", i64::MIN),
                row("/examples/lister.rs", 2693, 1044, t, 0),
                dir("/src", i64::MIN),
                row("/src/lib.rs", 7870, 1652, t, 0),
                dir("/vendor", i64::MIN),
                dir("/vendor/unrar", t),
                row("/vendor/unrar/acknow.txt", 4280, 1971, t, 0),
                row("/vendor/unrar/arccmt.cpp", 4090, 1419, t, 0),
                row("/vendor/unrar/archive.cpp", 7850, 2745, t, 0),
            ]
        );
        let lib = read(&v, &at(&root, "/src/lib.rs")).unwrap();
        assert_eq!(lib.len(), 7870);
        assert!(lib.starts_with(b"extern crate libc;"));
        let e = read(&v, &at(&root, "/vendor/unrar/archive.cpp")).unwrap_err();
        assert!(e.to_string().contains("archive.part2.rar"), "{e}");
    }

    /// Имена RAR5 — UTF-16 как в архиве; пустой файл — без распаковщика, с `seek`.
    #[test]
    fn unicode_names_and_empty_file() {
        let v = vfs();
        let root = root_of("rar", fixture("utf8.rar"));
        assert_eq!(
            tree(&v, &root),
            [row("/foo\u{2014}bar", 0, 0, 1_532_323_874, 0)]
        );
        let empty = at(&root, "/foo\u{2014}bar");
        assert_eq!(read(&v, &empty).unwrap(), b"");
        assert!(v.caps(&empty).has(Caps::SEEK));

        let root = root_of("rar", fixture("unicode.rar"));
        let name = "/te\u{2026}\u{2015}st\u{270c}";
        assert_eq!(tree(&v, &root), [row(name, 17, 17, 1_575_909_292, 0)]);
        assert_eq!(
            String::from_utf8(read(&v, &at(&root, name)).unwrap()).unwrap(),
            "\u{1f183}\u{1f134}\u{1f182}\u{1f143}\n"
        );
    }

    /// Не RAR, пустой и обрезанный файлы с расширением `.rar` — ошибки, без паники.
    #[test]
    fn broken_and_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        let good = std::fs::read(&fixture("version.rar").path).unwrap();
        for (name, bytes) in [
            ("empty.rar", Vec::new()),
            ("text.rar", b"not an archive at all".to_vec()),
            (
                "zip.rar",
                b"PK\x05\x06".iter().copied().chain([0; 18]).collect(),
            ),
            ("cut.rar", good[..40].to_vec()),
        ] {
            let f = tmp.path().join(name);
            std::fs::write(&f, &bytes).unwrap();
            let v = vfs();
            let root = root_of("rar", Location::local(f));
            let res = list(&v, &root).and_then(|es| {
                es.iter()
                    .try_for_each(|e| read(&v, &at(&root, &format!("/{}", e.name))).map(drop))
            });
            let e = res.expect_err(name);
            assert!(matches!(e, VfsError::Io { .. }), "{name}: {e:?}");
        }
        // Одна сигнатура — пустой архив.
        let f = tmp.path().join("sig.rar");
        std::fs::write(&f, &good[..7]).unwrap();
        let root = root_of("rar", Location::local(f));
        assert_eq!(list(&vfs(), &root).unwrap().len(), 0);
    }
}
