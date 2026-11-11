//! `ArcVfs<F>` — архив формата `F` как VFS-бэкенд своей схемы (S8 §0, §3.2).
//!
//! Адрес внутри архива — `Location { backend: F::scheme, path: "/dir", outer: Some(файл) }`.
//! Индекс строится при первом листинге (в потоке листинга, с отменой) и живёт в общем
//! [`ArcCache`]; повторный вход — из кэша, архив не читается (один `stat` файла: изменился
//! ли он). Только чтение: `Caps::READ_ONLY`, методы записи — по умолчанию `NoWrite`.
//! Изменение файла архива приходит наблюдением за его каталогом ([`Vfs::watch`]).
//!
//! Вложенный архив и архив на сервере (S8-31, §3.4) читаются через реестр VFS
//! ([`ArcCache::set_vfs`]): запись с `Caps::SEEK` (stored) — поддиапазоном, остальное —
//! спиллом на диск ([`crate::spill`]). Вложенность — не глубже [`MAX_NEST`].

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
use crate::format::{ArcSource, Format, Origin, Scan, format_error};
use crate::index::{ArchiveIndex, IndexBuilder, NodeId, OuterKey};
use crate::password::{self, Question, TRIES};
use crate::spill::{self, Spill, SpillQuestion};

/// Архивов в цепочке адреса файла архива — не больше (S8 §3.4; `Location` — до 8
/// контейнеров).
pub const MAX_NEST: usize = 8;

/// Звеньев в цепочке `outer` адреса, считая его самого.
pub(crate) fn chain(loc: &Location) -> usize {
    std::iter::successors(Some(loc), |l| l.outer.as_deref()).count()
}

/// Нижнее звено цепочки: файл на диске или на сервере.
fn base_of(loc: &Location) -> &Location {
    std::iter::successors(Some(loc), |l| l.outer.as_deref())
        .last()
        .unwrap_or(loc)
}

/// «Вложенность архивов глубже 8» (`arc-error-too-deep`).
pub(crate) fn too_deep_text() -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("max", i64::try_from(MAX_NEST).unwrap_or(i64::MAX));
    urfin_i18n::current().fmt("arc-error-too-deep", &a)
}

fn too_deep(outer: &Location) -> VfsError {
    VfsError::Io {
        path: outer.path.clone(),
        source: io::Error::new(io::ErrorKind::InvalidInput, too_deep_text()),
    }
}

fn name_of(l: &Location) -> String {
    l.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Начатый разбор оглавления: источник, разбор, построитель и итог первого шага.
type Begun = (Box<ArcSource>, Box<dyn Scan>, Box<IndexBuilder>, bool);

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

/// Каталог по адресу: ссылка на каталог — её цель.
fn dir_of(idx: &ArchiveIndex, p: &VfsPath) -> Result<NodeId, VfsError> {
    node_of(idx, p).map(|id| idx.follow(id))
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

    /// Ключ файла архива: `stat` без чтения (вложенного — `stat` записи по индексу
    /// внешнего архива).
    fn key(&self, outer: &Location) -> Result<OuterKey, VfsError> {
        if chain(outer) > MAX_NEST {
            return Err(too_deep(outer));
        }
        let m = if outer.is_local() {
            LocalFs::new().stat(outer, true)?
        } else {
            self.outer_vfs(outer)?.stat(outer, true)?
        };
        if m.kind.is_dir_like() {
            return Err(format_error(outer));
        }
        Ok(OuterKey {
            loc: outer.clone(),
            size: m.size,
            mtime: m.mtime,
        })
    }

    /// Backend файла архива не на диске.
    fn outer_vfs(&self, outer: &Location) -> Result<Arc<dyn Vfs>, VfsError> {
        self.cache.vfs_of(outer).ok_or(VfsError::Unsupported(
            "archive inside a container: no VFS registry",
        ))
    }

    /// Откуда читать архив `key` (S8 §3.4): файл на диске; запись другого архива с
    /// `Caps::SEEK` — поддиапазон (кроме rar: ему нужен путь); иначе — спилл.
    fn origin(&self, key: &OuterKey, cancel: &CancelToken) -> Result<Origin, VfsError> {
        let outer = &key.loc;
        if outer.is_local() {
            return Ok(Origin::Local);
        }
        let vfs = self.outer_vfs(outer)?;
        if outer.outer.is_some() && !self.fmt.needs_path() && vfs.caps(outer).has(Caps::SEEK) {
            return Ok(Origin::Range(vfs));
        }
        self.spill(key, &*vfs, cancel).map(Origin::Spill)
    }

    /// Копия архива `key` на диске; сверх лимитов — сначала вопрос.
    fn spill(
        &self,
        key: &OuterKey,
        vfs: &dyn Vfs,
        cancel: &CancelToken,
    ) -> Result<Arc<Spill>, VfsError> {
        let outer = &key.loc;
        let (max, ratio) = self.cache.spill_limits();
        // Степень сжатия — только у записи архива: у файла сервера `alloc_size` — диск.
        let packed = if outer.outer.is_some() {
            vfs.stat(outer, true)?.alloc_size
        } else {
            None
        };
        if let Some(over) = spill::over(key.size, packed, max, ratio) {
            self.confirm_spill(key, over, cancel)?;
        }
        let r = vfs.open_read(outer)?;
        let t0 = Instant::now();
        let s = spill::write(&self.cache.spill_dir(), key, r, cancel)?;
        tracing::info!(target: "arc", bytes = key.size, elapsed = ?t0.elapsed(), "spilled");
        Ok(Arc::new(s))
    }

    /// Вопрос спилла сверх лимита (как пароль, S8-35): нет кого спросить или «Отмена» —
    /// ошибка с причиной; отказ помнится, как отказ от пароля.
    fn confirm_spill(
        &self,
        key: &OuterKey,
        over: spill::Over,
        cancel: &CancelToken,
    ) -> Result<(), VfsError> {
        let outer = &key.loc;
        let Some(ask) = password::current(self.cache.asker()) else {
            return Err(spill::refused(outer, over));
        };
        let _turn = self.cache.ask_turn();
        if self.cache.declined(outer) {
            return Err(spill::refused(outer, over));
        }
        let q = SpillQuestion {
            name: name_of(outer),
            size: key.size,
            over,
        };
        if ask.spill(&q, cancel) {
            return Ok(());
        }
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        self.cache.decline(outer);
        Err(spill::refused(outer, over))
    }

    /// Индекс архива адреса `at`; нет в кэше — строится здесь. Блокирует: потоки
    /// листинга и job, не UI.
    pub fn index(&self, at: &VfsPath, cancel: &CancelToken) -> Result<Arc<ArchiveIndex>, VfsError> {
        let key = self.key(outer_of(at)?)?;
        match self.cache.claim(&key, cancel)? {
            Claim::Ready(idx) => Ok(idx),
            Claim::Build(t) => self.build(t, cancel),
        }
    }

    /// Разбор оглавления целиком.
    fn build(&self, t: Ticket, cancel: &CancelToken) -> Result<Arc<ArchiveIndex>, VfsError> {
        let key = t.key().clone();
        let mut last = None;
        let (src, mut scan, mut b, mut more) = self.begin(&t, cancel, &mut last)?;
        while more {
            more = self.step(&mut *scan, &mut b, &t, cancel, &mut last)?;
        }
        Ok(t.finish(with_source(b.finish(self.fmt.label(&key.loc), key), &src)))
    }

    /// Разбор и его первый шаг: зашифрованные имена (7z `-mhe`, rar `-hp`) падают здесь.
    fn try_begin(
        &self,
        t: &Ticket,
        origin: &Origin,
        cancel: &CancelToken,
        last: &mut Option<Instant>,
    ) -> Result<Begun, VfsError> {
        let src = Box::new(self.source(t.key(), origin.clone()));
        let mut scan = self.fmt.scan(&src)?;
        let mut b = Box::new(IndexBuilder::new());
        let more = self.step(&mut *scan, &mut b, t, cancel, last)?;
        Ok((src, scan, b, more))
    }

    /// [`Self::try_begin`]; без пароля имён — вопрос (S8-35), три попытки: пароль —
    /// [`ArcCache::set_password`] и разбор заново. Спилл — один раз, до попыток.
    fn begin(
        &self,
        t: &Ticket,
        cancel: &CancelToken,
        last: &mut Option<Instant>,
    ) -> Result<Begun, VfsError> {
        let origin = self.origin(t.key(), cancel)?;
        let err = match self.try_begin(t, &origin, cancel, last) {
            Err(e) if password::denied(&e) => e,
            r => return r,
        };
        let Some(ask) = password::current(self.cache.asker()) else {
            return Err(err);
        };
        let outer = t.key().loc.clone();
        let _turn = self.cache.ask_turn();
        if self.cache.declined(&outer) {
            return Err(err);
        }
        let q = Question {
            name: outer
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            left: TRIES,
            wrong: false,
            names: true,
        };
        let (mut out, mut err) = (None, err);
        let ok = password::obtain(&*ask, q, cancel, |pw| {
            self.cache.set_password(&outer, Some(pw));
            match self.try_begin(t, &origin, cancel, last) {
                Ok(v) => {
                    out = Some(v);
                    Ok(true)
                }
                Err(e) if password::denied(&e) => {
                    err = e;
                    Ok(false)
                }
                Err(e) => Err(e),
            }
        })?;
        match out {
            Some(v) if ok => Ok(v),
            _ if cancel.is_cancelled() => Err(VfsError::Cancelled),
            _ => {
                self.cache.decline(&outer);
                Err(err)
            }
        }
    }

    /// Пароль данных записей `ids` (S8-35): зашифрованы, а пароля нет — вопрос.
    fn ensure_password(&self, idx: &ArchiveIndex, ids: impl IntoIterator<Item = NodeId>) {
        let ctx = idx.ctx();
        let enc = |id: NodeId| idx.node(id).attrs.has(Attrs::ENCRYPTED);
        if ctx.has_password() || !ids.into_iter().any(enc) {
            return;
        }
        let src = self.src(idx);
        let open = |id: NodeId| self.fmt.open_entry(&src, idx, id);
        password::ensure_data(&self.cache, idx, &CancelToken::new(), &open);
    }

    /// Источник для разбора оглавления: с паролем, заданным до индекса.
    fn source(&self, key: &OuterKey, origin: Origin) -> ArcSource {
        ArcSource::with_password(key.clone(), self.cache.take_password(&key.loc))
            .with_origin(origin)
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

    /// Источник записей готового индекса: спилл — из индекса, поддиапазон — через реестр.
    fn src(&self, idx: &ArchiveIndex) -> ArcSource {
        let key = idx.outer_key.clone();
        let origin = if key.loc.is_local() {
            Origin::Local
        } else if let Some(s) = idx.ctx().spill() {
            Origin::Spill(s)
        } else {
            self.cache
                .vfs_of(&key.loc)
                .map_or(Origin::Missing, Origin::Range)
        };
        ArcSource::new(key).with_origin(origin)
    }
}

/// Пароль разбора оглавления — и записям: имена и данные 7z шифруются одним паролем.
/// Спилл живёт с индексом: вышел индекс из кэша и отпущен — файл удалён.
fn with_source(idx: ArchiveIndex, src: &ArcSource) -> ArchiveIndex {
    if let Some(pw) = src.password() {
        idx.ctx().set_password(Some(pw));
    }
    idx.ctx().set_spill(src.spill().cloned());
    idx
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
        link_target: idx.link_target(id).map(PathBuf::from),
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
        src: Box<ArcSource>,
        scan: Box<dyn Scan>,
        b: Box<IndexBuilder>,
        ticket: Ticket,
        at: usize,
        last: Option<Instant>,
        /// Итог первого шага, сделанного при старте.
        primed: Option<bool>,
    },
    Done,
}

impl<F: Format> Listing<F> {
    fn start(&mut self, cancel: &CancelToken) -> Result<State, VfsError> {
        let key = self.vfs.key(outer_of(&self.dir)?)?;
        let streaming = !self.vfs.fmt.catalog_first() && is_root(&self.dir);
        let idx = match self.vfs.cache.claim(&key, cancel)? {
            Claim::Ready(idx) => idx,
            Claim::Build(ticket) if streaming => {
                let mut last = None;
                let (src, scan, b, more) = self.vfs.begin(&ticket, cancel, &mut last)?;
                return Ok(State::Stream {
                    src,
                    scan,
                    b,
                    ticket,
                    at: 0,
                    last,
                    primed: Some(more),
                });
            }
            Claim::Build(ticket) => self.vfs.build(ticket, cancel)?,
        };
        let node = dir_of(&idx, &self.dir)?;
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
                src,
                mut scan,
                mut b,
                ticket,
                at,
                mut last,
                primed,
            } => {
                let more = match primed {
                    Some(more) => more,
                    None => self
                        .vfs
                        .step(&mut *scan, &mut b, &ticket, cancel, &mut last)?,
                };
                let at = b.root_since(at, out);
                if more {
                    self.state = State::Stream {
                        src,
                        scan,
                        b,
                        ticket,
                        at,
                        last,
                        primed: None,
                    };
                    return Ok(true);
                }
                let key = ticket.key().clone();
                let label = self.vfs.fmt.label(&key.loc);
                ticket.finish(with_source(b.finish(label, key), &src));
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
        let d = dir_of(&idx, dir)?;
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
        self.ensure_password(&idx, [id]);
        self.fmt.open_entry(&self.src(&idx), &idx, id)
    }

    fn space(&self, _at: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("archive: read-only, no free space"))
    }

    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }

    /// Наблюдение за файлом архива: каталог `outer` через `LocalFs::watch`, свой поток
    /// отбирает события этого файла. Файл изменился — `Overflow` (hub перечитает листинг,
    /// а он — новый индекс), удалён или переименован — `Gone`. У вложенного архива —
    /// за файлом на диске внизу цепочки (S8-31): перечитанный листинг пройдёт её заново.
    fn watch(&self, dir: &VfsPath, tx: WatchTx) -> Result<WatchGuard, VfsError> {
        let outer = base_of(outer_of(dir)?);
        if !outer.is_local() {
            return Err(VfsError::Unsupported("archive on a server: no watch"));
        }
        let (Some(parent), Some(name)) = (outer.path.parent(), outer.path.file_name()) else {
            return Err(VfsError::Unsupported("archive: no parent folder"));
        };
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

    /// План по индексу из кэша. Файлы из разных архивов или без индекса — `None`. I/O —
    /// только вопрос пароля зашифрованных записей и его проверка (поток job).
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
        self.ensure_password(&idx, pairs.iter().map(|&(_, id)| id));
        self.fmt.plan(&self.src(&idx), &idx, &pairs, parallel)
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
        let d = idx.follow(idx.lookup(&dir.path)?);
        let n = idx.node(idx.child(d, &e.name)?);
        // У каталога «Сжато» пусто, как и размер (`<DIR>`).
        (!n.kind.is_dir_like()).then_some(ExtCell::Size(n.packed))
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
