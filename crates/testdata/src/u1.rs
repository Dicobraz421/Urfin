//! U1 (S9 §1, S9-03): юникод и пограничные имена, ссылки, атрибуты для ФС Windows
//! (NTFS, `ReFS`, exFAT, FAT32).
//!
//! Источник правды — эталон `fixtures/u1.expect.json` ([`u1_expect`]): по нему
//! [`gen_u1`] создаёт записи, а [`assert_listing_matches`] сверяет листинг — что ФС
//! показала и что пропустила (`skipped`). Чего ФС или ОС не дали создать (symlink без
//! режима разработчика, ADS на exFAT) — в [`U1Report::skipped`], без паники. Пункты
//! Linux (не-UTF-8, `\n` в имени) отменены (№57).

use std::ffi::OsString;
use std::io::{self, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::plan::json::Json;
use crate::plan::verbatim;

const FIXTURE: &str = include_str!("../fixtures/u1.expect.json");

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// Файловая система тома.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Fs {
    Ntfs,
    Refs,
    Exfat,
    Fat32,
    /// Прочие: в эталоне нет столбца — всё как `any`.
    Other,
}

impl Fs {
    /// Столбец эталона: `ntfs`, `refs`, `exfat`, `fat32`.
    pub const fn key(self) -> &'static str {
        match self {
            Fs::Ntfs => "ntfs",
            Fs::Refs => "refs",
            Fs::Exfat => "exfat",
            Fs::Fat32 => "fat32",
            Fs::Other => "other",
        }
    }

    /// Из `DriveFormat` (`NTFS`, `ReFS`, `exFAT`, `FAT32`).
    pub fn from_format(s: &str) -> Fs {
        match s.to_ascii_lowercase().as_str() {
            "ntfs" => Fs::Ntfs,
            "refs" => Fs::Refs,
            "exfat" => Fs::Exfat,
            "fat32" => Fs::Fat32,
            _ => Fs::Other,
        }
    }

    /// ФС тома, на котором лежит `path`.
    pub fn detect(path: &Path) -> Fs {
        crate::sys::volume(path).map_or(Fs::Other, |v| Fs::from_format(&v.format))
    }
}

/// Ожидание по ФС.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Создаётся и виден в листинге.
    Ok,
    /// ФС не даёт — в `skipped`.
    Skip,
    /// Либо так, либо так (режим разработчика, версия ФС).
    Any,
}

/// Вид записи эталона.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpectKind {
    File,
    Dir,
    /// ADS `файл:поток`: в листинге не виден.
    Stream,
    /// Жёсткая ссылка на `target`: в листинге — файл.
    Hardlink,
    Symlink {
        dir: bool,
        abs: bool,
    },
    Junction,
}

/// Запись эталона.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectEntry {
    pub id: String,
    /// Имя в UTF-16 (может быть с непарным суррогатом).
    pub name: Vec<u16>,
    pub kind: ExpectKind,
    pub size: u64,
    /// Цель ссылки — путь от корня U1 через `/`.
    pub target: Option<String>,
    pub hidden: bool,
    pub system: bool,
    pub readonly: bool,
    pub sparse: bool,
    /// `(ФС, ожидание)` по столбцам эталона.
    pub fs: Vec<(String, Status)>,
    /// Что делает copy (легенда — поле `copy` эталона).
    pub copy: String,
}

impl ExpectEntry {
    pub fn status(&self, fs: Fs) -> Status {
        self.fs
            .iter()
            .find(|(k, _)| k == fs.key())
            .map_or(Status::Any, |(_, s)| *s)
    }

    /// Имя для сообщений.
    pub fn display_name(&self) -> String {
        String::from_utf16_lossy(&self.name)
    }
}

/// Эталон U1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct U1Expect {
    pub gen_ver: u32,
    pub entries: Vec<ExpectEntry>,
    /// Легенда copy: значение → описание.
    pub copy: Vec<(String, String)>,
}

impl U1Expect {
    pub fn parse(text: &str) -> Result<U1Expect, String> {
        let j = Json::parse(text)?;
        let copy = match j.get("copy") {
            Some(Json::Obj(kv)) => kv
                .iter()
                .map(|(k, v)| (k.clone(), v.as_string().unwrap_or_default()))
                .collect(),
            _ => return Err("u1: нет легенды copy".into()),
        };
        let entries = j
            .get("entries")
            .and_then(Json::as_arr)
            .ok_or("u1: нет entries")?
            .iter()
            .map(entry)
            .collect::<Result<_, _>>()?;
        Ok(U1Expect {
            gen_ver: j
                .get("gen_ver")
                .and_then(Json::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("u1: нет gen_ver")?,
            entries,
            copy,
        })
    }
}

fn entry(j: &Json) -> Result<ExpectEntry, String> {
    let s = |k: &str| j.get(k).and_then(Json::as_string);
    let flag = |k: &str| j.get(k).and_then(Json::as_bool).unwrap_or(false);
    let id = s("id").ok_or("u1: запись без id")?;
    let attrs: Vec<String> = j
        .get("attrs")
        .and_then(Json::as_arr)
        .unwrap_or_default()
        .iter()
        .filter_map(Json::as_string)
        .collect();
    let has = |a: &str| attrs.iter().any(|x| x == a);
    let kind = match s("kind").as_deref() {
        Some("file") => ExpectKind::File,
        Some("dir") => ExpectKind::Dir,
        Some("stream") => ExpectKind::Stream,
        Some("hardlink") => ExpectKind::Hardlink,
        Some("symlink") => ExpectKind::Symlink {
            dir: flag("dir"),
            abs: flag("abs"),
        },
        Some("junction") => ExpectKind::Junction,
        other => return Err(format!("u1 {id}: вид {other:?}")),
    };
    let fs = match j.get("fs") {
        Some(Json::Obj(kv)) => kv
            .iter()
            .map(|(k, v)| {
                let st = match v.as_string().as_deref() {
                    Some("ok") => Status::Ok,
                    Some("skip") => Status::Skip,
                    Some("any") => Status::Any,
                    other => return Err(format!("u1 {id}: {k} = {other:?}")),
                };
                Ok((k.clone(), st))
            })
            .collect::<Result<_, String>>()?,
        _ => return Err(format!("u1 {id}: нет fs")),
    };
    Ok(ExpectEntry {
        name: j
            .get("name")
            .and_then(Json::as_wide)
            .ok_or_else(|| format!("u1 {id}: нет name"))?
            .to_vec(),
        kind,
        size: j.get("size").and_then(Json::as_u64).unwrap_or(0),
        target: s("target"),
        hidden: has("hidden"),
        system: has("system"),
        readonly: has("readonly"),
        sparse: has("sparse"),
        fs,
        copy: s("copy").unwrap_or_default(),
        id,
    })
}

/// Эталон из `fixtures/u1.expect.json` (вшит в крейт).
pub fn u1_expect() -> U1Expect {
    U1Expect::parse(FIXTURE).expect("fixtures/u1.expect.json")
}

/// Что создано.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct U1Report {
    /// Абсолютный корень U1 (без `\\?\`).
    pub root: PathBuf,
    pub fs: Fs,
    /// `id` созданных записей.
    pub created: Vec<String>,
    /// `(id, причина)` того, что ФС или ОС не дали создать.
    pub skipped: Vec<(String, String)>,
}

/// Создать U1 в пустом (или новом) каталоге `root` по эталону [`u1_expect`].
pub fn gen_u1(root: &Path) -> io::Result<U1Report> {
    let abs = std::path::absolute(root)?;
    let vroot = verbatim(&abs)?;
    std::fs::create_dir_all(&vroot)?;
    if std::fs::read_dir(&vroot)?.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{}: U1 создаётся только в пустом каталоге", abs.display()),
        ));
    }
    let mut report = U1Report {
        fs: Fs::detect(&abs),
        root: abs,
        created: Vec::new(),
        skipped: Vec::new(),
    };
    for e in u1_expect().entries {
        let path = vroot.join(OsString::from_wide(&e.name));
        match create(&report.root, &vroot, &path, &e) {
            Ok(()) => report.created.push(e.id),
            Err(err) => {
                let why = if err.kind() == io::ErrorKind::AlreadyExists {
                    format!("имя занято (ФС не различает регистр?): {err}")
                } else {
                    err.to_string()
                };
                report.skipped.push((e.id, why));
            }
        }
    }
    Ok(report)
}

fn create(root: &Path, vroot: &Path, path: &Path, e: &ExpectEntry) -> io::Result<()> {
    let target = |t: &Option<String>| -> io::Result<String> {
        t.clone()
            .ok_or_else(|| io::Error::other(format!("{}: нет target", e.id)))
    };
    match &e.kind {
        ExpectKind::File | ExpectKind::Stream => create_file(path, e),
        ExpectKind::Dir => std::fs::create_dir(path),
        ExpectKind::Hardlink => std::fs::hard_link(crate::join(vroot, &target(&e.target)?), path),
        ExpectKind::Symlink { dir, abs } => {
            let t = target(&e.target)?;
            let to = if *abs {
                crate::join(root, &t)
            } else {
                PathBuf::from(t.replace('/', "\\"))
            };
            if *dir {
                std::os::windows::fs::symlink_dir(to, path)
            } else {
                std::os::windows::fs::symlink_file(to, path)
            }
        }
        ExpectKind::Junction => {
            let link = root.join(OsString::from_wide(&e.name));
            junction(&link, &crate::join(root, &target(&e.target)?))
        }
    }
}

fn create_file(path: &Path, e: &ExpectEntry) -> io::Result<()> {
    let attrs = if e.hidden { FILE_ATTRIBUTE_HIDDEN } else { 0 }
        | if e.system { FILE_ATTRIBUTE_SYSTEM } else { 0 };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(attrs)
        .open(path)?;
    let filled = fill(&mut f, path, e);
    drop(f);
    if filled.is_err() && e.kind == ExpectKind::File {
        // Создали сами, а дописать не вышло (sparse на exFAT): не оставлять полуфайл.
        let _ = std::fs::remove_file(path);
    }
    filled
}

fn fill(f: &mut std::fs::File, path: &Path, e: &ExpectEntry) -> io::Result<()> {
    let tag = format!("U1 {}\n", e.id);
    if e.sparse {
        crate::sys::set_sparse(path)?;
        f.set_len(e.size)?;
        f.write_all(tag.as_bytes())?;
    } else {
        let body: Vec<u8> = tag
            .bytes()
            .cycle()
            .take(usize::try_from(e.size).unwrap_or(0))
            .collect();
        f.write_all(&body)?;
    }
    if e.readonly {
        let mut p = std::fs::metadata(path)?.permissions();
        p.set_readonly(true);
        std::fs::set_permissions(path, p)?;
    }
    Ok(())
}

/// Junction без `unsafe`: `mklink /J` (прав администратора не нужно).
fn junction(link: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    let cmd = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let out = std::process::Command::new(cmd)
        .raw_arg(format!(
            "/d /c mklink /J \"{}\" \"{}\"",
            link.display(),
            target.display()
        ))
        .output()?;
    if out.status.success() && std::fs::symlink_metadata(link).is_ok() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "mklink /J: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Вид записи в листинге.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ListedKind {
    File,
    Dir,
    Symlink,
    Junction,
}

/// Запись листинга — то, что показал бы файловый менеджер.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Имя в UTF-16.
    pub name: Vec<u16>,
    pub kind: ListedKind,
    /// Размер файла (у каталогов и ссылок — 0).
    pub size: u64,
    pub hidden: bool,
    pub system: bool,
    pub readonly: bool,
    pub sparse: bool,
    /// Цель ссылки, как её вернула ОС.
    pub target: Option<String>,
}

/// Листинг корня U1 на ФС `fs` (тесты vfs и ops собирают его из своих записей).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub fs: Fs,
    /// Абсолютный корень U1 (для целей `abs`-ссылок и junction).
    pub root: PathBuf,
    /// `id`, которые генератор пропустил ([`U1Report::skipped`]).
    pub skipped: Vec<String>,
    pub entries: Vec<Listed>,
}

impl Listing {
    /// Листинг средствами std (+ `fsutil` для тега reparse-точки).
    pub fn read(report: &U1Report) -> io::Result<Listing> {
        let vroot = verbatim(&report.root)?;
        let mut entries = Vec::new();
        for de in std::fs::read_dir(&vroot)? {
            let de = de?;
            let m = std::fs::symlink_metadata(de.path())?;
            let a = m.file_attributes();
            let t = m.file_type();
            let kind = if t.is_symlink() {
                match crate::sys::reparse_tag(&de.path()) {
                    Ok(IO_REPARSE_TAG_MOUNT_POINT) => ListedKind::Junction,
                    _ => ListedKind::Symlink,
                }
            } else if t.is_dir() {
                ListedKind::Dir
            } else {
                ListedKind::File
            };
            entries.push(Listed {
                name: de.file_name().encode_wide().collect(),
                size: if kind == ListedKind::File { m.len() } else { 0 },
                hidden: a & FILE_ATTRIBUTE_HIDDEN != 0,
                system: a & FILE_ATTRIBUTE_SYSTEM != 0,
                readonly: a & FILE_ATTRIBUTE_READONLY != 0,
                sparse: a & FILE_ATTRIBUTE_SPARSE_FILE != 0,
                target: matches!(kind, ListedKind::Symlink | ListedKind::Junction)
                    .then(|| std::fs::read_link(de.path()).ok())
                    .flatten()
                    .map(|p| p.to_string_lossy().into_owned()),
                kind,
            });
        }
        Ok(Listing {
            fs: report.fs,
            root: report.root.clone(),
            skipped: report.skipped.iter().map(|(id, _)| id.clone()).collect(),
            entries,
        })
    }
}

/// Расхождения листинга с эталоном (пусто — совпал).
pub fn listing_mismatches(expect: &U1Expect, fs: &Listing) -> Vec<String> {
    let mut out = Vec::new();
    for e in &expect.entries {
        let st = e.status(fs.fs);
        let skipped = fs.skipped.contains(&e.id);
        if e.kind == ExpectKind::Stream {
            match st {
                Status::Ok if skipped => out.push(format!("{}: поток не создан", e.id)),
                Status::Skip if !skipped => out.push(format!("{}: нет в skipped", e.id)),
                _ => {}
            }
            continue;
        }
        let listed = fs.entries.iter().find(|l| l.name == e.name);
        match (st, listed) {
            (_, Some(_)) if skipped => out.push(format!("{}: в skipped, но в листинге", e.id)),
            (Status::Skip, Some(_)) => {
                out.push(format!(
                    "{}: ФС ({:?}) не должна была дать создать",
                    e.id, fs.fs
                ));
            }
            (Status::Ok, None) => out.push(format!(
                "{}: нет в листинге «{}»{}",
                e.id,
                e.display_name(),
                if skipped { " (в skipped)" } else { "" }
            )),
            (Status::Skip | Status::Any, None) if !skipped => {
                out.push(format!("{}: нет ни в листинге, ни в skipped", e.id));
            }
            (_, Some(l)) => check(e, l, &fs.root, &mut out),
            _ => {}
        }
    }
    for l in &fs.entries {
        if !expect.entries.iter().any(|e| e.name == l.name) {
            out.push(format!(
                "лишняя запись «{}»",
                String::from_utf16_lossy(&l.name)
            ));
        }
    }
    out
}

fn check(e: &ExpectEntry, l: &Listed, root: &Path, out: &mut Vec<String>) {
    let kind = match e.kind {
        ExpectKind::File | ExpectKind::Hardlink | ExpectKind::Stream => ListedKind::File,
        ExpectKind::Dir => ListedKind::Dir,
        ExpectKind::Symlink { .. } => ListedKind::Symlink,
        ExpectKind::Junction => ListedKind::Junction,
    };
    if l.kind != kind {
        out.push(format!("{}: вид {:?}, ожидался {kind:?}", e.id, l.kind));
        return;
    }
    if kind == ListedKind::File && l.size != e.size {
        out.push(format!("{}: размер {}, ожидался {}", e.id, l.size, e.size));
    }
    let attrs = |h, s, r, p| format!("hidden={h} system={s} readonly={r} sparse={p}");
    let got = attrs(l.hidden, l.system, l.readonly, l.sparse);
    let want = attrs(e.hidden, e.system, e.readonly, e.sparse);
    if got != want {
        out.push(format!("{}: {got}, ожидалось {want}", e.id));
    }
    let want_target = match (&e.kind, &e.target) {
        (ExpectKind::Symlink { abs: false, .. }, Some(t)) => {
            Some(PathBuf::from(t.replace('/', "\\")))
        }
        (ExpectKind::Symlink { abs: true, .. } | ExpectKind::Junction, Some(t)) => {
            Some(crate::join(root, t))
        }
        _ => None,
    };
    if let Some(want) = want_target {
        let got = l.target.as_deref().map(strip_verbatim);
        if !got.is_some_and(|g| same_path(Path::new(g), &want)) {
            out.push(format!(
                "{}: цель {:?}, ожидалась {}",
                e.id,
                l.target,
                want.display()
            ));
        }
    }
}

fn strip_verbatim(s: &str) -> &str {
    s.strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix(r"\??\"))
        .unwrap_or(s)
}

/// Пути равны: без учёта регистра или после разрешения (короткие имена 8.3).
fn same_path(a: &Path, b: &Path) -> bool {
    if a.as_os_str().eq_ignore_ascii_case(b.as_os_str()) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Сверить листинг с эталоном: паника со списком расхождений.
pub fn assert_listing_matches(expect: &U1Expect, fs: &Listing) {
    let m = listing_mismatches(expect, fs);
    assert!(
        m.is_empty(),
        "U1 на {:?} не совпал с эталоном:\n{}",
        fs.fs,
        m.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("urfin-u1-")
            .tempdir()
            .unwrap()
    }

    /// Снять read-only, чтобы временный каталог удалился.
    fn unlock(report: &U1Report) {
        let p = verbatim(&report.root).unwrap().join("readonly.txt");
        if let Ok(m) = std::fs::metadata(&p) {
            let mut perm = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&p, perm);
        }
    }

    /// Эталон читается: id и имена уникальны, у каждой записи — четыре ФС, copy из легенды.
    #[test]
    fn fixture_parses() {
        let x = u1_expect();
        assert_eq!(x.gen_ver, 1);
        assert!(x.entries.len() >= 30);
        for (i, e) in x.entries.iter().enumerate() {
            assert!(
                x.entries[..i]
                    .iter()
                    .all(|o| o.id != e.id && o.name != e.name),
                "{}",
                e.id
            );
            for fs in [Fs::Ntfs, Fs::Refs, Fs::Exfat, Fs::Fat32] {
                assert!(e.fs.iter().any(|(k, _)| k == fs.key()), "{} {fs:?}", e.id);
            }
            assert!(
                x.copy.iter().any(|(k, _)| *k == e.copy),
                "{}: {}",
                e.id,
                e.copy
            );
        }
        let lone = x.entries.iter().find(|e| e.id == "lone_surrogate").unwrap();
        assert!(String::from_utf16(&lone.name).is_err());
    }

    /// Генерация на томе временного каталога (NTFS): без паник, пропуски — только где
    /// эталон их допускает; листинг сверяется хелпером.
    #[test]
    fn generate_and_match() {
        let dir = tmp();
        let root = dir.path().join("U1");
        let report = gen_u1(&root).unwrap();
        let expect = u1_expect();
        for (id, why) in &report.skipped {
            let e = expect.entries.iter().find(|e| e.id == *id).unwrap();
            assert_ne!(e.status(report.fs), Status::Ok, "{id}: {why}");
            eprintln!("skipped {id}: {why}");
        }
        assert_eq!(
            report.created.len() + report.skipped.len(),
            expect.entries.len()
        );
        let listing = Listing::read(&report).unwrap();
        assert_listing_matches(&expect, &listing);

        // Хелпер ловит расхождения: пропавшая запись, лишняя, чужой размер.
        let mut bad = listing.clone();
        let i = bad
            .entries
            .iter()
            .position(|l| l.name == "café.txt".encode_utf16().collect::<Vec<_>>())
            .unwrap();
        bad.entries.remove(i);
        bad.entries.push(Listed {
            name: "extra.txt".encode_utf16().collect(),
            kind: ListedKind::File,
            size: 0,
            hidden: false,
            system: false,
            readonly: false,
            sparse: false,
            target: None,
        });
        if let Some(l) = bad.entries.iter_mut().find(|l| l.kind == ListedKind::File) {
            l.size += 1;
        }
        let m = listing_mismatches(&expect, &bad);
        assert!(
            m.iter().any(|s| s.starts_with("nfc: нет в листинге")),
            "{m:?}"
        );
        assert!(m.iter().any(|s| s.contains("extra.txt")), "{m:?}");
        assert!(m.iter().any(|s| s.contains(": размер ")), "{m:?}");

        // В непустой каталог — ошибка.
        assert!(gen_u1(&root).is_err());
        unlock(&report);
    }
}
