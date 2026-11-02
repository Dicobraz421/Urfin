//! Актуальность снимка (F01 §3.3).
//!
//! **Live** — пока открыта вкладка `du:`: один `ReadDirectoryChangesW` на всё поддерево
//! корня (поток ждёт ядро, 0% CPU), события — в каталоги снимка по пути, debounce
//! 250 мс (не дольше [`LiveCfg::max_wait`] от первого события), каталог перелистывается
//! один раз на пачку, новые подкаталоги досчитываются отдельным сканом и прививаются,
//! дельта — по агрегатам вверх ([`super::dvfs::DuRegistry::apply_relist`]).
//! Переполнение буфера — «●» у всего снимка, «Обновить». Вкладки анализа закрыты —
//! [`Live`] снят (drop), кэш пишет handler.
//!
//! Поток — обычный `std::thread` (как `watch-iocp`, S4-12): он только ждёт канал, а
//! остановку получает от владельца; завершение приложения его не ждёт.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use compact_str::CompactString;
use crossbeam_channel::{Receiver, Sender, select};
use rustc_hash::{FxHashMap, FxHashSet};
use urfin_core::Location;

use super::dvfs::{DuRegistry, Graft, Listed, Listing, SnapId};
use super::lister::{FindLister, ListerKind, NtLister, VfsLister};
use super::model::{DirId, DuTree, DuType};
use super::scan::{self, ScanCfg};
use crate::api::{EntryKind, Vfs, VfsError, WatchEvent, WatchGuard, WatchId, WatchTx};
use crate::volume::FsKind;
use crate::walk::{DirLister, DirRef, RawEntry};

/// Подсказок типа на каталог в пачке — не больше (разница мелких файлов, см.
/// `dvfs_live`).
const HINTS_MAX: usize = 16;

/// Листинг одного каталога тем же листером, что скан корня (размер на диске,
/// `FileId`).
#[derive(Debug)]
pub struct Relister {
    kind: ListerKind,
    track_min: u64,
    cluster: u64,
}

impl Relister {
    /// Листер по ФС корня (I/O: том корня).
    pub fn for_root(root: &Path, track_min: u64) -> Relister {
        let loc = Location::local(root);
        let vol = crate::volume::of(root).ok();
        let fs = vol.as_ref().map_or(FsKind::Other, |v| v.fs);
        Relister {
            kind: scan::pick_lister(ListerKind::Auto, &loc, fs),
            track_min,
            cluster: vol.map_or(0, |v| u64::from(v.cluster)),
        }
    }

    /// Файлы и подкаталоги `dir` (без рекурсии) и его `mtime`.
    pub fn list(&self, dir: &Path, fs: &dyn Vfs) -> Result<Listing, VfsError> {
        let mut l = Listing::default();
        let mut out = |e: &RawEntry<'_>| {
            match e.kind {
                EntryKind::Dir => {
                    l.dirs.push((e.name.to_name().0, e.mtime, e.file_id));
                }
                EntryKind::LinkDir | EntryKind::LinkFile | EntryKind::LinkBroken => {
                    l.link_skipped = true;
                }
                EntryKind::File | EntryKind::Other => {
                    let (name, escaped) = e.name.to_name();
                    l.files.push(Listed {
                        name,
                        escaped,
                        disk: e.disk,
                        logical: e.logical,
                        mtime: e.mtime,
                        file_id: e.file_id,
                        flags: e.flags,
                    });
                }
            }
            true
        };
        let r = DirRef::root(Location::local(dir), 0);
        match self.kind {
            ListerKind::Nt => NtLister {
                track_min: self.track_min,
            }
            .list(&r, &mut out),
            ListerKind::Find => FindLister {
                track_min: self.track_min,
                cluster: self.cluster,
            }
            .list(&r, &mut out),
            ListerKind::Vfs | ListerKind::Auto => VfsLister { vfs: fs }.list(&r, &mut out),
        }?;
        l.mtime = Some(scan::root_mtime(dir)).filter(|&m| m != 0);
        Ok(l)
    }
}

/// Перелистать каталоги `dirs` анализа `id` (родители раньше детей: прививка у
/// родителя покрывает новых детей) и применить к снимку: новые подкаталоги
/// досчитываются сканом `cfg` (остановка — `halted`). Каталог исчез или нет доступа —
/// пропуск (исчезновение увидит листинг родителя). Возвращает, сколько применено.
pub fn relist(
    reg: &DuRegistry,
    id: SnapId,
    dirs: &[(DirId, Vec<DuType>)],
    relister: &Relister,
    fs: &dyn Vfs,
    cfg: &ScanCfg,
    halted: &(dyn Fn() -> bool + Sync),
) -> usize {
    let mut done = 0;
    for (d, hint) in dirs {
        let d = *d;
        if halted() {
            break;
        }
        let Some(path) = reg.dir_path(id, d) else {
            break;
        };
        let l = match relister.list(&path, fs) {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(target: "vfs.du", error = %e, dir = %path.display(), "relist");
                continue;
            }
        };
        let known = reg.subdir_names(id, d);
        let mut grafts: Vec<Graft> = Vec::new();
        for (name, _, _) in &l.dirs {
            if known.contains(name) || halted() {
                continue;
            }
            let tree = DuTree::new();
            let out = scan::scan(
                &Location::local(path.join(name.as_str())),
                fs,
                cfg,
                &tree,
                halted,
                None,
                |_| {},
            );
            if !halted() {
                grafts.push((name.clone(), out.snapshot));
            }
        }
        if halted() {
            break;
        }
        if reg.apply_relist(id, d, &l, &grafts, hint) {
            done += 1;
        }
    }
    done
}

/// Параметры наблюдения.
#[derive(Clone, Debug)]
pub struct LiveCfg {
    /// Досчёт новых поддеревьев.
    pub scan: ScanCfg,
    /// Тишина после события до перелистывания.
    pub debounce: Duration,
    /// Дольше от первого события пачки не ждать (непрерывная запись).
    pub max_wait: Duration,
}

impl LiveCfg {
    pub fn new(scan: ScanCfg) -> LiveCfg {
        LiveCfg {
            scan,
            debounce: Duration::from_millis(250),
            max_wait: Duration::from_millis(700),
        }
    }
}

/// Наблюдение за поддеревом анализа. Drop — снять (поток выходит сам, не ждём).
#[derive(Debug)]
pub struct Live {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.take();
    }
}

impl Live {
    /// Начать наблюдение за `root` анализа `id`. `notify` зовётся из потока после
    /// каждой применённой пачки (перерисовать вкладки).
    pub fn start(
        reg: Arc<DuRegistry>,
        id: SnapId,
        root: PathBuf,
        fs: Arc<dyn Vfs>,
        cfg: LiveCfg,
        notify: Box<dyn Fn() + Send>,
    ) -> std::io::Result<Live> {
        let (stop, stop_rx) = crossbeam_channel::bounded::<()>(0);
        let thread = std::thread::Builder::new()
            .name("du-live".into())
            .spawn(move || {
                let (tx, rx) = crossbeam_channel::unbounded();
                let guard = match watch_tree(
                    &root,
                    WatchTx {
                        id: WatchId(u64::from(id)),
                        tx,
                    },
                ) {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::debug!(target: "vfs.du", error = %e, root = %root.display(), "live watch");
                        return;
                    }
                };
                tracing::debug!(target: "vfs.du", root = %root.display(), "live watch on");
                let relister = Relister::for_root(&root, cfg.scan.track_min);
                run(&reg, id, &*fs, &cfg, &relister, &rx, &stop_rx, &*notify);
                drop(guard);
                tracing::debug!(target: "vfs.du", root = %root.display(), "live watch off");
            })?;
        Ok(Live {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// Снять и дождаться потока (тесты: «после закрытия вкладки потоков нет»).
    pub fn join(mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Поток ещё работает.
    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

fn watch_tree(root: &Path, tx: WatchTx) -> Result<WatchGuard, VfsError> {
    #[cfg(windows)]
    {
        crate::sys::win_watch::watch_tree(root, tx)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, tx);
        Err(VfsError::Unsupported("subtree watch"))
    }
}

/// События пачки: каталоги (путь от корня) с подсказками типов, изменённые пути.
#[derive(Default)]
struct Batch {
    overflow: bool,
    parents: FxHashMap<CompactString, Vec<DuType>>,
    /// `Modified` без сведений, файл это или каталог.
    modified: FxHashSet<CompactString>,
}

/// Родитель и имя пути от корня (`a\b\c` → `a\b`, `c`).
fn split(rel: &str) -> (&str, &str) {
    rel.rsplit_once('\\').unwrap_or(("", rel))
}

impl Batch {
    fn parent_of(&mut self, rel: &str) {
        let (parent, name) = split(rel);
        let ty = super::filters::type_of(&crate::walk::RawName::Str {
            name,
            escaped: false,
        });
        let h = self.parents.entry(parent.into()).or_default();
        if h.len() < HINTS_MAX {
            h.push(ty);
        }
    }

    fn push(&mut self, ev: WatchEvent) {
        match ev {
            WatchEvent::Added(n, _) | WatchEvent::Removed(n) => self.parent_of(&n),
            // Смена каталога (его `mtime`) сопровождается событиями его детей.
            WatchEvent::Modified(_, Some(e)) if e.kind.is_dir_like() => {}
            WatchEvent::Modified(n, _) => {
                self.modified.insert(n);
            }
            WatchEvent::Renamed { from, to, .. } => {
                self.parent_of(&from);
                self.parent_of(&to);
            }
            WatchEvent::Overflow | WatchEvent::Gone => self.overflow = true,
        }
    }

    /// Каталоги снимка для перелистывания (ближайший существующий к родителю пути) с
    /// подсказками; родители раньше детей (`DirId` родителя меньше).
    fn dirs(mut self, reg: &DuRegistry, id: SnapId) -> Option<Vec<(DirId, Vec<DuType>)>> {
        let f = reg.frozen(id)?;
        for m in std::mem::take(&mut self.modified) {
            let (parent, _) = split(&m);
            // Путь — существующий каталог снимка: его меняют события детей.
            if f.nearest(Path::new(m.as_str())) != f.nearest(Path::new(parent)) {
                continue;
            }
            self.parent_of(&m);
        }
        let mut out: FxHashMap<DirId, Vec<DuType>> = FxHashMap::default();
        for (rel, hint) in self.parents {
            let d = f.nearest(Path::new(rel.as_str()));
            let h = out.entry(d).or_default();
            for t in hint {
                if h.len() < HINTS_MAX {
                    h.push(t);
                }
            }
        }
        let mut out: Vec<(DirId, Vec<DuType>)> = out.into_iter().collect();
        out.sort_unstable_by_key(|(d, _)| *d);
        Some(out)
    }
}

/// Цикл наблюдения: ждать событие (без таймаута), собрать пачку, перелистать.
#[allow(clippy::too_many_arguments, reason = "внутренний цикл потока")]
fn run(
    reg: &DuRegistry,
    id: SnapId,
    fs: &dyn Vfs,
    cfg: &LiveCfg,
    relister: &Relister,
    events: &Receiver<(WatchId, WatchEvent)>,
    stop: &Receiver<()>,
    notify: &dyn Fn(),
) {
    let halted = || !matches!(stop.try_recv(), Err(crossbeam_channel::TryRecvError::Empty));
    loop {
        let mut batch = Batch::default();
        select! {
            recv(stop) -> _ => return,
            recv(events) -> m => match m {
                Ok((_, ev)) => batch.push(ev),
                Err(_) => return,
            },
        }
        let first = Instant::now();
        loop {
            let left = cfg
                .max_wait
                .saturating_sub(first.elapsed())
                .min(cfg.debounce);
            if left.is_zero() {
                break;
            }
            select! {
                recv(stop) -> _ => return,
                recv(events) -> m => match m {
                    Ok((_, ev)) => batch.push(ev),
                    Err(_) => break,
                },
                default(left) => break,
            }
        }
        let overflow = batch.overflow;
        let Some(dirs) = batch.dirs(reg, id) else {
            // Идёт пересчёт: события ему не нужны.
            continue;
        };
        if overflow {
            tracing::debug!(target: "vfs.du", "live overflow: all changed");
            reg.mark_changed(id, None);
        }
        let t0 = Instant::now();
        let n = relist(reg, id, &dirs, relister, fs, &cfg.scan, &halted);
        tracing::debug!(target: "vfs.du", dirs = n, ms = t0.elapsed().as_millis(), "live relist");
        if n > 0 || overflow {
            notify();
        }
    }
}
