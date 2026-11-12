//! `WatchHub` (S4 §5.3, S4-14): события backend'ов → debounce → `panel.refresh`.
//!
//! [`Debouncer`] — чистая логика с внешними часами (тесты задают время сами):
//! коалесцирование по имени, flush в `min(t_last + debounce, t_first + max_latency)`,
//! `Rescan` при > 4096 имён или `Overflow`, шторм-режим. [`Hub`] — поток
//! `watch-hub`, подписки табов и опрос для ФС без уведомлений.
//!
//! Поток — обычный `std::thread`, не `Runtime::spawn_dedicated` (S0 v2 №7): без
//! дел он спит без таймаута (0% CPU) и выходит, когда уничтожен `VfsListing`
//! (канал управления закрыт). Под `spawn_dedicated` shutdown ждал бы его
//! пробуждения — отмена рантайма потоки не будит, выход из приложения тормозил бы.
//!
//! Жизненный цикл: задача листинга до чтения каталога вызывает [`Hub::prepare`]
//! (наблюдение того же каталога переиспользуется — debounce и шторм не
//! сбрасываются на каждом refresh), после `Done` — [`Hub::activate`]. До
//! активации события копятся: изменения во время листинга не теряются, а
//! приходят одним flush'ем после `Done`. `Hidden` снимает наблюдение и помечает
//! таб, `Shown` такого таба — `panel.refresh`, `Closed` — снимает.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use crossbeam_channel::{Receiver, Sender, select};
use rustc_hash::{FxHashMap, FxHashSet};
use urfin_core::{
    ArrangeSpec, Attrs, CancelToken, Command, Gen, Lane, ListingStatus, ListingUpdate, Location,
    MsgSink, Runtime, TabEvent, TabId, listing_msg, msg,
};

use crate::api::{Entry, Vfs, VfsError, WatchEvent, WatchGuard, WatchId, WatchTx};
use crate::cache::DirCache;
use crate::patch::{Changes, DirIndex, build_patch};
use crate::poll::Poller;
use crate::sort::ArrangeOpts;

/// Выход debounce (S4 §5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirChange {
    Names {
        added: Vec<Entry>,
        changed: Vec<Entry>,
        removed: Vec<CompactString>,
        /// Имена без `Entry` (backend без метаданных) — патч разрешит их `stat_entry`.
        unresolved: Vec<CompactString>,
    },
    Rescan,
    Gone,
}

/// Больше имён в окне — `Rescan`.
pub const MAX_NAMES: usize = 4096;
/// Столько flush'ей подряд по `max_latency` — шторм.
const STORM_FLUSHES: u8 = 3;
/// «Тяжёлый» flush — от стольких имён (или `Rescan`); три тяжёлых за
/// `STORM_PERIOD` — шторм, даже если поток событий рвётся (пишущий тормозит) и
/// flush'и идут по тишине. Работа по одному файлу тяжёлых flush'ей не даёт.
const STORM_NAMES: usize = 32;
/// В шторм — только `Rescan`, не чаще.
const STORM_PERIOD: Duration = Duration::from_secs(1);
/// Столько имён за `STORM_PERIOD` суммарно по flush'ам — шторм: пишущий медленный
/// (~200 файлов/с на занятой машине) даёт мелкие порции, ни одна не «тяжёлая», а
/// перечитывать по 3–4 раза в секунду всё равно незачем.
const STORM_RATE: usize = 64;
/// Тишина, после которой шторм кончается.
const STORM_QUIET: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
enum Pending {
    Added(Option<Entry>),
    Removed,
    Modified(Option<Entry>),
}

#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые флаги окна: rescan, gone, шторм, удержание"
)]
struct WState {
    names: FxHashMap<CompactString, Pending>,
    rescan: bool,
    gone: bool,
    t_first: Option<Instant>,
    t_last: Option<Instant>,
    /// Счёт flush'ей: по `max_latency` +1, по тишине −1; ≥ 3 — шторм.
    capped: u8,
    /// Время двух предыдущих тяжёлых flush'ей.
    heavy: [Option<Instant>; 2],
    /// Окно темпа: начало и имён в нём ([`STORM_RATE`]).
    rate: (Option<Instant>, usize),
    storm: bool,
    last_flush: Option<Instant>,
    /// Не сбрасывать (листинг ещё идёт).
    held: bool,
}

impl WState {
    fn has_pending(&self) -> bool {
        self.t_first.is_some()
    }

    fn deadline(&self, deb: Duration, max: Duration) -> Option<Instant> {
        if self.held {
            return None;
        }
        let (first, last) = (self.t_first?, self.t_last?);
        let normal = (last + deb).min(first + max);
        Some(match (self.storm, self.last_flush) {
            (true, Some(f)) => normal.max(f + STORM_PERIOD),
            _ => normal,
        })
    }

    fn merge(&mut self, name: CompactString, p: Pending) {
        if self.rescan || self.gone {
            return;
        }
        let next = match (self.names.remove(&name), p) {
            // Появился и исчез в одном окне — ничего.
            (Some(Pending::Added(_)), Pending::Removed) => return,
            // Исчез и появился — изменился.
            (Some(Pending::Removed), Pending::Added(e)) => Pending::Modified(e),
            // Новый и сразу изменён — всё ещё новый (свежее `Entry`, если есть).
            (Some(Pending::Added(old)), Pending::Modified(e)) => Pending::Added(e.or(old)),
            (Some(Pending::Modified(old)), Pending::Modified(e)) => Pending::Modified(e.or(old)),
            (_, p) => p,
        };
        self.names.insert(name, next);
        if self.names.len() > MAX_NAMES {
            self.rescan = true;
            self.names = FxHashMap::default();
        }
    }
}

/// Debounce всех наблюдений; время — параметром.
#[derive(Debug)]
pub struct Debouncer {
    debounce: Duration,
    max_latency: Duration,
    w: FxHashMap<WatchId, WState>,
}

impl Debouncer {
    pub fn new(debounce: Duration, max_latency: Duration) -> Debouncer {
        Debouncer {
            debounce,
            max_latency,
            w: FxHashMap::default(),
        }
    }

    pub fn push(&mut self, id: WatchId, ev: WatchEvent, now: Instant) {
        let s = self.w.entry(id).or_default();
        if s.storm && !s.has_pending() && s.last_flush.is_some_and(|f| now >= f + STORM_QUIET) {
            s.storm = false;
            s.capped = 0;
        }
        s.t_first.get_or_insert(now);
        s.t_last = Some(now);
        match ev {
            WatchEvent::Added(n, e) => s.merge(n, Pending::Added(e)),
            WatchEvent::Removed(n) => s.merge(n, Pending::Removed),
            WatchEvent::Modified(n, e) => s.merge(n, Pending::Modified(e)),
            WatchEvent::Renamed { from, to, entry } => {
                s.merge(from, Pending::Removed);
                s.merge(to, Pending::Added(entry));
            }
            WatchEvent::Overflow => {
                s.rescan = true;
                s.names = FxHashMap::default();
            }
            WatchEvent::Gone => {
                s.gone = true;
                s.names = FxHashMap::default();
            }
        }
    }

    /// Есть ли несброшенные изменения.
    pub fn has_pending(&self, id: WatchId) -> bool {
        self.w.get(&id).is_some_and(WState::has_pending)
    }

    /// Копить события наблюдения, не сбрасывая (`true`), или сбрасывать как обычно.
    pub fn hold(&mut self, id: WatchId, held: bool) {
        self.w.entry(id).or_default().held = held;
    }

    pub fn remove(&mut self, id: WatchId) {
        self.w.remove(&id);
    }

    /// Ближайший срок flush'а.
    pub fn deadline(&self) -> Option<Instant> {
        self.w
            .values()
            .filter_map(|s| s.deadline(self.debounce, self.max_latency))
            .min()
    }

    /// Изменения, срок которых настал.
    pub fn take_due(&mut self, now: Instant) -> Vec<(WatchId, DirChange)> {
        let (deb, max) = (self.debounce, self.max_latency);
        let mut out = Vec::new();
        for (&id, s) in &mut self.w {
            let Some(due) = s.deadline(deb, max) else {
                continue;
            };
            if due > now {
                continue;
            }
            let (first, last) = (s.t_first.unwrap_or(now), s.t_last.unwrap_or(now));
            let by_cap = now >= first + max && now < last + deb;
            // Одиночный flush по тишине (пишущий на миг замер) не сбрасывает счёт —
            // он убывает; конец шторма — только 2 с тишины.
            s.capped = if by_cap {
                s.capped.saturating_add(1)
            } else {
                s.capped.saturating_sub(1)
            };
            if s.rescan || s.names.len() >= STORM_NAMES {
                if s.heavy[0].is_some_and(|t| now < t + STORM_PERIOD) {
                    s.storm = true;
                }
                s.heavy = [s.heavy[1], Some(now)];
            }
            if s.capped >= STORM_FLUSHES {
                s.storm = true;
            }
            if s.rate.0.is_none_or(|t| now >= t + STORM_PERIOD) {
                s.rate = (Some(now), 0);
            }
            s.rate.1 += if s.rescan { STORM_NAMES } else { s.names.len() };
            if s.rate.1 >= STORM_RATE {
                s.storm = true;
            }
            let change = if s.gone {
                Some(DirChange::Gone)
            } else if s.rescan || s.storm {
                Some(DirChange::Rescan)
            } else {
                names_change(std::mem::take(&mut s.names))
            };
            s.names = FxHashMap::default();
            s.rescan = false;
            s.gone = false;
            s.t_first = None;
            s.t_last = None;
            s.last_flush = Some(now);
            if let Some(c) = change {
                out.push((id, c));
            }
        }
        out
    }
}

fn names_change(names: FxHashMap<CompactString, Pending>) -> Option<DirChange> {
    if names.is_empty() {
        return None;
    }
    let (mut added, mut changed, mut removed, mut unresolved) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (name, p) in names {
        match p {
            Pending::Added(Some(e)) => added.push(e),
            Pending::Modified(Some(e)) => changed.push(e),
            Pending::Added(None) | Pending::Modified(None) => unresolved.push(name),
            Pending::Removed => removed.push(name),
        }
    }
    Some(DirChange::Names {
        added,
        changed,
        removed,
        unresolved,
    })
}

enum Ctrl {
    /// Новое наблюдение таба; `guard: None` — ФС без уведомлений, после `activate`
    /// включится опрос.
    Subscribe {
        tab: TabId,
        generation: Gen,
        loc: Location,
        id: WatchId,
        guard: Option<WatchGuard>,
        vfs: Arc<dyn Vfs>,
    },
    /// Тот же каталог перечитывается: копить до `activate`.
    Reload {
        tab: TabId,
        generation: Gen,
    },
    /// Наблюдения нет (каталог не открылся).
    Unsubscribe {
        tab: TabId,
    },
    Activate {
        tab: TabId,
        generation: Gen,
        list_time: Duration,
    },
    Tab(TabEvent),
}

struct Sub {
    generation: Gen,
    loc: Location,
    id: WatchId,
    guard: Option<WatchGuard>,
    vfs: Arc<dyn Vfs>,
    poller: Option<Poller>,
    active: bool,
}

/// Подписки, видимые задачам листинга: таб → (каталог, наблюдение).
type Current = Arc<Mutex<FxHashMap<TabId, (Location, WatchId)>>>;

/// Ручка хаба для `VfsListing`.
pub(crate) struct Hub {
    ctrl: Sender<Ctrl>,
    events: Sender<(WatchId, WatchEvent)>,
    current: Current,
    next_id: AtomicU64,
}

impl Hub {
    pub(crate) fn start(rt: &Runtime, cfg: &HubCfg, cache: Arc<Mutex<DirCache>>) -> Hub {
        let (debounce, max_latency, poll) = (cfg.debounce, cfg.max_latency, cfg.poll);
        let (ctrl, ctrl_rx) = crossbeam_channel::unbounded();
        let (events, events_rx) = crossbeam_channel::unbounded();
        // Свой канал: копия отправителя `ctrl` в потоке не дала бы ему закрыться.
        let (snap_tx, snap_rx) = crossbeam_channel::unbounded();
        let current = Current::default();
        let mut worker = Worker {
            deb: Debouncer::new(debounce, max_latency),
            poll,
            subs: FxHashMap::default(),
            by_id: FxHashMap::default(),
            hidden: FxHashSet::default(),
            dirty: FxHashSet::default(),
            current: Arc::clone(&current),
            events: events.clone(),
            cache,
            next_cache_id: CACHE_IDS,
            left: FxHashMap::default(),
            snap_tx,
            patch: Arc::new(PatchCx {
                threshold: cfg.patch_threshold,
                opts: cfg.opts,
                indexes: Mutex::default(),
            }),
        };
        let (sink, cancel) = (rt.sink(), rt.token());
        let spawned = std::thread::Builder::new()
            .name("watch-hub".into())
            .spawn(move || {
                let run = std::panic::AssertUnwindSafe(|| {
                    worker.run(&ctrl_rx, &events_rx, &snap_rx, &sink, &cancel);
                });
                if std::panic::catch_unwind(run).is_err() {
                    tracing::error!("watch hub panicked; directory watching stopped");
                }
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start watch hub");
        }
        Hub {
            ctrl,
            events,
            current,
            next_id: AtomicU64::new(1),
        }
    }

    /// До чтения каталога (поток задачи листинга: открытие наблюдения — I/O).
    /// `true` — таб перечитывает свой же каталог (refresh): кэш тогда не нужен.
    pub(crate) fn prepare(
        &self,
        vfs: &Arc<dyn Vfs>,
        tab: TabId,
        generation: Gen,
        loc: &Location,
    ) -> bool {
        let same = self
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&tab)
            .is_some_and(|(l, _)| l == loc);
        if same {
            let _ = self.ctrl.send(Ctrl::Reload { tab, generation });
            return true;
        }
        let id = WatchId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let tx = WatchTx {
            id,
            tx: self.events.clone(),
        };
        let guard = match vfs.watch(loc, tx) {
            Ok(g) => Some(g),
            Err(VfsError::Unsupported(_)) => None,
            Err(e) => {
                tracing::warn!(error = %e, loc = %loc.path.display(), "watch failed");
                let _ = self.ctrl.send(Ctrl::Unsubscribe { tab });
                return false;
            }
        };
        let _ = self.ctrl.send(Ctrl::Subscribe {
            tab,
            generation,
            loc: loc.clone(),
            id,
            guard,
            vfs: Arc::clone(vfs),
        });
        false
    }

    /// После `Done`: сбрасывать накопленное.
    pub(crate) fn activate(&self, tab: TabId, generation: Gen, list_time: Duration) {
        let _ = self.ctrl.send(Ctrl::Activate {
            tab,
            generation,
            list_time,
        });
    }

    pub(crate) fn tab_event(&self, ev: TabEvent) {
        let _ = self.ctrl.send(Ctrl::Tab(ev));
    }

    /// Табы с наблюдением (тесты, диагностика).
    pub(crate) fn watched(&self) -> Vec<TabId> {
        let mut v: Vec<TabId> = self
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .copied()
            .collect();
        v.sort_by_key(|t| t.0);
        v
    }
}

enum Got {
    Event(Option<(WatchId, WatchEvent)>),
    Ctrl(Option<Ctrl>),
    Snap(Option<Snapshot>),
    Timeout,
}

/// Записи таба из core — снимок для опроса (у сервиса копии нет, ADR-0017).
struct Snapshot {
    tab: TabId,
    generation: Gen,
    entries: Arc<Vec<Entry>>,
}

struct Worker {
    deb: Debouncer,
    poll: Duration,
    subs: FxHashMap<TabId, Sub>,
    by_id: FxHashMap<WatchId, TabId>,
    hidden: FxHashSet<TabId>,
    dirty: FxHashSet<TabId>,
    current: Current,
    /// Для наблюдений кэша (их события идут в тот же канал).
    events: Sender<(WatchId, WatchEvent)>,
    cache: Arc<Mutex<DirCache>>,
    next_cache_id: u64,
    /// `Left` до подтверждения, что таб ушёл.
    left: FxHashMap<TabId, (Location, Arc<Vec<Entry>>)>,
    snap_tx: Sender<Snapshot>,
    patch: Arc<PatchCx>,
}

/// Номера наблюдений кэша — в старшей половине, чтобы не пересечься с табами.
const CACHE_IDS: u64 = 1 << 63;

/// Параметры хаба из `[vfs]`.
pub(crate) struct HubCfg {
    pub debounce: Duration,
    pub max_latency: Duration,
    pub poll: Duration,
    /// С этого числа записей — патч вместо перечитывания.
    pub patch_threshold: u32,
    pub opts: ArrangeOpts,
}

/// Общее для задач патча: порог, параметры порядка, кэш `DirIndex` по табам.
struct PatchCx {
    threshold: u32,
    opts: ArrangeOpts,
    indexes: Mutex<FxHashMap<TabId, DirIndex>>,
}

impl Worker {
    fn run(
        &mut self,
        ctrl: &Receiver<Ctrl>,
        events: &Receiver<(WatchId, WatchEvent)>,
        snaps: &Receiver<Snapshot>,
        sink: &MsgSink,
        cancel: &CancelToken,
    ) {
        while !cancel.is_cancelled() {
            let now = Instant::now();
            let wake = [self.deb.deadline(), self.poll_due()]
                .into_iter()
                .flatten()
                .min();
            // Без сроков — ждать без таймаута.
            let got = if let Some(w) = wake {
                select! {
                    recv(events) -> m => Got::Event(m.ok()),
                    recv(ctrl) -> m => Got::Ctrl(m.ok()),
                    recv(snaps) -> m => Got::Snap(m.ok()),
                    default(w.saturating_duration_since(now)) => Got::Timeout,
                }
            } else {
                select! {
                    recv(events) -> m => Got::Event(m.ok()),
                    recv(ctrl) -> m => Got::Ctrl(m.ok()),
                    recv(snaps) -> m => Got::Snap(m.ok()),
                }
            };
            match got {
                Got::Event(Some((id, ev))) if self.by_id.contains_key(&id) => {
                    self.deb.push(id, ev, Instant::now());
                }
                Got::Event(Some((id, _))) if id.0 >= CACHE_IDS => {
                    // Каталог в кэше изменился: устарел, наблюдение снять (вне блокировки).
                    let dropped = self
                        .cache
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .invalidate(id);
                    drop(dropped);
                }
                Got::Ctrl(Some(c)) => self.control(c, sink, cancel),
                Got::Snap(Some(snap)) => self.snapshot(&snap),
                // `VfsListing` уничтожен.
                Got::Ctrl(None) => return,
                Got::Event(_) | Got::Snap(None) | Got::Timeout => {}
            }
            self.run_polls(cancel);
            for (id, change) in self.deb.take_due(Instant::now()) {
                self.flush(id, change, sink, cancel);
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "диспетчер управляющих сообщений")]
    fn control(&mut self, c: Ctrl, sink: &MsgSink, cancel: &CancelToken) {
        match c {
            Ctrl::Subscribe {
                tab,
                generation,
                loc,
                id,
                guard,
                vfs,
            } => {
                // Опоздавшая подписка прежней навигации (её `watch` открывался дольше):
                // таб уже в новом каталоге, его наблюдение не снимаем. Гард — сразу.
                if self
                    .subs
                    .get(&tab)
                    .is_some_and(|s| s.generation > generation)
                {
                    return;
                }
                // Таб ушёл в другой каталог: прежний — в кэш, пока его наблюдение живо.
                self.commit_left(tab);
                self.drop_sub(tab);
                if self.hidden.contains(&tab) {
                    self.dirty.insert(tab);
                    return;
                }
                self.by_id.insert(id, tab);
                self.deb.hold(id, true);
                self.current
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(tab, (loc.clone(), id));
                self.subs.insert(
                    tab,
                    Sub {
                        generation,
                        loc,
                        id,
                        guard,
                        vfs,
                        poller: None,
                        active: false,
                    },
                );
            }
            Ctrl::Reload { tab, generation } => {
                // Тот же каталог перечитывается — таб никуда не ушёл.
                self.left.remove(&tab);
                if let Some(s) = self.subs.get_mut(&tab) {
                    s.generation = generation;
                    s.active = false;
                    self.deb.hold(s.id, true);
                }
            }
            Ctrl::Unsubscribe { tab } => {
                self.commit_left(tab);
                self.drop_sub(tab);
            }
            Ctrl::Activate {
                tab,
                generation,
                list_time,
            } => {
                let poll = self.poll;
                let Some(s) = self.subs.get_mut(&tab) else {
                    return;
                };
                if s.generation != generation {
                    return;
                }
                s.active = true;
                if s.guard.is_none() {
                    // Копии записей у сервиса нет (ADR-0017): снимок — записи core
                    // (сообщением из UI-потока); пока не пришёл — базовая линия первым
                    // опросом.
                    s.poller = Some(Poller::baseline(
                        Arc::clone(&s.vfs),
                        s.loc.clone(),
                        poll,
                        list_time,
                        Instant::now(),
                    ));
                    let (tx, loc) = (self.snap_tx.clone(), s.loc.clone());
                    let m = msg("vfs.poll_snapshot", move |cx| {
                        cx.keep_rev();
                        // Тот же каталог: при refresh до подмены — прежние записи; лишние
                        // изменения первого опроса безвредны, потерянных нет.
                        if let Some((_, t)) = cx.state.tab(tab)
                            && t.loc == loc
                        {
                            let _ = tx.send(Snapshot {
                                tab,
                                generation,
                                entries: Arc::clone(&t.listing.entries),
                            });
                        }
                    });
                    let _ = sink.send(m, cancel);
                }
                self.deb.hold(s.id, false);
            }
            Ctrl::Tab(TabEvent::Hidden(tab)) => {
                self.left.remove(&tab);
                self.hidden.insert(tab);
                if self.subs.contains_key(&tab) {
                    self.drop_sub(tab);
                    self.dirty.insert(tab);
                }
            }
            Ctrl::Tab(TabEvent::Shown(tab)) => {
                self.hidden.remove(&tab);
                if self.dirty.remove(&tab) {
                    let m = msg("vfs.dir_changed", move |cx| {
                        if cx.state.tab(tab).is_some() {
                            cx.dispatch(refresh(tab));
                        }
                    });
                    let _ = sink.send(m, cancel);
                }
            }
            Ctrl::Tab(TabEvent::Closed(tab)) => {
                self.commit_left(tab);
                self.drop_sub(tab);
                self.patch
                    .indexes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&tab);
                self.hidden.remove(&tab);
                self.dirty.remove(&tab);
            }
            Ctrl::Tab(TabEvent::Left { tab, loc, entries }) => {
                // `Left` приходит и на refresh (тот же каталог); в кэш — только когда
                // таб правда ушёл: следующая подписка, отписка или закрытие.
                let ok = self
                    .subs
                    .get(&tab)
                    .is_some_and(|s| s.loc == loc && s.active);
                if ok {
                    self.left.insert(tab, (loc, entries));
                } else {
                    self.left.remove(&tab);
                }
            }
        }
    }

    /// Таб ушёл из каталога: завершённый листинг — в кэш (S4-16). Свежесть —
    /// своё наблюдение, если каталог локальный и за время показа не менялся
    /// (открывается до снятия наблюдения таба — без окна, где событие потеряется).
    fn commit_left(&mut self, tab: TabId) {
        let Some((loc, entries)) = self.left.remove(&tab) else {
            return;
        };
        let Some(s) = self.subs.get(&tab) else {
            return;
        };
        if s.loc != loc {
            return;
        }
        let clean = !self.deb.has_pending(s.id);
        let watch = if clean && crate::cache::watchable(&loc) {
            let id = WatchId(self.next_cache_id);
            self.next_cache_id += 1;
            let tx = WatchTx {
                id,
                tx: self.events.clone(),
            };
            s.vfs.watch(&loc, tx).ok().map(|g| (id, g))
        } else {
            None
        };
        let dropped = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .put(loc, entries, watch);
        drop(dropped);
    }

    /// Снимок записей таба для опроса; `Arc` отпускается сразу (снимок — копия).
    fn snapshot(&mut self, snap: &Snapshot) {
        if let Some(s) = self.subs.get_mut(&snap.tab)
            && s.generation == snap.generation
            && let Some(p) = s.poller.as_mut()
        {
            p.prime(&snap.entries);
        }
    }

    fn drop_sub(&mut self, tab: TabId) {
        if let Some(s) = self.subs.remove(&tab) {
            self.by_id.remove(&s.id);
            self.deb.remove(s.id);
            self.current
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&tab);
            // Гард снимает наблюдение синхронно (хэндл закрыт).
            drop(s);
        }
    }

    fn poll_due(&self) -> Option<Instant> {
        self.subs
            .values()
            .filter(|s| s.active)
            .filter_map(|s| s.poller.as_ref().map(Poller::due))
            .min()
    }

    fn run_polls(&mut self, cancel: &CancelToken) {
        let now = Instant::now();
        for s in self.subs.values_mut() {
            let Some(p) = s.poller.as_mut() else {
                continue;
            };
            if !s.active || p.due() > now {
                continue;
            }
            match p.poll(cancel, now) {
                Ok(evs) => {
                    for ev in evs {
                        self.deb.push(s.id, ev, Instant::now());
                    }
                }
                Err(e) => tracing::debug!(error = %e, loc = %s.loc.path.display(), "poll failed"),
            }
        }
    }

    fn flush(&mut self, id: WatchId, change: DirChange, sink: &MsgSink, cancel: &CancelToken) {
        let Some(&tab) = self.by_id.get(&id) else {
            return;
        };
        let Some(s) = self.subs.get(&tab) else {
            return;
        };
        if !s.active {
            return;
        }
        let generation = s.generation;
        let m = if change == DirChange::Gone {
            // Каталога нет: наблюдение снимаем, таб — в ближайший существующий
            // родитель (поиск — I/O, здесь, не в UI-потоке).
            let up = existing_parent(s.vfs.as_ref(), &s.loc);
            self.drop_sub(tab);
            msg("vfs.dir_gone", move |cx| gone(cx, tab, generation, up))
        } else {
            let job = PatchJob {
                cx: Arc::clone(&self.patch),
                vfs: Arc::clone(&s.vfs),
                loc: s.loc.clone(),
            };
            msg("vfs.dir_changed", move |cx| {
                apply(cx, tab, generation, change, job);
            })
        };
        let _ = sink.send(m, cancel);
    }
}

/// Что нужно задаче патча кроме самого изменения.
struct PatchJob {
    cx: Arc<PatchCx>,
    vfs: Arc<dyn Vfs>,
    loc: Location,
}

/// В UI-потоке (S4 §5.3): таб и поколение совпали — большой каталог патчим в
/// фоне (S4 §5.4), маленький и `Rescan` — `panel.refresh`.
fn apply(
    cx: &mut urfin_core::Cx<'_>,
    tab: TabId,
    generation: Gen,
    change: DirChange,
    job: PatchJob,
) {
    let Some((_, t)) = cx.state.tab(tab) else {
        return;
    };
    if t.listing.generation != generation {
        return;
    }
    let big = t.listing.entries.len() >= job.cx.threshold as usize;
    // Идущий refresh патчу не мешает: его листинг заменит этот целиком.
    let settled = matches!(t.listing.status, ListingStatus::Complete);
    let DirChange::Names {
        added,
        changed,
        removed,
        unresolved,
    } = change
    else {
        tracing::debug!(target: "watch.flush", tab = tab.0, "rescan");
        cx.dispatch(refresh(tab));
        return;
    };
    if !big || !settled {
        tracing::debug!(target: "watch.flush", tab = tab.0, big, settled, "refresh");
        cx.dispatch(refresh(tab));
        return;
    }
    let mut upsert = added;
    upsert.extend(changed);
    let task = PatchTask {
        tab,
        generation,
        entries: Arc::clone(&t.listing.entries),
        order: Arc::clone(&t.listing.order),
        seq: t.listing.arrange_seq,
        spec: ArrangeSpec {
            sort: t.sort,
            filter: t.filter.clone(),
            show_hidden: t.show_hidden,
        },
        changes: Changes { upsert, removed },
        unresolved,
        job,
    };
    tracing::debug!(target: "watch.flush", tab = tab.0, "patch");
    // `stat_entry` неразрешённых — I/O, поэтому Interactive; само построение — O(n).
    cx.rt
        .spawn(Lane::Interactive, "vfs.patch", cx.rt.token(), move |tcx| {
            task.run(&tcx);
        });
}

struct PatchTask {
    tab: TabId,
    generation: Gen,
    entries: Arc<Vec<Entry>>,
    order: Arc<Vec<u32>>,
    seq: u32,
    spec: ArrangeSpec,
    changes: Changes,
    unresolved: Vec<CompactString>,
    job: PatchJob,
}

impl PatchTask {
    fn run(self, tcx: &urfin_core::TaskCx) {
        let PatchTask {
            tab,
            generation,
            entries,
            order,
            seq,
            spec,
            mut changes,
            unresolved,
            job,
        } = self;
        // Имена без `Entry`: `stat_entry`; нет файла — удалён.
        for name in unresolved {
            let hint = if crate::name::may_be_escaped(&name) {
                Attrs(Attrs::ESCAPED_NAME)
            } else {
                Attrs(0)
            };
            match job.vfs.stat_entry(&job.loc, &name, hint) {
                Ok(e) => changes.upsert.push(e),
                Err(VfsError::NotFound(_)) if hint.0 == 0 => changes.removed.push(name),
                Err(e) => {
                    tracing::debug!(error = %e, "patch: cannot resolve a name, refresh");
                    let _ = tcx.sink.send(refresh_msg(tab, generation), &tcx.cancel);
                    return;
                }
            }
        }
        let indexes = &job.cx.indexes;
        let mut index = indexes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&tab)
            .filter(|i| i.fits(&entries, generation))
            .unwrap_or_else(|| DirIndex::build(&entries, generation));
        let patch = build_patch(
            &entries,
            &order,
            &mut index,
            &changes,
            &spec,
            job.cx.opts,
            seq,
        );
        indexes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tab, index);
        // Отпустить `Arc` до применения: иначе `make_mut` в core скопирует записи.
        drop((entries, order));
        let Some(patch) = patch else {
            return;
        };
        let m = msg("vfs.patch", move |cx| {
            let Some((_, t)) = cx.state.tab(tab) else {
                return;
            };
            let fits = t.listing.generation == generation
                && t.listing.entries.len() == patch.base_len as usize
                && t.listing.arrange_seq == patch.seq;
            if fits {
                listing_msg(tab, generation, ListingUpdate::Patch(patch)).apply(cx);
            } else {
                // Листинг сменился, пока строился патч: изменения не теряем.
                cx.dispatch(refresh(tab));
            }
        });
        let _ = tcx.sink.send(m, &tcx.cancel);
    }
}

fn refresh_msg(tab: TabId, generation: Gen) -> Box<dyn urfin_core::Msg> {
    msg("vfs.dir_changed", move |cx| {
        if cx
            .state
            .tab(tab)
            .is_some_and(|(_, t)| t.listing.generation == generation)
        {
            cx.dispatch(refresh(tab));
        }
    })
}

/// Каталог таба удалён: перейти в `up` (или перечитать — тогда core покажет ошибку).
fn gone(cx: &mut urfin_core::Cx<'_>, tab: TabId, generation: Gen, up: Option<Location>) {
    let Some((side, t)) = cx.state.tab(tab) else {
        return;
    };
    if t.listing.generation != generation {
        return;
    }
    let side = match side {
        urfin_core::Side::Left => "left",
        urfin_core::Side::Right => "right",
    };
    match up {
        Some(up) => cx.dispatch(Command::new("panel.go").arg("side", side).arg("path", up)),
        None => cx.dispatch(refresh(tab)),
    }
}

fn existing_parent(vfs: &dyn Vfs, loc: &Location) -> Option<Location> {
    let mut cur = loc.parent()?;
    loop {
        if vfs.stat(&cur, true).is_ok() {
            return Some(cur);
        }
        cur = cur.parent()?;
    }
}

fn refresh(tab: TabId) -> Command {
    Command::new("panel.refresh").arg("tab", i64::from(tab.0))
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
