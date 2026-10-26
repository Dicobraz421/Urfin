//! Кэш каталогов (S4 §5.5, S4-16): возврат в недавний каталог — мгновенно,
//! «свежий» — вообще без чтения, «устаревший» — сразу из кэша и тихое
//! перечитывание (решение владельца №9).
//!
//! Наполняет хаб по `TabEvent::Left` (записи — `Arc` из core, без копии) и
//! только из завершённого листинга. Свежесть держит своё наблюдение (не больше
//! [`MAX_WATCHES`], только локальные несъёмные диски): первое же событие
//! делает запись устаревшей и снимает наблюдение. Сеть, UNC и съёмные тома не
//! наблюдаются и не держат хэндл — всегда устаревшие.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use urfin_core::{Entry, Location};

use crate::api::{WatchGuard, WatchId};

/// Держать ли для `loc` наблюдение кэша (локальный несъёмный диск).
pub fn watchable(loc: &Location) -> bool {
    #[cfg(windows)]
    {
        loc.backend == "local" && crate::sys::win_meta::cache_watchable(&loc.path)
    }
    #[cfg(not(windows))]
    {
        // Linux/macOS: наблюдение — S4-13 (`notify`); пока кэш всегда устаревший.
        let _ = loc;
        false
    }
}

/// Наблюдений для кэша не больше (LRU).
pub const MAX_WATCHES: usize = 8;

struct CachedDir {
    entries: Arc<Vec<Entry>>,
    fresh: bool,
    tick: u64,
    watch: Option<(WatchId, WatchGuard)>,
}

/// LRU каталогов с лимитами по числу каталогов и записей.
pub struct DirCache {
    map: FxHashMap<Location, CachedDir>,
    by_watch: FxHashMap<WatchId, Location>,
    tick: u64,
    max_dirs: usize,
    max_entries: usize,
    total: usize,
}

impl std::fmt::Debug for DirCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirCache")
            .field("dirs", &self.map.len())
            .field("entries", &self.total)
            .field("watches", &self.by_watch.len())
            .finish_non_exhaustive()
    }
}

impl DirCache {
    pub fn new(max_dirs: usize, max_entries: usize) -> DirCache {
        DirCache {
            map: FxHashMap::default(),
            by_watch: FxHashMap::default(),
            tick: 0,
            max_dirs,
            max_entries,
            total: 0,
        }
    }

    /// Положить каталог. `watch` — наблюдение, держащее свежесть (`None` —
    /// запись сразу устаревшая). Снятые наблюдения возвращаются: их гарды
    /// стоит уронить вне блокировки.
    #[must_use]
    pub fn put(
        &mut self,
        loc: Location,
        entries: Arc<Vec<Entry>>,
        watch: Option<(WatchId, WatchGuard)>,
    ) -> Vec<WatchGuard> {
        let mut dropped = Vec::new();
        if let Some((_, _, old)) = self.take(&loc) {
            dropped.extend(old);
        }
        if entries.len() > self.max_entries || self.max_dirs == 0 {
            dropped.extend(watch.map(|(_, g)| g));
            return dropped;
        }
        self.tick += 1;
        self.total += entries.len();
        if let Some((id, _)) = &watch {
            self.by_watch.insert(*id, loc.clone());
        }
        self.map.insert(
            loc,
            CachedDir {
                entries,
                fresh: watch.is_some(),
                tick: self.tick,
                watch,
            },
        );
        while self.map.len() > self.max_dirs || self.total > self.max_entries {
            let Some(oldest) = self.oldest(|_| true) else {
                break;
            };
            dropped.extend(self.take(&oldest).and_then(|(_, _, g)| g));
        }
        while self.by_watch.len() > MAX_WATCHES {
            let Some(oldest) = self.oldest(|d| d.watch.is_some()) else {
                break;
            };
            dropped.extend(self.stale(&oldest));
        }
        dropped
    }

    /// Забрать каталог (таб снова его показывает): записи и свежесть.
    pub fn take(&mut self, loc: &Location) -> Option<(Arc<Vec<Entry>>, bool, Option<WatchGuard>)> {
        let d = self.map.remove(loc)?;
        self.total -= d.entries.len();
        let guard = d.watch.map(|(id, g)| {
            self.by_watch.remove(&id);
            g
        });
        Some((d.entries, d.fresh, guard))
    }

    /// Событие наблюдения кэша: запись устарела, наблюдение больше не нужно —
    /// его гард возвращается (уронить вне блокировки). Чужой `id` — `None`.
    pub fn invalidate(&mut self, id: WatchId) -> Option<WatchGuard> {
        let loc = self.by_watch.get(&id)?.clone();
        self.stale(&loc)
    }

    /// Каталогов и записей в кэше.
    pub fn stats(&self) -> (usize, usize) {
        (self.map.len(), self.total)
    }

    /// Каталогов под наблюдением кэша.
    pub fn watches(&self) -> usize {
        self.by_watch.len()
    }

    fn stale(&mut self, loc: &Location) -> Option<WatchGuard> {
        let d = self.map.get_mut(loc)?;
        d.fresh = false;
        let (id, g) = d.watch.take()?;
        self.by_watch.remove(&id);
        Some(g)
    }

    fn oldest(&self, pick: impl Fn(&CachedDir) -> bool) -> Option<Location> {
        self.map
            .iter()
            .filter(|(_, d)| pick(d))
            .min_by_key(|(_, d)| d.tick)
            .map(|(l, _)| l.clone())
    }
}

#[cfg(test)]
mod tests {
    use urfin_core::{Attrs, EntryKind};

    use super::*;

    fn dir(n: usize) -> Arc<Vec<Entry>> {
        Arc::new(
            (0..n)
                .map(|i| Entry {
                    ctime: i64::MIN,
                    atime: i64::MIN,
                    name: format!("f{i}").into(),
                    size: 0,
                    mtime: 0,
                    attrs: Attrs(0),
                    kind: EntryKind::File,
                })
                .collect(),
        )
    }

    fn loc(p: &str) -> Location {
        Location::local(p)
    }

    fn guard() -> WatchGuard {
        WatchGuard::new(())
    }

    #[test]
    fn lru_by_dirs_and_entries() {
        let mut c = DirCache::new(3, 100);
        for p in ["/a", "/b", "/c"] {
            drop(c.put(loc(p), dir(10), None));
        }
        assert_eq!(c.stats(), (3, 30));
        // Касание: /a забрали и вернули — теперь самый свежий.
        let (e, ..) = c.take(&loc("/a")).unwrap();
        drop(c.put(loc("/a"), e, None));
        drop(c.put(loc("/d"), dir(10), None));
        assert!(c.take(&loc("/b")).is_none(), "/b is the oldest — evicted");
        assert_eq!(c.stats(), (3, 30));
        // Лимит записей: 95 вытесняет старые.
        drop(c.put(loc("/big"), dir(95), None));
        assert!(c.stats().1 <= 100);
        // Больше лимита — не кэшируется вовсе.
        drop(c.put(loc("/huge"), dir(101), None));
        assert!(c.take(&loc("/huge")).is_none());
    }

    #[test]
    fn freshness_follows_watch() {
        let mut c = DirCache::new(16, 1000);
        drop(c.put(loc("/w"), dir(1), Some((WatchId(1), guard()))));
        drop(c.put(loc("/n"), dir(1), None));
        assert!(c.invalidate(WatchId(99)).is_none());
        // Событие: устарел, наблюдение снято.
        assert!(c.invalidate(WatchId(1)).is_some());
        assert_eq!(c.watches(), 0);
        assert!(!c.take(&loc("/w")).unwrap().1);
        assert!(!c.take(&loc("/n")).unwrap().1, "no watch — stale");
    }

    /// Приёмка: сетевые каталоги без наблюдения (всегда устаревшие).
    #[cfg(windows)]
    #[test]
    fn network_paths_are_not_watched() {
        assert!(!watchable(&Location::local(r"\\server\share\dir")));
        assert!(!watchable(&Location::local(r"\\?\UNC\server\share")));
        assert!(watchable(&Location::local(std::env::temp_dir())));
    }

    #[test]
    fn at_most_eight_watches() {
        let mut c = DirCache::new(16, 1000);
        for i in 0..10u64 {
            drop(c.put(loc(&format!("/d{i}")), dir(1), Some((WatchId(i), guard()))));
        }
        assert_eq!(c.watches(), MAX_WATCHES);
        // Старейшие потеряли наблюдение и свежесть, но остались в кэше.
        assert!(!c.take(&loc("/d0")).unwrap().1);
        assert!(c.take(&loc("/d9")).unwrap().1);
    }
}
