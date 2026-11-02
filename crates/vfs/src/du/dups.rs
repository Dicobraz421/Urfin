//! Дубликаты (F01 §3.6): группы крупных файлов снимка по размеру, частичный хэш
//! (xxh3-64 первых 64 KiB) при входе в фильтр, полный (xxh3-128 потоком по 1 MiB) по
//! Ctrl+H. Группировка и разбиение — память снимка; хэши — блокирующий I/O, только из
//! задач Runtime (`du.dups`), не из UI-потока.

use std::hash::Hash;
use std::io::{self, Read};
use std::path::Path;

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use urfin_core::CancelToken;
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use super::model::{DuSnapshot, DupGroup, DupLevel, FileRec};

/// Начало файла для частичного хэша.
pub const HEAD: usize = 64 << 10;
/// Буфер полного хэша.
const CHUNK: usize = 1 << 20;
/// `FILE_FLAG_SEQUENTIAL_SCAN`: чтение подряд, кэш ОС не держит прочитанное.
const SEQUENTIAL_SCAN: u32 = 0x0800_0000;

/// Группы кандидатов (уровень `Size`): крупные файлы снимка с одинаковым логическим
/// размером не меньше `min`; без `OFFLINE` (облачные заглушки не гидрируются); жёсткие
/// ссылки (общий `file_id`) — одна запись: это не дубликаты. `live(i)` — файл ещё в
/// снимке (не удалён после скана).
pub fn candidates(snap: &DuSnapshot, min: u64, live: impl Fn(u32) -> bool) -> Vec<DupGroup> {
    let mut by: FxHashMap<u64, SmallVec<[u32; 4]>> = FxHashMap::default();
    for (i, f) in (0u32..).zip(&snap.files) {
        if f.logical < min.max(1) || f.attrs & FileRec::OFFLINE != 0 || !live(i) {
            continue;
        }
        let g = by.entry(f.logical).or_default();
        let link = f.file_id != 0
            && g.iter()
                .any(|&j| snap.files[j as usize].file_id == f.file_id);
        if !link {
            g.push(i);
        }
    }
    let mut out: Vec<DupGroup> = by
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(size, files)| DupGroup {
            size,
            files,
            level: DupLevel::Size,
        })
        .collect();
    order(snap, &mut out);
    out
}

/// Порядок: в группе — самый старый `mtime` первым (вероятный оригинал), группы — по
/// размеру ↓.
pub fn order(snap: &DuSnapshot, groups: &mut [DupGroup]) {
    for g in groups.iter_mut() {
        g.files
            .sort_by_key(|&i| (snap.files.get(i as usize).map_or(i64::MAX, |f| f.mtime), i));
    }
    groups.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.files.first().cmp(&b.files.first()))
    });
}

/// Разбить группу по хэшам файлов: одинаковый хэш — новая группа уровня `level`;
/// файлы без хэша (не прочитались) уходят; меньше двух — группы нет. Порядок файлов
/// внутри сохраняется.
pub fn split<H: Eq + Hash + Copy>(
    g: &DupGroup,
    hash: impl Fn(u32) -> Option<H>,
    level: DupLevel,
) -> Vec<DupGroup> {
    let mut buckets: Vec<(H, SmallVec<[u32; 4]>)> = Vec::new();
    for &i in &g.files {
        let Some(h) = hash(i) else {
            continue;
        };
        match buckets.iter_mut().find(|(k, _)| *k == h) {
            Some((_, v)) => v.push(i),
            None => buckets.push((h, SmallVec::from_slice(&[i]))),
        }
    }
    buckets
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(_, files)| DupGroup {
            size: g.size,
            files,
            level,
        })
        .collect()
}

/// Уровень после частичного хэша: файл не длиннее `HEAD` прочитан целиком — `Full`.
pub fn after_head(size: u64) -> DupLevel {
    if size <= HEAD as u64 {
        DupLevel::Full
    } else {
        DupLevel::Partial
    }
}

fn open_seq(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(SEQUENTIAL_SCAN)
        .open(path)
}

/// xxh3-64 первых [`HEAD`] байт файла.
pub fn head_hash(path: &Path) -> io::Result<u64> {
    let mut f = open_seq(path)?;
    let mut buf = vec![0u8; HEAD];
    let mut n = 0;
    while n < HEAD {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(xxh3_64(&buf[..n]))
}

/// xxh3-128 всего файла потоком по 1 MiB; `cancel` проверяется между кусками
/// (отмена — `Interrupted`, ≤ одного куска чтения). `read` — прочитано байт, для
/// прогресса задачи.
pub fn full_hash(path: &Path, cancel: &CancelToken, read: &mut dyn FnMut(u64)) -> io::Result<u128> {
    let mut f = open_seq(path)?;
    let mut buf = vec![0u8; CHUNK];
    let mut h = Xxh3::new();
    loop {
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let k = f.read(&mut buf)?;
        if k == 0 {
            return Ok(h.digest128());
        }
        h.update(&buf[..k]);
        read(k as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_by_hash() {
        let g = DupGroup {
            size: 10,
            files: SmallVec::from_slice(&[1, 2, 3, 4, 5]),
            level: DupLevel::Size,
        };
        let h = |i: u32| match i {
            1 | 3 => Some(7u64),
            2 | 4 => Some(9),
            _ => None,
        };
        let out = split(&g, h, DupLevel::Partial);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].files.as_slice(), &[1, 3]);
        assert_eq!(out[1].files.as_slice(), &[2, 4]);
        assert!(out.iter().all(|g| g.level == DupLevel::Partial));
        assert_eq!(split(&g, |_| None::<u64>, DupLevel::Full).len(), 0);
        assert_eq!(after_head(HEAD as u64), DupLevel::Full);
        assert_eq!(after_head(HEAD as u64 + 1), DupLevel::Partial);
    }

    /// Отмена полного хэша — сразу, без чтения дальше.
    #[test]
    fn full_hash_cancels() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.bin");
        std::fs::write(&p, vec![3u8; 3 * CHUNK]).unwrap();
        let cancel = CancelToken::new();
        let mut seen = 0;
        let a = full_hash(&p, &cancel, &mut |n| seen += n).unwrap();
        assert_eq!(seen, 3 * CHUNK as u64);
        assert_eq!(a, xxhash_rust::xxh3::xxh3_128(&vec![3u8; 3 * CHUNK]));
        cancel.cancel();
        let e = full_hash(&p, &cancel, &mut |_| {}).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
    }
}
