//! Размер каталогов (S4 §5.6, S4-17): handler `vfs`, команда
//! `vfs.dir_size {all?}`. Цели — выделенные каталоги, иначе каталог под
//! курсором, при `all` — все каталоги таба. Считается задачей (Job: прогресс,
//! пауза, отмена) общим обходчиком [`crate::walk`]; раз в 100 мс и в конце в
//! листинг уходит `Patch.update` с размером и `SIZE_KNOWN` (S0 v2 №5), при
//! сортировке по размеру — с пересчитанным порядком.
//!
//! Ссылки и junction не обходятся (размер 0); облачные заглушки — по
//! логическому размеру из листинга, без гидрации. Дедупликация хардлинков на
//! unix — с быстрым путём Linux (S4-08, `statx` даёт `nlink`/`ino`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use compact_str::CompactString;
use urfin_core::{
    ArgSpec, ArrangeSpec, Attrs, CmdError, Command, CommandMeta, Cx, Entry, EntryKind, Gen,
    Handler, JobSpec, JobSummary, Lane, ListingUpdate, Location, Patch, SortKey, TabId, ValueKind,
    listing_msg, msg,
};

use crate::api::VfsRegistry;
use crate::service::VfsCfg;
use crate::sort::ArrangeOpts;

/// Период обновления размеров в листинге.
const TICK: Duration = Duration::from_millis(100);

static COMMANDS: [CommandMeta; 1] = [CommandMeta {
    id: "vfs.dir_size",
    title: "Directory sizes",
    category: "vfs",
    args: &[ArgSpec {
        name: "all",
        kind: ValueKind::Bool,
        required: false,
    }],
    repeatable: false,
}];

/// Handler `vfs`.
pub struct VfsHandler {
    reg: Arc<VfsRegistry>,
    cfg: VfsCfg,
}

impl std::fmt::Debug for VfsHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VfsHandler").finish_non_exhaustive()
    }
}

impl VfsHandler {
    pub fn new(reg: Arc<VfsRegistry>, cfg: VfsCfg) -> VfsHandler {
        VfsHandler { reg, cfg }
    }
}

impl Handler for VfsHandler {
    fn namespace(&self) -> &'static str {
        "vfs"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "vfs.dir_size" => self.dir_size(cmd.args.bool("all").unwrap_or(false), cx),
            other => Err(CmdError::Unknown(other.into())),
        }
    }

    /// Реестр — в `AppState.ext` для колонок схем (v6 №1).
    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if cx.state.ext.get::<crate::api::Schemes>().is_none() {
            cx.state
                .ext
                .insert(crate::api::Schemes(Arc::clone(&self.reg)));
        }
    }
}

fn text(key: &str) -> String {
    urfin_i18n::current().fmt(key, &urfin_i18n::FluentArgs::new())
}

/// Каталог для подсчёта: только настоящие каталоги (ссылки — нет).
fn is_dir(e: &Entry) -> bool {
    e.kind == EntryKind::Dir
}

impl VfsHandler {
    fn dir_size(&self, all: bool, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let t = cx.state.panel(cx.state.active).tab();
        let entries = &t.listing.entries;
        let mut targets: Vec<u32> = if all {
            (0u32..)
                .zip(entries.iter())
                .filter(|(_, e)| is_dir(e))
                .map(|(i, _)| i)
                .collect()
        } else {
            (0u32..)
                .zip(entries.iter())
                .filter(|(i, e)| is_dir(e) && t.selection.contains(*i))
                .map(|(i, _)| i)
                .collect()
        };
        if targets.is_empty()
            && !all
            && let Some(urfin_core::Row::Entry { idx, entry }) = t.current()
            && is_dir(entry)
        {
            targets.push(idx);
        }
        if targets.is_empty() {
            return Err(CmdError::Rejected(text("vfs-dir-size-none").into()));
        }
        let fs = Arc::clone(
            self.reg
                .get(&t.loc)
                .map_err(|e| CmdError::Rejected(e.to_string().into()))?,
        );
        let roots: Vec<Location> = targets
            .iter()
            .map(|&i| crate::walk::child(&t.loc, &entries[i as usize]))
            .collect();
        let names: Vec<CompactString> = targets
            .iter()
            .map(|&i| entries[i as usize].name.clone())
            .collect();
        let task = SizeTask {
            tab: t.id,
            loc: t.loc.clone(),
            targets,
            names,
            roots,
            threads: self.cfg.dir_size_threads,
            opts: self.cfg.arrange_opts(),
        };
        let handle = cx.state.jobs.start(
            JobSpec {
                kind: "dir_size",
                title: text("vfs-job-dir-size"),
                lane: Lane::Interactive,
                priority: 0,
                owner_ns: "vfs",
            },
            cx.rt.sink(),
        );
        cx.rt
            .spawn_dedicated("dir-size".into(), cx.rt.token(), move |tcx| {
                task.run(handle, &*fs, &tcx);
            });
        Ok(())
    }
}

struct SizeTask {
    tab: TabId,
    loc: Location,
    /// Индексы каталогов в листинге на старте (сверяются по имени).
    targets: Vec<u32>,
    names: Vec<CompactString>,
    roots: Vec<Location>,
    threads: u8,
    opts: ArrangeOpts,
}

impl SizeTask {
    fn run(self, job: urfin_core::JobHandle, vfs: &dyn crate::api::Vfs, tcx: &urfin_core::TaskCx) {
        let threads = crate::volume::walk_threads(self.threads, &self.loc);
        tracing::debug!(target: "vfs.dir_size", threads, roots = self.roots.len(), loc = %self.loc.path.display());
        let sizes: Vec<AtomicU64> = self.roots.iter().map(|_| AtomicU64::new(0)).collect();
        let progress = job.progress();
        let visit = |root: usize, _dir: &Location, e: &Entry| {
            if e.kind == EntryKind::File {
                sizes[root].fetch_add(e.size, Ordering::Relaxed);
                progress.bytes_done.fetch_add(e.size, Ordering::Relaxed);
                progress.items_done.fetch_add(1, Ordering::Relaxed);
            }
        };
        let stop = || tcx.cancel.is_cancelled() || job.checkpoint().is_err();
        let mut sent: Vec<u64> = vec![u64::MAX; self.roots.len()];
        let flush = |sent: &mut Vec<u64>| {
            let updates: Vec<(u32, CompactString, u64)> = (0..self.roots.len())
                .filter_map(|r| {
                    let s = sizes[r].load(Ordering::Relaxed);
                    (s != sent[r]).then(|| {
                        sent[r] = s;
                        (self.targets[r], self.names[r].clone(), s)
                    })
                })
                .collect();
            if updates.is_empty() {
                return;
            }
            let (tab, loc, opts) = (self.tab, self.loc.clone(), self.opts);
            let m = msg("vfs.dir_size", move |cx| {
                apply_sizes(cx, tab, &loc, &updates, opts);
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        };
        let stats = crate::walk::walk(
            vfs,
            &self.roots,
            threads,
            job.cancel_token(),
            &stop,
            &visit,
            &|_| {},
            || flush(&mut sent),
            TICK,
        );
        let cancelled = job.cancel_token().is_cancelled() || tcx.cancel.is_cancelled();
        if !cancelled {
            // В конце — все размеры, даже не менявшиеся с прошлого тика (тот патч
            // мог не подойти к листингу).
            sent.fill(u64::MAX);
            flush(&mut sent);
        }
        let summary = JobSummary {
            items: progress.items_done.load(Ordering::Relaxed),
            bytes: progress.bytes_done.load(Ordering::Relaxed),
            skipped: 0,
            errors: stats.errors.load(Ordering::Relaxed),
        };
        job.finish(if cancelled {
            Err("cancelled".into())
        } else {
            Ok(summary)
        });
    }
}

/// UI-поток: размеры → `Patch.update` (с `SIZE_KNOWN`). Сортировка по размеру —
/// порядок пересчитывается в фоне (вставкой, O(n + k log n)).
fn apply_sizes(
    cx: &mut Cx<'_>,
    tab: TabId,
    loc: &Location,
    updates: &[(u32, CompactString, u64)],
    opts: ArrangeOpts,
) {
    let Some((_, t)) = cx.state.tab(tab) else {
        return;
    };
    if t.loc != *loc {
        return;
    }
    let entries = &t.listing.entries;
    let mut update = Vec::with_capacity(updates.len());
    for (idx, name, size) in updates {
        let i = if entries.get(*idx as usize).is_some_and(|e| e.name == *name) {
            *idx
        } else if let Some(p) = entries.iter().position(|e| e.name == *name) {
            u32::try_from(p).unwrap_or(u32::MAX)
        } else {
            continue;
        };
        let mut e = entries[i as usize].clone();
        e.size = *size;
        e.attrs = Attrs(e.attrs.0 | Attrs::SIZE_KNOWN);
        update.push((i, e));
    }
    if update.is_empty() {
        return;
    }
    let generation = t.listing.generation;
    let seq = t.listing.arrange_seq;
    if t.sort.key != SortKey::Size {
        let p = Patch {
            base_len: u32::try_from(entries.len()).unwrap_or(u32::MAX),
            seq,
            update,
            remove: Vec::new(),
            append: Vec::new(),
            order: Arc::clone(&t.listing.order),
        };
        listing_msg(tab, generation, ListingUpdate::Patch(p)).apply(cx);
        return;
    }
    let (entries, order) = (Arc::clone(entries), Arc::clone(&t.listing.order));
    let spec = ArrangeSpec {
        sort: t.sort,
        filter: t.filter.clone(),
        show_hidden: t.show_hidden,
    };
    cx.rt
        .spawn_cpu("vfs.dir_size.order", cx.rt.token(), move |tcx| {
            let p = crate::patch::update_patch(&entries, &order, update, &spec, opts, seq);
            drop((entries, order));
            let m = msg("vfs.dir_size", move |cx| {
                apply_if_fits(cx, tab, generation, p);
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Патч к листингу, если тот не сменился, пока считался порядок (иначе следующий
/// тик пришлёт размеры заново).
fn apply_if_fits(cx: &mut Cx<'_>, tab: TabId, generation: Gen, p: Patch) {
    let fits = cx.state.tab(tab).is_some_and(|(_, t)| {
        t.listing.generation == generation
            && t.listing.entries.len() == p.base_len as usize
            && t.listing.arrange_seq == p.seq
    });
    if fits {
        listing_msg(tab, generation, ListingUpdate::Patch(p)).apply(cx);
    }
}
