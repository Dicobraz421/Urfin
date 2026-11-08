//! Обход дерева (S8 §2.2): `DirVisitor` поверх общего обходчика `walk_with`, листеры анализа
//! места. Глубина — в `DirRef.tag`. Содержимое читается в `done`: хэндл каталога уже закрыт.
//!
//! Попадания одного файла дописываются в стор одним вызовом и подряд: на этом держится ряд
//! файлов панели. Буфер сбрасывается по 256 попаданиям или 16 мс.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use urfin_core::search::{Hit, SearchStore};
use urfin_core::{Attrs, Entry, EntryKind, Location};
use urfin_vfs::du::{FindLister, ListerKind, NtLister, VfsLister, pick_lister};
use urfin_vfs::walk::{DirLister, DirRef, DirVisitor, RawEntry, WalkOpts, child, walk_with};
use urfin_vfs::{Caps, Vfs, VfsError, VfsRegistry, name, volume};

use crate::SearchCfg;
use crate::content::{CompiledText, ContentSearcher, FileHit};
use crate::matcher::EntryFilter;
use crate::query::{QueryError, SearchQuery};

/// Попаданий в буфере, после которых он сбрасывается.
const FLUSH_HITS: usize = 256;
/// Самый долгий срок попадания в буфере.
const FLUSH_EVERY: Duration = Duration::from_millis(16);

/// Что нужно поиску по дереву.
pub struct TreeSearch<'a> {
    pub query: &'a SearchQuery,
    pub cfg: &'a SearchCfg,
    pub reg: &'a VfsRegistry,
    pub store: &'a SearchStore,
    /// Каталоги без доступа: для `search.errors`.
    pub denied: &'a Mutex<Vec<Location>>,
    /// Отмена.
    pub halted: &'a (dyn Fn() -> bool + Sync),
    /// Разбудить UI: в сторе новые строки.
    pub poke: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for TreeSearch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeSearch")
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

/// Итог обхода.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Упёрлись в `max_hits`.
    pub capped: bool,
    /// Корни, которые не открылись.
    pub failed_roots: Vec<Location>,
}

/// Буфер попаданий: общий для потоков обхода.
struct Writer<'a> {
    store: &'a SearchStore,
    poke: &'a (dyn Fn() + Sync),
    pending: Mutex<Pending>,
    /// Занято из `limit`: лимит не перескакивает и при параллельной записи.
    taken: AtomicU64,
    limit: u64,
    capped: AtomicBool,
}

struct Pending {
    hits: Vec<Hit>,
    flushed: Instant,
}

impl Writer<'_> {
    /// Попадания одного файла (или каталога без текста): подряд и за один `append`.
    fn push(&self, mut hits: Vec<Hit>) {
        let n = hits.len() as u64;
        let before = self.taken.fetch_add(n, Ordering::Relaxed);
        if before + n > self.limit {
            self.capped.store(true, Ordering::Relaxed);
            hits.truncate(usize::try_from(self.limit.saturating_sub(before)).unwrap_or(0));
        }
        if hits.is_empty() {
            return;
        }
        let ready = {
            let mut p = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            p.hits.append(&mut hits);
            if p.hits.len() >= FLUSH_HITS || p.flushed.elapsed() >= FLUSH_EVERY {
                p.flushed = Instant::now();
                std::mem::take(&mut p.hits)
            } else {
                Vec::new()
            }
        };
        self.append(ready);
    }

    /// Всё из буфера — в стор.
    fn flush(&self) {
        let ready = {
            let mut p = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            p.flushed = Instant::now();
            std::mem::take(&mut p.hits)
        };
        self.append(ready);
    }

    fn append(&self, hits: Vec<Hit>) {
        if !hits.is_empty() {
            self.store.append(hits);
            (self.poke)();
        }
    }

    fn capped(&self) -> bool {
        self.capped.load(Ordering::Relaxed)
    }
}

/// Накопитель каталога.
#[derive(Default)]
struct Acc {
    /// Прошли фильтр: имя пока без пути.
    found: Vec<Entry>,
    files: u64,
    /// Путь каталога от корня с разделителем на конце.
    prefix: Option<(CompactString, bool)>,
}

struct Visitor<'a> {
    run: &'a TreeSearch<'a>,
    filter: &'a EntryFilter,
    root: u16,
    root_loc: &'a Location,
    vfs: &'a dyn Vfs,
    writer: &'a Writer<'a>,
    stop: &'a (dyn Fn() -> bool + Sync),
    /// Имена `[search] skip`, в нижнем регистре.
    skip: &'a [String],
    /// Матчеры, если у этого корня читается содержимое.
    text: Option<&'a Arc<CompiledText>>,
    pool: Mutex<Vec<ContentSearcher>>,
    root_failed: AtomicBool,
}

/// Атрибуты записи: у листера только скрытый, системный, офлайн и сжатый.
fn attrs_of(flags: u8, escaped: bool) -> Attrs {
    let mut a = 0;
    for (raw, attr) in [
        (RawEntry::HIDDEN, Attrs::HIDDEN),
        (RawEntry::SYSTEM, Attrs::SYSTEM),
        (RawEntry::OFFLINE, Attrs::OFFLINE),
        (RawEntry::COMPRESSED, Attrs::COMPRESSED),
    ] {
        if flags & raw != 0 {
            a |= attr;
        }
    }
    if escaped {
        a |= Attrs::ESCAPED_NAME;
    }
    Attrs(a)
}

impl Visitor<'_> {
    fn prefix<'b>(&self, dir: &DirRef, acc: &'b mut Acc) -> &'b (CompactString, bool) {
        acc.prefix.get_or_insert_with(|| {
            let rel = dir.loc.path.strip_prefix(&self.root_loc.path);
            let rel = rel.unwrap_or(Path::new(""));
            if rel.as_os_str().is_empty() {
                return (CompactString::default(), false);
            }
            let (mut s, escaped) = name::from_os(rel.as_os_str());
            s.push(if dir.loc.is_local() { '\\' } else { '/' });
            (s, escaped)
        })
    }

    /// Запись файла с именем от корня: строк столько, сколько совпадений.
    fn relative(prefix: &(CompactString, bool), e: &Entry) -> Entry {
        let mut name = CompactString::with_capacity(prefix.0.len() + e.name.len());
        name.push_str(&prefix.0);
        name.push_str(&e.name);
        let mut attrs = e.attrs;
        if prefix.1 {
            attrs.0 |= Attrs::ESCAPED_NAME;
        }
        Entry {
            name,
            size: e.size,
            mtime: e.mtime,
            ctime: e.ctime,
            atime: e.atime,
            attrs,
            kind: e.kind,
        }
    }

    fn name_hit(&self, e: Entry) -> Hit {
        Hit {
            e,
            root: self.root,
            line: 0,
            hl: (0, 0),
            preview: None,
            status: 0,
        }
    }

    /// Попадания по имени: без чтения файлов.
    fn write_names(&self, dir: &DirRef, acc: &mut Acc) {
        let prefix = self.prefix(dir, acc).clone();
        let hits = acc.found.drain(..);
        let hits = hits.map(|e| self.name_hit(Self::relative(&prefix, &e)));
        self.writer.push(hits.collect());
    }

    /// Читает файл: локальный — напрямую, остальные — через `Vfs::open_read`.
    fn scan(
        &self,
        s: &mut ContentSearcher,
        dir: &DirRef,
        e: &Entry,
        out: &mut Vec<FileHit>,
    ) -> Result<bool, VfsError> {
        let stop = || (self.stop)();
        let io = |source| VfsError::Io {
            path: dir.loc.path.join(&*e.name),
            source,
        };
        if dir.loc.is_local() {
            let f = std::fs::File::open(name::join_os(&dir.loc, e)).map_err(io)?;
            s.search(f, &stop, out).map_err(io)
        } else {
            let f = self.vfs.open_read(&child(&dir.loc, e))?;
            s.search(f, &stop, out).map_err(io)
        }
    }

    /// Содержимое файлов каталога: попадания файла — одним блоком.
    fn write_content(&self, dir: &DirRef, acc: &mut Acc, text: &Arc<CompiledText>) {
        let prefix = self.prefix(dir, acc).clone();
        let mut s = self
            .pool
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .unwrap_or_else(|| ContentSearcher::new(Arc::clone(text), self.run.cfg.max_per_file));
        let mut lines = Vec::new();
        for e in acc.found.drain(..) {
            if (self.stop)() {
                break;
            }
            if self.run.query.skip_offline && e.attrs.has(Attrs::OFFLINE) {
                continue;
            }
            if let Err(err) = self.scan(&mut s, dir, &e, &mut lines) {
                tracing::debug!(target: "search", error = %err, "file skipped");
            }
            if lines.is_empty() {
                continue;
            }
            let e = Self::relative(&prefix, &e);
            let hits = lines.drain(..).map(|l| Hit {
                e: e.clone(),
                root: self.root,
                line: l.line,
                hl: l.hl,
                preview: l.preview,
                status: 0,
            });
            self.writer.push(hits.collect());
        }
        self.pool
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(s);
    }
}

impl DirVisitor for Visitor<'_> {
    type Acc = Acc;

    fn entry(&self, dir: &DirRef, acc: &mut Acc, e: &RawEntry<'_>) -> Option<u32> {
        let q = self.run.query;
        if !q.hidden && e.flags & RawEntry::HIDDEN != 0 {
            return None;
        }
        let is_dir = e.kind == EntryKind::Dir;
        acc.files += u64::from(!e.kind.is_dir_like());
        let wanted = self.filter.kind_ok(e.kind);
        if !wanted && !is_dir {
            return None;
        }
        let (leaf, escaped) = e.name.to_name();
        if is_dir && !self.skip.is_empty() {
            let lower = leaf.to_lowercase();
            if self.skip.iter().any(|s| s == lower.as_str()) {
                return None;
            }
        }
        if wanted {
            let name_ok = if self.filter.wants_path() {
                let prefix = &self.prefix(dir, acc).0;
                let rel = format!("{prefix}{leaf}");
                self.filter.name_ok(&leaf, &rel)
            } else {
                self.filter.name_ok(&leaf, "")
            };
            if name_ok && self.filter.meta_ok(e.kind, e.logical, e.mtime) {
                acc.found.push(Entry {
                    name: leaf,
                    size: e.logical,
                    mtime: e.mtime,
                    ctime: i64::MIN,
                    atime: i64::MIN,
                    attrs: attrs_of(e.flags, escaped),
                    kind: e.kind,
                });
                // Большой каталог: первые строки — не дожидаясь конца листинга.
                if self.text.is_none() && acc.found.len() >= FLUSH_HITS {
                    self.write_names(dir, acc);
                }
            }
        }
        (is_dir && q.depth.is_none_or(|d| dir.tag < u32::from(d))).then_some(dir.tag + 1)
    }

    fn done(&self, dir: &DirRef, mut acc: Acc, res: Result<(), VfsError>) {
        let info = &self.run.store.info;
        info.dirs.fetch_add(1, Ordering::Relaxed);
        info.files.fetch_add(acc.files, Ordering::Relaxed);
        match res {
            Ok(()) | Err(VfsError::Cancelled) => {}
            Err(VfsError::Denied(_)) => {
                info.denied.fetch_add(1, Ordering::Relaxed);
                self.run
                    .denied
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(dir.loc.clone());
            }
            Err(e) => {
                tracing::debug!(target: "search", error = %e, "folder skipped");
                if dir.tag == 0 {
                    self.root_failed.store(true, Ordering::Relaxed);
                }
            }
        }
        if acc.found.is_empty() {
            return;
        }
        match self.text {
            Some(text) => self.write_content(dir, &mut acc, text),
            None => self.write_names(dir, &mut acc),
        }
    }
}

/// Содержимое у локальных файлов и схем с `Caps::SEEK`; остальные корни — только по имени.
fn readable(root: &Location, vfs: &dyn Vfs) -> bool {
    root.is_local() || vfs.caps(root).has(Caps::SEEK)
}

/// Потоков чтения содержимого на SSD (#345): на тёплом кэше оно упирается в CPU, а не в диск,
/// поэтому — по числу ядер, как у `rg`, но не больше 12. HDD и сеть (1 поток), `[search]
/// threads` и поиск только по имени — как у обхода (`volume::walk_threads`, решение №6).
const CONTENT_THREADS: usize = 12;

fn threads_for(configured: u8, root: &Location, content: bool) -> usize {
    let walk = volume::walk_threads(configured, root);
    if !content || configured > 0 || walk <= 1 {
        return walk;
    }
    let cores = std::thread::available_parallelism().map_or(walk, std::num::NonZero::get);
    cores.min(CONTENT_THREADS).max(walk)
}

/// Один проход по корню: листер по ФС, потоки по носителю.
fn walk_root(
    run: &TreeSearch<'_>,
    index: usize,
    root: &Location,
    vfs: &dyn Vfs,
    parts: (&EntryFilter, &[String], Option<&Arc<CompiledText>>),
    writer: &Writer<'_>,
    tick: &mut dyn FnMut(),
) -> bool {
    let (filter, skip, text) = parts;
    let stop = || (run.halted)() || writer.capped();
    let vol = root
        .is_local()
        .then(|| volume::of(&root.path).ok())
        .flatten();
    let fs = vol.as_ref().map_or(volume::FsKind::Other, |v| v.fs);
    // `track_min`: размер на диске поиску не нужен, лишнего syscall на сжатые файлы нет.
    let nt = NtLister {
        track_min: u64::MAX,
    };
    let find = FindLister {
        track_min: u64::MAX,
        cluster: 0,
    };
    let via_vfs = VfsLister { vfs };
    let lister: &dyn DirLister = match pick_lister(ListerKind::Auto, root, fs) {
        ListerKind::Nt => &nt,
        ListerKind::Find => &find,
        ListerKind::Vfs | ListerKind::Auto => &via_vfs,
    };
    let visitor = Visitor {
        run,
        filter,
        root: u16::try_from(index).unwrap_or(u16::MAX),
        root_loc: root,
        vfs,
        writer,
        stop: &stop,
        skip,
        text: text.filter(|_| readable(root, vfs)),
        pool: Mutex::new(Vec::new()),
        root_failed: AtomicBool::new(false),
    };
    let threads = threads_for(run.cfg.threads, root, visitor.text.is_some());
    tracing::debug!(target: "search", threads, ?fs, "walk");
    let opts = WalkOpts {
        threads,
        first_tick: FLUSH_EVERY,
        period: FLUSH_EVERY,
    };
    let roots = vec![DirRef::root(root.clone(), 0)];
    walk_with(lister, roots, opts, &stop, &visitor, || {
        writer.flush();
        tick();
    });
    !visitor.root_failed.load(Ordering::Relaxed)
}

/// Поиск по всем корням: блокирует поток до конца или отмены. Корень — свой проход: потоки и
/// листер у носителей разные. `tick` — в этом потоке, примерно раз в 16 мс.
pub fn search_tree(run: &TreeSearch<'_>, mut tick: impl FnMut()) -> Result<Outcome, QueryError> {
    let text = run
        .query
        .text
        .as_ref()
        .map(|t| CompiledText::new(t).map(Arc::new))
        .transpose()?;
    let filter = EntryFilter::new(run.query);
    let skip: Vec<String> = run
        .cfg
        .skip
        .iter()
        .map(|s| s.as_str().to_lowercase())
        .collect();
    let writer = Writer {
        store: run.store,
        poke: run.poke,
        pending: Mutex::new(Pending {
            hits: Vec::new(),
            flushed: Instant::now(),
        }),
        taken: AtomicU64::new(0),
        limit: run.cfg.max_hits,
        capped: AtomicBool::new(false),
    };
    let mut failed_roots = Vec::new();
    for (i, root) in run.query.roots.iter().enumerate() {
        if (run.halted)() || writer.capped() {
            break;
        }
        let Ok(vfs) = run.reg.get(root) else {
            failed_roots.push(root.clone());
            continue;
        };
        let parts = (&filter, skip.as_slice(), text.as_ref());
        if !walk_root(run, i, root, vfs.as_ref(), parts, &writer, &mut tick) {
            failed_roots.push(root.clone());
        }
    }
    writer.flush();
    Ok(Outcome {
        capped: writer.capped(),
        failed_roots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_threads_only_for_auto_local_content() {
        let local = Location::local(std::env::temp_dir());
        let walk = volume::walk_threads(0, &local);
        assert_eq!(threads_for(0, &local, false), walk, "имя — как у обхода");
        assert_eq!(
            threads_for(3, &local, true),
            3,
            "`[search] threads` — как задано"
        );
        let content = threads_for(0, &local, true);
        if walk <= 1 {
            assert_eq!(content, walk, "HDD — один поток");
        } else {
            assert!(
                content >= walk && content <= CONTENT_THREADS.max(walk),
                "{content}"
            );
        }
    }
}
