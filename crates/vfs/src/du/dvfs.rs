//! Вкладка `du:` (F01 §2.3): реестр открытых анализов [`DuRegistry`] и backend
//! [`DuVfs`] схемы `du`.
//!
//! `Location { backend: "du", path: "/<snap_id>/<rel>" }`: `snap_id` — номер анализа в
//! реестре, `rel` — путь от корня анализа. Листинг каталога — подкаталоги с размерами
//! из снимка (`SIZE_KNOWN`; недосчитанные — `SIZE_PARTIAL`, «≥ X») и файлы живым
//! листингом этого одного каталога. `stat`/`open_read`/`local_path` — к настоящему
//! пути, поэтому F3, F4 и F8 работают штатно.
//!
//! Удалённое после скана (F8, F01-06) вычитается из снимка без пересчёта: перечитанный
//! каталог сравнивается с прошлым листингом — исчезнувшие подкаталоги и файлы уходят
//! из сумм каталога и всех предков.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use compact_str::CompactString;
use rustc_hash::{FxHashMap, FxHashSet};
use urfin_core::{AppState, Attrs, CancelToken, Entry, EntryKind, JobId, Location};

use super::model::{DirId, DuNode, DuSnapshot, DuTree, NodeFlags, SizeMode};
use crate::api::{
    Caps, ListOpts, ListStream, Metadata, Space, Vfs, VfsError, VfsPath, VfsRead, WatchGuard,
    WatchTx,
};
use crate::local::LocalFs;

/// Схема вкладки анализа места.
pub const SCHEME: &str = "du";

/// Номер открытого анализа (`du:/<snap_id>/…`).
pub type SnapId = u32;

/// Сводка анализа для UI (путь, итоги, состояние скана).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuInfo {
    pub id: SnapId,
    /// Корень анализа.
    pub root: PathBuf,
    /// Каталог вкладки на диске (корень + `rel`).
    pub dir: PathBuf,
    /// Файлов в анализе.
    pub files: u64,
    /// Размер корня (на диске или логический — `[du] size`).
    pub bytes: u64,
    /// Идёт подсчёт.
    pub scanning: bool,
    /// Итог неполный: подсчёт идёт или был остановлен («≥ X»).
    pub partial: bool,
    /// Открыт из кэша (дата подсчёта — `scanned_at`).
    pub from_cache: bool,
    /// Конец подсчёта, нс от Unix epoch; 0 — ещё считается.
    pub scanned_at: i64,
    pub took_ms: u32,
    /// Каталогов без доступа.
    pub denied: usize,
}

/// Открытые анализы. Один на процесс: его делят backend [`DuVfs`] (листинг, F3/F4),
/// handler `du` и UI (через [`DuState`]). Блокировка — короткая, без I/O под ней.
#[derive(Default)]
pub struct DuRegistry {
    slots: Mutex<Vec<Slot>>,
    /// Растёт на каждом изменении (тик скана, конец, вычитание удалённого) — ключ
    /// перерисовки UI.
    rev: AtomicU64,
    /// Только что открытые вкладки: вид по умолчанию (по размеру ↓) им ставит handler
    /// `du` в начале следующего pump, когда `tab.new` и `panel.go` уже выполнились.
    opened: Mutex<Vec<Location>>,
}

impl std::fmt::Debug for DuRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuRegistry")
            .field("rev", &self.rev())
            .finish_non_exhaustive()
    }
}

struct Slot {
    root: PathBuf,
    src: Src,
    job: Option<JobId>,
    from_cache: bool,
    /// Прошлый живой листинг файлов каталога: имя → учтённый размер. Отличие при
    /// следующем листинге — удалённое/добавленное после скана.
    seen: FxHashMap<DirId, FxHashMap<CompactString, u64>>,
    /// Снимок изменён после последней записи кэша.
    dirty: bool,
}

enum Src {
    /// Идёт скан: живое дерево на атомиках.
    Live(Arc<DuTree>),
    Frozen(Box<Frozen>),
}

/// Снимок с индексом детей и удалёнными после скана каталогами.
struct Frozen {
    snap: DuSnapshot,
    /// Дети каталога `d` — `kids[start[d]..start[d + 1]]`.
    start: Vec<u32>,
    kids: Vec<DirId>,
    gone: FxHashSet<DirId>,
}

impl Frozen {
    fn new(snap: DuSnapshot) -> Frozen {
        let n = snap.dirs.len();
        let mut start = vec![0u32; n + 1];
        for d in snap.dirs.iter().skip(1) {
            if let Some(c) = start.get_mut(d.parent as usize + 1) {
                *c += 1;
            }
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut kids = vec![0; start[n] as usize];
        for (id, d) in (0u32..).zip(&snap.dirs).skip(1) {
            if let Some(at) = fill.get_mut(d.parent as usize) {
                kids[*at as usize] = id;
                *at += 1;
            }
        }
        Frozen {
            snap,
            start,
            kids,
            gone: FxHashSet::default(),
        }
    }

    fn children(&self, d: DirId) -> impl Iterator<Item = DirId> + '_ {
        let d = d as usize;
        let (a, b) = (
            self.start.get(d).copied().unwrap_or(0) as usize,
            self.start.get(d + 1).copied().unwrap_or(0) as usize,
        );
        self.kids[a..b]
            .iter()
            .copied()
            .filter(|k| !self.gone.contains(k))
    }

    fn child(&self, d: DirId, name: &str) -> Option<DirId> {
        self.children(d).find(|&k| self.snap.name(k) == name)
    }

    /// Прибавить `delta` (может быть отрицательной) к каталогу и всем предкам.
    fn add(&mut self, d: DirId, disk: i64, logical: i64, files: i64) {
        let mut cur = d;
        while let Some(n) = self.snap.dirs.get_mut(cur as usize) {
            n.disk = n.disk.saturating_add_signed(disk);
            n.logical = n.logical.saturating_add_signed(logical);
            n.files =
                u32::try_from(i64::from(n.files).saturating_add(files).max(0)).unwrap_or(u32::MAX);
            cur = n.parent;
        }
    }
}

impl Src {
    fn node(&self, d: DirId) -> Option<DuNode> {
        match self {
            Src::Live(t) => t.get(d),
            Src::Frozen(f) => f.snap.dirs.get(d as usize).copied(),
        }
    }

    fn size_mode(&self) -> SizeMode {
        match self {
            Src::Live(_) => SizeMode::Disk,
            Src::Frozen(f) => f.snap.size_mode,
        }
    }

    /// Каталог по пути от корня.
    fn resolve(&self, rel: &[CompactString]) -> Option<DirId> {
        let mut cur: DirId = 0;
        for name in rel {
            cur = match self {
                Src::Live(t) => live_child(t, cur, name)?,
                Src::Frozen(f) => f.child(cur, name)?,
            };
        }
        Some(cur)
    }

    /// Подкаталоги `d`: имя → узел.
    fn subdirs(&self, d: DirId) -> FxHashMap<CompactString, (DirId, DuNode)> {
        match self {
            Src::Live(t) => (1..t.len())
                .filter_map(|id| t.get(id).filter(|n| n.parent == d).map(|n| (id, n)))
                .map(|(id, n)| (CompactString::from(t.name(id)), (id, n)))
                .collect(),
            Src::Frozen(f) => f
                .children(d)
                .map(|k| (f.snap.name(k).into(), (k, f.snap.dirs[k as usize])))
                .collect(),
        }
    }
}

/// Подкаталог `name` у `parent` живого дерева (O(n) — только во время скана).
fn live_child(t: &DuTree, parent: DirId, name: &str) -> Option<DirId> {
    (1..t.len()).find(|&id| t.get(id).is_some_and(|n| n.parent == parent) && t.name(id) == name)
}

/// Размер узла по режиму.
fn node_size(n: &DuNode, mode: SizeMode) -> u64 {
    match mode {
        SizeMode::Disk => n.disk,
        SizeMode::Logical => n.logical,
    }
}

impl DuRegistry {
    pub fn new() -> Arc<DuRegistry> {
        Arc::new(DuRegistry::default())
    }

    fn slots(&self) -> MutexGuard<'_, Vec<Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.rev.fetch_add(1, Ordering::Release);
    }

    /// Номер изменения: растёт на тике скана, конце, вычитании удалённого.
    pub fn rev(&self) -> u64 {
        self.rev.load(Ordering::Acquire)
    }

    /// Открытый анализ корня `root` (регистр не важен).
    pub fn find(&self, root: &Path) -> Option<SnapId> {
        let key = root.to_string_lossy().to_lowercase();
        self.slots()
            .iter()
            .position(|s| s.root.to_string_lossy().to_lowercase() == key)
            .and_then(|i| u32::try_from(i).ok())
    }

    /// Новый анализ, который сейчас считается (`tree` наполняет задача `job`).
    pub fn open_live(&self, root: PathBuf, tree: Arc<DuTree>, job: JobId) -> SnapId {
        let mut slots = self.slots();
        let id = u32::try_from(slots.len()).unwrap_or(u32::MAX);
        slots.push(Slot {
            root,
            src: Src::Live(tree),
            job: Some(job),
            from_cache: false,
            seen: FxHashMap::default(),
            dirty: false,
        });
        drop(slots);
        self.bump();
        id
    }

    /// Новый анализ из готового снимка (кэш, фикстура).
    pub fn open_snapshot(&self, snap: DuSnapshot, from_cache: bool) -> SnapId {
        let mut slots = self.slots();
        let id = u32::try_from(slots.len()).unwrap_or(u32::MAX);
        slots.push(Slot {
            root: snap.root.clone(),
            src: Src::Frozen(Box::new(Frozen::new(snap))),
            job: None,
            from_cache,
            seen: FxHashMap::default(),
            dirty: false,
        });
        drop(slots);
        self.bump();
        id
    }

    /// Пересчёт анализа `id` заново: живое дерево вместо снимка.
    pub fn restart(&self, id: SnapId, tree: Arc<DuTree>, job: JobId) {
        if let Some(s) = self.slots().get_mut(id as usize) {
            s.src = Src::Live(tree);
            s.job = Some(job);
            s.from_cache = false;
            s.seen.clear();
            s.dirty = false;
        }
        self.bump();
    }

    /// Вкладка на `loc` открывается сейчас (см. [`DuRegistry::take_opened`]).
    pub fn opened(&self, loc: Location) {
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(loc);
    }

    /// Открытые после прошлого вызова вкладки.
    pub fn take_opened(&self) -> Vec<Location> {
        std::mem::take(&mut *self.opened.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Тик скана: дерево изменилось.
    pub fn touch(&self) {
        self.bump();
    }

    /// Скан закончен (или остановлен — снимок с `PARTIAL`).
    pub fn finish(&self, id: SnapId, snap: DuSnapshot) {
        if let Some(s) = self.slots().get_mut(id as usize) {
            s.src = Src::Frozen(Box::new(Frozen::new(snap)));
            s.job = None;
            s.seen.clear();
            s.dirty = false;
        }
        self.bump();
    }

    /// Задача подсчёта анализа, пока идёт.
    pub fn job(&self, id: SnapId) -> Option<JobId> {
        self.slots().get(id as usize).and_then(|s| s.job)
    }

    /// Корень анализа.
    pub fn root(&self, id: SnapId) -> Option<PathBuf> {
        self.slots().get(id as usize).map(|s| s.root.clone())
    }

    /// Снимок для записи в кэш, если он изменился после прошлой (флаг снимается).
    pub fn take_dirty(&self, id: SnapId) -> Option<DuSnapshot> {
        let mut slots = self.slots();
        let s = slots.get_mut(id as usize)?;
        match &s.src {
            Src::Frozen(f) if s.dirty => {
                s.dirty = false;
                Some(f.snap.clone())
            }
            _ => None,
        }
    }

    /// Анализы, изменённые после записи кэша.
    pub fn dirty(&self) -> Vec<SnapId> {
        (0u32..)
            .zip(self.slots().iter())
            .filter(|(_, s)| s.dirty)
            .map(|(i, _)| i)
            .collect()
    }

    /// Настоящий путь адреса `du:` (`None` — не `du:` или анализа нет).
    pub fn local(&self, loc: &Location) -> Option<PathBuf> {
        let (id, rel) = parse(loc)?;
        let root = self.root(id)?;
        Some(join_rel(&root, &rel))
    }

    /// Сводка для UI по адресу вкладки.
    pub fn info(&self, loc: &Location) -> Option<DuInfo> {
        let (id, rel) = parse(loc)?;
        let slots = self.slots();
        let s = slots.get(id as usize)?;
        let total = s.src.node(0)?;
        let mode = s.src.size_mode();
        let (scanned_at, took_ms, partial, denied) = match &s.src {
            Src::Live(t) => (0, 0, true, t.denied_count()),
            Src::Frozen(f) => (
                f.snap.scanned_at,
                f.snap.took_ms,
                f.snap.flags & DuSnapshot::PARTIAL != 0,
                f.snap.denied.len(),
            ),
        };
        Some(DuInfo {
            id,
            root: s.root.clone(),
            dir: join_rel(&s.root, &rel),
            files: u64::from(total.files),
            bytes: node_size(&total, mode),
            scanning: s.job.is_some(),
            partial,
            from_cache: s.from_cache,
            scanned_at,
            took_ms,
            denied,
        })
    }

    /// Листинг каталога вкладки: подкаталоги из снимка, файлы — живым листингом.
    /// I/O — вне блокировки.
    pub fn list(&self, loc: &Location, fs: &dyn Vfs) -> Result<Vec<Entry>, VfsError> {
        let (id, rel) = parse(loc).ok_or(VfsError::NotFound(loc.path.clone()))?;
        let (root, dir, subdirs, mode, large, live) = {
            let slots = self.slots();
            let s = slots
                .get(id as usize)
                .ok_or_else(|| VfsError::NotFound(loc.path.clone()))?;
            let dir = s
                .src
                .resolve(&rel)
                .ok_or_else(|| VfsError::NotFound(loc.path.clone()))?;
            // Крупные файлы каталога: размер на диске из снимка.
            let large: FxHashMap<CompactString, (u64, u64)> = match &s.src {
                Src::Frozen(f) => f
                    .snap
                    .files
                    .iter()
                    .filter(|r| r.dir == dir)
                    .map(|r| (f.snap.file_name(r).into(), (r.disk, r.logical)))
                    .collect(),
                Src::Live(_) => FxHashMap::default(),
            };
            (
                s.root.clone(),
                dir,
                s.src.subdirs(dir),
                s.src.size_mode(),
                large,
                matches!(s.src, Src::Live(_)),
            )
        };
        let path = join_rel(&root, &rel);
        let on_fs = read_all(fs, &Location::local(&path))?;
        let mut out = Vec::with_capacity(on_fs.len());
        let mut files: FxHashMap<CompactString, u64> = FxHashMap::default();
        let mut on_disk: FxHashSet<CompactString> = FxHashSet::default();
        for mut e in on_fs {
            if e.kind == EntryKind::Dir {
                on_disk.insert(e.name.clone());
                match subdirs.get(&e.name) {
                    Some((_, n)) => {
                        e.size = node_size(n, mode);
                        let partial = if n.flags.has(NodeFlags::DONE) {
                            0
                        } else {
                            Attrs::SIZE_PARTIAL
                        };
                        e.attrs = Attrs(e.attrs.0 | Attrs::SIZE_KNOWN | partial);
                    }
                    // Подкаталог ещё не дошёл до скана: «≥ 0».
                    None if live => {
                        e.attrs = Attrs(e.attrs.0 | Attrs::SIZE_KNOWN | Attrs::SIZE_PARTIAL);
                    }
                    None => {}
                }
            } else if !e.kind.is_dir_like() {
                if let Some(&(disk, logical)) = large.get(&e.name) {
                    e.size = match mode {
                        SizeMode::Disk => disk,
                        SizeMode::Logical => logical,
                    };
                }
                files.insert(e.name.clone(), e.size);
            }
            out.push(e);
        }
        if !live {
            self.reconcile(id, dir, &subdirs, &on_disk, files);
        }
        Ok(out)
    }

    /// Вычесть удалённое после скана (F01-06 §3.7): исчезнувшие подкаталоги — их
    /// суммы, файлы — разница с прошлым листингом каталога; у каталога и всех предков.
    fn reconcile(
        &self,
        id: SnapId,
        dir: DirId,
        subdirs: &FxHashMap<CompactString, (DirId, DuNode)>,
        on_disk: &FxHashSet<CompactString>,
        files: FxHashMap<CompactString, u64>,
    ) {
        let mut slots = self.slots();
        let Some(s) = slots.get_mut(id as usize) else {
            return;
        };
        let Src::Frozen(f) = &mut s.src else {
            return;
        };
        let mut changed = false;
        for (name, (k, _)) in subdirs {
            if on_disk.contains(name) || f.gone.contains(k) {
                continue;
            }
            // Суммы — текущие (поддерево могло уже уменьшиться); узел обнуляется,
            // чтобы снимок из кэша не вычел его второй раз.
            let Some(n) = f.snap.dirs.get(*k as usize).copied() else {
                continue;
            };
            f.add(
                n.parent,
                -i64::try_from(n.disk).unwrap_or(i64::MAX),
                -i64::try_from(n.logical).unwrap_or(i64::MAX),
                -i64::from(n.files),
            );
            if let Some(m) = f.snap.dirs.get_mut(*k as usize) {
                m.disk = 0;
                m.logical = 0;
                m.files = 0;
            }
            f.gone.insert(*k);
            changed = true;
        }
        if let Some(prev) = s.seen.get(&dir) {
            let mut bytes: i64 = 0;
            let mut count: i64 = 0;
            for (name, &was) in prev {
                match files.get(name) {
                    None => {
                        bytes -= i64::try_from(was).unwrap_or(i64::MAX);
                        count -= 1;
                    }
                    Some(&now) if now != was => {
                        bytes += i64::try_from(now).unwrap_or(i64::MAX)
                            - i64::try_from(was).unwrap_or(i64::MAX);
                    }
                    Some(_) => {}
                }
            }
            for (name, &now) in &files {
                if !prev.contains_key(name) {
                    bytes += i64::try_from(now).unwrap_or(i64::MAX);
                    count += 1;
                }
            }
            if bytes != 0 || count != 0 {
                f.add(dir, bytes, bytes, count);
                changed = true;
            }
        }
        s.seen.insert(dir, files);
        if changed {
            s.dirty = true;
            drop(slots);
            self.bump();
        }
    }
}

/// Весь каталог одним списком.
fn read_all(fs: &dyn Vfs, dir: &Location) -> Result<Vec<Entry>, VfsError> {
    let mut s = fs.list(dir, &ListOpts::default())?;
    let mut out = Vec::new();
    let cancel = CancelToken::new();
    while s.next_batch(&mut out, &cancel)? {}
    Ok(out)
}

/// Адрес каталога `rel` анализа `id`.
pub fn location(id: SnapId, rel: &Path) -> Location {
    let mut path = PathBuf::from(format!("/{id}"));
    path.push(rel);
    Location {
        backend: SCHEME.into(),
        authority: None,
        path,
        outer: None,
    }
}

/// `du:/<id>/<rel>` → номер анализа и компоненты `rel`.
pub fn parse(loc: &Location) -> Option<(SnapId, Vec<CompactString>)> {
    if loc.backend != SCHEME || loc.outer.is_some() {
        return None;
    }
    let mut parts = loc.path.components().filter_map(|c| match c {
        Component::Normal(s) => Some(s.to_str().map(CompactString::from)),
        Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        Component::ParentDir => Some(None),
    });
    let id = parts.next()??.parse().ok()?;
    let rel = parts.collect::<Option<Vec<_>>>()?;
    Some((id, rel))
}

fn join_rel(root: &Path, rel: &[CompactString]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in rel {
        p.push(crate::name::to_os(c, crate::name::may_be_escaped(c)));
    }
    p
}

/// Ext-состояние вкладок `du:` (S0 v6 №3): общий реестр анализов. Кладёт handler `du`.
#[derive(Clone, Debug)]
pub struct DuState {
    pub reg: Arc<DuRegistry>,
}

/// Реестр анализов из состояния (после первого pump handler `du`).
pub fn registry(s: &AppState) -> Option<&Arc<DuRegistry>> {
    s.ext.get::<DuState>().map(|d| &d.reg)
}

/// Настоящий путь адреса `du:` как `Location::local` (иначе `None`): F8, Enter, F3/F4
/// во вкладке анализа идут к файлам на диске.
pub fn localize(s: &AppState, loc: &Location) -> Option<Location> {
    (loc.backend == SCHEME)
        .then(|| registry(s)?.local(loc))
        .flatten()
        .map(Location::local)
}

/// Backend схемы `du:`.
#[derive(Debug)]
pub struct DuVfs {
    reg: Arc<DuRegistry>,
    fs: LocalFs,
}

impl DuVfs {
    pub fn new(reg: Arc<DuRegistry>) -> DuVfs {
        DuVfs {
            reg,
            fs: LocalFs::new(),
        }
    }

    fn real(&self, p: &VfsPath) -> Result<Location, VfsError> {
        self.reg
            .local(p)
            .map(Location::local)
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))
    }
}

/// Готовый список одним батчем.
struct Ready(Option<Vec<Entry>>);

impl ListStream for Ready {
    fn next_batch(
        &mut self,
        out: &mut Vec<Entry>,
        _cancel: &CancelToken,
    ) -> Result<bool, VfsError> {
        if let Some(v) = self.0.take() {
            out.extend(v);
        }
        Ok(false)
    }

    fn size_hint(&self) -> Option<u32> {
        self.0.as_ref().and_then(|v| u32::try_from(v.len()).ok())
    }
}

impl Vfs for DuVfs {
    fn scheme(&self) -> &'static str {
        SCHEME
    }

    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps(Caps::LOCAL_PATH)
    }

    fn list(&self, dir: &VfsPath, _opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Ok(Box::new(Ready(Some(self.reg.list(dir, &self.fs)?))))
    }

    fn stat(&self, p: &VfsPath, follow: bool) -> Result<Metadata, VfsError> {
        self.fs.stat(&self.real(p)?, follow)
    }

    fn stat_entry(&self, dir: &VfsPath, name: &str, attrs_hint: Attrs) -> Result<Entry, VfsError> {
        self.fs.stat_entry(&self.real(dir)?, name, attrs_hint)
    }

    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        self.fs.open_read(&self.real(p)?)
    }

    fn space(&self, at: &VfsPath) -> Result<Space, VfsError> {
        self.fs.space(&self.real(at)?)
    }

    fn local_path(&self, p: &VfsPath) -> Option<PathBuf> {
        self.reg.local(p)
    }

    /// Изменения на диске — задача F01-10 (RDCW); без опроса: вкладку обновляют тики
    /// скана и `panel.refresh`.
    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Ok(WatchGuard::new(()))
    }
}

#[cfg(test)]
#[path = "dvfs_tests.rs"]
mod tests;
