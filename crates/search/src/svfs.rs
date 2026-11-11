//! Панель результатов `search:` (S8 §2.4): реестр сторов [`SearchStores`] и backend
//! [`SearchVfs`].
//!
//! `search:/<id>` — строка на файл с попаданием, `search:/<id>?f=errors` — каталоги без
//! доступа (вопрос В5). Имя строки — путь от корня поиска (`src\scan.rs`); у поиска по
//! нескольким корням — с корнем впереди (`C:\Dev\src\scan.rs`): одинаковый путь в двух
//! корнях даёт две разные строки, и [`Vfs::resolve`] находит корень по имени.
//!
//! Листинг читает стор из памяти: куски от курсора, нет новых строк и поиск идёт — ждёт до
//! 50 мс. `panel.refresh` готового поиска (листинг без нового наблюдения) просит
//! перепроверку: handler ставит параллельный `stat` на `Lane::Bulk`, исчезнувшие и
//! изменённые строки уходят событиями наблюдения, патч строит hub. Сам hub схему не
//! опрашивает: наблюдение у неё есть.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant};

use compact_str::{CompactString, ToCompactString, format_compact};
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use urfin_core::search::{Hit, SearchId, SearchStore};
use urfin_core::{
    Args, Attrs, CancelToken, Command, Entry, EntryKind, Lane, Location, OpenResolver, Runtime,
};
use urfin_vfs::walk::child;
use urfin_vfs::{
    Caps, ColumnSpec, ColumnView, ExtCell, ListOpts, ListStream, LocalFs, Metadata, Space, Vfs,
    VfsError, VfsPath, VfsRead, VfsRegistry, WatchEvent, WatchGuard, WatchTx, name,
};

/// Схема панели результатов.
pub const SCHEME: &str = "search";
/// Запрос вида «каталоги без доступа».
const ERRORS: &str = "?f=errors";
/// Нет новых строк, поиск идёт: столько ждём, потом отдаём пустой батч (проверка отмены).
const WAIT: Duration = Duration::from_millis(50);
/// Шаг ожидания новых строк.
const STEP: Duration = Duration::from_millis(2);
/// Строк в батче листинга после первого.
const BATCH: usize = 2048;
/// Строк в одной задаче перепроверки: задачи `Bulk` короткие, пул делит их на потоки.
const RECHECK_PART: usize = 4096;
/// Перепроверка нашла изменения: `refresh`, который они вызовут, не перепроверяет снова.
const QUIET: Duration = Duration::from_secs(2);
/// Изменений больше — одно `Overflow` вместо имён (как у hub: дальше всё равно перечитывание).
const MAX_EVENTS: usize = 4096;

/// Вид панели `search:`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// Строка на файл с попаданием.
    Files,
    /// Каталоги без доступа (`?f=errors`).
    Errors,
}

/// Разобранный адрес `search:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub id: SearchId,
    pub view: View,
    /// Имя строки у адреса элемента (`search:/3/src\a.rs`); у самой панели — `None`.
    pub name: Option<String>,
}

/// Адрес панели поиска `id`.
pub fn location(id: SearchId, view: View) -> Location {
    let q = if view == View::Errors { ERRORS } else { "" };
    Location {
        backend: SCHEME.into(),
        authority: None,
        path: PathBuf::from(format!("/{}{q}", id.0)),
        outer: None,
    }
}

/// `search:/<id>[?f=errors][/<имя>]` → номер, вид и имя.
pub fn parse(loc: &Location) -> Option<Address> {
    if loc.backend != SCHEME || loc.outer.is_some() {
        return None;
    }
    let s = loc.path.to_str()?.trim_start_matches(['/', '\\']);
    let (head, rest) = match s.find(['/', '\\']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (id, view) = match head.strip_suffix(ERRORS) {
        Some(id) => (id, View::Errors),
        None => (head, View::Files),
    };
    Some(Address {
        id: SearchId(id.parse().ok()?),
        view,
        name: rest.filter(|r| !r.is_empty()).map(str::to_owned),
    })
}

/// Подпись корня в имени строки и строки «без доступа»: путь ОС или uri.
fn label(loc: &Location) -> CompactString {
    if loc.is_local() {
        let s = loc.path.to_string_lossy();
        s.trim_end_matches('\\').to_compact_string()
    } else {
        let s = loc.to_uri();
        s.trim_end_matches('/').to_compact_string()
    }
}

/// Строки панели `range`: номер строки и первое попадание файла.
fn each_file(store: &SearchStore, range: Range<usize>, mut f: impl FnMut(u32, &Hit)) {
    let firsts: Vec<u32> = store.files(range.clone()).iter().copied().collect();
    let (Some(&lo), Some(&hi)) = (firsts.first(), firsts.last()) else {
        return;
    };
    let rows = store.rows(lo as usize..hi as usize + 1);
    let mut rows = rows.iter();
    // Номер строки, которую `rows` отдаст следующей.
    let mut at = lo;
    for (i, &r) in (range.start as u32..).zip(&firsts) {
        let Some(hit) = rows.nth((r - at) as usize) else {
            return;
        };
        at = r + 1;
        f(i, hit);
    }
}

/// Первое попадание строки панели `i`.
fn first_hit(store: &SearchStore, i: u32) -> Option<Hit> {
    let r = *store.files(i as usize..i as usize + 1).iter().next()?;
    store
        .rows(r as usize..r as usize + 1)
        .iter()
        .next()
        .cloned()
}

fn key(root: u16, rel: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = FxHasher::default();
    root.hash(&mut h);
    rel.hash(&mut h);
    h.finish()
}

/// Имя → строка панели для ячеек «Стр» и «Первое совпадение» (только текстовый поиск).
/// Открытая адресация по `u32`: 4 Б на слот, имена читаются из стора.
#[derive(Debug, Default)]
struct NameIndex {
    /// Строка + 1; 0 — пусто.
    slots: Vec<u32>,
    /// Строк проиндексировано (от начала).
    len: u32,
}

impl NameIndex {
    /// Проиндексировать строки до `upto`.
    fn catch_up(&mut self, store: &SearchStore, upto: usize) {
        let from = self.len as usize;
        if from >= upto {
            return;
        }
        if upto * 4 > self.slots.len() * 3 {
            let cap = (upto * 4 / 3 + 1).next_power_of_two().max(1024);
            self.slots = vec![0; cap];
            each_file(store, 0..from, |i, h| self.put(i, key(h.root, &h.e.name)));
        }
        each_file(store, from..upto, |i, h| {
            self.put(i, key(h.root, &h.e.name));
        });
        self.len = upto as u32;
    }

    fn put(&mut self, i: u32, k: u64) {
        let mask = self.slots.len() - 1;
        let mut p = k as usize & mask;
        while self.slots[p] != 0 {
            p = (p + 1) & mask;
        }
        self.slots[p] = i + 1;
    }

    fn find(&self, store: &SearchStore, root: u16, rel: &str) -> Option<Hit> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut p = key(root, rel) as usize & mask;
        while self.slots[p] != 0 {
            let h = first_hit(store, self.slots[p] - 1)?;
            if h.root == root && h.e.name == rel {
                return Some(h);
            }
            p = (p + 1) & mask;
        }
        None
    }

    fn bytes(&self) -> u64 {
        (self.slots.capacity() * size_of::<u32>()) as u64
    }
}

/// Правки перепроверки поверх стора: стор только дописывается.
#[derive(Debug, Default)]
struct Edits {
    /// Строки панели, чьих файлов больше нет.
    gone: FxHashSet<u32>,
    /// Новые метаданные строки.
    fresh: FxHashMap<u32, Entry>,
}

/// Поиск в реестре: стор и то, что нужно его панели.
#[derive(Debug)]
struct Slot {
    store: Arc<SearchStore>,
    denied: Arc<Mutex<Vec<Location>>>,
    /// Поиск с текстом: колонки «Стр» и «Первое совпадение».
    text: bool,
    /// Подписи корней в именах; пусто у поиска с одним корнем.
    labels: Box<[CompactString]>,
    edits: RwLock<Edits>,
    index: Mutex<NameIndex>,
    watchers: Mutex<Vec<(u64, WatchTx)>>,
    next_watch: AtomicU64,
    /// Наблюдение открыто, листинга после него ещё не было: следующий листинг — показ
    /// панели, а не `panel.refresh`.
    shown: AtomicBool,
    quiet_until: Mutex<Option<Instant>>,
    rechecking: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Slot {
    /// Имя строки панели.
    fn shown_name(&self, h: &Hit) -> CompactString {
        match self.labels.get(usize::from(h.root)) {
            Some(l) => format_compact!("{l}\\{}", h.e.name),
            None => h.e.name.clone(),
        }
    }

    /// Имя строки → корень и путь от него. Корни вложены — берётся длинный.
    fn split<'a>(&self, name: &'a str) -> Option<(u16, &'a str)> {
        if self.labels.is_empty() {
            return Some((0, name));
        }
        let found = self.labels.iter().enumerate().filter_map(|(i, l)| {
            let rel = name.strip_prefix(l.as_str())?.strip_prefix('\\')?;
            Some((i, l.len(), rel))
        });
        let (i, _, rel) = found.max_by_key(|&(_, n, _)| n)?;
        Some((u16::try_from(i).ok()?, rel))
    }

    /// Настоящий адрес файла `rel` корня `root`.
    fn real_of(&self, root: u16, rel: &str, attrs: Attrs) -> Option<Location> {
        let root = self.store.roots.get(usize::from(root))?;
        let e = Entry {
            name: rel.into(),
            size: 0,
            mtime: i64::MIN,
            ctime: i64::MIN,
            atime: i64::MIN,
            attrs,
            kind: EntryKind::File,
        };
        Some(child(root, &e))
    }

    /// Настоящий адрес строки `name` вида `view`.
    fn real(&self, view: View, name: &str, attrs: Attrs) -> Option<Location> {
        match view {
            View::Files => {
                let (root, rel) = self.split(name)?;
                self.real_of(root, rel, attrs)
            }
            View::Errors => lock(&self.denied)
                .iter()
                .find(|d| label(d) == name)
                .cloned(),
        }
    }

    /// Строки панели `range` с правками перепроверки.
    fn fill(&self, range: Range<usize>, out: &mut Vec<Entry>) {
        let edits = self.edits.read().unwrap_or_else(PoisonError::into_inner);
        each_file(&self.store, range.clone(), |i, h| {
            if edits.gone.contains(&i) {
                return;
            }
            let mut e = edits.fresh.get(&i).cloned().unwrap_or_else(|| h.e.clone());
            e.name = self.shown_name(h);
            out.push(e);
        });
        drop(edits);
        if self.text {
            lock(&self.index).catch_up(&self.store, range.end);
        }
    }

    fn errors(&self) -> Vec<Entry> {
        lock(&self.denied)
            .iter()
            .map(|d| Entry {
                name: label(d),
                size: 0,
                mtime: i64::MIN,
                ctime: i64::MIN,
                atime: i64::MIN,
                attrs: Attrs::default(),
                kind: EntryKind::Dir,
            })
            .collect()
    }

    /// Листинг без нового наблюдения у готового поиска — `panel.refresh`: нужна
    /// перепроверка (кроме `refresh`, который вызвала она сама).
    fn wants_recheck(&self) -> bool {
        let show = self.shown.swap(false, Ordering::AcqRel);
        let quiet = lock(&self.quiet_until).is_some_and(|t| Instant::now() < t);
        !show && !quiet && self.store.is_done()
    }

    fn extra_bytes(&self) -> u64 {
        let edits = self.edits.read().unwrap_or_else(PoisonError::into_inner);
        let entry = size_of::<Entry>() + size_of::<u32>();
        let edits = edits.gone.capacity() * size_of::<u32>() + edits.fresh.capacity() * entry;
        // Индекс строит листинг; UI-поток его не ждёт.
        let index = self.index.try_lock().map_or(0, |i| i.bytes());
        index + edits as u64
    }
}

/// Открытые поиски для [`SearchVfs`]: общий у backend'а и handler `search`.
///
/// Время жизни сторов решает handler (`life.rs`): он кладёт стор сюда при запуске и
/// убирает, когда политика его отпускает.
pub struct SearchStores {
    slots: Mutex<FxHashMap<SearchId, Arc<Slot>>>,
    /// Backend'ы не-локальных корней. Слабая: реестр держит и [`SearchVfs`].
    reg: Mutex<Weak<VfsRegistry>>,
    local: LocalFs,
    /// Поиски, которым нужна перепроверка (`panel.refresh`).
    wanted: Mutex<Vec<SearchId>>,
}

impl std::fmt::Debug for SearchStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchStores")
            .field("stores", &lock(&self.slots).len())
            .finish_non_exhaustive()
    }
}

impl SearchStores {
    pub fn new() -> Arc<SearchStores> {
        Arc::new(SearchStores {
            slots: Mutex::default(),
            reg: Mutex::new(Weak::new()),
            local: LocalFs::new(),
            wanted: Mutex::default(),
        })
    }

    /// Реестр, через который читаются не-локальные корни.
    pub fn bind(&self, reg: &Arc<VfsRegistry>) {
        *lock(&self.reg) = Arc::downgrade(reg);
    }

    /// Стор поиска — в реестр. `text` — поиск с текстом (колонки совпадения).
    pub fn insert(&self, store: &Arc<SearchStore>, denied: &Arc<Mutex<Vec<Location>>>, text: bool) {
        let labels = if store.roots.len() > 1 {
            store.roots.iter().map(label).collect()
        } else {
            Box::default()
        };
        let slot = Slot {
            store: Arc::clone(store),
            denied: Arc::clone(denied),
            text,
            labels,
            edits: RwLock::default(),
            index: Mutex::default(),
            watchers: Mutex::default(),
            next_watch: AtomicU64::new(1),
            shown: AtomicBool::new(false),
            quiet_until: Mutex::new(None),
            rechecking: AtomicBool::new(false),
        };
        lock(&self.slots).insert(store.id, Arc::new(slot));
    }

    pub fn remove(&self, id: SearchId) {
        lock(&self.slots).remove(&id);
    }

    pub fn contains(&self, id: SearchId) -> bool {
        lock(&self.slots).contains_key(&id)
    }

    fn slot(&self, id: SearchId) -> Option<Arc<Slot>> {
        lock(&self.slots).get(&id).cloned()
    }

    /// Поиски, которые просят перепроверку; список очищается.
    pub fn take_wanted(&self) -> Vec<SearchId> {
        std::mem::take(&mut *lock(&self.wanted))
    }

    /// Память поиска: строки стора (оценка по выборке имён и фрагментов) и то, что
    /// добавляет панель — индекс имён и правки перепроверки.
    pub fn approx_bytes(&self, id: SearchId) -> u64 {
        let Some(slot) = self.slot(id) else {
            return 0;
        };
        store_bytes(&slot.store) + slot.extra_bytes()
    }

    /// Только то, что добавляет панель (индекс имён, правки).
    pub fn extra_bytes(&self, id: SearchId) -> u64 {
        self.slot(id).map_or(0, |s| s.extra_bytes())
    }

    /// Строка `e` панели `dir` (`search:/3`): настоящий адрес и номер строки первого
    /// совпадения (0 — поиск без текста или индекс имён занят листингом). Без I/O.
    pub fn locate(&self, dir: &Location, e: &Entry) -> Option<(Location, u32)> {
        let a = parse(dir).filter(|a| a.name.is_none())?;
        let slot = self.slot(a.id)?;
        let loc = slot.real(a.view, &e.name, e.attrs)?;
        let line = if a.view == View::Files && slot.text {
            let (root, rel) = slot.split(&e.name)?;
            slot.index
                .try_lock()
                .ok()
                .and_then(|i| i.find(&slot.store, root, rel))
                .map_or(0, |h| h.line)
        } else {
            0
        };
        Some((loc, line))
    }

    fn backend(&self, loc: &Location) -> Result<Backend<'_>, VfsError> {
        if loc.is_local() {
            return Ok(Backend::Local(&self.local));
        }
        let reg = lock(&self.reg)
            .upgrade()
            .ok_or(VfsError::Unsupported("search: no registry"))?;
        let fs = Arc::clone(reg.get(loc)?);
        Ok(Backend::Other(fs))
    }
}

/// Backend настоящего адреса.
enum Backend<'a> {
    Local(&'a LocalFs),
    Other(Arc<dyn Vfs>),
}

impl Backend<'_> {
    fn fs(&self) -> &dyn Vfs {
        match self {
            Backend::Local(l) => *l,
            Backend::Other(v) => v.as_ref(),
        }
    }
}

/// Оценка памяти стора: `Hit` и индекс файла — точно, кучи имён и фрагментов — по первым
/// 1024 строкам.
fn store_bytes(store: &SearchStore) -> u64 {
    const SAMPLE: usize = 1024;
    let rows = store.rows_len();
    let files = store.files_len();
    let heap = |s: &CompactString| {
        if s.is_heap_allocated() {
            s.capacity()
        } else {
            0
        }
    };
    let sample = store.rows(0..SAMPLE);
    let n = sample.len().max(1);
    let sampled: usize = sample
        .iter()
        .map(|h| heap(&h.e.name) + h.preview.as_ref().map_or(0, heap))
        .sum();
    let heap = sampled as u64 * rows as u64 / n as u64;
    (rows * size_of::<Hit>() + files * size_of::<u32>()) as u64 + heap
}

/// Перепроверка: части стора — задачами `Bulk`, итог — когда отпущена последняя.
struct Recheck {
    slot: Arc<Slot>,
    stores: Arc<SearchStores>,
    found: Mutex<Found>,
}

#[derive(Default)]
struct Found {
    gone: Vec<(u32, CompactString)>,
    fresh: Vec<(u32, Entry)>,
}

/// `Entry` строки по свежим метаданным; имя и экранирование — прежние.
fn refreshed(e: &Entry, m: &Metadata) -> Entry {
    Entry {
        name: e.name.clone(),
        size: m.size,
        mtime: m.mtime,
        ctime: m.btime.unwrap_or(e.ctime),
        atime: m.atime,
        attrs: Attrs(m.attrs.0 | (e.attrs.0 & Attrs::ESCAPED_NAME)),
        kind: m.kind,
    }
}

fn changed(e: &Entry, m: &Metadata) -> bool {
    e.kind != m.kind || e.mtime != m.mtime || (!e.kind.is_dir_like() && e.size != m.size)
}

impl Recheck {
    fn run(&self, part: Range<usize>, cancel: &CancelToken) {
        let mut hits = Vec::with_capacity(part.len());
        each_file(&self.slot.store, part, |i, h| hits.push((i, h.clone())));
        let mut found = Found::default();
        for (i, h) in hits {
            if cancel.is_cancelled() {
                break;
            }
            let shown = {
                let edits = self
                    .slot
                    .edits
                    .read()
                    .unwrap_or_else(PoisonError::into_inner);
                if edits.gone.contains(&i) {
                    continue;
                }
                edits.fresh.get(&i).cloned()
            };
            let e = shown.as_ref().unwrap_or(&h.e);
            let Some(real) = self.slot.real_of(h.root, &h.e.name, h.e.attrs) else {
                continue;
            };
            let res = self
                .stores
                .backend(&real)
                .and_then(|b| b.fs().stat(&real, false));
            match res {
                Err(VfsError::NotFound(_)) => found.gone.push((i, self.slot.shown_name(&h))),
                Ok(m) if changed(e, &m) => {
                    let mut fresh = refreshed(e, &m);
                    fresh.name.clone_from(&h.e.name);
                    found.fresh.push((i, fresh));
                }
                Ok(_) => {}
                Err(err) => tracing::debug!(target: "search", error = %err, "recheck: stat failed"),
            }
        }
        let mut all = lock(&self.found);
        all.gone.append(&mut found.gone);
        all.fresh.append(&mut found.fresh);
    }
}

impl Drop for Recheck {
    /// Все части отработали (или отменены): правки — в стор панели, события — в hub.
    fn drop(&mut self) {
        let found = std::mem::take(&mut *lock(&self.found));
        let slot = &self.slot;
        let mut events = Vec::with_capacity(found.gone.len() + found.fresh.len());
        {
            let mut edits = slot.edits.write().unwrap_or_else(PoisonError::into_inner);
            for (i, name) in found.gone {
                edits.fresh.remove(&i);
                edits.gone.insert(i);
                events.push(WatchEvent::Removed(name));
            }
            for (i, e) in found.fresh {
                let mut shown = e.clone();
                if let Some(h) = first_hit(&slot.store, i) {
                    shown.name = slot.shown_name(&h);
                }
                edits.fresh.insert(i, e);
                events.push(WatchEvent::Modified(shown.name.clone(), Some(shown)));
            }
        }
        if !events.is_empty() {
            *lock(&slot.quiet_until) = Some(Instant::now() + QUIET);
            if events.len() > MAX_EVENTS {
                events = vec![WatchEvent::Overflow];
            }
            for (_, w) in lock(&slot.watchers).iter() {
                for ev in &events {
                    let _ = w.tx.send((w.id, ev.clone()));
                }
            }
            tracing::debug!(target: "search", id = slot.store.id.0, changes = events.len(), "recheck");
        }
        slot.rechecking.store(false, Ordering::Release);
    }
}

/// Перепроверить попадания поиска `id`: `stat` частями по `Lane::Bulk`. Вторая
/// перепроверка, пока идёт первая, не начинается.
pub(crate) fn spawn_recheck(stores: &Arc<SearchStores>, id: SearchId, rt: &Runtime) {
    let Some(slot) = stores.slot(id) else {
        return;
    };
    if slot.rechecking.swap(true, Ordering::AcqRel) {
        return;
    }
    let n = slot.store.files_len();
    let job = Arc::new(Recheck {
        slot,
        stores: Arc::clone(stores),
        found: Mutex::default(),
    });
    let cancel = rt.token();
    for from in (0..n).step_by(RECHECK_PART) {
        let part = from..n.min(from + RECHECK_PART);
        let job = Arc::clone(&job);
        rt.spawn(Lane::Bulk, "search.recheck", cancel.clone(), move |tcx| {
            job.run(part, &tcx.cancel);
        });
    }
}

/// Enter по каталогу панели `search:` (и по строке «без доступа») — `panel.go` в настоящий
/// каталог: у схемы своих каталогов нет.
struct EnterReal(Arc<SearchStores>);

impl OpenResolver for EnterReal {
    fn resolve(&self, _dir: &Location, _e: &Entry, _args: &Args) -> Option<Command> {
        None
    }

    fn enter_dir(&self, dir: &Location, e: &Entry, _args: &Args) -> Option<Command> {
        let (loc, _) = self.0.locate(dir, e)?;
        Some(Command::new("panel.go").arg("path", loc))
    }
}

/// Резолвер Enter для core: каталоги из результатов поиска `stores`.
pub fn enter_resolver(stores: &Arc<SearchStores>) -> Arc<dyn OpenResolver> {
    Arc::new(EnterReal(Arc::clone(stores)))
}

/// Снимает наблюдение панели.
struct Unwatch {
    slot: Weak<Slot>,
    key: u64,
}

impl Drop for Unwatch {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.upgrade() {
            lock(&slot.watchers).retain(|(k, _)| *k != self.key);
        }
    }
}

/// Листинг панели: строки стора от курсора; поиск идёт — ждёт новых.
struct FilesStream {
    slot: Arc<Slot>,
    at: usize,
    first: usize,
}

impl ListStream for FilesStream {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        let t0 = Instant::now();
        loop {
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            // Сначала `done`: после него строк уже не прибавится.
            let done = self.slot.store.is_done();
            let n = self.slot.store.files_len();
            if self.at < n {
                let take = if self.at == 0 { self.first } else { BATCH };
                let end = n.min(self.at + take);
                self.slot.fill(self.at..end, out);
                self.at = end;
                return Ok(!(done && end == n));
            }
            if done {
                return Ok(false);
            }
            if t0.elapsed() >= WAIT {
                return Ok(true);
            }
            std::thread::sleep(STEP);
        }
    }

    fn size_hint(&self) -> Option<u32> {
        let s = &self.slot.store;
        s.is_done()
            .then(|| u32::try_from(s.files_len()).ok())
            .flatten()
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
}

/// «Стр»: номер строки первого совпадения. Ширина — 34 макета и отступ 12 перед ней:
/// имя обрезается там же, где на макете.
const LINE: ColumnSpec = ColumnSpec {
    id: "line",
    title: "ui-search-col-line",
    width: 46.0,
    right: true,
    view: ColumnView::Text,
    mono: true,
};

/// «Первое совпадение»: фрагмент с подсветкой (`screen-searchgrep`). Колонки прижаты
/// к правому краю: ширина ставит «Стр» и фрагмент на места макета при ширине окна 1280.
const HIT: ColumnSpec = ColumnSpec {
    id: "hit",
    title: "ui-search-col-hit",
    width: 338.0,
    right: false,
    view: ColumnView::Snippet,
    mono: true,
};

static NAME_COLUMNS: [ColumnSpec; 2] = [ColumnSpec::SIZE, ColumnSpec::MTIME];
static TEXT_COLUMNS: [ColumnSpec; 2] = [LINE, HIT];

/// Backend схемы `search:`.
#[derive(Debug)]
pub struct SearchVfs {
    stores: Arc<SearchStores>,
}

impl SearchVfs {
    pub fn new(stores: Arc<SearchStores>) -> SearchVfs {
        SearchVfs { stores }
    }

    /// Поиск адреса; нет такого — `NotFound`.
    fn open(&self, p: &VfsPath) -> Result<(Address, Arc<Slot>), VfsError> {
        let a = parse(p).ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        let slot = self
            .stores
            .slot(a.id)
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        Ok((a, slot))
    }

    /// Настоящий адрес элемента `p` (`search:/3/src\a.rs`) и его backend.
    fn real(&self, p: &VfsPath) -> Result<(Location, Backend<'_>), VfsError> {
        let (a, slot) = self.open(p)?;
        let name = a.name.ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        let attrs = if name::may_be_escaped(&name) {
            Attrs(Attrs::ESCAPED_NAME)
        } else {
            Attrs::default()
        };
        let loc = slot
            .real(a.view, &name, attrs)
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        let b = self.stores.backend(&loc)?;
        Ok((loc, b))
    }
}

/// Метаданные самой панели: каталог без своей ФС.
fn view_meta() -> Metadata {
    Metadata {
        kind: EntryKind::Dir,
        attrs: Attrs::default(),
        size: 0,
        alloc_size: None,
        mtime: i64::MIN,
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

impl Vfs for SearchVfs {
    fn scheme(&self) -> &'static str {
        SCHEME
    }

    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps(Caps::VIRTUAL | Caps::READ_ONLY)
    }

    fn list(&self, dir: &VfsPath, opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        let (a, slot) = self.open(dir)?;
        if a.name.is_some() {
            return Err(VfsError::NotDir(dir.path.clone()));
        }
        if a.view == View::Errors {
            return Ok(Box::new(Ready(Some(slot.errors()))));
        }
        if slot.wants_recheck() {
            lock(&self.stores.wanted).push(a.id);
        }
        Ok(Box::new(FilesStream {
            slot,
            at: 0,
            first: (opts.first_batch_hint as usize).max(1),
        }))
    }

    fn stat(&self, p: &VfsPath, follow: bool) -> Result<Metadata, VfsError> {
        if parse(p).is_some_and(|a| a.name.is_none()) {
            self.open(p)?;
            return Ok(view_meta());
        }
        let (loc, b) = self.real(p)?;
        b.fs().stat(&loc, follow)
    }

    fn stat_entry(&self, dir: &VfsPath, name: &str, _attrs_hint: Attrs) -> Result<Entry, VfsError> {
        let mut p = dir.clone();
        p.path = PathBuf::from(format!("{}/{name}", dir.path.to_string_lossy()));
        let (loc, b) = self.real(&p)?;
        let m = b.fs().stat(&loc, false)?;
        let escaped = if name::may_be_escaped(name) {
            Attrs::ESCAPED_NAME
        } else {
            0
        };
        let base = Entry {
            name: name.into(),
            size: 0,
            mtime: i64::MIN,
            ctime: i64::MIN,
            atime: i64::MIN,
            attrs: Attrs(escaped),
            kind: m.kind,
        };
        Ok(refreshed(&base, &m))
    }

    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        let (loc, b) = self.real(p)?;
        b.fs().open_read(&loc)
    }

    /// Место — у первого корня поиска.
    fn space(&self, at: &VfsPath) -> Result<Space, VfsError> {
        let (_, slot) = self.open(at)?;
        let root = slot
            .store
            .roots
            .first()
            .ok_or(VfsError::Unsupported("search: no roots"))?;
        self.stores.backend(root)?.fs().space(root)
    }

    fn local_path(&self, p: &VfsPath) -> Option<PathBuf> {
        let (loc, b) = self.real(p).ok()?;
        if loc.is_local() {
            Some(loc.path)
        } else {
            b.fs().local_path(&loc)
        }
    }

    /// Наблюдение — свои события перепроверки; hub схему не опрашивает.
    fn watch(&self, dir: &VfsPath, tx: WatchTx) -> Result<WatchGuard, VfsError> {
        let (a, slot) = self.open(dir)?;
        if a.view == View::Errors || a.name.is_some() {
            return Ok(WatchGuard::new(()));
        }
        let key = slot.next_watch.fetch_add(1, Ordering::Relaxed);
        lock(&slot.watchers).push((key, tx));
        slot.shown.store(true, Ordering::Release);
        Ok(WatchGuard::new(Unwatch {
            slot: Arc::downgrade(&slot),
            key,
        }))
    }

    /// Адрес источника: корень поиска и путь от него; у «без доступа» — сам каталог.
    fn resolve(&self, base: &VfsPath, item: &Entry) -> Result<VfsPath, VfsError> {
        let (a, slot) = self.open(base)?;
        if a.name.is_some() {
            return Err(VfsError::NotDir(base.path.clone()));
        }
        slot.real(a.view, &item.name, item.attrs)
            .ok_or_else(|| VfsError::NotFound(PathBuf::from(item.name.as_str())))
    }

    /// Поиск по имени — Размер · Изменён; с текстом — Стр · Первое совпадение.
    fn columns(&self, dir: &VfsPath) -> &'static [ColumnSpec] {
        match self.open(dir) {
            Ok((a, slot)) if a.view == View::Files && slot.text => &TEXT_COLUMNS,
            Ok((a, _)) if a.view == View::Files => &NAME_COLUMNS,
            _ => &[],
        }
    }

    /// Ячейки строки из памяти; индекс занят листингом — ячейка пустая до следующего кадра.
    fn ext(&self, dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        let (a, slot) = self.open(dir).ok()?;
        if a.view != View::Files || !slot.text || !matches!(col, "line" | "hit") {
            return None;
        }
        let (root, rel) = slot.split(&e.name)?;
        let hit = slot.index.try_lock().ok()?.find(&slot.store, root, rel)?;
        match col {
            "line" => (hit.line > 0).then(|| ExtCell::Text(format_compact!("{}", hit.line))),
            _ => Some(ExtCell::Snippet {
                text: hit.preview?,
                hl: hit.hl,
            }),
        }
    }
}
