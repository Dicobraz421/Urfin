//! `FakeListing`: детерминированный листинг без ФС для тестов и бенчей (feature
//! `fake`). Каталоги задаются заранее, записи приходят чанками, как от S4.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, PoisonError};

use rustc_hash::FxHashMap;

use crate::cancel::CancelToken;
use crate::entry::{Attrs, Entry, EntryKind};
use crate::ids::{Gen, TabId};
use crate::loc::Location;
use crate::runtime::{Lane, Runtime};
use crate::services::{
    ArrangeReq, ArrangeSpec, ListingService, ListingUpdate, LoadReq, TabEvent, listing_msg,
};
use crate::tab::SortKey;

/// Вызов `load`, как его увидел сервис.
#[derive(Clone, Debug)]
pub struct FakeLoad {
    pub tab: TabId,
    pub generation: Gen,
    pub seq: u32,
    pub loc: Location,
    pub cancel: CancelToken,
}

#[derive(Default)]
struct Inner {
    dirs: FxHashMap<Location, Arc<Vec<Entry>>>,
    loads: Vec<FakeLoad>,
    arranges: Vec<(TabId, u32)>,
    events: Vec<TabEvent>,
}

/// Листинг из заранее заданных каталогов. Неизвестный каталог — `Failed`.
pub struct FakeListing {
    inner: Mutex<Inner>,
    chunk: usize,
    tree: Option<FakeTree>,
    silent: AtomicBool,
    unsorted: AtomicBool,
}

/// Бесконечное детерминированное дерево: каталог без явного содержимого
/// генерируется по пути (до S4 — чтобы по окну можно было ходить).
#[derive(Clone, Copy, Debug)]
pub struct FakeTree {
    pub dirs: u32,
    pub files: u32,
    /// Глубже — только файлы.
    pub max_depth: u32,
}

impl Default for FakeListing {
    fn default() -> Self {
        Self::new(2048)
    }
}

impl std::fmt::Debug for FakeListing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeListing")
            .field("chunk", &self.chunk)
            .finish_non_exhaustive()
    }
}

impl FakeListing {
    /// `chunk` — записей в чанке (как `Batcher` S4: первый чанк сразу).
    pub fn new(chunk: usize) -> Self {
        FakeListing {
            inner: Mutex::default(),
            chunk: chunk.max(1),
            tree: None,
            silent: AtomicBool::new(false),
            unsorted: AtomicBool::new(false),
        }
    }

    /// Неизвестные каталоги генерируются по `tree` вместо `Failed`.
    #[must_use]
    pub fn with_tree(mut self, tree: FakeTree) -> Self {
        self.tree = Some(tree);
        self
    }

    fn generate(tree: FakeTree, loc: &Location) -> Vec<Entry> {
        let depth = loc.path.components().count() as u32;
        let seed = loc
            .path
            .to_string_lossy()
            .bytes()
            .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
        let dirs = if depth < tree.max_depth { tree.dirs } else { 0 };
        let exts = ["txt", "rs", "toml", "md", "png", "zip", "exe", "log"];
        let mut v = Vec::with_capacity((dirs + tree.files + 1) as usize);
        v.extend((0..dirs).map(|i| Self::dir(&format!("folder-{i:02}"))));
        v.extend((0..tree.files).map(|i| {
            let mut e = Self::file(
                &format!("file-{i:04}.{}", exts[i as usize % exts.len()]),
                seed.wrapping_add(u64::from(i) * 7919) % 50_000_000,
            );
            e.mtime = 1_700_000_000_000_000_000 + i64::from(i) * 60_000_000_000;
            e
        }));
        let mut hidden = Self::file(".hidden", 1);
        hidden.attrs = Attrs(Attrs::HIDDEN | Attrs::DOTFILE);
        v.push(hidden);
        v
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Как `VfsListing` без копии записей (ADR-0017): чанки в порядке прихода,
    /// в конце `DoneUnsorted` вместо `Order` + `Done`.
    pub fn set_unsorted(&self, on: bool) {
        self.unsorted.store(on, AtomicOrdering::Relaxed);
    }

    /// «Медленный диск»: загрузки записываются, но ничего не присылают.
    pub fn set_silent(&self, on: bool) {
        self.silent.store(on, AtomicOrdering::Relaxed);
    }

    /// Задать (или заменить) содержимое каталога.
    pub fn set_dir(&self, loc: Location, entries: Vec<Entry>) {
        self.lock().dirs.insert(loc, Arc::new(entries));
    }

    /// `dirs` каталогов `d000000…` и `files` файлов `f000000…` (размер = номер).
    pub fn set_generated(&self, loc: Location, dirs: u32, files: u32) {
        let mut v = Vec::with_capacity((dirs + files) as usize);
        v.extend((0..dirs).map(|i| Self::dir(&format!("d{i:06}"))));
        v.extend((0..files).map(|i| Self::file(&format!("f{i:06}"), u64::from(i))));
        self.set_dir(loc, v);
    }

    pub fn file(name: &str, size: u64) -> Entry {
        Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: name.into(),
            size,
            mtime: 0,
            attrs: Attrs::default(),
            kind: EntryKind::File,
        }
    }

    pub fn dir(name: &str) -> Entry {
        Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: name.into(),
            size: 0,
            mtime: 0,
            attrs: Attrs::default(),
            kind: EntryKind::Dir,
        }
    }

    pub fn loads(&self) -> Vec<FakeLoad> {
        self.lock().loads.clone()
    }

    /// `(tab, seq)` каждого `arrange`.
    pub fn arranges(&self) -> Vec<(TabId, u32)> {
        self.lock().arranges.clone()
    }

    pub fn events(&self) -> Vec<TabEvent> {
        self.lock().events.clone()
    }
}

/// Видима ли запись: hidden и фильтр (фильтр — только к файлам).
fn visible(e: &Entry, v: &ArrangeSpec) -> bool {
    let hidden = e.attrs.has(Attrs::HIDDEN) || e.attrs.has(Attrs::DOTFILE);
    (v.show_hidden || !hidden)
        && (e.kind.is_dir_like() || v.filter.as_ref().is_none_or(|m| m.matches(&e.name)))
}

fn ext(name: &str) -> &str {
    name.rfind('.')
        .filter(|&i| i > 0)
        .map_or("", |i| &name[i + 1..])
}

/// Порядок без natural sort (это S4): имена без учёта регистра.
fn arrange(entries: &[Entry], v: &ArrangeSpec) -> Vec<u32> {
    let mut order: Vec<u32> = (0..entries.len() as u32)
        .filter(|&i| visible(&entries[i as usize], v))
        .collect();
    if v.sort.key == SortKey::Unsorted {
        return order;
    }
    let by_name = |a: &Entry, b: &Entry| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    order.sort_by(|&a, &b| {
        let (a, b) = (&entries[a as usize], &entries[b as usize]);
        if v.sort.dirs_first && a.kind.is_dir_like() != b.kind.is_dir_like() {
            return if a.kind.is_dir_like() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let key = match v.sort.key {
            SortKey::Ext => ext(&a.name)
                .to_lowercase()
                .cmp(&ext(&b.name).to_lowercase()),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Mtime => a.mtime.cmp(&b.mtime),
            SortKey::Name | SortKey::Unsorted => Ordering::Equal,
        }
        .then_with(|| by_name(a, b));
        if v.sort.desc { key.reverse() } else { key }
    });
    order
}

impl ListingService for FakeListing {
    fn load(&self, req: LoadReq, rt: &Runtime) {
        let entries = {
            let mut g = self.lock();
            g.loads.push(FakeLoad {
                tab: req.tab,
                generation: req.generation,
                seq: req.seq,
                loc: req.loc.clone(),
                cancel: req.cancel.clone(),
            });
            g.dirs
                .get(&req.loc)
                .cloned()
                .or_else(|| self.tree.map(|t| Arc::new(Self::generate(t, &req.loc))))
        };
        if self.silent.load(AtomicOrdering::Relaxed) {
            return;
        }
        let chunk = self.chunk;
        let unsorted = self.unsorted.load(AtomicOrdering::Relaxed);
        rt.spawn(
            Lane::Interactive,
            "fake.list",
            req.cancel.clone(),
            move |cx| {
                let send = |u| {
                    cx.sink
                        .send(listing_msg(req.tab, req.generation, u), &cx.cancel)
                        .is_ok()
                };
                let Some(all) = entries else {
                    send(ListingUpdate::Failed("not found".into()));
                    return;
                };
                let mut base = 0u32;
                for part in all.chunks(chunk) {
                    let visible: Vec<u32> = (0..part.len() as u32)
                        .filter(|&i| visible(&part[i as usize], &req.view))
                        .map(|i| base + i)
                        .collect();
                    if !send(ListingUpdate::Chunk {
                        entries: part.to_vec(),
                        visible,
                    }) {
                        return;
                    }
                    base += part.len() as u32;
                }
                if unsorted && req.view.sort.key != SortKey::Unsorted {
                    send(ListingUpdate::DoneUnsorted);
                    return;
                }
                if req.view.sort.key != SortKey::Unsorted
                    && !send(ListingUpdate::Order {
                        order: Arc::new(arrange(&all, &req.view)),
                        for_len: base,
                        seq: req.seq,
                    })
                {
                    return;
                }
                send(ListingUpdate::Done);
            },
        );
    }

    fn arrange(&self, req: ArrangeReq, rt: &Runtime) {
        self.lock().arranges.push((req.tab, req.seq));
        rt.spawn_cpu("fake.arrange", req.cancel.clone(), move |cx| {
            let u = ListingUpdate::Order {
                order: Arc::new(arrange(&req.entries, &req.view)),
                for_len: req.entries.len() as u32,
                seq: req.seq,
            };
            let _ = cx
                .sink
                .send(listing_msg(req.tab, req.generation, u), &cx.cancel);
        });
    }

    fn tab_event(&self, ev: TabEvent) {
        self.lock().events.push(ev);
    }
}
