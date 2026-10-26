//! Наблюдение опросом (S4 §5.2, S4-13): для ФС без уведомлений (`Vfs::watch` →
//! `Unsupported`: часть SMB/NAS, FUSE). Каталог перечитывается раз в
//! `max(poll_interval, 10 × время последнего листинга)`, изменения — дифф по имени,
//! размеру, mtime, виду и атрибутам. Расписание и «только видимые табы» — hub (S4-14).

use std::cmp::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::{CancelToken, Entry, ListOpts, Vfs, VfsError, VfsPath, WatchEvent};

/// Во сколько раз интервал опроса длиннее последнего листинга (сеть медленная —
/// реже опрашиваем).
const LIST_FACTOR: u32 = 10;

/// Опрос одного каталога.
pub struct Poller {
    vfs: Arc<dyn Vfs>,
    dir: VfsPath,
    base: Duration,
    /// Снимок, отсортированный по байтам имени.
    snap: Vec<Entry>,
    /// Снимка ещё нет: первый опрос его только запоминает.
    primed: bool,
    last_list: Duration,
    due: Instant,
}

impl std::fmt::Debug for Poller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Poller")
            .field("dir", &self.dir)
            .field("entries", &self.snap.len())
            .field("interval", &self.interval())
            .finish_non_exhaustive()
    }
}

impl Poller {
    /// `entries` — уже показанный листинг (снимок без повторного чтения);
    /// `last_list` — сколько он читался (`Duration::ZERO`, если неизвестно).
    pub fn new(
        vfs: Arc<dyn Vfs>,
        dir: VfsPath,
        entries: &[Entry],
        base: Duration,
        last_list: Duration,
        now: Instant,
    ) -> Poller {
        let mut snap = entries.to_vec();
        snap.sort_unstable_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        let mut p = Poller {
            vfs,
            dir,
            base,
            snap,
            primed: true,
            last_list,
            due: now,
        };
        p.due = now + p.interval();
        p
    }

    /// Без снимка: первый опрос (через интервал) запоминает каталог, изменения
    /// — со второго.
    pub fn baseline(
        vfs: Arc<dyn Vfs>,
        dir: VfsPath,
        base: Duration,
        last_list: Duration,
        now: Instant,
    ) -> Poller {
        let mut p = Poller::new(vfs, dir, &[], base, last_list, now);
        p.primed = false;
        p
    }

    /// Снимок показанного листинга вместо базовой линии первым опросом.
    pub fn prime(&mut self, entries: &[Entry]) {
        let mut snap = entries.to_vec();
        snap.sort_unstable_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        self.snap = snap;
        self.primed = true;
    }

    /// Текущий интервал опроса.
    pub fn interval(&self) -> Duration {
        self.base.max(self.last_list * LIST_FACTOR)
    }

    /// Когда опрашивать в следующий раз.
    pub fn due(&self) -> Instant {
        self.due
    }

    /// Перечитать каталог и вернуть изменения со времени прошлого снимка.
    /// Каталога нет — `[Gone]`; прочие ошибки (сеть) — `Err`, снимок прежний,
    /// следующий опрос по расписанию.
    pub fn poll(
        &mut self,
        cancel: &CancelToken,
        now: Instant,
    ) -> Result<Vec<WatchEvent>, VfsError> {
        let t0 = Instant::now();
        let listed = self.list(cancel);
        self.last_list = t0.elapsed();
        self.due = now + self.interval();
        let mut fresh = match listed {
            Ok(v) => v,
            Err(VfsError::NotFound(_) | VfsError::NotDir(_)) => {
                self.snap.clear();
                return Ok(vec![WatchEvent::Gone]);
            }
            Err(e) => return Err(e),
        };
        fresh.sort_unstable_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        let events = if self.primed {
            diff(&self.snap, &fresh)
        } else {
            self.primed = true;
            Vec::new()
        };
        self.snap = fresh;
        Ok(events)
    }

    fn list(&self, cancel: &CancelToken) -> Result<Vec<Entry>, VfsError> {
        let mut s = self.vfs.list(&self.dir, &ListOpts::default())?;
        let mut out = Vec::with_capacity(self.snap.len());
        while s.next_batch(&mut out, cancel)? {}
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        Ok(out)
    }
}

/// Слияние двух снимков, отсортированных по байтам имени.
fn diff(old: &[Entry], new: &[Entry]) -> Vec<WatchEvent> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        let ord = match (old.get(i), new.get(j)) {
            (Some(a), Some(b)) => a.name.as_bytes().cmp(b.name.as_bytes()),
            (Some(_), None) => Ordering::Less,
            (None, _) => Ordering::Greater,
        };
        match ord {
            Ordering::Less => {
                out.push(WatchEvent::Removed(old[i].name.clone()));
                i += 1;
            }
            Ordering::Greater => {
                out.push(WatchEvent::Added(new[j].name.clone(), Some(new[j].clone())));
                j += 1;
            }
            Ordering::Equal => {
                let (a, b) = (&old[i], &new[j]);
                if a.size != b.size || a.mtime != b.mtime || a.kind != b.kind || a.attrs != b.attrs
                {
                    out.push(WatchEvent::Modified(b.name.clone(), Some(b.clone())));
                }
                i += 1;
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Attrs, EntryKind};

    fn e(name: &str, size: u64, mtime: i64) -> Entry {
        Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: name.into(),
            size,
            mtime,
            attrs: Attrs(0),
            kind: EntryKind::File,
        }
    }

    fn names(ev: &[WatchEvent]) -> Vec<String> {
        ev.iter()
            .map(|e| match e {
                WatchEvent::Added(n, _) => format!("+{n}"),
                WatchEvent::Removed(n) => format!("-{n}"),
                WatchEvent::Modified(n, _) => format!("~{n}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn diff_added_removed_modified() {
        let old = [e("a", 1, 1), e("b", 2, 2), e("c", 3, 3)];
        let new = [e("a", 1, 1), e("b", 2, 5), e("d", 4, 4)];
        assert_eq!(names(&diff(&old, &new)), ["~b", "-c", "+d"]);
        assert!(diff(&new, &new).is_empty());
        assert_eq!(names(&diff(&[], &old)), ["+a", "+b", "+c"]);
        assert_eq!(names(&diff(&old, &[])), ["-a", "-b", "-c"]);
    }

    /// Имена, различающиеся только регистром, — разные (сравнение по байтам).
    #[test]
    fn diff_is_byte_exact() {
        let old = [e("A", 1, 1)];
        let new = [e("A", 1, 1), e("a", 1, 1)];
        assert_eq!(names(&diff(&old, &new)), ["+a"]);
    }
}
