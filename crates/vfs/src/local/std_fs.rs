//! Листинг и метаданные через `std::fs` (`read_dir` + `symlink_metadata`):
//! последний рубеж после fast path ОС (S4-07…09) и эталон в тестах.
//!
//! Без лишних обращений к ФС: на Windows метаданные записи приходят вместе с
//! `FindNextFileW`, тип ссылки — по атрибуту каталога (цель не открывается: облачные
//! файлы `OneDrive` не скачиваются). На unix тип ссылки — `stat` цели при
//! `resolve_links`.

use std::fs::{self, DirEntry, File, ReadDir};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use compact_str::CompactString;

use crate::api::{
    Attrs, CancelToken, Entry, EntryKind, ListOpts, ListStream, Metadata, VfsError, VfsRead,
};

/// Записей за вызов после первого батча.
const BATCH: usize = 1024;

pub(crate) struct StdList {
    dir: PathBuf,
    rd: ReadDir,
    opts: ListOpts,
    first: bool,
}

pub(crate) fn list(dir: &Path, opts: &ListOpts) -> Result<StdList, VfsError> {
    let rd = fs::read_dir(dir).map_err(|e| VfsError::from_io(dir, e))?;
    Ok(StdList {
        dir: dir.to_path_buf(),
        rd,
        opts: opts.clone(),
        first: true,
    })
}

impl ListStream for StdList {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let n = if std::mem::take(&mut self.first) {
            (self.opts.first_batch_hint as usize).max(1)
        } else {
            BATCH
        };
        for _ in 0..n {
            let Some(item) = self.rd.next() else {
                return Ok(false);
            };
            let de = item.map_err(|e| VfsError::from_io(&self.dir, e))?;
            match entry_of(&de, &self.opts) {
                Ok(e) => out.push(e),
                // Запись исчезла между перечислением и метаданными — пропустить.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(VfsError::from_io(de.path(), e)),
            }
        }
        Ok(true)
    }
}

fn entry_of(de: &DirEntry, opts: &ListOpts) -> io::Result<Entry> {
    let md = de.metadata()?;
    let (name, escaped) = crate::name::from_os(&de.file_name());
    let kind = kind_of(&md, || fs::metadata(de.path()), opts.resolve_links);
    let mut e = entry(name, &md, kind);
    if escaped {
        e.attrs = Attrs(e.attrs.0 | Attrs::ESCAPED_NAME);
    }
    Ok(e)
}

/// Тип записи по `symlink_metadata`; `follow` — метаданные цели ссылки.
fn kind_of(
    md: &fs::Metadata,
    follow: impl FnOnce() -> io::Result<fs::Metadata>,
    resolve_links: bool,
) -> EntryKind {
    let ft = md.file_type();
    if ft.is_symlink() {
        link_kind(md, follow, resolve_links)
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

#[cfg(windows)]
fn link_kind(
    md: &fs::Metadata,
    _follow: impl FnOnce() -> io::Result<fs::Metadata>,
    _resolve_links: bool,
) -> EntryKind {
    use std::os::windows::fs::MetadataExt;
    // Symlink и junction: каталог или файл — по атрибуту самой ссылки. Цель не
    // открывается (сеть, облако); `LinkBroken` выдаёт только `stat` (S4 §3.1).
    if md.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::LinkDir
    } else {
        EntryKind::LinkFile
    }
}

#[cfg(not(windows))]
fn link_kind(
    _md: &fs::Metadata,
    follow: impl FnOnce() -> io::Result<fs::Metadata>,
    resolve_links: bool,
) -> EntryKind {
    if !resolve_links {
        return EntryKind::LinkFile;
    }
    match follow() {
        Ok(t) if t.is_dir() => EntryKind::LinkDir,
        Ok(_) => EntryKind::LinkFile,
        Err(_) => EntryKind::LinkBroken,
    }
}

fn entry(name: CompactString, md: &fs::Metadata, kind: EntryKind) -> Entry {
    let attrs = attrs_of(&name, md, kind);
    Entry {
        size: if kind == EntryKind::File || kind == EntryKind::LinkFile {
            md.len()
        } else {
            0
        },
        mtime: md.modified().map_or(i64::MIN, nanos),
        ctime: md.created().map_or(i64::MIN, nanos),
        atime: md.accessed().map_or(i64::MIN, nanos),
        name,
        attrs,
        kind,
    }
}

/// Наносекунды от Unix epoch (до 1970 — отрицательные; вне i64 — `i64::MIN`).
pub(crate) fn nanos(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MIN),
        Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}

#[cfg(windows)]
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

#[cfg(windows)]
fn attrs_of(name: &str, md: &fs::Metadata, _kind: EntryKind) -> Attrs {
    use std::os::windows::fs::MetadataExt;
    windows_attrs(name, md.file_attributes())
}

/// Атрибуты записи из `FILE_ATTRIBUTE_*` — общее правило std-пути и fast path.
#[cfg(windows)]
pub(crate) fn windows_attrs(name: &str, fa: u32) -> Attrs {
    // FILE_ATTRIBUTE_*; OFFLINE — ещё и «скачивается при открытии» (облако).
    const MAP: [(u32, u32); 7] = [
        (0x2, Attrs::HIDDEN),
        (0x4, Attrs::SYSTEM),
        (0x1, Attrs::READONLY),
        (0x20, Attrs::ARCHIVE),
        (0x400, Attrs::REPARSE),
        (0x800, Attrs::COMPRESSED),
        (0x4000, Attrs::ENCRYPTED),
    ];
    const OFFLINE: u32 = 0x1000 | 0x4_0000 | 0x40_0000;
    let mut a = MAP
        .iter()
        .filter(|(w, _)| fa & w != 0)
        .fold(0, |a, (_, f)| a | f);
    if fa & OFFLINE != 0 {
        a |= Attrs::OFFLINE;
    }
    if crate::name::is_dotfile(name) {
        a |= Attrs::DOTFILE;
    }
    Attrs(a)
}

#[cfg(not(windows))]
fn attrs_of(name: &str, md: &fs::Metadata, kind: EntryKind) -> Attrs {
    use std::os::unix::fs::PermissionsExt;
    let mut a = 0;
    if crate::name::is_dotfile(name) {
        a |= Attrs::DOTFILE;
    }
    if kind == EntryKind::File && md.permissions().mode() & 0o111 != 0 {
        a |= Attrs::EXEC;
    }
    if md.permissions().readonly() {
        a |= Attrs::READONLY;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        const UF_HIDDEN: u32 = 0x8000;
        if md.st_flags() & UF_HIDDEN != 0 {
            a |= Attrs::HIDDEN;
        }
    }
    Attrs(a)
}

pub(crate) fn stat(p: &Path, follow: bool) -> Result<Metadata, VfsError> {
    let md = if follow {
        fs::metadata(p)
    } else {
        fs::symlink_metadata(p)
    }
    .map_err(|e| VfsError::from_io(p, e))?;
    let (name, _) = p
        .file_name()
        .map_or_else(|| (CompactString::default(), false), crate::name::from_os);
    let kind = kind_of(&md, || fs::metadata(p), true);
    // Битая ссылка — только здесь, не в листинге (S4 §3.1): цель может быть медленной.
    let kind = if !follow
        && matches!(kind, EntryKind::LinkDir | EntryKind::LinkFile)
        && fs::metadata(p).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
    {
        EntryKind::LinkBroken
    } else {
        kind
    };
    let e = entry(name, &md, kind);
    let link_target = (!follow && md.file_type().is_symlink())
        .then(|| fs::read_link(p).ok())
        .flatten();
    let extra = platform_meta(&md, p, follow);
    Ok(Metadata {
        kind,
        attrs: e.attrs,
        size: e.size,
        alloc_size: extra.alloc_size,
        mtime: e.mtime,
        atime: md.accessed().map_or(i64::MIN, nanos),
        btime: md.created().ok().map(nanos),
        link_target,
        ..extra
    })
}

/// Windows: по хэндлу — время изменения метаданных, размер на диске, жёсткие ссылки,
/// тег reparse, `FileId` (S4 §3.1).
#[cfg(windows)]
fn platform_meta(_md: &fs::Metadata, p: &Path, follow: bool) -> Metadata {
    match crate::sys::win_meta::stat_extra(p, follow) {
        Some(x) => Metadata {
            ctime: (x.change != i64::MIN).then_some(x.change),
            alloc_size: Some(x.alloc),
            nlink: Some(x.nlink),
            reparse_tag: x.reparse_tag,
            file_id: Some((x.file_id.0, x.file_id.1)),
            ..empty_meta()
        },
        None => empty_meta(),
    }
}

#[cfg(not(windows))]
fn platform_meta(md: &fs::Metadata, _p: &Path, _follow: bool) -> Metadata {
    use std::os::unix::fs::MetadataExt;
    Metadata {
        ctime: Some(md.ctime() * 1_000_000_000 + md.ctime_nsec()),
        mode: Some(md.mode()),
        uid: Some(md.uid()),
        gid: Some(md.gid()),
        nlink: u32::try_from(md.nlink()).ok(),
        file_id: Some((md.dev(), u128::from(md.ino()))),
        ..empty_meta()
    }
}

fn empty_meta() -> Metadata {
    Metadata {
        kind: EntryKind::Other,
        attrs: Attrs::default(),
        size: 0,
        alloc_size: None,
        mtime: i64::MIN,
        atime: i64::MIN,
        ctime: None,
        btime: None,
        mode: None,
        uid: None,
        gid: None,
        nlink: None,
        file_id: None,
        reparse_tag: None,
        link_target: None,
    }
}

/// Одна запись каталога по имени (патчи watcher'а); `escaped` — имя
/// экранировано (`Attrs::ESCAPED_NAME`).
pub(crate) fn stat_entry(dir: &Path, name: &str, escaped: bool) -> Result<Entry, VfsError> {
    let p = dir.join(crate::name::to_os(name, escaped));
    let md = fs::symlink_metadata(&p).map_err(|e| VfsError::from_io(&p, e))?;
    let kind = kind_of(&md, || fs::metadata(&p), true);
    let mut e = entry(name.into(), &md, kind);
    if escaped {
        e.attrs = Attrs(e.attrs.0 | Attrs::ESCAPED_NAME);
    }
    Ok(e)
}

pub(crate) struct StdRead {
    file: File,
    len: Option<u64>,
}

pub(crate) fn open_read(p: &Path) -> Result<StdRead, VfsError> {
    let file = File::open(p).map_err(|e| VfsError::from_io(p, e))?;
    let len = file.metadata().ok().map(|m| m.len());
    Ok(StdRead { file, len })
}

impl Read for StdRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for StdRead {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl VfsRead for StdRead {
    fn len(&self) -> Option<u64> {
        self.len
    }
}
