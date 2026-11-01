//! Кэш снимков анализа места на диске (F01 §2.2): `state\du\<serial>-<xxh3 корня>.du`,
//! LRU по времени открытия (`[du] cache_keep`, `cache_max`). Каталог — от `state_dir`
//! [`Paths`] (в portable-режиме — `<exe>\data\state\du\`, как у сессии).
//!
//! Формат v1, little-endian: заголовок (magic, `ver`, `flags`, длина заголовка, длина
//! meta, CRC32 заголовка) · meta (postcard) · `dirs` (записи по 48 Б в раскладке
//! [`DuNode`]) · арена имён · `files` (по 48 Б в раскладке [`FileRec`]) · типы и
//! `denied` (postcard) · трейлер `xxh3_64` всего, что до него. Записи пишутся и читаются
//! поле за полем — без `unsafe`, байты те же, что у `repr(C)` на x86-64.
//!
//! Всё здесь — блокирующий I/O: звать из задач Runtime (запись — `Lane::Bulk`),
//! не из UI-потока.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use urfin_core::Paths;
use xxhash_rust::xxh3::xxh3_64;

use super::model::{
    DirId, DuNode, DuSnapshot, DuType, FileRec, NameRef, NodeFlags, SizeMode, TypeAgg, UsnMark,
    VolumeKey,
};
use super::{DuCfg, parse_size};
use crate::volume::FsKind;

const MAGIC: &[u8; 6] = b"URFDU\0";
const VERSION: u16 = 1;
/// magic · ver · flags · длина заголовка · длина meta · CRC32.
const HEADER: usize = 20;
const REC: usize = 48;
const TRAILER: usize = 8;
const NO_PARENT: DirId = u32::MAX;
/// Недописанный `*.tmp` (упал процесс) старше — удаляется при уборке.
const STALE_TMP: Duration = Duration::from_secs(3600);
const CACHE_MAX: u64 = 256 << 20;

/// Ошибка кэша.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("du cache io: {0}")]
    Io(#[from] io::Error),
    /// Файл битый или чужой версии.
    #[error("du cache corrupt: {0}")]
    Corrupt(&'static str),
    #[error("du cache encode: {0}")]
    Encode(String),
    /// Снимок не помещается в `[du] cache_max` / `cache_keep = 0`: не записан.
    #[error("du snapshot of {0} bytes exceeds [du] cache limits")]
    Limit(u64),
}

/// Meta-секция (postcard).
#[derive(Serialize, Deserialize)]
struct Meta {
    root: PathBuf,
    serial: u32,
    fs: FsKind,
    scanned_at: i64,
    took_ms: u32,
    generation: u64,
    usn: Option<(u64, i64)>,
    size_mode: SizeMode,
    track_min: u64,
    flags: u8,
    dirs: u32,
    names: u32,
    files: u32,
}

type Tail = ([u64; DuType::COUNT], [u64; DuType::COUNT], Vec<DirId>);

fn count(n: usize) -> Result<u32, CacheError> {
    u32::try_from(n).map_err(|_| CacheError::Encode("too many records".into()))
}

/// Снимок в байты формата v1.
pub fn encode(s: &DuSnapshot) -> Result<Vec<u8>, CacheError> {
    let meta = Meta {
        root: s.root.clone(),
        serial: s.volume.serial,
        fs: s.volume.fs,
        scanned_at: s.scanned_at,
        took_ms: s.took_ms,
        generation: s.generation,
        usn: s.usn.map(|u| (u.journal_id, u.next_usn)),
        size_mode: s.size_mode,
        track_min: s.track_min,
        flags: s.flags,
        dirs: count(s.dirs.len())?,
        names: count(s.names.len())?,
        files: count(s.files.len())?,
    };
    let meta = postcard::to_stdvec(&meta).map_err(|e| CacheError::Encode(e.to_string()))?;
    let tail: (&[u64; DuType::COUNT], &[u64; DuType::COUNT], &Vec<DirId>) =
        (&s.types.disk, &s.types.files, &s.denied);
    let tail = postcard::to_stdvec(&tail).map_err(|e| CacheError::Encode(e.to_string()))?;

    let size = HEADER
        + meta.len()
        + (s.dirs.len() + s.files.len()) * REC
        + s.names.len()
        + tail.len()
        + TRAILER;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&VERSION.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    #[allow(clippy::cast_possible_truncation, reason = "константа 20")]
    b.extend_from_slice(&(HEADER as u16).to_le_bytes());
    b.extend_from_slice(&count(meta.len())?.to_le_bytes());
    let crc = crc32fast::hash(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    b.extend_from_slice(&meta);
    for d in &s.dirs {
        put_dir(&mut b, d);
    }
    b.extend_from_slice(&s.names);
    for f in &s.files {
        put_file(&mut b, f);
    }
    b.extend_from_slice(&tail);
    let sum = xxh3_64(&b);
    b.extend_from_slice(&sum.to_le_bytes());
    debug_assert_eq!(b.len(), size);
    Ok(b)
}

fn put_name(b: &mut Vec<u8>, n: NameRef) {
    let NameRef { off, len } = n;
    b.extend_from_slice(&off.to_le_bytes());
    b.extend_from_slice(&len.to_le_bytes());
}

/// Раскладка `DuNode`: parent 0 · name 4 · flags 10 · files 12 · `file_id` 16 · disk 24 ·
/// logical 32 · mtime 40.
fn put_dir(b: &mut Vec<u8>, d: &DuNode) {
    b.extend_from_slice(&d.parent.to_le_bytes());
    put_name(b, d.name);
    b.extend_from_slice(&d.flags.0.to_le_bytes());
    b.extend_from_slice(&d.files.to_le_bytes());
    b.extend_from_slice(&d.file_id.to_le_bytes());
    b.extend_from_slice(&d.disk.to_le_bytes());
    b.extend_from_slice(&d.logical.to_le_bytes());
    b.extend_from_slice(&d.mtime.to_le_bytes());
}

/// Раскладка `FileRec`: dir 0 · name 4 · ty 10 · attrs 11 · (0) 12 · disk 16 ·
/// logical 24 · mtime 32 · `file_id` 40.
fn put_file(b: &mut Vec<u8>, f: &FileRec) {
    b.extend_from_slice(&f.dir.to_le_bytes());
    put_name(b, f.name);
    b.push(f.ty as u8);
    b.push(f.attrs);
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&f.disk.to_le_bytes());
    b.extend_from_slice(&f.logical.to_le_bytes());
    b.extend_from_slice(&f.mtime.to_le_bytes());
    b.extend_from_slice(&f.file_id.to_le_bytes());
}

fn u16_at(r: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([r[at], r[at + 1]])
}

fn u32_at(r: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(r[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(r: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(r[at..at + 8].try_into().expect("8 bytes"))
}

fn name_at(r: &[u8], at: usize) -> NameRef {
    NameRef {
        off: u32_at(r, at),
        len: u16_at(r, at + 4),
    }
}

fn get_dir(r: &[u8; REC]) -> DuNode {
    DuNode {
        parent: u32_at(r, 0),
        name: name_at(r, 4),
        flags: NodeFlags(u16_at(r, 10)),
        files: u32_at(r, 12),
        file_id: u64_at(r, 16),
        disk: u64_at(r, 24),
        logical: u64_at(r, 32),
        mtime: u64_at(r, 40).cast_signed(),
    }
}

fn get_file(r: &[u8; REC]) -> Result<FileRec, CacheError> {
    Ok(FileRec {
        dir: u32_at(r, 0),
        name: name_at(r, 4),
        ty: *DuType::ALL
            .get(usize::from(r[10]))
            .ok_or(CacheError::Corrupt("file type"))?,
        attrs: r[11],
        disk: u64_at(r, 16),
        logical: u64_at(r, 24),
        mtime: u64_at(r, 32).cast_signed(),
        file_id: u64_at(r, 40),
    })
}

/// Секция `[at, at + len)` в пределах `end`.
fn take(at: &mut usize, len: usize, end: usize) -> Result<std::ops::Range<usize>, CacheError> {
    let start = *at;
    let stop = start
        .checked_add(len)
        .filter(|&e| e <= end)
        .ok_or(CacheError::Corrupt("truncated"))?;
    *at = stop;
    Ok(start..stop)
}

/// Байты формата v1 в снимок. Проверяет заголовок, версию, трейлер и ссылки (родитель
/// раньше ребёнка, имена в арене): дальше снимок читается без проверок.
pub fn decode(b: &[u8]) -> Result<DuSnapshot, CacheError> {
    if b.len() < HEADER + TRAILER || &b[..6] != MAGIC {
        return Err(CacheError::Corrupt("magic"));
    }
    if crc32fast::hash(&b[..16]) != u32_at(b, 16) {
        return Err(CacheError::Corrupt("header crc"));
    }
    if u16_at(b, 6) != VERSION {
        return Err(CacheError::Corrupt("version"));
    }
    let end = b.len() - TRAILER;
    if xxh3_64(&b[..end]) != u64_at(b, end) {
        return Err(CacheError::Corrupt("trailer"));
    }
    let mut at = usize::from(u16_at(b, 10));
    if at < HEADER {
        return Err(CacheError::Corrupt("header length"));
    }
    let meta = take(&mut at, u32_at(b, 12) as usize, end)?;
    let meta: Meta = postcard::from_bytes(&b[meta]).map_err(|_| CacheError::Corrupt("meta"))?;
    let (n, m) = (meta.dirs as usize, meta.files as usize);
    let dirs = take(&mut at, n.saturating_mul(REC), end)?;
    let names = take(&mut at, meta.names as usize, end)?;
    let files = take(&mut at, m.saturating_mul(REC), end)?;
    let ((tdisk, tfiles, denied), rest): (Tail, _) =
        postcard::take_from_bytes(&b[at..end]).map_err(|_| CacheError::Corrupt("tail"))?;
    if !rest.is_empty() {
        return Err(CacheError::Corrupt("tail"));
    }

    let names = b[names].to_vec();
    let name_ok = |r: NameRef| r.range().end <= names.len();
    let dirs: Vec<DuNode> = b[dirs].as_chunks::<REC>().0.iter().map(get_dir).collect();
    let tree_ok = dirs.first().is_some_and(|d| d.parent == NO_PARENT)
        && dirs
            .iter()
            .enumerate()
            .skip(1)
            .all(|(i, d)| (d.parent as usize) < i && name_ok(d.name));
    if !tree_ok {
        return Err(CacheError::Corrupt("dirs"));
    }
    let files = b[files]
        .as_chunks::<REC>()
        .0
        .iter()
        .map(|r| {
            let f = get_file(r)?;
            if (f.dir as usize) < n && name_ok(f.name) {
                Ok(f)
            } else {
                Err(CacheError::Corrupt("files"))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if denied.iter().any(|&d| d as usize >= n) {
        return Err(CacheError::Corrupt("denied"));
    }
    Ok(DuSnapshot {
        root: meta.root,
        volume: VolumeKey {
            serial: meta.serial,
            fs: meta.fs,
        },
        scanned_at: meta.scanned_at,
        took_ms: meta.took_ms,
        generation: meta.generation,
        usn: meta.usn.map(|(journal_id, next_usn)| UsnMark {
            journal_id,
            next_usn,
        }),
        dirs,
        names,
        files,
        types: TypeAgg {
            disk: tdisk,
            files: tfiles,
        },
        denied,
        size_mode: meta.size_mode,
        track_min: meta.track_min,
        flags: meta.flags,
    })
}

/// Корень для ключа: `\` вместо `/`, без хвостового разделителя, в нижнем регистре.
fn norm_root(root: &Path) -> String {
    let mut s = root.to_string_lossy().replace('/', "\\");
    while s.len() > 1 && s.ends_with('\\') {
        s.pop();
    }
    s.to_lowercase()
}

/// Имя файла снимка: `<serial:08x>-<xxh3_64(корень):016x>.du`.
pub fn file_name(serial: u32, root: &Path) -> String {
    format!(
        "{serial:08x}-{:016x}.du",
        xxh3_64(norm_root(root).as_bytes())
    )
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Каталог снимков с лимитами LRU.
#[derive(Clone, Debug)]
pub struct DuCache {
    dir: PathBuf,
    keep: usize,
    max_bytes: u64,
}

impl DuCache {
    /// `state_dir\du\` из [`Paths`] (portable — тот же резолвер, что у сессии).
    pub fn new(paths: &Paths, cfg: &DuCfg) -> DuCache {
        DuCache::in_dir(paths.state_dir.join("du"), cfg)
    }

    /// Снимки в `dir`; неразборный `cache_max` — 256 MiB.
    pub fn in_dir(dir: PathBuf, cfg: &DuCfg) -> DuCache {
        let max_bytes = parse_size(&cfg.cache_max).unwrap_or_else(|| {
            tracing::warn!(target: "vfs.du", value = %cfg.cache_max, "bad [du] cache_max");
            CACHE_MAX
        });
        DuCache {
            dir,
            keep: usize::from(cfg.cache_keep),
            max_bytes,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Путь снимка корня `root` на томе `serial`.
    pub fn path(&self, serial: u32, root: &Path) -> PathBuf {
        self.dir.join(file_name(serial, root))
    }

    /// Снимок из кэша; нет или битый — `None` (битый или чужой версии удаляется).
    /// Отмечает время открытия (mtime файла) для LRU.
    pub fn load(&self, serial: u32, root: &Path) -> Option<DuSnapshot> {
        let path = self.path(serial, root);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::debug!(target: "vfs.du", error = %e, path = %path.display(), "cache read");
                }
                return None;
            }
        };
        let snap = match decode(&bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(target: "vfs.du", error = %e, path = %path.display(), "cache dropped");
                let _ = fs::remove_file(&path);
                return None;
            }
        };
        if snap.volume.serial != serial || norm_root(&snap.root) != norm_root(root) {
            return None;
        }
        let touched = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        if let Err(e) = touched {
            tracing::debug!(target: "vfs.du", error = %e, "cache touch");
        }
        Some(snap)
    }

    /// Записать снимок (`*.tmp` + rename поверх) и убрать лишнее по LRU. Снимок,
    /// который один не помещается в лимиты, не пишется ([`CacheError::Limit`]).
    pub fn store(&self, snap: &DuSnapshot) -> Result<PathBuf, CacheError> {
        let bytes = encode(snap)?;
        let len = bytes.len() as u64;
        if self.keep == 0 || len > self.max_bytes {
            return Err(CacheError::Limit(len));
        }
        let path = self.path(snap.volume.serial, &snap.root);
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(
            "{}.{}-{}.tmp",
            file_name(snap.volume.serial, &snap.root),
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let written = fs::File::create(&tmp).and_then(|mut f| {
            f.write_all(&bytes)?;
            f.sync_all()
        });
        if let Err(e) = written.and_then(|()| fs::rename(&tmp, &path)) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        self.trim(Some(&path))?;
        Ok(path)
    }

    /// LRU: свежие по mtime (времени открытия) остаются, пока их не больше
    /// `cache_keep` и сумма не больше `cache_max`; первый не поместившийся и все
    /// старше удаляются. `newest` — только что записанный (первый при равных
    /// mtime). Заодно — брошенные `*.tmp`.
    pub fn trim(&self, newest: Option<&Path>) -> io::Result<()> {
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let now = SystemTime::now();
        let mut snaps = Vec::new();
        for e in rd.flatten() {
            let path = e.path();
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            match path.extension().and_then(|x| x.to_str()) {
                Some("du") => snaps.push((newest == Some(path.as_path()), mtime, md.len(), path)),
                Some("tmp") if now.duration_since(mtime).is_ok_and(|d| d > STALE_TMP) => {
                    let _ = fs::remove_file(&path);
                }
                _ => {}
            }
        }
        snaps.sort_unstable_by_key(|s| std::cmp::Reverse((s.0, s.1)));
        let (mut kept, mut total, mut full) = (0usize, 0u64, false);
        for (_, _, len, path) in snaps {
            full = full || kept >= self.keep || total + len > self.max_bytes;
            if full {
                tracing::debug!(target: "vfs.du", path = %path.display(), "cache evicted");
                fs::remove_file(&path)?;
            } else {
                kept += 1;
                total += len;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DuSnapshot {
        let mut names = Vec::new();
        let mut name = |s: &str| {
            let off = u32::try_from(names.len()).unwrap();
            names.extend_from_slice(s.as_bytes());
            NameRef::new(off, u16::try_from(s.len()).unwrap(), false)
        };
        let node = |parent, name, files| DuNode {
            parent,
            name,
            flags: NodeFlags(NodeFlags::DONE),
            files,
            file_id: 7,
            disk: 4096 * u64::from(files),
            logical: 4000 * u64::from(files),
            mtime: -5,
        };
        let dirs = vec![
            node(NO_PARENT, NameRef::default(), 3),
            node(0, name("Видео"), 2),
            node(1, name("sub"), 1),
        ];
        let files = vec![FileRec {
            dir: 1,
            name: name("big.mkv"),
            ty: DuType::Video,
            attrs: FileRec::COMPRESSED,
            disk: 9 << 20,
            logical: 10 << 20,
            mtime: 1_700_000_000,
            file_id: u64::MAX,
        }];
        let mut types = TypeAgg::default();
        types.disk[DuType::Video as usize] = 9 << 20;
        types.files[DuType::Video as usize] = 1;
        DuSnapshot {
            root: PathBuf::from(r"C:\Users\me"),
            volume: VolumeKey {
                serial: 0xDEAD_BEEF,
                fs: FsKind::Ntfs,
            },
            scanned_at: 123,
            took_ms: 45,
            generation: 2,
            usn: Some(UsnMark {
                journal_id: 1,
                next_usn: -1,
            }),
            dirs,
            names,
            files,
            types,
            denied: vec![2],
            size_mode: SizeMode::Logical,
            track_min: 1 << 20,
            flags: DuSnapshot::PARTIAL,
        }
    }

    fn cfg(keep: u16, max: &str) -> DuCfg {
        DuCfg {
            cache_keep: keep,
            cache_max: max.into(),
            ..DuCfg::default()
        }
    }

    #[test]
    fn roundtrip() {
        let s = sample();
        let b = encode(&s).unwrap();
        assert_eq!(&b[..6], MAGIC);
        let back = decode(&b).unwrap();
        assert_eq!(back, s);
        assert_eq!(encode(&back).unwrap(), b);
        assert_eq!(back.name(1), "Видео");
        assert_eq!(back.file_name(&back.files[0]), "big.mkv");
    }

    /// Порча в любом месте, обрезка, чужая версия — ошибка, без паники.
    #[test]
    fn corrupt_rejected() {
        let b = encode(&sample()).unwrap();
        for i in 0..b.len() {
            let mut x = b.clone();
            x[i] ^= 0x40;
            assert!(decode(&x).is_err(), "byte {i}");
        }
        for len in 0..b.len() {
            assert!(decode(&b[..len]).is_err(), "len {len}");
        }
        let mut v2 = b.clone();
        v2[6] = 2;
        let crc = crc32fast::hash(&v2[..16]);
        v2[16..20].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(decode(&v2), Err(CacheError::Corrupt("version"))));
    }

    /// Сломанные ссылки при верном трейлере (ошибка записи) не проходят.
    #[test]
    fn bad_links_rejected() {
        let reseal = |s: &DuSnapshot| decode(&encode(s).unwrap());
        let mut s = sample();
        s.dirs[1].parent = 2;
        assert!(matches!(reseal(&s), Err(CacheError::Corrupt("dirs"))));
        let mut s = sample();
        s.dirs[2].name = NameRef::new(1000, 3, false);
        assert!(reseal(&s).is_err());
        let mut s = sample();
        s.files[0].dir = 3;
        assert!(matches!(reseal(&s), Err(CacheError::Corrupt("files"))));
        let mut s = sample();
        s.denied = vec![9];
        assert!(matches!(reseal(&s), Err(CacheError::Corrupt("denied"))));
    }

    #[test]
    fn key_ignores_case_and_trailing_slash() {
        let a = file_name(1, Path::new(r"C:\Users\Me\"));
        assert_eq!(a, file_name(1, Path::new("c:/users/me")));
        assert_ne!(a, file_name(2, Path::new(r"C:\Users\Me")));
        assert!(a.starts_with("00000001-"));
        assert_eq!(Path::new(&a).extension(), Some("du".as_ref()));
        assert_eq!(a.len(), 8 + 1 + 16 + 3);
    }

    #[test]
    fn portable_path() {
        let paths = Paths::in_dir(r"X:\Urfin\data");
        let c = DuCache::new(&paths, &DuCfg::default());
        assert_eq!(c.dir(), paths.state_dir.join("du"));
        assert_eq!(c.max_bytes, 256 << 20);
        assert_eq!(c.keep, 8);
    }

    #[test]
    fn store_load_and_broken_file_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let c = DuCache::in_dir(tmp.path().join("du"), &DuCfg::default());
        let s = sample();
        assert!(c.load(s.volume.serial, &s.root).is_none());
        let path = c.store(&s).unwrap();
        assert_eq!(path, c.path(s.volume.serial, &s.root));
        assert_eq!(
            c.load(s.volume.serial, Path::new(r"c:\users\me\")),
            Some(s.clone())
        );
        assert_eq!(fs::read_dir(c.dir()).unwrap().count(), 1, "no tmp left");

        let mut b = fs::read(&path).unwrap();
        let mid = b.len() / 2;
        b[mid] ^= 1;
        fs::write(&path, &b).unwrap();
        assert!(c.load(s.volume.serial, &s.root).is_none());
        assert!(!path.exists(), "broken cache removed");
    }

    fn snap_at(root: &str) -> DuSnapshot {
        DuSnapshot {
            root: PathBuf::from(root),
            ..sample()
        }
    }

    fn age(path: &Path, secs: u64) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn lru_keep_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let c = DuCache::in_dir(tmp.path().to_path_buf(), &cfg(2, "1 MiB"));
        let a = c.store(&snap_at(r"C:\a")).unwrap();
        age(&a, 300);
        let b = c.store(&snap_at(r"C:\b")).unwrap();
        age(&b, 200);
        // Открытие `a` делает его свежее `b`.
        assert!(c.load(0xDEAD_BEEF, Path::new(r"C:\a")).is_some());
        let n = c.store(&snap_at(r"C:\c")).unwrap();
        assert!(a.exists() && n.exists());
        assert!(!b.exists(), "least recently opened evicted");
    }

    #[test]
    fn lru_size_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let one = encode(&snap_at(r"C:\a")).unwrap().len();
        let c = DuCache::in_dir(
            tmp.path().to_path_buf(),
            &cfg(8, &(one * 2 + one / 2).to_string()),
        );
        let mut paths = Vec::new();
        for (i, root) in [r"C:\a", r"C:\b", r"C:\c"].into_iter().enumerate() {
            let p = c.store(&snap_at(root)).unwrap();
            age(&p, 100 - 10 * i as u64);
            paths.push(p);
        }
        c.trim(None).unwrap();
        assert!(!paths[0].exists());
        assert!(paths[1].exists() && paths[2].exists());
        let total: u64 = fs::read_dir(c.dir())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= c.max_bytes);

        let tiny = DuCache::in_dir(tmp.path().join("tiny"), &cfg(8, "16"));
        assert!(matches!(tiny.store(&sample()), Err(CacheError::Limit(_))));
        let off = DuCache::in_dir(tmp.path().join("off"), &cfg(0, "1 MiB"));
        assert!(matches!(off.store(&sample()), Err(CacheError::Limit(_))));
        assert!(!tmp.path().join("off").exists());
    }

    #[test]
    fn stale_tmp_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let c = DuCache::in_dir(tmp.path().to_path_buf(), &DuCfg::default());
        let old = tmp.path().join("x.du.1-0.tmp");
        let new = tmp.path().join("y.du.1-1.tmp");
        fs::write(&old, b"x").unwrap();
        fs::write(&new, b"y").unwrap();
        age(&old, 7200);
        c.trim(None).unwrap();
        assert!(!old.exists() && new.exists());
    }

    mod roundtrip_prop {
        use proptest::prelude::*;

        use super::*;

        fn snapshot() -> impl Strategy<Value = DuSnapshot> {
            let dirs = prop::collection::vec(
                (
                    any::<u32>(),
                    "[a-zа-я0-9 ]{0,12}",
                    any::<(u16, u32, u64, u64, u64, i64)>(),
                ),
                1..40,
            );
            let files = prop::collection::vec(
                (
                    any::<u32>(),
                    "[a-z.]{0,8}",
                    0..10usize,
                    any::<(u8, u64, u64, i64, u64)>(),
                ),
                0..20,
            );
            (
                dirs,
                files,
                any::<(u32, i64, u32, u64, bool, u8, u64)>(),
                any::<[u64; 10]>(),
            )
                .prop_map(|(dirs, files, meta, tdisk)| {
                    let mut names = Vec::new();
                    let mut put = |s: &str| {
                        let off = u32::try_from(names.len()).unwrap();
                        names.extend_from_slice(s.as_bytes());
                        NameRef::new(
                            off,
                            u16::try_from(s.len()).unwrap(),
                            s.len().is_multiple_of(3),
                        )
                    };
                    let n = u32::try_from(dirs.len()).unwrap();
                    let dirs: Vec<DuNode> = dirs
                        .iter()
                        .enumerate()
                        .map(|(i, (p, name, (fl, files, id, disk, logical, mtime)))| {
                            let i = u32::try_from(i).unwrap();
                            DuNode {
                                parent: if i == 0 { NO_PARENT } else { p % i },
                                name: put(name),
                                flags: NodeFlags(*fl),
                                files: *files,
                                file_id: *id,
                                disk: *disk,
                                logical: *logical,
                                mtime: *mtime,
                            }
                        })
                        .collect();
                    let files = files
                        .iter()
                        .map(|(d, name, ty, (attrs, disk, logical, mtime, id))| FileRec {
                            dir: d % n,
                            name: put(name),
                            ty: DuType::ALL[*ty],
                            attrs: *attrs,
                            disk: *disk,
                            logical: *logical,
                            mtime: *mtime,
                            file_id: *id,
                        })
                        .collect();
                    let (serial, scanned_at, took_ms, generation, usn, flags, track_min) = meta;
                    DuSnapshot {
                        root: PathBuf::from(format!(r"D:\корень\{serial}")),
                        volume: VolumeKey {
                            serial,
                            fs: FsKind::Refs,
                        },
                        scanned_at,
                        took_ms,
                        generation,
                        usn: usn.then_some(UsnMark {
                            journal_id: generation,
                            next_usn: scanned_at,
                        }),
                        denied: (0..n).filter(|i| i % 3 == 1).collect(),
                        dirs,
                        names,
                        files,
                        types: TypeAgg {
                            disk: tdisk,
                            files: tdisk.map(|v| v >> 7),
                        },
                        size_mode: if usn {
                            SizeMode::Disk
                        } else {
                            SizeMode::Logical
                        },
                        track_min,
                        flags,
                    }
                })
        }

        proptest! {
            #[test]
            fn roundtrip_any(s in snapshot()) {
                let b = encode(&s).unwrap();
                let back = decode(&b).unwrap();
                prop_assert_eq!(&back, &s);
                prop_assert_eq!(encode(&back).unwrap(), b);
            }
        }
    }
}
