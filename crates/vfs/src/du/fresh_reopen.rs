//! Повторное открытие из кэша (F01 §3.3, F01-11): с правами — USN-журнал тома с
//! сохранённой позиции, затронутые каталоги перелистываются; без прав, при
//! пересозданном или усечённом журнале — `mtime` каждого каталога снимка, отличие —
//! «●». Ctrl+R при «●» перелистывает только их ([`relist_changed`]).

use std::path::{Path, PathBuf};

use compact_str::CompactString;
use rustc_hash::{FxHashMap, FxHashSet};
use urfin_core::Location;

use super::{Relister, relist};
use crate::api::Vfs;
use crate::du::dvfs::{DuRegistry, Frozen, SnapId};
use crate::du::model::{DirId, DuType, UsnMark};
use crate::du::scan::{self, ScanCfg};

/// Каталоги для перелистывания с подсказками типов.
type Dirs = Vec<(DirId, Vec<DuType>)>;

/// Позиция журнала тома сейчас.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct JournalPos {
    pub id: u64,
    /// Самая старая запись в журнале.
    pub first: i64,
    pub next: i64,
}

/// Как дочитать изменения снимка.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Журнал тот же и не усечён: записи с `from`.
    Usn { from: i64 },
    /// Нет прав или позиции, журнал пересоздан или усечён — перепроверка `mtime`.
    Mtime,
}

/// Путь дочитки: сохранённая позиция `saved` и журнал тома `cur` (`None` — нет прав
/// или журнала).
pub fn plan(saved: Option<UsnMark>, cur: Option<JournalPos>) -> Plan {
    match (saved, cur) {
        (Some(s), Some(c))
            if s.journal_id == c.id && s.next_usn >= c.first && s.next_usn <= c.next =>
        {
            Plan::Usn { from: s.next_usn }
        }
        _ => Plan::Mtime,
    }
}

/// Позиция USN-журнала тома `root` (NTFS, права администратора); иначе `None`.
pub fn usn_mark(root: &Path) -> Option<UsnMark> {
    journal(root).map(|(_, j)| UsnMark {
        journal_id: j.id,
        next_usn: j.next,
    })
}

#[cfg(windows)]
type Volume = crate::sys::win_usn::Volume;
#[cfg(not(windows))]
type Volume = ();

#[cfg(windows)]
fn journal(root: &Path) -> Option<(Volume, JournalPos)> {
    use crate::sys::win_usn;
    let v = win_usn::open(root)
        .map_err(|e| tracing::debug!(target: "vfs.du", error = %e, "usn: volume"))
        .ok()?;
    let j = win_usn::query(&v, root)
        .map_err(|e| tracing::debug!(target: "vfs.du", error = %e, "usn: journal"))
        .ok()?;
    Some((
        v,
        JournalPos {
            id: j.id,
            first: j.first,
            next: j.next,
        },
    ))
}

#[cfg(not(windows))]
fn journal(_root: &Path) -> Option<(Volume, JournalPos)> {
    None
}

/// `FileId` каталога (NTFS: номер записи MFT с последовательностью); 0 — не узнали.
pub fn file_id(p: &Path) -> u64 {
    #[cfg(windows)]
    {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "FRN NTFS — младшие 64 бита FILE_ID_128"
        )]
        crate::sys::win_meta::stat_extra(p, true).map_or(0, |e| e.file_id.1 as u64)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        0
    }
}

/// Итог дочитки.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// Шли по USN-журналу.
    pub usn: bool,
    /// Каталогов перелистано (USN).
    pub relisted: usize,
    /// Каталогов отмечено «●» (`mtime`).
    pub changed: usize,
}

/// Дочитать изменения снимка `id` после открытия из кэша (вне UI-потока): с правами —
/// USN-журнал, затронутые каталоги перелистываются; иначе — «●» по `mtime` (у
/// исчезнувшего — у родителя). Рост файла без смены `mtime` каталога так не виден
/// (принято, F01 §8).
pub fn check(
    reg: &DuRegistry,
    id: SnapId,
    fs: &dyn Vfs,
    cfg: &ScanCfg,
    halted: &(dyn Fn() -> bool + Sync),
) -> Checked {
    let (Some(f), Some(root)) = (reg.frozen(id), reg.root(id)) else {
        return Checked::default();
    };
    if let Some((vol, j)) = journal(&root)
        && let Plan::Usn { from } = plan(f.snap.usn, Some(j))
        && let Some((dirs, end)) = usn_dirs(&vol, &root, &f, j, from)
    {
        drop(f);
        let lister = Relister::for_root(&root, cfg.track_min);
        let relisted = relist(reg, id, &dirs, &lister, fs, cfg, halted);
        if !halted() {
            reg.set_usn(
                id,
                UsnMark {
                    journal_id: j.id,
                    next_usn: end,
                },
            );
        }
        tracing::info!(target: "vfs.du", relisted, "fresh: usn");
        return Checked {
            usn: true,
            relisted,
            changed: 0,
        };
    }
    let threads = crate::volume::walk_threads(cfg.threads, &Location::local(&root));
    let changed = mtime_changed(&f, &root, threads, halted);
    drop(f);
    let n = if halted() {
        0
    } else {
        reg.mark_changed(id, Some(&changed))
    };
    tracing::info!(target: "vfs.du", changed = n, "fresh: mtime");
    Checked {
        usn: false,
        relisted: 0,
        changed: n,
    }
}

/// Каталоги снимка — родители записей журнала с `from` (по `FileId`), и позиция после
/// прочитанного. `None` — журнал не прочитан (пересоздан между запросом и чтением).
#[cfg(windows)]
fn usn_dirs(
    vol: &Volume,
    root: &Path,
    f: &Frozen,
    j: JournalPos,
    from: i64,
) -> Option<(Dirs, i64)> {
    let by_id: FxHashMap<u64, DirId> = (0u32..)
        .zip(&f.snap.dirs)
        .filter(|(_, n)| n.file_id != 0)
        .map(|(k, n)| (n.file_id, k))
        .collect();
    let mut hit: FxHashSet<DirId> = FxHashSet::default();
    let j = crate::sys::win_usn::Journal {
        id: j.id,
        first: j.first,
        next: j.next,
    };
    let end = crate::sys::win_usn::read(vol, root, j, from, &mut |_, parent, _| {
        if let Some(&d) = by_id.get(&parent) {
            hit.insert(d);
        }
    })
    .map_err(|e| tracing::debug!(target: "vfs.du", error = %e, "usn: read"))
    .ok()?;
    let mut dirs: Vec<(DirId, Vec<DuType>)> = hit
        .into_iter()
        .filter(|&d| !f.dead(d))
        .map(|d| (d, Vec::new()))
        .collect();
    dirs.sort_unstable_by_key(|(d, _)| *d);
    Some((dirs, end))
}

#[cfg(not(windows))]
fn usn_dirs(
    _vol: &Volume,
    _root: &Path,
    _f: &Frozen,
    _j: JournalPos,
    _from: i64,
) -> Option<(Dirs, i64)> {
    None
}

/// Пути каталогов снимка (родитель всегда раньше ребёнка: id выдаются при листинге
/// родителя; иначе — по цепочке имён).
fn paths(f: &Frozen, root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::with_capacity(f.snap.dirs.len());
    for (k, n) in (0u32..).zip(&f.snap.dirs) {
        let p = if k == 0 {
            root.to_path_buf()
        } else if n.parent < k {
            out[n.parent as usize].join(f.snap.name(k))
        } else {
            root.join(
                f.rel_names(k)
                    .iter()
                    .map(CompactString::as_str)
                    .collect::<PathBuf>(),
            )
        };
        out.push(p);
    }
    out
}

/// Каталоги, чей `mtime` на диске не совпал со снимком; исчезнувший — его родитель.
/// `threads` потоков, у каждого — свой кусок каталогов.
pub(crate) fn mtime_changed(
    f: &Frozen,
    root: &Path,
    threads: usize,
    halted: &(dyn Fn() -> bool + Sync),
) -> Vec<DirId> {
    let dirs = &f.snap.dirs;
    let paths = paths(f, root);
    let ids: Vec<DirId> = (0u32..)
        .zip(dirs)
        .filter(|(k, n)| n.mtime != 0 && !f.dead(*k))
        .map(|(k, _)| k)
        .collect();
    let chunk = ids.len().div_ceil(threads.max(1)).max(1);
    let out = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for part in ids.chunks(chunk) {
            let (paths, out) = (&paths, &out);
            s.spawn(move || {
                let mut mine = Vec::new();
                for &k in part {
                    if halted() {
                        break;
                    }
                    let n = &dirs[k as usize];
                    let p = &paths[k as usize];
                    match std::fs::symlink_metadata(p) {
                        Ok(_) => {
                            let m = scan::root_mtime(p);
                            if m != 0 && m != n.mtime {
                                mine.push(k);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound && k != 0 => {
                            mine.push(n.parent);
                        }
                        Err(_) => {}
                    }
                }
                out.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend(mine);
            });
        }
    });
    let mut v = out
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    v.sort_unstable();
    v.dedup();
    v
}

/// Ctrl+R при «●»: перелистать только каталоги с «●» (новые поддеревья — досчётом).
/// `None` — «●» нет или их больше половины снимка (переполнение наблюдения): нужен
/// полный пересчёт.
pub fn relist_changed(
    reg: &DuRegistry,
    id: SnapId,
    fs: &dyn Vfs,
    cfg: &ScanCfg,
    halted: &(dyn Fn() -> bool + Sync),
) -> Option<usize> {
    let changed = reg.changed(id);
    let total = reg.frozen(id)?.snap.dirs.len();
    if changed.is_empty() || changed.len() * 2 > total {
        return None;
    }
    let root = reg.root(id)?;
    let relister = Relister::for_root(&root, cfg.track_min);
    let dirs: Vec<(DirId, Vec<DuType>)> = changed.into_iter().map(|d| (d, Vec::new())).collect();
    Some(relist(reg, id, &dirs, &relister, fs, cfg, halted))
}

#[cfg(test)]
mod tests {
    use super::{JournalPos, Plan, plan};
    use crate::du::model::UsnMark;

    const J: JournalPos = JournalPos {
        id: 7,
        first: 1000,
        next: 5000,
    };

    fn mark(id: u64, next: i64) -> Option<UsnMark> {
        Some(UsnMark {
            journal_id: id,
            next_usn: next,
        })
        .filter(|m| m.journal_id != 0)
    }

    #[test]
    fn same_journal_reads_from_mark() {
        assert_eq!(plan(mark(7, 2000), Some(J)), Plan::Usn { from: 2000 });
        assert_eq!(plan(mark(7, 5000), Some(J)), Plan::Usn { from: 5000 });
    }

    #[test]
    fn recreated_truncated_or_no_rights_fall_back_to_mtime() {
        // Пересоздан: другой id.
        assert_eq!(plan(mark(8, 2000), Some(J)), Plan::Mtime);
        // Усечён: позиция старше первой записи.
        assert_eq!(plan(mark(7, 999), Some(J)), Plan::Mtime);
        // Позиция из будущего (журнал пересоздан с тем же id после сброса).
        assert_eq!(plan(mark(7, 6000), Some(J)), Plan::Mtime);
        // Нет прав / журнала; снимок без позиции.
        assert_eq!(plan(mark(7, 2000), None), Plan::Mtime);
        assert_eq!(plan(None, Some(J)), Plan::Mtime);
    }
}
