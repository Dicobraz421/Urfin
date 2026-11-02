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

use super::dups;
use super::filters::{self, TypeGroup, View};
use super::model::{
    DirId, DuFilter, DuNode, DuSnapshot, DuTree, DuType, DupGroup, DupLevel, FileRec, NodeFlags,
    SizeMode,
};
use super::views::{Check, Hint, MapDir, Row};
use crate::api::{
    Caps, ColumnSpec, ColumnView, ExtCell, ListOpts, ListStream, Metadata, Space, Vfs, VfsError,
    VfsPath, VfsRead, WatchGuard, WatchTx,
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
    /// Каталогов с «●»: изменились после подсчёта (F01-11).
    pub changed: usize,
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
    /// Правила «Временных», раскрытые для окружения (по умолчанию — процесса).
    rules: std::sync::OnceLock<Vec<filters::Expanded>>,
    /// Порог дубликатов, байт (`[du] dup_min`); 0 — по умолчанию.
    dup_min: AtomicU64,
    /// Анализы, которым карта ждёт каталоги «Временных» (считает задача Runtime).
    temp_queue: Mutex<Vec<SnapId>>,
    /// Снимки-фикстуры (`--screens`, тесты): без наблюдения и дочитки (F01 §3.3).
    fixed: Mutex<FxHashSet<SnapId>>,
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
    /// Вид последнего листинга (`View::query`): тот же снова — перечитывание (проверка
    /// строк на диске), другой — показ из снимка без I/O.
    last_view: Option<String>,
    /// Раскрытые группы «Типов»: бит `DuType as u8`.
    open: u16,
    /// Подсказки строк последнего листинга «Временных».
    hints: FxHashMap<CompactString, Hint>,
    /// Дубликаты (F01-09): считаются при первом показе вида, сбрасываются пересчётом.
    dups: Option<Dups>,
    /// Правок снимка (вычитание удалённого, новый снимок): ключ годности `temp`.
    edits: u64,
    /// Каталоги «Временных» для карты и номер правки, на котором посчитаны.
    temp: Option<(u64, Arc<FxHashSet<DirId>>)>,
    /// Карта уже попросила посчитать `temp`.
    temp_wanted: bool,
}

/// Дубликаты анализа (F01 §3.6): группы, задача хэширования, отметки к установке.
#[derive(Debug, Default)]
struct Dups {
    groups: Vec<DupGroup>,
    /// Частичный хэш уже запускался (или группы задала фикстура).
    headed: bool,
    job: Option<JobId>,
    /// Строки к отметке после полного хэша: все копии, кроме первой (решение
    /// владельца 2026-10-22, F01 §8).
    marks: Vec<CompactString>,
}

/// Сводка дубликатов вкладки для строки внизу и статуса (`screen-dudups`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DupsInfo {
    /// Групп на экране.
    pub groups: usize,
    /// Из них совпали по полному хэшу.
    pub checked: usize,
    /// Файлов в группах.
    pub files: usize,
    /// Идёт хэширование.
    pub hashing: bool,
}

/// Заголовок группы дубликатов: копий на диске и уровень проверки.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DupHead {
    pub copies: u32,
    pub level: DupLevel,
}

impl Slot {
    fn new(root: PathBuf, src: Src, job: Option<JobId>, from_cache: bool) -> Slot {
        Slot {
            root,
            src,
            job,
            from_cache,
            seen: FxHashMap::default(),
            dirty: false,
            last_view: None,
            open: 0,
            hints: FxHashMap::default(),
            dups: None,
            edits: 0,
            temp: None,
            temp_wanted: false,
        }
    }
}

enum Src {
    /// Идёт скан: живое дерево на атомиках.
    Live(Arc<DuTree>),
    /// Снимок; фоновые чтения (каталоги «Временных» для карты) держат свою ссылку,
    /// правка при чужой ссылке — копия (`Arc::make_mut`).
    Frozen(Arc<Frozen>),
}

/// Снимок с индексами детей и файлов каталогов, удалённое после скана.
#[derive(Clone)]
pub(super) struct Frozen {
    pub(super) snap: DuSnapshot,
    /// Дети каталога `d` — `kids[start[d]..start[d + 1]]`.
    start: Vec<u32>,
    kids: Vec<DirId>,
    /// Крупные файлы каталога `d` (индексы `snap.files`, по размеру ↓) —
    /// `fidx[fstart[d]..fstart[d + 1]]`.
    fstart: Vec<u32>,
    fidx: Vec<u32>,
    pub(super) gone: FxHashSet<DirId>,
    /// Крупные файлы, удалённые после скана.
    pub(super) gone_files: FxHashSet<u32>,
    /// Живых каталогов с «●» ([`Frozen::recount`]).
    pub(super) changed: usize,
}

/// Индекс «родитель → элементы»: `start` длиной `n + 1` и элементы по родителю.
fn index(n: usize, parents: impl Iterator<Item = (u32, u32)> + Clone) -> (Vec<u32>, Vec<u32>) {
    let mut start = vec![0u32; n + 1];
    for (p, _) in parents.clone() {
        if let Some(c) = start.get_mut(p as usize + 1) {
            *c += 1;
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut items = vec![0; start[n] as usize];
    for (p, id) in parents {
        if let Some(at) = fill.get_mut(p as usize) {
            items[*at as usize] = id;
            *at += 1;
        }
    }
    (start, items)
}

impl Frozen {
    fn new(snap: DuSnapshot) -> Frozen {
        let n = snap.dirs.len();
        let (start, kids) = index(
            n,
            (0u32..)
                .zip(&snap.dirs)
                .skip(1)
                .map(|(id, d)| (d.parent, id)),
        );
        // `files` уже по размеру ↓ — внутри каталога порядок тот же.
        let (fstart, fidx) = index(n, (0u32..).zip(&snap.files).map(|(i, f)| (f.dir, i)));
        let mut f = Frozen {
            snap,
            start,
            kids,
            fstart,
            fidx,
            gone: FxHashSet::default(),
            gone_files: FxHashSet::default(),
            changed: 0,
        };
        f.recount();
        f
    }

    pub(super) fn children(&self, d: DirId) -> impl Iterator<Item = DirId> + '_ {
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

    /// Крупные файлы каталога (индексы `snap.files`), удалённые — тоже.
    pub(super) fn files_of(&self, d: DirId) -> impl Iterator<Item = u32> + '_ {
        let d = d as usize;
        let (a, b) = (
            self.fstart.get(d).copied().unwrap_or(0) as usize,
            self.fstart.get(d + 1).copied().unwrap_or(0) as usize,
        );
        self.fidx[a..b].iter().copied()
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

    /// Каталог удалён: его суммы — из родителя и предков, узел обнуляется (снимок из
    /// кэша не вычтет его второй раз), крупные файлы внутри — удалены.
    fn drop_dir(&mut self, k: DirId) {
        if self.gone.contains(&k) {
            return;
        }
        let Some(n) = self.snap.dirs.get(k as usize).copied() else {
            return;
        };
        self.add(
            n.parent,
            -i64::try_from(n.disk).unwrap_or(i64::MAX),
            -i64::try_from(n.logical).unwrap_or(i64::MAX),
            -i64::from(n.files),
        );
        if let Some(m) = self.snap.dirs.get_mut(k as usize) {
            m.disk = 0;
            m.logical = 0;
            m.files = 0;
        }
        self.gone.insert(k);
        let inside: Vec<u32> = (0u32..)
            .zip(&self.snap.files)
            .filter(|(i, f)| !self.gone_files.contains(i) && self.dead(f.dir))
            .map(|(i, _)| i)
            .collect();
        for i in inside {
            // Суммы каталогов уже вычтены с узлом — только типы.
            self.forget_file(i);
        }
        self.recount();
    }

    /// Крупный файл ушёл из снимка: суммы по типам.
    fn forget_file(&mut self, i: u32) {
        let Some(f) = self.snap.files.get(i as usize).copied() else {
            return;
        };
        if !self.gone_files.insert(i) {
            return;
        }
        let ty = f.ty as usize;
        let mut sums = vec![&mut self.snap.types];
        if f.attrs & FileRec::IN_BUILD != 0 {
            sums.push(&mut self.snap.build);
        }
        for t in sums {
            t.disk[ty] = t.disk[ty].saturating_sub(f.disk);
            t.files[ty] = t.files[ty].saturating_sub(1);
        }
    }

    /// Сумма и число файлов группы «Типов»: сборки — `build`, тип — вне сборок.
    pub(super) fn group_sums(&self, g: TypeGroup) -> (u64, u64) {
        let (t, b) = (&self.snap.types, &self.snap.build);
        match g {
            TypeGroup::Build => (b.disk.iter().sum(), b.files.iter().sum()),
            TypeGroup::Type(ty) => {
                let i = ty as usize;
                (
                    t.disk[i].saturating_sub(b.disk[i]),
                    t.files[i].saturating_sub(b.files[i]),
                )
            }
        }
    }

    /// Крупный файл удалён: из сумм каталога, предков и типов.
    fn drop_file(&mut self, i: u32) {
        let Some(f) = self.snap.files.get(i as usize).copied() else {
            return;
        };
        if self.gone_files.contains(&i) {
            return;
        }
        self.add(
            f.dir,
            -i64::try_from(f.disk).unwrap_or(i64::MAX),
            -i64::try_from(f.logical).unwrap_or(i64::MAX),
            -1,
        );
        self.forget_file(i);
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
        slots.push(Slot::new(root, Src::Live(tree), Some(job), false));
        drop(slots);
        self.bump();
        id
    }

    /// Новый анализ из готового снимка (кэш, фикстура).
    pub fn open_snapshot(&self, snap: DuSnapshot, from_cache: bool) -> SnapId {
        let mut slots = self.slots();
        let id = u32::try_from(slots.len()).unwrap_or(u32::MAX);
        slots.push(Slot::new(
            snap.root.clone(),
            Src::Frozen(Arc::new(Frozen::new(snap))),
            None,
            from_cache,
        ));
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
            s.dups = None;
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
            s.src = Src::Frozen(Arc::new(Frozen::new(snap)));
            s.edits += 1;
            s.job = None;
            s.seen.clear();
            s.dirty = false;
            s.dups = None;
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

    /// Правила «Временных» (по умолчанию — окружение процесса).
    pub fn rules(&self) -> &[filters::Expanded] {
        match self.rules.get() {
            Some(r) => r,
            None => filters::rules(),
        }
    }

    /// Свои правила «Временных» (тесты, фикстура `--screens`); только до первого
    /// использования.
    pub fn set_rules(&self, rules: Vec<filters::Expanded>) {
        let _ = self.rules.set(rules);
    }

    /// Группа «Типов» `g` анализа вкладки `loc` раскрыта.
    pub fn is_open(&self, loc: &Location, g: TypeGroup) -> bool {
        self.open_mask(loc) & g.bit() != 0
    }

    /// Раскрытые группы «Типов» анализа вкладки `loc`: биты [`TypeGroup::bit`].
    pub fn open_mask(&self, loc: &Location) -> u16 {
        parse(loc)
            .and_then(|(id, _)| self.slots().get(id as usize).map(|s| s.open))
            .unwrap_or(0)
    }

    /// Раскрыть или свернуть группу «Типов» (Enter на группе); `false` — анализа нет.
    /// Следующий листинг — показ из снимка, не перечитывание (без I/O).
    pub fn toggle_group(&self, id: SnapId, g: TypeGroup) -> bool {
        let ok = self
            .slots()
            .get_mut(id as usize)
            .map(|s| {
                s.open ^= g.bit();
                s.last_view = None;
            })
            .is_some();
        self.bump();
        ok
    }

    /// Файлов в группе «Типов» (колонка «Файлов»).
    pub fn type_files(&self, loc: &Location, g: TypeGroup) -> Option<u64> {
        let (id, _) = parse(loc)?;
        let slots = self.slots();
        match &slots.get(id as usize)?.src {
            Src::Frozen(f) => Some(f.group_sums(g).1),
            Src::Live(_) => None,
        }
    }

    /// Порог дубликатов (`[du] dup_min`); по умолчанию 1 MiB.
    pub fn set_dup_min(&self, bytes: u64) {
        self.dup_min.store(bytes, Ordering::Relaxed);
    }

    fn dup_min(&self) -> u64 {
        match self.dup_min.load(Ordering::Relaxed) {
            0 => 1 << 20,
            n => n,
        }
    }

    /// Файлы для хэширования (вне UI-потока: группы считаются здесь же, под
    /// блокировкой, без I/O): `full` — полный хэш непроверенных групп (Ctrl+H), иначе —
    /// частичный, один раз на анализ. Корень анализа и пути файлов от него; `None` —
    /// делать нечего.
    pub fn dup_plan(&self, id: SnapId, full: bool) -> Option<(PathBuf, Vec<(u32, CompactString)>)> {
        let min = self.dup_min();
        let mut slots = self.slots();
        let s = slots.get_mut(id as usize)?;
        let Src::Frozen(f) = &s.src else {
            return None;
        };
        let d = s.dups.get_or_insert_with(|| Dups {
            groups: f.dup_candidates(min),
            ..Dups::default()
        });
        if !full && std::mem::replace(&mut d.headed, true) {
            return None;
        }
        let mut memo = FxHashMap::default();
        let plan: Vec<(u32, CompactString)> = d
            .groups
            .iter()
            .filter(|g| {
                if full {
                    g.level != DupLevel::Full
                } else {
                    g.level == DupLevel::Size
                }
            })
            .flat_map(|g| g.files.iter().copied())
            .filter(|&i| f.live_file(i))
            .map(|i| (i, f.file_rel(i, &mut memo)))
            .collect();
        (!plan.is_empty()).then(|| (s.root.clone(), plan))
    }

    /// Хэши готовы: группы разбиваются (без хэша — файл уходит), у частичного —
    /// уровень `Partial` (файлы до 64 KiB — `Full`), у полного — `Full`; совпавшие по
    /// полному хэшу — к отметке (все копии, кроме первой). Задача снята.
    pub fn dup_apply(&self, id: SnapId, hashes: &FxHashMap<u32, u128>, full: bool) {
        let mut slots = self.slots();
        let Some(s) = slots.get_mut(id as usize) else {
            return;
        };
        let (Src::Frozen(f), Some(d)) = (&s.src, s.dups.as_mut()) else {
            return;
        };
        let mut memo = FxHashMap::default();
        let mut out = Vec::with_capacity(d.groups.len());
        for g in std::mem::take(&mut d.groups) {
            let touched = if full {
                g.level != DupLevel::Full
            } else {
                g.level == DupLevel::Size
            };
            if !touched {
                out.push(g);
                continue;
            }
            let level = if full {
                DupLevel::Full
            } else {
                dups::after_head(g.size)
            };
            for n in dups::split(&g, |i| hashes.get(&i).copied(), level) {
                if full {
                    d.marks
                        .extend(n.files.iter().skip(1).map(|&i| f.file_rel(i, &mut memo)));
                }
                out.push(n);
            }
        }
        dups::order(&f.snap, &mut out);
        d.groups = out;
        d.job = None;
        drop(slots);
        self.bump();
    }

    /// Задача хэширования дубликатов анализа, пока идёт.
    pub fn dup_job(&self, id: SnapId) -> Option<JobId> {
        self.slots()
            .get(id as usize)
            .and_then(|s| s.dups.as_ref())
            .and_then(|d| d.job)
    }

    /// Задача хэширования запущена (`Some`) или снята без результата (`None`).
    pub fn set_dup_job(&self, id: SnapId, job: Option<JobId>) {
        if let Some(s) = self.slots().get_mut(id as usize) {
            s.dups.get_or_insert_with(Dups::default).job = job;
        }
        self.bump();
    }

    /// Строки к отметке после полного хэша (забираются один раз).
    pub fn take_marks(&self, id: SnapId) -> Vec<CompactString> {
        self.slots()
            .get_mut(id as usize)
            .and_then(|s| s.dups.as_mut())
            .map(|d| std::mem::take(&mut d.marks))
            .unwrap_or_default()
    }

    /// Анализы с отметками к установке.
    pub fn pending_marks(&self) -> Vec<SnapId> {
        (0u32..)
            .zip(self.slots().iter())
            .filter(|(_, s)| s.dups.as_ref().is_some_and(|d| !d.marks.is_empty()))
            .map(|(i, _)| i)
            .collect()
    }

    /// Группы дубликатов готовы (фикстура `--screens`, тесты): без хэширования,
    /// отметки — как после полного хэша.
    pub fn seed_dups(&self, id: SnapId, groups: Vec<DupGroup>) {
        let mut slots = self.slots();
        let Some(s) = slots.get_mut(id as usize) else {
            return;
        };
        let Src::Frozen(f) = &s.src else {
            return;
        };
        let mut memo = FxHashMap::default();
        let marks = groups
            .iter()
            .filter(|g| g.level == DupLevel::Full)
            .flat_map(|g| g.files.iter().skip(1))
            .map(|&i| f.file_rel(i, &mut memo))
            .collect();
        s.dups = Some(Dups {
            groups,
            headed: true,
            job: None,
            marks,
        });
        drop(slots);
        self.bump();
    }

    /// Сводка дубликатов вкладки `loc` (`None` — групп ещё нет).
    pub fn dups_info(&self, loc: &Location) -> Option<DupsInfo> {
        let (id, _) = parse(loc)?;
        let slots = self.slots();
        let s = slots.get(id as usize)?;
        let (Src::Frozen(f), Some(d)) = (&s.src, s.dups.as_ref()) else {
            return None;
        };
        let mut info = DupsInfo {
            hashing: d.job.is_some(),
            ..DupsInfo::default()
        };
        for g in &d.groups {
            let n = f.live_members(g).len();
            if n > 1 {
                info.groups += 1;
                info.files += n;
                info.checked += usize::from(g.level == DupLevel::Full);
            }
        }
        Some(info)
    }

    /// Заголовки групп вкладки `loc` по номеру группы (`ctime` строки-заголовка).
    pub fn dup_heads(&self, loc: &Location) -> Vec<DupHead> {
        let Some((id, _)) = parse(loc) else {
            return Vec::new();
        };
        let slots = self.slots();
        let Some(s) = slots.get(id as usize) else {
            return Vec::new();
        };
        let (Src::Frozen(f), Some(d)) = (&s.src, s.dups.as_ref()) else {
            return Vec::new();
        };
        d.groups
            .iter()
            .map(|g| DupHead {
                copies: u32::try_from(f.live_members(g).len()).unwrap_or(u32::MAX),
                level: g.level,
            })
            .collect()
    }

    /// Подсказки строк «Временных» (последний листинг): имя строки → «что это».
    pub fn hints(&self, loc: &Location) -> FxHashMap<CompactString, Hint> {
        parse(loc)
            .and_then(|(id, _)| self.slots().get(id as usize).map(|s| s.hints.clone()))
            .unwrap_or_default()
    }

    /// Данные карты каталога вкладки (F01 §3.4); пока идёт подсчёт — `None`. Память
    /// снимка, без I/O. Каталоги «Временных» (обход всего снимка) считает задача
    /// Runtime ([`DuRegistry::take_temp_wanted`], [`DuRegistry::compute_temp`]) — до
    /// неё карта без пометок «Временных».
    pub fn map(&self, loc: &Location) -> Option<MapDir> {
        let (id, rel) = parse(loc)?;
        let rev = self.rev();
        let mut slots = self.slots();
        let s = slots.get_mut(id as usize)?;
        let d = s.src.resolve(&rel)?;
        let Src::Frozen(f) = &s.src else {
            return None;
        };
        let temp = match &s.temp {
            Some((at, t)) if *at == s.edits => Arc::clone(t),
            _ => {
                if !s.temp_wanted {
                    s.temp_wanted = true;
                    self.temp_queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(id);
                }
                Arc::default()
            }
        };
        Some(MapDir {
            key: (id, d),
            rev,
            items: f.map_items(d, &temp),
        })
    }

    /// Анализы, которым карта ждёт каталоги «Временных» (забираются один раз).
    pub fn take_temp_wanted(&self) -> Vec<SnapId> {
        std::mem::take(
            &mut *self
                .temp_queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Каталоги «Временных» анализа `id` для карты: обход снимка вне блокировки (вне
    /// UI-потока — из задачи Runtime); снимок за это время изменился — результат не
    /// нужен, карта попросит снова.
    pub fn compute_temp(&self, id: SnapId) {
        let (frozen, at) = {
            let slots = self.slots();
            let Some(s) = slots.get(id as usize) else {
                return;
            };
            let Src::Frozen(f) = &s.src else {
                return;
            };
            (Arc::clone(f), s.edits)
        };
        let dirs = frozen.temp_dirs(self.rules());
        drop(frozen);
        if let Some(s) = self.slots().get_mut(id as usize) {
            s.temp_wanted = false;
            if s.edits == at {
                s.temp = Some((at, Arc::new(dirs)));
            }
        }
        self.bump();
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
        let (scanned_at, took_ms, partial, denied, changed) = match &s.src {
            Src::Live(t) => (0, 0, true, t.denied_count(), 0),
            Src::Frozen(f) => (
                f.snap.scanned_at,
                f.snap.took_ms,
                f.snap.flags & DuSnapshot::PARTIAL != 0,
                f.snap.denied.len(),
                f.changed,
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
            changed,
        })
    }

    /// Листинг вкладки: «По папкам» — подкаталоги из снимка и файлы живым листингом
    /// каталога; фильтры (F01 §3.5) — из снимка. I/O — вне блокировки.
    pub fn list(&self, loc: &Location, fs: &dyn Vfs) -> Result<Vec<Entry>, VfsError> {
        let (id, rel, v) = view(loc).ok_or(VfsError::NotFound(loc.path.clone()))?;
        if v.filter == DuFilter::Dirs {
            self.list_dirs(id, &rel, loc, fs)
        } else {
            self.list_view(id, &v, loc, fs, now_nanos())
        }
    }

    /// Вид фильтра: строки из снимка без I/O. Тот же вид второй раз подряд —
    /// перечитывание (после операций, F01 §3.7): строки, которых нет на диске,
    /// вычитаются из снимка.
    pub(super) fn list_view(
        &self,
        id: SnapId,
        v: &View,
        loc: &Location,
        fs: &dyn Vfs,
        now: i64,
    ) -> Result<Vec<Entry>, VfsError> {
        let not_found = || VfsError::NotFound(loc.path.clone());
        let min = self.dup_min();
        let build = |s: &mut Slot, rules: &[filters::Expanded]| -> Vec<Row> {
            let mut hints = FxHashMap::default();
            if v.filter == DuFilter::Dups
                && s.dups.is_none()
                && let Src::Frozen(f) = &s.src
            {
                s.dups = Some(Dups {
                    groups: f.dup_candidates(min),
                    ..Dups::default()
                });
            }
            let groups = s.dups.as_ref().map_or(&[][..], |d| &d.groups[..]);
            let rows = match &s.src {
                Src::Frozen(f) => f.view_rows(v, now, s.open, rules, groups, &mut hints),
                Src::Live(t) if v.filter == DuFilter::Large => live_large(t),
                Src::Live(_) => Vec::new(),
            };
            s.hints = hints;
            rows
        };
        let (root, rows, again) = {
            let mut slots = self.slots();
            let s = slots.get_mut(id as usize).ok_or_else(not_found)?;
            let key = v.query();
            let again = s.last_view.as_deref() == Some(key.as_str());
            s.last_view = Some(key);
            let rows = build(s, self.rules());
            (
                s.root.clone(),
                rows,
                again && matches!(s.src, Src::Frozen(_)),
            )
        };
        let entries = |rows: Vec<Row>| rows.into_iter().map(|r| r.entry).collect();
        if !again {
            return Ok(entries(rows));
        }
        let missing: Vec<Check> = rows
            .iter()
            .filter(|r| r.check != Check::None)
            .filter(|r| {
                let path = join_name(&root, &r.entry.name);
                matches!(
                    fs.stat(&Location::local(path), false),
                    Err(VfsError::NotFound(_))
                )
            })
            .map(|r| r.check)
            .collect();
        if missing.is_empty() {
            return Ok(entries(rows));
        }
        let rows = {
            let mut slots = self.slots();
            let s = slots.get_mut(id as usize).ok_or_else(not_found)?;
            if let Src::Frozen(f) = &mut s.src {
                let f = Arc::make_mut(f);
                s.edits += 1;
                for c in missing {
                    match c {
                        Check::File(i) => f.drop_file(i),
                        Check::Dir(d) => f.drop_dir(d),
                        Check::None => {}
                    }
                }
                s.dirty = true;
            }
            build(s, self.rules())
        };
        self.bump();
        Ok(entries(rows))
    }

    /// «По папкам»: подкаталоги из снимка, файлы — живым листингом каталога.
    fn list_dirs(
        &self,
        id: SnapId,
        rel: &[CompactString],
        loc: &Location,
        fs: &dyn Vfs,
    ) -> Result<Vec<Entry>, VfsError> {
        let (root, dir, subdirs, mode, large, live) = {
            let mut slots = self.slots();
            let s = slots
                .get_mut(id as usize)
                .ok_or_else(|| VfsError::NotFound(loc.path.clone()))?;
            s.last_view = None;
            let dir = s
                .src
                .resolve(rel)
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
        let path = join_rel(&root, rel);
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
        let f = Arc::make_mut(f);
        let mut changed = false;
        for (name, (k, _)) in subdirs {
            if on_disk.contains(name) || f.gone.contains(k) {
                continue;
            }
            // Суммы — текущие (поддерево могло уже уменьшиться).
            f.drop_dir(*k);
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
                        // Суммы по типам: крупный — по записи снимка, мелкий — по имени.
                        let rec = f
                            .files_of(dir)
                            .find(|i| f.snap.file_name(&f.snap.files[*i as usize]) == name);
                        if let Some(i) = rec {
                            f.forget_file(i);
                        } else {
                            let ty = DuType::of(&crate::walk::RawName::Str {
                                name,
                                escaped: false,
                            }) as usize;
                            let build = f.snap.in_build(dir);
                            let mut sums = vec![&mut f.snap.types];
                            if build {
                                sums.push(&mut f.snap.build);
                            }
                            for t in sums {
                                t.disk[ty] = t.disk[ty].saturating_sub(was);
                                t.files[ty] = t.files[ty].saturating_sub(1);
                            }
                        }
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
            s.edits += 1;
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

/// Адрес вида `v` анализа `id` (фильтр — у корня анализа): `du:/3?f=large`.
pub fn location_view(id: SnapId, v: &View) -> Location {
    Location {
        backend: SCHEME.into(),
        authority: None,
        path: PathBuf::from(format!("/{id}{}", v.query())),
        outer: None,
    }
}

/// `du:/<id>/<rel>` → номер анализа и компоненты `rel` (каталог на диске — корень и
/// `rel`).
pub fn parse(loc: &Location) -> Option<(SnapId, Vec<CompactString>)> {
    view(loc).map(|(id, rel, _)| (id, rel))
}

/// `du:/<id>[?f=…]/<rel>` → номер анализа, `rel` и вид. Фильтр стоит у корня
/// анализа (`du:/3?f=temp`); путь после него — вход в каталог из строки фильтра, это
/// снова «По папкам».
pub fn view(loc: &Location) -> Option<(SnapId, Vec<CompactString>, View)> {
    if loc.backend != SCHEME || loc.outer.is_some() {
        return None;
    }
    let mut parts = loc.path.components().filter_map(|c| match c {
        Component::Normal(s) => Some(s.to_str().map(CompactString::from)),
        Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        Component::ParentDir => Some(None),
    });
    let head = parts.next()??;
    let (id, q) = match head.split_once('?') {
        Some((id, q)) => (id, Some(q)),
        None => (head.as_str(), None),
    };
    let id = id.parse().ok()?;
    let mut v = match q {
        Some(q) => View::parse_query(q)?,
        None => View::default(),
    };
    let rel = parts.collect::<Option<Vec<_>>>()?;
    if !rel.is_empty() {
        v.filter = DuFilter::Dirs;
    }
    Some((id, rel, v))
}

fn join_rel(root: &Path, rel: &[CompactString]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in rel {
        p.push(crate::name::to_os(c, crate::name::may_be_escaped(c)));
    }
    p
}

/// Путь строки плоского вида (`dir\name` от корня).
pub(super) fn join_name(root: &Path, name: &str) -> PathBuf {
    let rel: Vec<CompactString> = name.split('\\').map(CompactString::from).collect();
    join_rel(root, &rel)
}

fn now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// «Крупные» во время подсчёта — текущий топ (F01 §3.2).
fn live_large(t: &DuTree) -> Vec<Row> {
    const TOP: usize = 1000;
    let mut memo: FxHashMap<DirId, CompactString> = FxHashMap::default();
    let mut path = |d: DirId| -> CompactString {
        if let Some(p) = memo.get(&d) {
            return p.clone();
        }
        let mut parts = Vec::new();
        let mut cur = d;
        while cur != 0 {
            let Some(n) = t.get(cur) else {
                break;
            };
            parts.push(t.name(cur));
            cur = n.parent;
        }
        parts.reverse();
        let p = CompactString::from(parts.join("\\"));
        memo.insert(d, p.clone());
        p
    };
    t.top_files(TOP)
        .into_iter()
        .map(|r| {
            let dir = path(r.dir);
            let name = t.file_name(&r);
            Row {
                entry: Entry {
                    name: if dir.is_empty() {
                        name.into()
                    } else {
                        compact_str::format_compact!("{dir}\\{name}")
                    },
                    size: r.disk,
                    mtime: r.mtime,
                    ctime: i64::MIN,
                    atime: i64::MIN,
                    attrs: Attrs::default(),
                    kind: EntryKind::File,
                },
                check: Check::None,
            }
        })
        .collect()
}

/// Ext-состояние вкладок `du:` (S0 v6 №3): общий реестр анализов и карта во второй
/// панели (`du.map`). Кладёт handler `du`.
#[derive(Clone, Debug)]
pub struct DuState {
    pub reg: Arc<DuRegistry>,
    /// Карта (F01 §3.4) во второй панели, пока активна вкладка `du:`.
    pub map: bool,
}

/// Вид вкладки `du:` (`None` — не `du:`).
pub fn filter_of(loc: &Location) -> Option<DuFilter> {
    view(loc).map(|(_, _, v)| v.filter)
}

/// Карта включена (`du.map`).
pub fn map_on(s: &AppState) -> bool {
    s.ext.get::<DuState>().is_some_and(|d| d.map)
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

/// Колонки вкладки: доля в папке — полоса 120 и процент (`screen-dulist`), размер, дата.
const SHARE: ColumnSpec = ColumnSpec {
    id: "share",
    title: "du-col-share",
    width: 176.0,
    right: false,
    view: ColumnView::Bar,
    mono: false,
};

static COLUMNS: [ColumnSpec; 3] = [SHARE, ColumnSpec::SIZE, ColumnSpec::MTIME];

/// «Дубликаты» (`screen-dudups`): Группа · где лежит · Изменён · Размер, без доли.
static DUP_COLUMNS: [ColumnSpec; 2] = [ColumnSpec::MTIME, ColumnSpec::SIZE];

/// «Типы» (`screen-dutypes`): Группа · Файлов · Доля · Размер.
static TYPE_COLUMNS: [ColumnSpec; 3] = [
    ColumnSpec {
        id: "files",
        title: "du-col-files",
        width: 90.0,
        right: true,
        view: ColumnView::Text,
        mono: false,
    },
    SHARE,
    ColumnSpec::SIZE,
];

/// Колонки вида: у «Типов» — число файлов группы, у «Временных» — без даты
/// (`screen-dutypes`, `screen-dutemp`); прочие — Доля · Размер · Изменён.
fn columns_for(filter: DuFilter) -> &'static [ColumnSpec] {
    match filter {
        DuFilter::Types => &TYPE_COLUMNS,
        DuFilter::Temp => &COLUMNS[..2],
        DuFilter::Dups => &DUP_COLUMNS,
        _ => &COLUMNS,
    }
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
        Caps(Caps::LOCAL_PATH | Caps::VIRTUAL)
    }

    /// Имя · Доля · Размер · Изменён (F01 §2.3) при любом виде панели; у видов
    /// фильтров — свои (по адресу `?f=`, без I/O).
    fn columns(&self, dir: &VfsPath) -> &'static [ColumnSpec] {
        filter_of(dir).map_or(&COLUMNS, columns_for)
    }

    /// «Файлов» у групп «Типов» — из сумм снимка (память, без I/O).
    fn ext(&self, dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        if col != "files" || e.kind != EntryKind::Other {
            return None;
        }
        let n = self.reg.type_files(dir, TypeGroup::from_label(&e.name)?)?;
        Some(ExtCell::Text(urfin_i18n::fmt::count(n).into()))
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

#[path = "dvfs_live.rs"]
mod live;
pub use live::{Graft, Listed, Listing};

#[cfg(test)]
#[path = "dvfs_tests.rs"]
mod tests;
