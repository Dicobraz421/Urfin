//! `ArcVfs<F>` — архив формата `F` как VFS-бэкенд своей схемы (S8 §0, §3.2).
//!
//! Адрес внутри архива — `Location { backend: F::scheme, path: "/dir", outer: Some(файл) }`.
//! Индекс строится при первом листинге (в потоке листинга, с отменой) и живёт в общем
//! [`ArcCache`]; повторный вход — из кэша, архив не читается (один `stat` файла: изменился
//! ли он). Только чтение: `Caps::READ_ONLY`, методы записи — по умолчанию `NoWrite`.
//! Изменение файла архива приходит наблюдением за его каталогом ([`Vfs::watch`]).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Attrs, CancelToken, Entry, Location};
use urfin_vfs::{
    Caps, ColumnSpec, ColumnView, ExtCell, ExtractStream, ListOpts, ListStream, LocalFs, Metadata,
    Space, Vfs, VfsError, VfsPath, VfsRead, WatchEvent, WatchGuard, WatchTx,
};

use crate::cache::{ArcCache, Claim, Ticket};
use crate::format::{ArcSource, Format, Scan, format_error};
use crate::index::{ArchiveIndex, IndexBuilder, NodeId, OuterKey};

/// Строк в батче листинга после первого.
const BATCH: usize = 4096;

/// Промежуточная сводка потокового индекса — не чаще.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// «Сжато»: сжатый размер записи (`screen-archive`, ширина 80).
pub const PACKED: ColumnSpec = ColumnSpec {
    id: "packed",
    title: "arc-col-packed",
    width: 80.0,
    right: true,
    view: ColumnView::Size,
    mono: false,
};

static COLUMNS: [ColumnSpec; 3] = [ColumnSpec::SIZE, PACKED, ColumnSpec::MTIME];

/// Архив формата `F` как VFS.
pub struct ArcVfs<F: Format> {
    fmt: Arc<F>,
    cache: Arc<ArcCache>,
}

impl<F: Format> Clone for ArcVfs<F> {
    fn clone(&self) -> Self {
        ArcVfs {
            fmt: Arc::clone(&self.fmt),
            cache: Arc::clone(&self.cache),
        }
    }
}

impl<F: Format> std::fmt::Debug for ArcVfs<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArcVfs")
            .field("scheme", &self.fmt.scheme())
            .finish_non_exhaustive()
    }
}

fn outer_of(at: &VfsPath) -> Result<&Location, VfsError> {
    at.outer
        .as_deref()
        .ok_or_else(|| VfsError::NotFound(at.path.clone()))
}

fn node_of(idx: &ArchiveIndex, p: &VfsPath) -> Result<NodeId, VfsError> {
    idx.lookup(&p.path)
        .ok_or_else(|| VfsError::NotFound(p.path.clone()))
}

fn is_root(p: &VfsPath) -> bool {
    p.path
        .to_string_lossy()
        .split(['/', '\\'])
        .all(|c| c.is_empty() || c == ".")
}

impl<F: Format> ArcVfs<F> {
    pub fn new(fmt: F, cache: Arc<ArcCache>) -> ArcVfs<F> {
        ArcVfs {
            fmt: Arc::new(fmt),
            cache,
        }
    }

    pub fn format(&self) -> &F {
        &self.fmt
    }

    pub fn cache(&self) -> &Arc<ArcCache> {
        &self.cache
    }

    /// Ключ файла архива: `stat` без чтения.
    fn key(outer: &Location) -> Result<OuterKey, VfsError> {
        if !outer.is_local() {
            return Err(VfsError::Unsupported("archive inside a container: S8-31"));
        }
        let m = LocalFs::new().stat(outer, true)?;
        if m.kind.is_dir_like() {
            return Err(format_error(outer));
        }
        Ok(OuterKey {
            loc: outer.clone(),
            size: m.size,
            mtime: m.mtime,
        })
    }

    /// Индекс архива адреса `at`; нет в кэше — строится здесь. Блокирует: потоки
    /// листинга и job, не UI.
    pub fn index(&self, at: &VfsPath, cancel: &CancelToken) -> Result<Arc<ArchiveIndex>, VfsError> {
        let key = Self::key(outer_of(at)?)?;
        match self.cache.claim(&key, cancel)? {
            Claim::Ready(idx) => Ok(idx),
            Claim::Build(t) => self.build(t, cancel),
        }
    }

    /// Разбор оглавления целиком.
    fn build(&self, t: Ticket, cancel: &CancelToken) -> Result<Arc<ArchiveIndex>, VfsError> {
        let key = t.key().clone();
        let src = ArcSource::new(key.clone());
        let mut scan = self.fmt.scan(&src)?;
        let mut b = IndexBuilder::new();
        let mut last = None;
        while self.step(&mut *scan, &mut b, &t, cancel, &mut last)? {}
        Ok(t.finish(b.finish(self.fmt.label(&key.loc), key)))
    }

    /// Шаг разбора; у потокового формата — промежуточная сводка.
    fn step(
        &self,
        scan: &mut dyn Scan,
        b: &mut IndexBuilder,
        t: &Ticket,
        cancel: &CancelToken,
        last: &mut Option<Instant>,
    ) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let more = scan.step(b, cancel)?;
        // Первая сводка — сразу после первой порции, дальше — не чаще `PROGRESS_EVERY`.
        if more && !self.fmt.catalog_first() && last.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY) {
            *last = Some(Instant::now());
            let key = t.key();
            t.progress(b.summary(self.fmt.label(&key.loc), key.size, false));
        }
        Ok(more)
    }

    fn src(idx: &ArchiveIndex) -> ArcSource {
        ArcSource::new(idx.outer_key.clone())
    }
}

fn metadata(idx: &ArchiveIndex, id: NodeId) -> Metadata {
    let n = idx.node(id);
    Metadata {
        kind: n.kind,
        attrs: n.attrs,
        size: n.size,
        alloc_size: Some(n.packed),
        mtime: n.mtime,
        atime: i64::MIN,
        ctime: None,
        btime: None,
        mode: None,
        uid: None,
        gid: None,
        nlink: None,
        file_id: None,
        reparse_tag: None,
        link_target: None,
    }
}

/// Листинг каталога: индекс — в первом `next_batch` (там есть отмена).
struct Listing<F: Format> {
    vfs: ArcVfs<F>,
    dir: VfsPath,
    first: usize,
    state: State,
}

enum State {
    Start,
    Kids {
        idx: Arc<ArchiveIndex>,
        node: NodeId,
        at: usize,
    },
    /// Корень потокового формата, пока индекс строится.
    Stream {
        scan: Box<dyn Scan>,
        b: Box<IndexBuilder>,
        ticket: Ticket,
        at: usize,
        last: Option<Instant>,
    },
    Done,
}

impl<F: Format> Listing<F> {
    fn start(&mut self, cancel: &CancelToken) -> Result<State, VfsError> {
        let key = ArcVfs::<F>::key(outer_of(&self.dir)?)?;
        let streaming = !self.vfs.fmt.catalog_first() && is_root(&self.dir);
        let idx = match self.vfs.cache.claim(&key, cancel)? {
            Claim::Ready(idx) => idx,
            Claim::Build(ticket) if streaming => {
                let src = ArcSource::new(key.clone());
                return Ok(State::Stream {
                    scan: self.vfs.fmt.scan(&src)?,
                    b: Box::new(IndexBuilder::new()),
                    ticket,
                    at: 0,
                    last: None,
                });
            }
            Claim::Build(ticket) => self.vfs.build(ticket, cancel)?,
        };
        let node = node_of(&idx, &self.dir)?;
        if !idx.node(node).kind.is_dir_like() {
            return Err(VfsError::NotDir(self.dir.path.clone()));
        }
        Ok(State::Kids { idx, node, at: 0 })
    }
}

impl<F: Format> ListStream for Listing<F> {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if matches!(self.state, State::Start) {
            self.state = self.start(cancel)?;
        }
        match std::mem::replace(&mut self.state, State::Done) {
            State::Start | State::Done => Ok(false),
            State::Kids { idx, node, at } => {
                let kids = idx.children(node);
                let take = if at == 0 { self.first } else { BATCH };
                let end = kids.len().min(at + take);
                out.extend(kids[at..end].iter().map(|&k| idx.entry(k)));
                if end < kids.len() {
                    self.state = State::Kids { idx, node, at: end };
                    return Ok(true);
                }
                Ok(false)
            }
            State::Stream {
                mut scan,
                mut b,
                ticket,
                at,
                mut last,
            } => {
                let more = self
                    .vfs
                    .step(&mut *scan, &mut b, &ticket, cancel, &mut last)?;
                let at = b.root_since(at, out);
                if more {
                    self.state = State::Stream {
                        scan,
                        b,
                        ticket,
                        at,
                        last,
                    };
                    return Ok(true);
                }
                let key = ticket.key().clone();
                let label = self.vfs.fmt.label(&key.loc);
                ticket.finish(b.finish(label, key));
                Ok(false)
            }
        }
    }
}

impl<F: Format> Vfs for ArcVfs<F> {
    fn scheme(&self) -> &'static str {
        self.fmt.scheme()
    }

    /// Из UI-потока (правка F4, колонки): только кэш в памяти.
    fn caps(&self, at: &VfsPath) -> Caps {
        let entry = at
            .outer
            .as_deref()
            .and_then(|o| self.cache.latest(o))
            .and_then(|idx| {
                let id = idx.lookup(&at.path)?;
                (!idx.node(id).kind.is_dir_like()).then(|| self.fmt.entry_caps(&idx, id))
            })
            .unwrap_or(0);
        Caps(Caps::READ_ONLY | entry)
    }

    fn list(&self, dir: &VfsPath, opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        outer_of(dir)?;
        Ok(Box::new(Listing {
            vfs: self.clone(),
            dir: dir.clone(),
            first: (opts.first_batch_hint as usize).max(1),
            state: State::Start,
        }))
    }

    fn stat(&self, p: &VfsPath, _follow: bool) -> Result<Metadata, VfsError> {
        let idx = self.index(p, &CancelToken::new())?;
        Ok(metadata(&idx, node_of(&idx, p)?))
    }

    fn stat_entry(&self, dir: &VfsPath, name: &str, _hint: Attrs) -> Result<Entry, VfsError> {
        let idx = self.index(dir, &CancelToken::new())?;
        let d = node_of(&idx, dir)?;
        let id = idx
            .child(d, name)
            .ok_or_else(|| VfsError::NotFound(dir.path.join(name)))?;
        Ok(idx.entry(id))
    }

    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        let idx = self.index(p, &CancelToken::new())?;
        let id = node_of(&idx, p)?;
        if idx.node(id).kind.is_dir_like() {
            return Err(VfsError::from_io(
                p.path.clone(),
                io::Error::from(io::ErrorKind::IsADirectory),
            ));
        }
        self.fmt.open_entry(&Self::src(&idx), &idx, id)
    }

    fn space(&self, _at: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("archive: read-only, no free space"))
    }

    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }

    /// Наблюдение за файлом архива: каталог `outer` через `LocalFs::watch`, свой поток
    /// отбирает события этого файла. Файл изменился — `Overflow` (hub перечитает листинг,
    /// а он — новый индекс), удалён или переименован — `Gone`.
    fn watch(&self, dir: &VfsPath, tx: WatchTx) -> Result<WatchGuard, VfsError> {
        let outer = outer_of(dir)?;
        let (Some(parent), Some(name)) = (outer.path.parent(), outer.path.file_name()) else {
            return Err(VfsError::Unsupported("archive: no parent folder"));
        };
        if !outer.is_local() {
            return Err(VfsError::Unsupported("archive inside a container: S8-31"));
        }
        let name = name.to_string_lossy().to_lowercase();
        let (itx, irx) = crossbeam_channel::unbounded();
        let inner = LocalFs::new().watch(
            &Location::local(parent.to_path_buf()),
            WatchTx { id: tx.id, tx: itx },
        )?;
        std::thread::Builder::new()
            .name("arc-watch".into())
            .spawn(move || {
                // Кончается, когда наблюдение каталога снято (гард сброшен).
                for (_, ev) in irx {
                    let Some(out) = archive_event(&name, ev) else {
                        continue;
                    };
                    if tx.tx.send((tx.id, out)).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| VfsError::from_io(outer.path.clone(), e))?;
        Ok(WatchGuard::new(inner))
    }

    /// План по индексу из кэша, без I/O. Файлы из разных архивов или без индекса — `None`.
    fn extract_plan(&self, files: &[VfsPath], parallel: u8) -> Option<Vec<Box<dyn ExtractStream>>> {
        let outer = files.first()?.outer.as_deref()?;
        let idx = self.cache.latest(outer)?;
        let mut pairs = Vec::with_capacity(files.len());
        for (i, f) in files.iter().enumerate() {
            if f.outer.as_deref() != Some(outer) || f.backend != self.fmt.scheme() {
                return None;
            }
            let id = idx.lookup(&f.path)?;
            if idx.node(id).kind.is_dir_like() {
                return None;
            }
            pairs.push((u32::try_from(i).ok()?, id));
        }
        self.fmt.plan(&Self::src(&idx), &idx, &pairs, parallel)
    }

    fn columns(&self, _dir: &VfsPath) -> &'static [ColumnSpec] {
        &COLUMNS
    }

    /// «Сжато» из индекса в кэше (UI-поток, без I/O).
    fn ext(&self, dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        if col != PACKED.id {
            return None;
        }
        let idx = self.cache.latest(dir.outer.as_deref()?)?;
        let d = idx.lookup(&dir.path)?;
        let id = idx.child(d, &e.name)?;
        Some(ExtCell::Size(idx.node(id).packed))
    }

    fn transfer_channels(&self) -> u16 {
        self.fmt.transfer_channels()
    }
}

/// Событие каталога архива → событие для панели внутри архива (`name` — имя файла
/// архива в нижнем регистре).
fn archive_event(name: &str, ev: WatchEvent) -> Option<WatchEvent> {
    let is = |n: &str| n.to_lowercase() == name;
    match ev {
        WatchEvent::Added(n, _) | WatchEvent::Modified(n, _) if is(&n) => {
            Some(WatchEvent::Overflow)
        }
        WatchEvent::Removed(n) if is(&n) => Some(WatchEvent::Gone),
        WatchEvent::Renamed { from, .. } if is(&from) => Some(WatchEvent::Gone),
        WatchEvent::Renamed { to, .. } if is(&to) => Some(WatchEvent::Overflow),
        WatchEvent::Overflow => Some(WatchEvent::Overflow),
        WatchEvent::Gone => Some(WatchEvent::Gone),
        _ => None,
    }
}

/// Корень архива `file` схемы `scheme`: куда ведёт Enter.
pub fn root_of(scheme: &str, file: Location) -> Location {
    Location {
        backend: scheme.into(),
        authority: None,
        path: PathBuf::from("/"),
        outer: Some(Arc::new(file)),
    }
}

#[cfg(test)]
#[path = "avfs_tests.rs"]
pub(crate) mod tests;
