//! `VfsListing` — `core::ListingService` поверх `Vfs` (S4 §2.2).
//!
//! Загрузка: задача на `Lane::Interactive`, `ListStream` → `Batcher` → `Chunk`.
//! Первый чанк отсортирован внутри себя (верх списка сразу правильный), дальше
//! `visible` — в порядке прихода, полный `Order` — на конце (геометрический
//! пересорт по ходу — S4-06), затем `Done`. Ошибка → `Failed`. Отмена (навигация)
//! — тихий выход, после неё ничего не отправляется.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use urfin_core::{
    ArrangeReq, ArrangeSpec, Entry, ListingService, ListingUpdate, LoadReq, Runtime, SortKey,
    TabEvent, TabId, TaskCx, listing_msg,
};

use crate::api::{ListOpts, VfsError, VfsRegistry};
use crate::batch::Batcher;
use crate::cache::DirCache;
use crate::hub::{Hub, HubCfg};
use crate::sort::{ArrangeOpts, arrange, visible};

/// Секция `[vfs]` конфига (S4 §2.2). Поля кэша и watcher'а читают S4-12…16.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct VfsCfg {
    pub cache_dirs: u16,
    pub cache_entries: u32,
    /// Меньше записей — полный refresh вместо патча.
    pub patch_threshold: u32,
    pub debounce_ms: u16,
    pub max_latency_ms: u16,
    pub poll_interval_ms: u32,
    /// Решение владельца №8: на Windows dotfiles видны.
    pub dotfiles_hidden_on_windows: bool,
    /// Решение владельца №10.
    pub natural_sort: bool,
    /// 0 — авто: 4 на SSD, 1 на HDD или сети.
    pub dir_size_threads: u8,
}

impl Default for VfsCfg {
    fn default() -> Self {
        VfsCfg {
            cache_dirs: 16,
            cache_entries: 200_000,
            patch_threshold: 5000,
            debounce_ms: 50,
            max_latency_ms: 250,
            poll_interval_ms: 2000,
            dotfiles_hidden_on_windows: false,
            natural_sort: true,
            dir_size_threads: 0,
        }
    }
}

impl VfsCfg {
    /// Параметры порядка для `arrange`.
    pub fn arrange_opts(&self) -> ArrangeOpts {
        ArrangeOpts {
            natural: self.natural_sort,
            dotfiles_hidden: !cfg!(windows) || self.dotfiles_hidden_on_windows,
        }
    }
}

pub struct VfsListing {
    reg: Arc<VfsRegistry>,
    cfg: VfsCfg,
    /// Поток наблюдения поднимается с первым листингом (нужен рантайм).
    hub: OnceLock<Arc<Hub>>,
    cache: Arc<Mutex<DirCache>>,
}

impl VfsListing {
    pub fn new(reg: Arc<VfsRegistry>, cfg: VfsCfg) -> VfsListing {
        let cache = DirCache::new(cfg.cache_dirs.into(), cfg.cache_entries as usize);
        VfsListing {
            reg,
            cfg,
            hub: OnceLock::new(),
            cache: Arc::new(Mutex::new(cache)),
        }
    }

    fn hub(&self, rt: &Runtime) -> Arc<Hub> {
        let ms = |v: u32| Duration::from_millis(u64::from(v));
        Arc::clone(self.hub.get_or_init(|| {
            Arc::new(Hub::start(
                rt,
                &HubCfg {
                    debounce: ms(self.cfg.debounce_ms.into()),
                    max_latency: ms(self.cfg.max_latency_ms.into()),
                    poll: ms(self.cfg.poll_interval_ms),
                    patch_threshold: self.cfg.patch_threshold,
                    opts: self.cfg.arrange_opts(),
                },
                Arc::clone(&self.cache),
            ))
        }))
    }

    /// Каталогов и записей в кэше (тесты, диагностика).
    #[doc(hidden)]
    pub fn cache_stats(&self) -> (usize, usize) {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats()
    }

    /// Табы, за каталогами которых сейчас наблюдают (тесты, диагностика).
    #[doc(hidden)]
    pub fn watched_tabs(&self) -> Vec<TabId> {
        self.hub.get().map(|h| h.watched()).unwrap_or_default()
    }
}

impl std::fmt::Debug for VfsListing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VfsListing")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl ListingService for VfsListing {
    fn load(&self, req: LoadReq, rt: &Runtime) {
        let reg = Arc::clone(&self.reg);
        let opts = self.cfg.arrange_opts();
        let hub = self.hub(rt);
        let cache = Arc::clone(&self.cache);
        rt.spawn(
            urfin_core::Lane::Interactive,
            "vfs.list",
            req.cancel.clone(),
            move |cx| load(&reg, &req, opts, &cx, &hub, &cache),
        );
    }

    fn tab_event(&self, ev: TabEvent) {
        if let Some(h) = self.hub.get() {
            h.tab_event(ev);
        }
    }

    fn arrange(&self, req: ArrangeReq, rt: &Runtime) {
        let opts = self.cfg.arrange_opts();
        rt.spawn_cpu("vfs.arrange", req.cancel.clone(), move |cx| {
            if cx.cancel.is_cancelled() {
                return;
            }
            let order = arrange(&req.entries, &req.view, opts);
            let u = ListingUpdate::Order {
                order: Arc::new(order),
                for_len: u32::try_from(req.entries.len()).unwrap_or(u32::MAX),
                seq: req.seq,
            };
            let _ = cx
                .sink
                .send(listing_msg(req.tab, req.generation, u), &cx.cancel);
        });
    }
}

/// Поток листинга. Ошибки — `Failed`, отмена — молча.
fn load(
    reg: &VfsRegistry,
    req: &LoadReq,
    opts: ArrangeOpts,
    cx: &TaskCx,
    hub: &Hub,
    cache: &Mutex<DirCache>,
) {
    let t0 = Instant::now();
    // Наблюдение — до чтения: изменения во время листинга придут после `Done`.
    let reread = reg
        .get(&req.loc)
        .is_ok_and(|fs| hub.prepare(fs, req.tab, req.generation, &req.loc));
    let send = |u: ListingUpdate| {
        cx.sink
            .send(listing_msg(req.tab, req.generation, u), &cx.cancel)
            .is_ok()
    };
    // Refresh своего каталога кэш не берёт: он старше показанного листинга.
    let cached = if reread {
        None
    } else {
        cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(&req.loc)
    };
    if let Some((entries, fresh, guard)) = cached {
        drop(guard);
        from_cache(req, opts, cx, hub, &send, entries, fresh, t0);
        return;
    }
    match stream(reg, req, opts, cx, &send, t0) {
        Ok(Some((count, chunks))) => {
            // Один чанк пришёл уже отсортированным; больше — финальный порядок
            // считает core по своим записям (копии здесь нет, ADR-0017).
            let done = if chunks > 1 && req.view.sort.key != SortKey::Unsorted {
                ListingUpdate::DoneUnsorted
            } else {
                ListingUpdate::Done
            };
            if send(done) {
                tracing::debug!(target: "list.done", elapsed = ?t0.elapsed(), entries = count, chunks, loc = %req.loc.path.display());
                hub.activate(req.tab, req.generation, t0.elapsed());
            }
        }
        // Отменено: ничего не отправляем.
        Ok(None) | Err(VfsError::Cancelled) => {}
        Err(e) => {
            if !cx.cancel.is_cancelled() {
                tracing::warn!(error = %e, loc = %req.loc.path.display(), "listing failed");
                send(ListingUpdate::Failed(e.to_string().into()));
            }
        }
    }
}

/// Каталог из кэша (S4-16): одним отсортированным чанком, без чтения. Устаревший —
/// затем `panel.refresh`: перечитывание с подменой листинга целиком, курсор и
/// выделение — по именам (решение №9: «кэш мгновенно + тихое обновление»).
#[allow(clippy::too_many_arguments, reason = "контекст задачи листинга")]
fn from_cache(
    req: &LoadReq,
    opts: ArrangeOpts,
    cx: &TaskCx,
    hub: &Hub,
    send: &impl Fn(ListingUpdate) -> bool,
    entries: Arc<Vec<Entry>>,
    fresh: bool,
    t0: Instant,
) {
    // Обычно `Arc` единственный (таб ушёл) — без копии.
    let entries = Arc::try_unwrap(entries).unwrap_or_else(|a| (*a).clone());
    let visible = arrange(&entries, &req.view, opts);
    let n = entries.len();
    if !send(ListingUpdate::Chunk { entries, visible }) || !send(ListingUpdate::Done) {
        return;
    }
    tracing::debug!(target: "list.cache", fresh, elapsed = ?t0.elapsed(), entries = n, loc = %req.loc.path.display());
    hub.activate(req.tab, req.generation, Duration::ZERO);
    if !fresh {
        let (tab, generation) = (req.tab, req.generation);
        let m = urfin_core::msg("vfs.cache_stale", move |cx| {
            if cx
                .state
                .tab(tab)
                .is_some_and(|(_, t)| t.listing.generation == generation)
            {
                cx.dispatch(urfin_core::Command::new("panel.refresh").arg("tab", i64::from(tab.0)));
            }
        });
        let _ = cx.sink.send(m, &cx.cancel);
    }
}

/// Чтение и чанки: первый — отсортированный, остальные — в порядке прихода
/// (записи уходят в core без копии здесь, ADR-0017). `Ok(Some((записей,
/// чанков)))`; `Ok(None)` — отменено или отправка не прошла (отмена).
fn stream(
    reg: &VfsRegistry,
    req: &LoadReq,
    opts: ArrangeOpts,
    cx: &TaskCx,
    send: &impl Fn(ListingUpdate) -> bool,
    t0: Instant,
) -> Result<Option<(usize, usize)>, VfsError> {
    let fs = reg.get(&req.loc)?;
    let mut s = fs.list(&req.loc, &ListOpts::default())?;
    let (mut count, mut chunks) = (0usize, 0usize);
    let mut pending: Vec<Entry> = Vec::new();
    let mut batcher = Batcher::default();
    loop {
        let more = s.next_batch(&mut pending, &cx.cancel)?;
        if cx.cancel.is_cancelled() {
            return Ok(None);
        }
        if !batcher.should_flush(pending.len(), !more) {
            if more {
                continue;
            }
            break;
        }
        let first = batcher.is_first();
        let visible = chunk_visible(&pending, count, &req.view, opts, first);
        count += pending.len();
        chunks += 1;
        if !send(ListingUpdate::Chunk {
            entries: std::mem::take(&mut pending),
            visible,
        }) {
            return Ok(None);
        }
        if first {
            tracing::debug!(target: "list.first_chunk", elapsed = ?t0.elapsed(), entries = count);
        }
        batcher.flushed();
        if !more {
            break;
        }
    }
    Ok(Some((count, chunks)))
}

/// `visible` чанка: абсолютные индексы прошедших hidden и фильтр. Первый чанк —
/// отсортирован (его `base` = 0), остальные — в порядке прихода.
fn chunk_visible(
    chunk: &[Entry],
    base: usize,
    view: &ArrangeSpec,
    opts: ArrangeOpts,
    first: bool,
) -> Vec<u32> {
    if first && base == 0 {
        return arrange(chunk, view, opts);
    }
    let base = u32::try_from(base).unwrap_or(u32::MAX);
    (0u32..)
        .zip(chunk)
        .filter(|(_, e)| visible(e, view, opts))
        .map(|(i, _)| base + i)
        .collect()
}
