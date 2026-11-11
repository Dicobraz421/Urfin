//! `ArcCache` — индексы открытых архивов (S8 §3.2): LRU по [`OuterKey`], лимит по
//! памяти (по умолчанию 256 мебибайт). Один на процесс: общий у backend'ов всех схем архивов в обоих
//! реестрах VFS (листинг вкладок и handler'ы) и у handler `arc`.
//!
//! Индекс строит первый, кому он нужен; остальные ждут его, а не читают архив второй раз.
//! Сводки для `urfin_core::arc::ArcModel` копятся событиями: их забирает handler `arc` в
//! UI-потоке.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use urfin_core::arc::ArcSummary;
use urfin_core::{CancelToken, Location};
use urfin_vfs::{Vfs, VfsError, VfsRegistry};
use zeroize::Zeroizing;

use crate::config::ArcCfg;
use crate::index::{ArchiveIndex, OuterKey};
use crate::password::{Ask, DECLINE_FOR};
use crate::spill;

/// Лимит по умолчанию (`[arc] cache_mib`).
pub const DEFAULT_LIMIT: u64 = 256 << 20;

/// Как часто ожидающий индекс проверяет отмену.
const WAIT_STEP: Duration = Duration::from_millis(20);

/// Изменение сводок для `ArcModel`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArcEvent {
    /// Сводка архива (адрес файла архива): новая или выросла.
    Set(Location, ArcSummary),
    /// Индекс вышел из кэша.
    Remove(Location),
}

/// Чем кончилось построение.
#[derive(Default)]
enum Outcome {
    #[default]
    Building,
    Ready(Arc<ArchiveIndex>),
    /// Строитель сдался: строить самим.
    Abandoned,
}

/// Строящийся индекс: ждущие получат его или `None` (строитель сдался — строить самим).
#[derive(Default)]
struct Pending {
    done: Mutex<Outcome>,
    cv: Condvar,
}

impl Pending {
    fn settle(&self, v: Option<Arc<ArchiveIndex>>) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) =
            v.map_or(Outcome::Abandoned, Outcome::Ready);
        self.cv.notify_all();
    }

    fn wait(&self, cancel: &CancelToken) -> Result<Option<Arc<ArchiveIndex>>, VfsError> {
        let mut g = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match &*g {
                Outcome::Ready(idx) => return Ok(Some(Arc::clone(idx))),
                Outcome::Abandoned => return Ok(None),
                Outcome::Building => {}
            }
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            g = self
                .cv
                .wait_timeout(g, WAIT_STEP)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

enum Slot {
    Building(Arc<Pending>),
    Ready { idx: Arc<ArchiveIndex>, used: u64 },
}

#[derive(Default)]
struct Inner {
    slots: HashMap<OuterKey, Slot>,
    /// Последний ключ файла архива: по нему — колонки и ячейки из UI-потока, без `stat`.
    latest: HashMap<Location, OuterKey>,
    bytes: u64,
    tick: u64,
    events: Vec<ArcEvent>,
    /// Пароли архивов до индекса ([`ArcCache::set_password`]).
    passwords: HashMap<Location, Zeroizing<String>>,
    /// Кто спрашивает пароль ([`ArcCache::set_asker`]).
    asker: Option<Arc<dyn Ask>>,
    /// Отказы от пароля и спилла: архив и когда к нему обращались в последний раз.
    declined: HashMap<Location, Instant>,
    /// Реестр VFS для архивов не на диске ([`ArcCache::set_vfs`]).
    vfs: Option<Weak<VfsRegistry>>,
    /// Каталог спилла ([`ArcCache::set_spill_dir`]) и подметён ли он в этом запуске.
    spill_dir: Option<PathBuf>,
    swept: bool,
    /// `[arc] spill_max`, `ratio_max`.
    spill_max: u64,
    ratio_max: u32,
}

/// Кэш индексов.
pub struct ArcCache {
    inner: Mutex<Inner>,
    limit: u64,
    /// Вопросы пароля — по одному: второй ждёт первого и видит его итог.
    asking: Mutex<()>,
}

impl std::fmt::Debug for ArcCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.lock();
        f.debug_struct("ArcCache")
            .field("archives", &g.slots.len())
            .field("bytes", &g.bytes)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

/// Итог [`ArcCache::claim`].
pub(crate) enum Claim {
    Ready(Arc<ArchiveIndex>),
    /// Строить вызывающему; без [`Ticket::finish`] ждущие узнают, что строить им.
    Build(Ticket),
}

/// Право построить индекс ключа.
pub(crate) struct Ticket {
    cache: Arc<ArcCache>,
    key: OuterKey,
    pending: Arc<Pending>,
    done: bool,
}

impl Ticket {
    pub(crate) fn key(&self) -> &OuterKey {
        &self.key
    }

    /// Промежуточная сводка (индекс ещё строится).
    pub(crate) fn progress(&self, s: ArcSummary) {
        self.cache
            .push_event(ArcEvent::Set(self.key.loc.clone(), s));
    }

    /// Индекс готов: в кэш, ждущим и в сводку.
    pub(crate) fn finish(mut self, idx: ArchiveIndex) -> Arc<ArchiveIndex> {
        self.done = true;
        let idx = Arc::new(idx);
        self.cache.insert(&self.key, Arc::clone(&idx));
        self.pending.settle(Some(Arc::clone(&idx)));
        idx
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut g = self.cache.lock();
        if matches!(g.slots.get(&self.key), Some(Slot::Building(p)) if Arc::ptr_eq(p, &self.pending))
        {
            g.slots.remove(&self.key);
        }
        drop(g);
        self.pending.settle(None);
    }
}

impl ArcCache {
    /// Кэш с лимитом `limit` байт.
    pub fn new(limit: u64) -> Arc<ArcCache> {
        let cfg = ArcCfg::default();
        Arc::new(ArcCache {
            inner: Mutex::new(Inner {
                spill_max: cfg.spill_max,
                ratio_max: cfg.ratio_max,
                ..Inner::default()
            }),
            limit,
            asking: Mutex::new(()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push_event(&self, e: ArcEvent) {
        self.lock().events.push(e);
    }

    /// Готовый индекс ключа, иначе право строить; строит другой — ждём его.
    pub(crate) fn claim(
        self: &Arc<Self>,
        key: &OuterKey,
        cancel: &CancelToken,
    ) -> Result<Claim, VfsError> {
        loop {
            let pending = {
                let mut g = self.lock();
                g.tick += 1;
                let tick = g.tick;
                match g.slots.get_mut(key) {
                    Some(Slot::Ready { idx, used }) => {
                        *used = tick;
                        return Ok(Claim::Ready(Arc::clone(idx)));
                    }
                    Some(Slot::Building(p)) => Arc::clone(p),
                    None => {
                        let pending = Arc::new(Pending::default());
                        g.slots
                            .insert(key.clone(), Slot::Building(Arc::clone(&pending)));
                        return Ok(Claim::Build(Ticket {
                            cache: Arc::clone(self),
                            key: key.clone(),
                            pending,
                            done: false,
                        }));
                    }
                }
            };
            if let Some(idx) = pending.wait(cancel)? {
                return Ok(Claim::Ready(idx));
            }
        }
    }

    fn insert(&self, key: &OuterKey, idx: Arc<ArchiveIndex>) {
        let mut g = self.lock();
        g.tick += 1;
        let tick = g.tick;
        // Файл изменился: прежний индекс того же файла больше не нужен.
        if let Some(old) = g.latest.insert(key.loc.clone(), key.clone())
            && old != *key
            && let Some(Slot::Ready { idx, .. }) = g.slots.remove(&old)
        {
            g.bytes -= idx.approx_bytes();
        }
        g.bytes += idx.approx_bytes();
        g.events.push(ArcEvent::Set(key.loc.clone(), idx.summary()));
        g.slots.insert(key.clone(), Slot::Ready { idx, used: tick });
        self.evict(&mut g, key);
    }

    /// Вытеснение самых давних, пока не уложимся в лимит; `keep` остаётся всегда.
    fn evict(&self, g: &mut Inner, keep: &OuterKey) {
        while g.bytes > self.limit {
            let victim = g
                .slots
                .iter()
                .filter(|(k, _)| *k != keep)
                .filter_map(|(k, s)| match s {
                    Slot::Ready { used, .. } => Some((*used, k.clone())),
                    Slot::Building(_) => None,
                })
                .min_by_key(|(used, _)| *used)
                .map(|(_, k)| k);
            let Some(k) = victim else {
                return;
            };
            if let Some(Slot::Ready { idx, .. }) = g.slots.remove(&k) {
                g.bytes -= idx.approx_bytes();
            }
            if g.latest.get(&k.loc) == Some(&k) {
                g.latest.remove(&k.loc);
                g.events.push(ArcEvent::Remove(k.loc.clone()));
            }
        }
    }

    /// Последний готовый индекс файла архива `outer` — без `stat` и чтения (UI-поток:
    /// колонки, ячейки, `caps`).
    pub fn latest(&self, outer: &Location) -> Option<Arc<ArchiveIndex>> {
        let g = self.lock();
        let key = g.latest.get(outer)?;
        match g.slots.get(key) {
            Some(Slot::Ready { idx, .. }) => Some(Arc::clone(idx)),
            _ => None,
        }
    }

    /// Готовый индекс ключа без ожидания и построения.
    pub fn get(&self, key: &OuterKey) -> Option<Arc<ArchiveIndex>> {
        match self.lock().slots.get(key) {
            Some(Slot::Ready { idx, .. }) => Some(Arc::clone(idx)),
            _ => None,
        }
    }

    /// Пароль архива `outer` до его индекса: 7z с зашифрованными именами без него не
    /// листается (`arc-error-no-password`; вопрос — S8-35). Его берёт следующий разбор
    /// оглавления и кладёт в [`ArchiveIndex::ctx`]; не подошёл — разбор падает той же
    /// ошибкой, пароль забыт. `None` — забыть.
    pub fn set_password(&self, outer: &Location, pw: Option<&str>) {
        let mut g = self.lock();
        match pw {
            Some(pw) => {
                g.passwords
                    .insert(outer.clone(), Zeroizing::new(pw.to_owned()));
            }
            None => {
                g.passwords.remove(outer);
            }
        }
    }

    /// Кто спрашивает пароль, когда он нужен потоку VFS (S8-35): handler `arc` ставит
    /// вопрос в UI. Без него пароль — только [`ArcCache::set_password`] и `ctx()`.
    pub fn set_asker(&self, ask: Option<Arc<dyn Ask>>) {
        self.lock().asker = ask;
    }

    pub(crate) fn asker(&self) -> Option<Arc<dyn Ask>> {
        self.lock().asker.clone()
    }

    /// Очередь вопросов пароля.
    pub(crate) fn ask_turn(&self) -> std::sync::MutexGuard<'_, ()> {
        self.asking.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// От пароля `outer` недавно отказались: не спрашивать; обращение продлевает отказ.
    pub(crate) fn declined(&self, outer: &Location) -> bool {
        let mut g = self.lock();
        let now = Instant::now();
        g.declined
            .retain(|_, t| now.duration_since(*t) < DECLINE_FOR);
        match g.declined.get_mut(outer) {
            Some(t) => {
                *t = now;
                true
            }
            None => false,
        }
    }

    /// Отказ от пароля или спилла `outer` (Esc или попытки кончились).
    pub(crate) fn decline(&self, outer: &Location) {
        self.lock().declined.insert(outer.clone(), Instant::now());
    }

    /// Пароль для разбора оглавления `outer` — один раз.
    pub(crate) fn take_password(&self, outer: &Location) -> Option<Zeroizing<String>> {
        self.lock().passwords.remove(outer)
    }

    /// Изменения сводок с прошлого вызова (handler `arc`, UI-поток).
    pub fn take_events(&self) -> Vec<ArcEvent> {
        std::mem::take(&mut self.lock().events)
    }

    /// Реестр VFS приложения (S8-31): через него читаются вложенные архивы и архивы на
    /// сервере. Держится слабой ссылкой (реестр держит backend'ы с этим кэшем); без него
    /// архив — только файл на диске.
    pub fn set_vfs(&self, reg: &Arc<VfsRegistry>) {
        self.lock().vfs = Some(Arc::downgrade(reg));
    }

    /// Backend адреса `loc` из реестра [`ArcCache::set_vfs`].
    pub(crate) fn vfs_of(&self, loc: &Location) -> Option<Arc<dyn Vfs>> {
        let reg = self.lock().vfs.as_ref()?.upgrade()?;
        reg.get(loc).ok().cloned()
    }

    /// Каталог спилла (`<cache_dir>\arc`, S8 §3.4); остатки прошлых запусков старше суток
    /// удаляются в фоне. Без вызова — `%TEMP%\urfin-arc`.
    pub fn set_spill_dir(&self, dir: PathBuf) {
        {
            let mut g = self.lock();
            g.spill_dir = Some(dir.clone());
            g.swept = true;
        }
        let swept = std::thread::Builder::new()
            .name("arc-spill-sweep".into())
            .spawn(move || spill::sweep(&dir, spill::STALE));
        if let Err(e) = swept {
            tracing::warn!(target: "arc", error = %e, "spill sweep not started");
        }
    }

    /// Каталог спилла; первый вызов в запуске без [`ArcCache::set_spill_dir`] подметает
    /// его здесь (поток VFS).
    pub(crate) fn spill_dir(&self) -> PathBuf {
        let (dir, sweep) = {
            let mut g = self.lock();
            let dir = g
                .spill_dir
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("urfin-arc"));
            (dir, !std::mem::replace(&mut g.swept, true))
        };
        if sweep {
            spill::sweep(&dir, spill::STALE);
        }
        dir
    }

    /// Лимиты спилла: больше `max` байт или сжатие записи больше `ratio:1` — вопрос.
    pub fn set_spill_limits(&self, max: u64, ratio: u32) {
        let mut g = self.lock();
        g.spill_max = max;
        g.ratio_max = ratio.max(1);
    }

    pub(crate) fn spill_limits(&self) -> (u64, u32) {
        let g = self.lock();
        (g.spill_max, g.ratio_max)
    }

    /// Индексов в кэше и их оценка памяти, байты.
    pub fn usage(&self) -> (usize, u64) {
        let g = self.lock();
        let n = g
            .slots
            .values()
            .filter(|s| matches!(s, Slot::Ready { .. }))
            .count();
        (n, g.bytes)
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;
    use crate::index::{EntryLoc, IndexBuilder, RawEntry};

    fn key(name: &str, mtime: i64) -> OuterKey {
        OuterKey {
            loc: Location::local(format!(r"C:\{name}")),
            size: 10,
            mtime,
        }
    }

    fn index(key: &OuterKey, files: usize) -> ArchiveIndex {
        let mut b = IndexBuilder::new();
        let names: Vec<String> = (0..files).map(|i| format!("f{i}")).collect();
        for n in &names {
            b.push(RawEntry::file(n, 1, 1, EntryLoc::Index(0)));
        }
        b.finish("zip", key.clone())
    }

    fn build(c: &Arc<ArcCache>, k: &OuterKey, files: usize) -> Arc<ArchiveIndex> {
        match c.claim(k, &CancelToken::new()).unwrap() {
            Claim::Build(t) => t.finish(index(k, files)),
            Claim::Ready(i) => i,
        }
    }

    #[test]
    #[allow(clippy::many_single_char_names, reason = "короткий тест")]
    fn second_claim_hits_and_events_report_summary() {
        let c = ArcCache::new(DEFAULT_LIMIT);
        let k = key("a.zip", 1);
        let i = build(&c, &k, 3);
        assert!(
            matches!(c.claim(&k, &CancelToken::new()).unwrap(), Claim::Ready(j) if Arc::ptr_eq(&i, &j))
        );
        assert!(c.latest(&k.loc).is_some());
        let ev = c.take_events();
        assert!(
            matches!(&ev[..], [ArcEvent::Set(l, s)] if *l == k.loc && s.files == 3 && s.complete)
        );
        assert_eq!(c.take_events().len(), 0);
    }

    #[test]
    fn changed_file_replaces_old_key() {
        let c = ArcCache::new(DEFAULT_LIMIT);
        let old = key("a.zip", 1);
        build(&c, &old, 3);
        let new = key("a.zip", 2);
        let i = build(&c, &new, 5);
        assert!(c.get(&old).is_none());
        assert_eq!(c.usage().0, 1);
        assert!(Arc::ptr_eq(&c.latest(&new.loc).unwrap(), &i));
    }

    #[test]
    fn lru_evicts_oldest_and_reports_removal() {
        let one = index(&key("x", 0), 1000).approx_bytes();
        let c = ArcCache::new(one * 2 + one / 2);
        let (a, b, d) = (key("a.zip", 1), key("b.zip", 1), key("d.zip", 1));
        build(&c, &a, 1000);
        build(&c, &b, 1000);
        // `a` свежее `b`: вытеснен будет `b`.
        build(&c, &a, 1000);
        c.take_events();
        build(&c, &d, 1000);
        assert!(c.get(&a).is_some());
        assert!(c.get(&b).is_none());
        assert!(c.latest(&b.loc).is_none());
        let ev = c.take_events();
        assert!(ev.contains(&ArcEvent::Remove(b.loc.clone())), "{ev:?}");
        assert!(c.usage().1 <= c.limit());
    }

    #[test]
    fn waiters_get_the_builders_index_or_build_after_abort() {
        let c = ArcCache::new(DEFAULT_LIMIT);
        let k = key("big.zip", 1);
        let Claim::Build(t) = c.claim(&k, &CancelToken::new()).unwrap() else {
            panic!("первый строит");
        };
        let waiter = {
            let (c, k) = (Arc::clone(&c), k.clone());
            thread::spawn(move || match c.claim(&k, &CancelToken::new()).unwrap() {
                Claim::Ready(i) => i.len(),
                Claim::Build(_) => usize::MAX,
            })
        };
        thread::sleep(Duration::from_millis(30));
        t.finish(index(&k, 7));
        assert_eq!(waiter.join().unwrap(), 8);

        let k2 = key("broken.zip", 1);
        let Claim::Build(t) = c.claim(&k2, &CancelToken::new()).unwrap() else {
            panic!("первый строит");
        };
        let waiter = {
            let (c, k2) = (Arc::clone(&c), k2.clone());
            thread::spawn(move || {
                matches!(c.claim(&k2, &CancelToken::new()).unwrap(), Claim::Build(_))
            })
        };
        thread::sleep(Duration::from_millis(30));
        drop(t);
        assert!(waiter.join().unwrap(), "строитель сдался — строит ждущий");
    }

    #[test]
    fn waiting_is_cancellable() {
        let c = ArcCache::new(DEFAULT_LIMIT);
        let k = key("slow.zip", 1);
        let _t = c.claim(&k, &CancelToken::new()).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(c.claim(&k, &cancel), Err(VfsError::Cancelled)));
    }
}
