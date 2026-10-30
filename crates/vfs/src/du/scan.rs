//! Скан анализа места (F01 §3.1–3.2): обход [`walk_with`] листером по ФС корня,
//! агрегация в [`DuTree`] (атомики, читается UI без копий), тики для частичных
//! результатов, крупные файлы, суммы по типам, отмена, заморозка в [`DuSnapshot`].
//!
//! [`scan`] — блокирующий (фоновый поток); [`start`] — задача `du` (Job: прогресс,
//! пауза, отмена) на выделенном потоке `spawn_dedicated`, события — в UI-поток.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use urfin_core::{
    Cx, DeviceGate, DeviceJobs, DeviceLease, JobHandle, JobId, JobSpec, JobState, JobSummary, Lane,
    Location, Progress, msg,
};

use super::links::LinkSet;
use super::lister::{FindLister, ListerKind, NtLister, VfsLister};
use super::model::{
    DirId, DuSnapshot, DuTree, DuType, FileRec, NameRef, NodeFlags, SizeMode, SnapMeta, VolumeKey,
};
use super::{DuCfg, parse_size};
use crate::api::{EntryKind, Vfs, VfsError};
use crate::volume::FsKind;
use crate::walk::{DirLister, DirRef, DirVisitor, RawEntry, WalkOpts, walk_with};

/// Порог крупного файла по умолчанию (`[du] track_min`).
const TRACK_MIN: u64 = 1 << 20;

/// Параметры скана.
#[derive(Clone, Debug)]
pub struct ScanCfg {
    /// 0 — авто (`volume::walk_threads`).
    pub threads: u8,
    /// Порог крупного файла, байт.
    pub track_min: u64,
    pub size_mode: SizeMode,
    pub lister: ListerKind,
    /// Первый тик — раньше периода (первые строки < 100 мс).
    pub first_tick: Duration,
    pub tick: Duration,
}

impl Default for ScanCfg {
    fn default() -> Self {
        ScanCfg {
            threads: 0,
            track_min: TRACK_MIN,
            size_mode: SizeMode::Disk,
            lister: ListerKind::Auto,
            first_tick: Duration::from_millis(30),
            tick: Duration::from_millis(100),
        }
    }
}

impl ScanCfg {
    /// Из секции `[du]`; неразборный `track_min` — 1 MiB.
    pub fn from_du(c: &DuCfg) -> ScanCfg {
        ScanCfg {
            threads: c.threads,
            track_min: parse_size(&c.track_min).unwrap_or_else(|| {
                tracing::warn!(target: "vfs.du", value = %c.track_min, "bad [du] track_min");
                TRACK_MIN
            }),
            size_mode: c.size,
            ..ScanCfg::default()
        }
    }
}

/// Итог скана.
#[derive(Debug)]
pub struct ScanOutcome {
    pub snapshot: DuSnapshot,
    /// Каталогов прочитано.
    pub dirs: u64,
    /// Ошибок листинга (включая «нет доступа»).
    pub errors: u64,
    /// Время до первого тика с прочитанным корнем.
    pub first_rows: Option<Duration>,
}

/// Наполнение дерева по записям листинга.
struct DuVisitor<'a> {
    tree: &'a DuTree,
    links: LinkSet,
    track_min: u64,
    size_mode: SizeMode,
    progress: Option<&'a Progress>,
}

#[derive(Default)]
struct Acc {
    disk: u64,
    logical: u64,
    files: u32,
    tdisk: [u64; DuType::COUNT],
    tfiles: [u64; DuType::COUNT],
    large: Vec<(FileRec, CompactString, bool)>,
    link_skipped: bool,
}

impl DirVisitor for DuVisitor<'_> {
    type Acc = Acc;

    fn entry(&self, dir: &DirRef, acc: &mut Acc, e: &RawEntry<'_>) -> Option<u32> {
        match e.kind {
            EntryKind::Dir => self.tree.add_dir(dir.tag, &e.name, e.mtime, e.file_id),
            EntryKind::LinkDir | EntryKind::LinkFile | EntryKind::LinkBroken => {
                acc.link_skipped = true;
                None
            }
            EntryKind::File | EntryKind::Other => {
                acc.files += 1;
                // Повторная жёсткая ссылка: имя есть, размер уже учтён.
                if !self.links.first(e.file_id) {
                    return None;
                }
                acc.disk += e.disk;
                acc.logical += e.logical;
                let ty = DuType::of(&e.name);
                acc.tdisk[ty as usize] += e.disk;
                acc.tfiles[ty as usize] += 1;
                if e.disk.max(e.logical) >= self.track_min {
                    let (name, escaped) = e.name.to_name();
                    acc.large.push((
                        FileRec {
                            dir: dir.tag,
                            name: NameRef::default(),
                            ty,
                            attrs: e.flags,
                            disk: e.disk,
                            logical: e.logical,
                            mtime: e.mtime,
                            file_id: e.file_id,
                        },
                        name,
                        escaped,
                    ));
                }
                None
            }
        }
    }

    fn done(&self, dir: &DirRef, acc: Acc, res: Result<(), VfsError>) {
        let id: DirId = dir.tag;
        let finished = match res {
            Ok(()) => true,
            Err(VfsError::Cancelled) => false,
            Err(VfsError::Denied(_)) => {
                self.tree.deny(id);
                true
            }
            Err(e) => {
                // Каталог исчез или ошибка ОС: считается пустым.
                tracing::debug!(target: "vfs.du", error = %e, "list failed");
                true
            }
        };
        self.tree.add_sizes(id, acc.disk, acc.logical, acc.files);
        self.tree.add_types(&acc.tdisk, &acc.tfiles);
        self.tree.add_large(&acc.large);
        if acc.link_skipped {
            self.tree.set_flag(id, NodeFlags::LINK_SKIPPED);
        }
        if let Some(p) = self.progress {
            let bytes = match self.size_mode {
                SizeMode::Disk => acc.disk,
                SizeMode::Logical => acc.logical,
            };
            p.items_done
                .fetch_add(u64::from(acc.files), Ordering::Relaxed);
            p.bytes_done.fetch_add(bytes, Ordering::Relaxed);
        }
        if finished {
            self.tree.complete(id);
        }
    }
}

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// Листер по ФС корня: NTFS/ReFS — `Nt` (ФС отказала в классе информации — `Find`),
/// прочие ФС и не-локальные backend'ы — `Vfs`.
fn pick_lister(want: ListerKind, root: &Location, fs: FsKind) -> ListerKind {
    match want {
        ListerKind::Auto if root.is_local() && matches!(fs, FsKind::Ntfs | FsKind::Refs) => {
            match super::lister::nt_supported(&root.path) {
                Ok(false) => ListerKind::Find,
                // Ошибка корня (нет доступа) — её покажет листинг.
                Ok(true) | Err(_) => ListerKind::Nt,
            }
        }
        ListerKind::Auto => ListerKind::Vfs,
        k => k,
    }
}

/// `mtime` корня (нс от Unix epoch; нет — 0).
fn root_mtime(p: &std::path::Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// Скан `root` в `tree` (пустое дерево из [`DuTree::new`]). Блокирует поток до конца
/// обхода или отмены (`halted`). `tick(tree)` — в этом же потоке: первый через
/// `first_tick`, дальше раз в `tick`. `vfs` — backend корня (для `VfsLister`).
pub fn scan(
    root: &Location,
    vfs: &dyn Vfs,
    cfg: &ScanCfg,
    tree: &DuTree,
    halted: &(dyn Fn() -> bool + Sync),
    progress: Option<&Progress>,
    mut tick: impl FnMut(&DuTree),
) -> ScanOutcome {
    let t0 = Instant::now();
    let _span = tracing::info_span!("du.scan", root = %root.path.display()).entered();
    let local = root.is_local();
    let vol = if local {
        crate::volume::of(&root.path).ok()
    } else {
        None
    };
    let fs = vol.as_ref().map_or(FsKind::Other, |v| v.fs);
    let mut flags = 0u8;
    let kind = pick_lister(cfg.lister, root, fs);
    let nt = NtLister {
        track_min: cfg.track_min,
    };
    let find = FindLister {
        track_min: cfg.track_min,
        cluster: vol.as_ref().map_or(0, |v| u64::from(v.cluster)),
    };
    let via_vfs = VfsLister { vfs };
    let (lister, links): (&dyn DirLister, LinkSet) = match kind {
        ListerKind::Nt if fs == FsKind::Ntfs => (&nt, LinkSet::mft()),
        ListerKind::Nt => (&nt, LinkSet::hashed()),
        ListerKind::Find => {
            flags |= DuSnapshot::APPROX;
            (&find, LinkSet::Off)
        }
        ListerKind::Vfs | ListerKind::Auto => (&via_vfs, LinkSet::Off),
    };
    if local {
        tree.set_root_meta(root_mtime(&root.path), 0);
    }
    let threads = crate::volume::walk_threads(cfg.threads, root);
    tracing::debug!(target: "vfs.du", threads, lister = ?kind, ?fs, "scan");
    let visitor = DuVisitor {
        tree,
        links,
        track_min: cfg.track_min,
        size_mode: cfg.size_mode,
        progress,
    };
    let mut first_rows = None;
    let stats = walk_with(
        lister,
        vec![DirRef::root(root.clone(), 0)],
        WalkOpts {
            threads,
            first_tick: cfg.first_tick,
            period: cfg.tick,
        },
        halted,
        &visitor,
        || {
            if first_rows.is_none() && root_listed(tree) {
                let d = t0.elapsed();
                tracing::info!(name: "du.first_rows", target: "vfs.du", ms = d.as_millis());
                first_rows = Some(d);
            }
            tick(tree);
        },
    );
    if !tree.total().flags.has(NodeFlags::DONE) {
        flags |= DuSnapshot::PARTIAL;
    }
    let took = t0.elapsed();
    let snapshot = tree.freeze(SnapMeta {
        root: root.path.clone(),
        volume: VolumeKey {
            serial: vol
                .as_ref()
                .map_or(0, |v| u32::try_from(v.id & 0xFFFF_FFFF).unwrap_or(0)),
            fs,
        },
        scanned_at: now_nanos(),
        took_ms: u32::try_from(took.as_millis()).unwrap_or(u32::MAX),
        size_mode: cfg.size_mode,
        flags,
    });
    tracing::info!(
        target: "vfs.du",
        dirs = snapshot.dirs.len(),
        files = snapshot.dirs.first().map_or(0, |d| d.files),
        ms = took.as_millis(),
        partial = flags & DuSnapshot::PARTIAL != 0,
        "scan done"
    );
    ScanOutcome {
        snapshot,
        dirs: stats.dirs.load(Ordering::Relaxed),
        errors: stats.errors.load(Ordering::Relaxed),
        first_rows,
    }
}

/// Корень прочитан хотя бы частично: есть что показать.
fn root_listed(tree: &DuTree) -> bool {
    let r = tree.total();
    tree.len() > 1 || r.files > 0 || r.flags.0 != 0
}

/// Очередь устройства (S5-19, общая с копированием): ждать, пока на диске корня идёт
/// столько job, сколько позволяет `[ops.device_jobs]`; ожидание — `Queued`. Не локальный
/// корень — без очереди.
fn device_slot(
    gate: &Arc<DeviceGate>,
    jobs: DeviceJobs,
    job: &JobHandle,
    root: &Location,
    stop: &urfin_core::CancelToken,
) -> Option<DeviceLease> {
    if !root.is_local() {
        return None;
    }
    let vol = crate::volume::of(&root.path).ok()?;
    let key = crate::volume::device_key(&root.path).unwrap_or(vol.id);
    let slot = (key, crate::volume::jobs_for(&jobs, vol.media));
    let mut queued = false;
    let lease = gate.acquire(
        job.id(),
        &[slot],
        &Arc::default(),
        job.cancel_token(),
        stop,
        &mut || {
            queued = true;
            job.set_state(JobState::Queued);
        },
    );
    if queued && lease.is_ok() {
        job.set_state(JobState::Running);
    }
    lease.ok()
}

/// Событие скана в UI-потоке.
#[derive(Debug)]
pub enum ScanEvent {
    /// Тик: дерево читается напрямую (видимые строки, атомики, без копий).
    Progress(Arc<DuTree>),
    /// Конец; при отмене — снимок с [`DuSnapshot::PARTIAL`].
    Done(Arc<DuSnapshot>),
}

/// Обработчик событий скана (UI-поток).
pub type OnScan = Arc<dyn Fn(&mut Cx<'_>, ScanEvent) + Send + Sync>;

/// Запустить задачу `du`: скан `root` на выделенном потоке, события — `on` в
/// UI-потоке. `title` — строка задачи (Fluent — у вызывающего). Возвращает id
/// задачи и живое дерево.
pub fn start(
    cx: &mut Cx<'_>,
    vfs: Arc<dyn Vfs>,
    root: Location,
    cfg: ScanCfg,
    title: String,
    on: OnScan,
) -> (JobId, Arc<DuTree>) {
    let job = cx.state.jobs.start(
        JobSpec {
            kind: "du",
            title,
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "du",
        },
        cx.rt.sink(),
    );
    let id = job.id();
    let tree = Arc::new(DuTree::new());
    let live = Arc::clone(&tree);
    let (gate, jobs) = (cx.rt.devices(), DeviceJobs::from_config(&cx.state.config));
    cx.rt
        .spawn_dedicated("disk-usage-scan".into(), cx.rt.token(), move |tcx| {
            // Отменён в очереди — скан ниже сразу остановится и отдаст PARTIAL.
            let _lease = device_slot(&gate, jobs, &job, &root, &tcx.cancel);
            let halted = || tcx.cancel.is_cancelled() || job.checkpoint().is_err();
            let send = |ev: ScanEvent| {
                let on = Arc::clone(&on);
                let _ = tcx
                    .sink
                    .send(msg("du.scan", move |cx| on(cx, ev)), &tcx.cancel);
            };
            let out = scan(
                &root,
                &*vfs,
                &cfg,
                &live,
                &halted,
                Some(job.progress()),
                |_| send(ScanEvent::Progress(Arc::clone(&live))),
            );
            let cancelled = out.snapshot.flags & DuSnapshot::PARTIAL != 0;
            let summary = JobSummary {
                items: u64::from(out.snapshot.dirs.first().map_or(0, |d| d.files)),
                bytes: job.progress().bytes_done.load(Ordering::Relaxed),
                skipped: u64::try_from(out.snapshot.denied.len()).unwrap_or(u64::MAX),
                errors: out.errors,
            };
            send(ScanEvent::Done(Arc::new(out.snapshot)));
            job.finish(if cancelled {
                Err("cancelled".into())
            } else {
                Ok(summary)
            });
        });
    (id, tree)
}
