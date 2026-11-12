//! Дедуп жёстких ссылок (F01 §3.1): размер файла учитывается при первой встрече
//! его `FileId`, повторные ссылки — без размера.
//!
//! NTFS — растущий атомарный битсет по номеру записи MFT (младшие 48 бит `FileId`):
//! сегменты по 2^20 бит (128 KiB) создаются при первой записи в них; ~5M записей
//! тома — ≤ 640 КБ. Записи за пределами сегментов и прочие ФС (`ReFS`) —
//! шардированный `FxHashSet`. `FileId == 0` (неизвестен) — всегда «первая встреча».
//!
//! `ReFS` (F01-13): `FileId` 128-битный; в 64-битный ключ записи обхода он сводится
//! [`link_key`] — id со старшей половиной 0 (NTFS) как есть, прочие — xxh3-64 от всех
//! 128 бит (разные id совпадут с вероятностью ~2⁻⁶⁴ на пару).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use rustc_hash::FxHashSet;

const SEG_SHIFT: u32 = 20;
const SEG_BITS: u64 = 1 << SEG_SHIFT;
const SEG_WORDS: usize = (SEG_BITS / 64) as usize;
/// Сегментов максимум: 2^32 записей MFT.
const MAX_SEGS: usize = 1 << 12;
const RECORD_MASK: u64 = (1 << 48) - 1;
const SHARDS: usize = 16;

/// 64-битный ключ дедупа из 128-битного `FileId` (`ReFS`, F01-13). Старшие 64 бита
/// нулевые (NTFS, `FileIdFullDirectoryInformation`) — младшие как есть: битсет MFT и
/// прежнее поведение. Иначе — xxh3-64 от 16 байт id, с поднятым старшим битом, чтобы
/// не совпасть ни с 0 («неизвестен»), ни с 64-битными id того же тома.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "листер с `FileId` — только Windows")
)]
pub fn link_key(id: u128) -> u64 {
    match u64::try_from(id) {
        Ok(lo) => lo,
        Err(_) => xxhash_rust::xxh3::xxh3_64(&id.to_le_bytes()) | (1 << 63),
    }
}

/// Множество встреченных `FileId`.
pub enum LinkSet {
    /// Дедуп выключен (`FindLister`, `VfsLister`: `FileId` нет).
    Off,
    /// NTFS: битсет по номеру записи MFT.
    Mft(Box<Bits>),
    /// Прочие ФС с `FileId`: множество целиком.
    Hashed(Box<Shards>),
}

pub struct Bits {
    segs: Box<[OnceLock<Box<[AtomicU64]>>]>,
    overflow: Shards,
}

pub struct Shards([Mutex<FxHashSet<u64>>; SHARDS]);

impl Shards {
    fn new() -> Shards {
        Shards(std::array::from_fn(|_| Mutex::new(FxHashSet::default())))
    }

    fn insert(&self, id: u64) -> bool {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "номер шарда — младшие биты хэша"
        )]
        let shard = (id.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 60) as usize % SHARDS;
        self.0[shard]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id)
    }
}

impl LinkSet {
    pub fn mft() -> LinkSet {
        LinkSet::Mft(Box::new(Bits {
            segs: (0..MAX_SEGS).map(|_| OnceLock::new()).collect(),
            overflow: Shards::new(),
        }))
    }

    pub fn hashed() -> LinkSet {
        LinkSet::Hashed(Box::new(Shards::new()))
    }

    /// `true` — файл встречен впервые (его размер учитывается).
    pub fn first(&self, file_id: u64) -> bool {
        if file_id == 0 {
            return true;
        }
        match self {
            LinkSet::Off => true,
            LinkSet::Hashed(s) => s.insert(file_id),
            LinkSet::Mft(b) => {
                let rec = file_id & RECORD_MASK;
                let Ok(seg) = usize::try_from(rec >> SEG_SHIFT) else {
                    return b.overflow.insert(rec);
                };
                let Some(slot) = b.segs.get(seg) else {
                    return b.overflow.insert(rec);
                };
                let words =
                    slot.get_or_init(|| (0..SEG_WORDS).map(|_| AtomicU64::new(0)).collect());
                let bit = rec & (SEG_BITS - 1);
                #[allow(clippy::cast_possible_truncation, reason = "bit < 2^20")]
                let word = &words[(bit >> 6) as usize];
                let mask = 1u64 << (bit & 63);
                word.fetch_or(mask, Ordering::Relaxed) & mask == 0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_meeting_only() {
        for set in [LinkSet::mft(), LinkSet::hashed()] {
            // Одна запись MFT, разные номера последовательности не важны.
            assert!(set.first(0x0001_0000_0000_0042));
            assert!(!set.first(0x0001_0000_0000_0042));
            assert!(set.first(0x43));
            // Неизвестный FileId — всегда первая встреча.
            assert!(set.first(0));
            assert!(set.first(0));
            // Далеко за первым сегментом и за пределом сегментов.
            assert!(set.first(5_000_000));
            assert!(!set.first(5_000_000));
            assert!(set.first(1 << 40));
            assert!(!set.first(1 << 40));
        }
        let off = LinkSet::Off;
        assert!(off.first(7) && off.first(7));
    }

    /// F01-13: дедуп по 128-битным id (подставной источник: id, как их даёт `ReFS`).
    #[test]
    fn refs_128bit_ids_dedup() {
        // Одинаковые младшие 64 бита, разные старшие — разные файлы.
        let a: u128 = (7 << 64) | 0x42;
        let b: u128 = (9 << 64) | 0x42;
        let set = LinkSet::hashed();
        assert!(set.first(link_key(a)));
        assert!(set.first(link_key(b)));
        // Вторая жёсткая ссылка того же файла — без размера.
        assert!(!set.first(link_key(a)));
        assert!(!set.first(link_key(b)));
        // 64-битный id не сливается со 128-битным с теми же младшими битами.
        assert!(set.first(link_key(0x42)));
        assert_ne!(link_key(a), 0);
        assert_ne!(link_key(u128::MAX), link_key(u128::from(u64::MAX)));
        // Подставной обход: три ссылки двух файлов — размер учтён дважды.
        let ids = [a, b, a, b, a];
        let set = LinkSet::hashed();
        let firsts = ids.iter().filter(|&&id| set.first(link_key(id))).count();
        assert_eq!(firsts, 2);
    }

    /// F01-13: на NTFS (старшие 64 бита нулевые) ключ — прежний 64-битный `FileId`.
    #[test]
    fn ntfs_ids_unchanged() {
        for id in [0u64, 0x43, 0x0001_0000_0000_0042, 1 << 40, u64::MAX] {
            assert_eq!(link_key(u128::from(id)), id);
        }
        let set = LinkSet::mft();
        assert!(set.first(link_key(0x0001_0000_0000_0042)));
        assert!(!set.first(link_key(0x0002_0000_0000_0042)));
    }

    #[test]
    fn concurrent_single_winner() {
        let set = LinkSet::mft();
        let wins = std::sync::atomic::AtomicU32::new(0);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for id in 1..=1000u64 {
                        if set.first(id * 3) {
                            wins.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(wins.load(Ordering::Relaxed), 1000);
    }
}
