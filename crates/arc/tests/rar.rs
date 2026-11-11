//! RAR (S8-34): листинг, чтение, пароли, тома, ошибки; без feature `rar` — файл открывает ОС.
//!
//! Создавать RAR нашим кодом нельзя (лицензия `UnRAR`), фикстуры — готовые файлы
//! (`tests/fixtures/rar/README.md`): тестовые архивы крейта `unrar` 0.5.8 (сверены с
//! `7z l -slt`) и корпус RAR5 `r5-*.rar` от Rar 7.23 (`gen.ps1`, сверен с `UnRAR lt`).
//! Время там местное, здесь — UTC.

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

    /// DOS-время RAR4 — местное: секунды UTC по поясу системы (как `UnRAR`).
    fn local(y: i16, mo: i8, d: i8, h: i8, mi: i8, sec: i8) -> i64 {
        let dt = jiff::civil::DateTime::new(y, mo, d, h, mi, sec, 0).unwrap();
        jiff::tz::TimeZone::system()
            .to_timestamp(dt)
            .unwrap()
            .as_second()
    }

    const GITIGNORE: &[u8] = b"target\nCargo.lock\n";

    /// RAR 2.9 (RAR4): `VERSION` 11 Б, сжато 21, 2015-08-07 17:21:08 (местное).
    #[test]
    fn rar4_lists_and_reads() {
        let v = vfs();
        let root = root_of("rar", fixture("version.rar"));
        assert_eq!(
            tree(&v, &root),
            [row("/VERSION", 11, 21, local(2015, 8, 7, 17, 21, 8), 0)]
        );
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
            [row(
                "/.gitignore",
                18,
                32,
                local(2015, 8, 3, 16, 9, 26),
                enc
            )]
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
        let t = local(2015, 8, 3, 16, 9, 26);
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

    // Корпус RAR5 (`tests/fixtures/rar/gen.ps1`, Rar 7.23): исходные файлы детерминированы,
    // ожидаемые значения — `UnRAR lt`. Время RAR5 — UTC: запись N — `T0` + N минут.

    /// 2024-03-01 12:00:00 UTC — `$Base` в `gen.ps1`.
    const T0: i64 = 1_709_294_400;

    fn min(m: i64) -> i64 {
        T0 + 60 * m
    }

    /// Псевдослучайные байты — `Lcg` в `gen.ps1`.
    fn lcg(seed: u32, len: usize) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 16) as u8
            })
            .collect()
    }

    /// Текст — `Text` в `gen.ps1`: `count` строк «line i of name».
    fn text(name: &str, count: usize) -> Vec<u8> {
        use std::fmt::Write as _;
        let mut s = String::new();
        for i in 0..count {
            let _ = writeln!(s, "line {i} of {name}");
        }
        s.into_bytes()
    }

    fn dir_row(p: &str, secs: i64) -> Row {
        (p.into(), EntryKind::Dir, 0, None, secs, 0)
    }

    /// Все файлы `names` одной выборкой: один поток; номера входа — в порядке записей
    /// архива, данные — как в исходных файлах.
    fn plan_all(v: &dyn Vfs, root: &Location, names: &[(&str, Vec<u8>)]) -> Vec<u32> {
        let files: Vec<Location> = names.iter().map(|(p, _)| at(root, p)).collect();
        let streams = v.extract_plan(&files, 4).expect("rar — план выборки");
        assert_eq!(streams.len(), 1, "rar — один читатель");
        let cancel = CancelToken::new();
        let mut order = Vec::new();
        for mut s in streams {
            while let Some((i, mut r)) = s.next(&cancel).unwrap() {
                let mut got = Vec::new();
                r.read_to_end(&mut got).unwrap();
                let (p, want) = &names[i as usize];
                assert!(got == *want, "{p}: {} Б вместо {}", got.len(), want.len());
                order.push(i);
            }
        }
        order
    }

    /// RAR5 `-m3 -htb -rr1%`: дерево с каталогами (и пустым), пустой файл, имена
    /// кириллицей, с `№`, суррогатной парой и CJK, атрибуты Windows, BLAKE2, запись
    /// восстановления.
    #[test]
    fn rar5_tree_names_and_attributes() {
        let v = vfs();
        let root = root_of("rar", fixture("r5-basic.rar"));
        let privet = "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}.txt";
        let katalog = "\u{43a}\u{430}\u{442}\u{430}\u{43b}\u{43e}\u{433}";
        let fail = "\u{444}\u{430}\u{439}\u{43b} \u{2116} 1.txt";
        let smile = "\u{1f600} \u{65e5}\u{672c}.txt";
        let p = |s: &str| format!("/{katalog}/{s}");
        assert_eq!(
            tree(&v, &root),
            [
                dir_row("/docs", min(20)),
                row(
                    "/docs/.hidden",
                    18,
                    18,
                    min(9),
                    Attrs::HIDDEN | Attrs::DOTFILE
                ),
                dir_row("/docs/a", min(21)),
                row("/docs/a/deep.txt", 171, 53, min(7), 0),
                row("/docs/ro.txt", 34, 34, min(8), Attrs::READONLY),
                row("/empty.txt", 0, 0, min(2), 0),
                row("/random.bin", 3000, 3000, min(3), 0),
                row("/readme.txt", 870, 92, min(1), 0),
                dir_row("/void", min(22)),
                row(&format!("/{privet}"), 81, 56, min(4), 0),
                dir_row(&format!("/{katalog}"), min(23)),
                row(&p(fail), 145, 62, min(5), 0),
                row(&p(smile), 182, 63, min(6), 0),
            ]
        );
        let (pv, pf, ps) = (format!("/{privet}"), p(fail), p(smile));
        let data = [
            ("/readme.txt", text("readme.txt", 40)),
            ("/empty.txt", Vec::new()),
            ("/random.bin", lcg(1, 3000)),
            (pv.as_str(), text(privet, 3)),
            (pf.as_str(), text(fail, 5)),
            (ps.as_str(), text(smile, 7)),
            ("/docs/a/deep.txt", text("deep.txt", 9)),
            ("/docs/ro.txt", text("ro.txt", 2)),
            ("/docs/.hidden", text(".hidden", 1)),
        ];
        for (name, want) in &data {
            assert!(read(&v, &at(&root, name)).unwrap() == *want, "{name}");
        }
        assert_eq!(list(&v, &at(&root, "/void")).unwrap().len(), 0);
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert!(!idx.solid && !idx.encrypted && idx.cipher.is_none());
        let s = idx.summary();
        assert_eq!((s.files, s.unpacked, s.packed), (9, 4501, 3378));
        // Пустой файл — без распаковщика первым, остальные — в порядке записей.
        assert_eq!(plan_all(&v, &root, &data), [1, 8, 6, 7, 2, 0, 4, 5, 3]);
    }

    /// RAR5 solid (`-m5 -s`) из 50 файлов в двух каталогах: листинг, чтение файла из
    /// середины, выборка всех в обратном порядке — один поток по порядку записей (что это
    /// один проход распаковщика — `plan_solid_is_one_pass` в `formats/rar.rs`).
    #[test]
    fn solid_of_fifty_files() {
        /// «Сжато» по записям, `UnRAR lt`.
        const PACKED: [u64; 50] = [
            84, 18, 19, 19, 26, 26, 26, 27, 27, 28, 30, 32, 32, 36, 38, 78, 39, 40, 41, 41, 42, 47,
            44, 46, 49, 51, 51, 53, 54, 57, 98, 58, 58, 60, 61, 63, 63, 63, 63, 64, 68, 68, 69, 70,
            70, 106, 70, 71, 73, 75,
        ];
        let v = vfs();
        let root = root_of("rar", fixture("r5-solid.rar"));
        let name = |n: usize| format!("f{n:02}.txt");
        let path = |n: usize| format!("/{}/{}", if n < 25 { "a" } else { "b" }, name(n));
        let mut want = vec![dir_row("/a", min(60)), dir_row("/b", min(61))];
        for (n, &packed) in PACKED.iter().enumerate() {
            let size = text(&name(n), 10 + n).len() as u64;
            want.push(row(
                &path(n),
                size,
                packed,
                min(i64::try_from(n).unwrap()),
                0,
            ));
        }
        want.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(tree(&v, &root), want);
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert!(idx.solid);
        assert_eq!(idx.summary().files, 50);
        assert_eq!(
            read(&v, &at(&root, &path(37))).unwrap(),
            text(&name(37), 47)
        );
        let paths: Vec<String> = (0..50).rev().map(path).collect();
        let data: Vec<(&str, Vec<u8>)> = (0..50)
            .rev()
            .zip(&paths)
            .map(|(n, p)| (p.as_str(), text(&name(n), 10 + n)))
            .collect();
        let order: Vec<u32> = (0..50).rev().collect();
        assert_eq!(plan_all(&v, &root, &data), order);
    }

    /// Многотомный RAR5 (`-v16k`, 4 тома): `2-big.bin` идёт через все тома, «Сжато» — сумма
    /// частей; чтение и выборка проходят все тома.
    #[test]
    fn volumes_read_through_all_parts() {
        let v = vfs();
        let root = root_of("rar", fixture("r5-vol.part1.rar"));
        assert_eq!(
            tree(&v, &root),
            [
                row("/1-before.txt", 470, 77, min(1), 0),
                row(
                    "/2-big.bin",
                    52_000,
                    16_102 + 16_224 + 16_224 + 3_602,
                    min(2),
                    0
                ),
                row("/3-after.txt", 680, 83, min(3), 0),
            ]
        );
        let data = [
            ("/1-before.txt", text("1-before.txt", 20)),
            ("/2-big.bin", lcg(7, 52_000)),
            ("/3-after.txt", text("3-after.txt", 30)),
        ];
        for (name, want) in &data {
            assert!(read(&v, &at(&root, name)).unwrap() == *want, "{name}");
        }
        assert_eq!(plan_all(&v, &root, &data), [0, 1, 2]);
    }

    /// Нет среднего или последнего тома: листинг кончается на пропуске, файлы до него
    /// читаются, файл через пропуск — ошибка с именем тома.
    #[test]
    fn volumes_with_a_missing_part() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rar");
        for (missing, big_packed) in [(2, 16_102), (4, 16_102 + 16_224 + 16_224)] {
            let tmp = tempfile::tempdir().unwrap();
            for n in (1..=4).filter(|&n| n != missing) {
                let f = format!("r5-vol.part{n}.rar");
                std::fs::copy(src.join(&f), tmp.path().join(&f)).unwrap();
            }
            let v = vfs();
            let root = root_of("rar", Location::local(tmp.path().join("r5-vol.part1.rar")));
            assert_eq!(
                tree(&v, &root),
                [
                    row("/1-before.txt", 470, 77, min(1), 0),
                    row("/2-big.bin", 52_000, big_packed, min(2), 0),
                ],
                "нет тома {missing}"
            );
            assert_eq!(
                read(&v, &at(&root, "/1-before.txt")).unwrap(),
                text("1-before.txt", 20)
            );
            let e = read(&v, &at(&root, "/2-big.bin")).unwrap_err();
            let vol = format!("r5-vol.part{missing}.rar");
            assert!(e.to_string().contains(&vol), "нет тома {missing}: {e}");
        }
    }

    /// RAR5, данные зашифрованы (`-p`, пароль кириллицей — UTF-8 → UTF-16 для `UnRAR`):
    /// имена видны, пустой файл читается без пароля, данные — только с верным.
    #[test]
    fn rar5_data_password_in_cyrillic() {
        let v = vfs();
        let root = root_of("rar", fixture("r5-pw.rar"));
        let enc = Attrs::ENCRYPTED;
        assert_eq!(
            tree(&v, &root),
            [
                dir_row("/open", min(10)),
                row("/open/empty.txt", 0, 16, min(2), enc),
                row("/secret.txt", 1090, 112, min(1), enc),
            ]
        );
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert_eq!(idx.cipher, Some("AES-256"));
        assert_eq!(read(&v, &at(&root, "/open/empty.txt")).unwrap(), b"");
        let f = at(&root, "/secret.txt");
        assert!(denied(&read(&v, &f).unwrap_err()), "без пароля");
        idx.ctx().set_password(Some("parol"));
        let r = read(&v, &f);
        assert!(r.as_ref().is_err_and(denied), "неверный пароль: {r:?}");
        idx.ctx()
            .set_password(Some("\u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}"));
        assert_eq!(read(&v, &f).unwrap(), text("secret.txt", 50));
    }

    /// RAR5, заголовки зашифрованы (`-hp`) и solid: пароль до листинга, затем дерево,
    /// чтение и выборка одним потоком.
    #[test]
    fn rar5_encrypted_headers_solid() {
        let v = vfs();
        let outer = fixture("r5-hp.rar");
        let root = root_of("rar", outer.clone());
        assert!(denied(&list(&v, &root).unwrap_err()), "без пароля");
        v.cache().set_password(&outer, Some("wrong"));
        assert!(denied(&list(&v, &root).unwrap_err()), "неверный пароль");
        v.cache().set_password(&outer, Some("secret"));
        let enc = Attrs::ENCRYPTED;
        assert_eq!(
            tree(&v, &root),
            [
                row("/three.bin", 2000, 2016, min(3), enc),
                dir_row("/x", min(10)),
                row("/x/one.txt", 560, 176, min(1), enc),
                row("/x/two.txt", 560, 80, min(2), enc),
            ]
        );
        let idx = v.index(&root, &CancelToken::new()).unwrap();
        assert!(idx.solid && idx.encrypted);
        assert_eq!(idx.cipher, Some("AES-256"));
        let data = [
            ("/x/two.txt", text("two.txt", 30)),
            ("/three.bin", lcg(3, 2000)),
            ("/x/one.txt", text("one.txt", 30)),
        ];
        assert_eq!(read(&v, &at(&root, "/three.bin")).unwrap(), data[1].1);
        assert_eq!(plan_all(&v, &root, &data), [2, 0, 1]);
    }
}
