//! Общий обходчик дерева каталогов (S0 v5 №14): пул потоков, общая очередь
//! каталогов (LIFO — обход близок к «в глубину», открытых хэндлов родителей мало).
//! [`walk`] — поверх `Vfs::list` (размер каталогов S4-17, позже S5 и S8);
//! [`walk_with`] — поверх [`DirLister`] с колбэками на запись и на каталог
//! (анализ места F-01: `NtLister`, `FindLister`, `VfsLister`). Планировщик у них общий.
//!
//! Ссылки на каталоги (`LinkDir`: symlink, junction) не обходятся — циклов нет.
//! Ошибки отдельных каталогов (нет доступа) не прерывают обход: считаются в
//! [`WalkStats::errors`].

use std::any::Any;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use urfin_core::{CancelToken, Entry, EntryKind, Location};

use crate::api::{ListOpts, Vfs, VfsError};

/// Как часто проверять отмену, пока поток ждёт работы.
const IDLE_CHECK: Duration = Duration::from_millis(20);
/// Как часто листинг одного каталога проверяет отмену (записей).
const HALT_EVERY: u32 = 256;

/// Итоги обхода.
#[derive(Debug, Default)]
pub struct WalkStats {
    pub dirs: AtomicU64,
    pub errors: AtomicU64,
}

/// Обойти деревья `roots` в `threads` потоках. `visit(root, dir, entry)` —
/// для каждой записи каждого каталога (из любого потока); `root_done(root)` —
/// когда дерево корня обойдено целиком. `tick` зовётся в вызывающем потоке
/// примерно раз в `period`, пока идёт обход. Отмена — `cancel` (и между
/// каталогами — `stop`); проверяется на каждой порции листинга.
#[allow(clippy::too_many_arguments, reason = "обходчик с колбэками прогресса")]
pub fn walk(
    vfs: &dyn Vfs,
    roots: &[Location],
    threads: usize,
    cancel: &CancelToken,
    stop: &(dyn Fn() -> bool + Sync),
    visit: &(dyn Fn(usize, &Location, &Entry) + Sync),
    root_done: &(dyn Fn(usize) + Sync),
    tick: impl FnMut(),
    period: Duration,
) -> WalkStats {
    let halted = || cancel.is_cancelled() || stop();
    let work = VfsWork {
        vfs,
        cancel,
        halted: &halted,
        visit,
        root_done,
        // Сколько каталогов корня ещё не дочитано (в очереди или в работе).
        pending: roots.iter().map(|_| AtomicUsize::new(1)).collect(),
        stats: WalkStats::default(),
    };
    let init = roots.iter().cloned().enumerate().collect();
    run_pool(&work, init, threads, &halted, tick, period, period);
    work.stats
}

struct VfsWork<'a> {
    vfs: &'a dyn Vfs,
    cancel: &'a CancelToken,
    halted: &'a (dyn Fn() -> bool + Sync),
    visit: &'a (dyn Fn(usize, &Location, &Entry) + Sync),
    root_done: &'a (dyn Fn(usize) + Sync),
    pending: Vec<AtomicUsize>,
    stats: WalkStats,
}

impl Work for VfsWork<'_> {
    type Item = (usize, Location);
    type After = usize;
    type Scratch = Vec<Entry>;

    fn run(
        &self,
        (root, dir): (usize, Location),
        out: &mut Vec<Entry>,
        children: &mut Vec<(usize, Location)>,
    ) -> usize {
        match self.vfs.list(&dir, &ListOpts::default()) {
            Ok(mut st) => loop {
                out.clear();
                let more = st.next_batch(out, self.cancel).unwrap_or_else(|_| {
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    false
                });
                for e in out.iter() {
                    (self.visit)(root, &dir, e);
                    if e.kind == EntryKind::Dir {
                        children.push((root, child(&dir, e)));
                    }
                }
                if !more || (self.halted)() {
                    break;
                }
            },
            Err(_) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.stats.dirs.fetch_add(1, Ordering::Relaxed);
        self.pending[root].fetch_add(children.len(), Ordering::AcqRel);
        root
    }

    fn after(&self, root: usize) {
        if self.pending[root].fetch_sub(1, Ordering::AcqRel) == 1 && !(self.halted)() {
            (self.root_done)(root);
        }
    }
}

/// Подкаталог `e` каталога `dir` (тот же backend и контейнер).
pub fn child(dir: &Location, e: &Entry) -> Location {
    Location {
        backend: dir.backend.clone(),
        path: crate::name::join_os(dir, e),
        outer: dir.outer.clone(),
    }
}

// ---------------------------------------------------------------------------
// walk_with: листеры (F01 §3.1)

/// Хэндл родителя, который листер отдаёт детям (открытие относительно родителя).
/// Тип — дело листера.
pub type Parent = Arc<dyn Any + Send + Sync>;

/// Каталог в очереди обхода.
#[derive(Clone)]
pub struct DirRef {
    /// Полный адрес (для ошибок, запасных путей и `VfsLister`).
    pub loc: Location,
    /// Метка вызывающего (у анализа места — `DirId`).
    pub tag: u32,
    /// Хэндл родителя от того же листера; `None` у корней.
    pub parent: Option<Parent>,
}

impl DirRef {
    /// Корень обхода.
    pub fn root(loc: Location, tag: u32) -> DirRef {
        DirRef {
            loc,
            tag,
            parent: None,
        }
    }
}

impl std::fmt::Debug for DirRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirRef")
            .field("loc", &self.loc)
            .field("tag", &self.tag)
            .field("parent", &self.parent.is_some())
            .finish()
    }
}

/// Имя записи без копии: UTF-16 из буфера ОС или строка `Entry`.
#[derive(Copy, Clone, Debug)]
pub enum RawName<'a> {
    Wide(&'a [u16]),
    Str { name: &'a str, escaped: bool },
}

impl RawName<'_> {
    /// Имя для показа и арены (с флагом экранирования, S4 §0).
    pub fn to_name(&self) -> (CompactString, bool) {
        match *self {
            RawName::Wide(w) => crate::name::escape_wide(w),
            RawName::Str { name, escaped } => (CompactString::from(name), escaped),
        }
    }

    /// Имя для пути ОС.
    pub fn to_os(&self) -> Cow<'_, OsStr> {
        match *self {
            #[cfg(windows)]
            RawName::Wide(w) => {
                use std::os::windows::ffi::OsStringExt;
                Cow::Owned(std::ffi::OsString::from_wide(w))
            }
            #[cfg(not(windows))]
            RawName::Wide(w) => Cow::Owned(String::from_utf16_lossy(w).into()),
            RawName::Str { name, escaped } => crate::name::to_os(name, escaped),
        }
    }

    /// Расширение ASCII в нижнем регистре (после последней `.`, не у dotfile);
    /// длиннее 16 байт или не ASCII — `None`. Без аллокаций.
    pub fn ext_lower<'b>(&self, buf: &'b mut [u8; 16]) -> Option<&'b str> {
        let n = match *self {
            RawName::Wide(w) => {
                let dot = w.iter().rposition(|&u| u == u16::from(b'.'))?;
                let e = &w[dot + 1..];
                if dot == 0 || e.is_empty() || e.len() > buf.len() {
                    return None;
                }
                for (b, &u) in buf.iter_mut().zip(e) {
                    *b = u8::try_from(u).ok().filter(u8::is_ascii)?;
                }
                e.len()
            }
            RawName::Str { name, .. } => {
                let e = crate::name::ext(name);
                if e.is_empty() || e.len() > buf.len() || !e.is_ascii() {
                    return None;
                }
                buf[..e.len()].copy_from_slice(e.as_bytes());
                e.len()
            }
        };
        let s = &mut buf[..n];
        s.make_ascii_lowercase();
        std::str::from_utf8(s).ok()
    }
}

/// Запись каталога от листера: размеры уже в терминах анализа места (F01 §3.1).
#[derive(Copy, Clone, Debug)]
pub struct RawEntry<'a> {
    pub name: RawName<'a>,
    pub kind: EntryKind,
    /// Логический размер (у каталогов 0).
    pub logical: u64,
    /// На диске: `AllocationSize` с уточнением для сжатых; облачные — 0.
    pub disk: u64,
    /// нс от Unix epoch.
    pub mtime: i64,
    /// `FileId`; 0 — неизвестен (дедуп ссылок выключен).
    pub file_id: u64,
    /// [`RawEntry::OFFLINE`] | [`RawEntry::COMPRESSED`] | [`RawEntry::HIDDEN`] | [`RawEntry::SYSTEM`].
    pub flags: u8,
}

impl RawEntry<'_> {
    /// Облачная заглушка / офлайн: данных на диске нет.
    pub const OFFLINE: u8 = 1;
    /// Сжатый, разреженный или WOF.
    pub const COMPRESSED: u8 = 1 << 1;
    pub const HIDDEN: u8 = 1 << 2;
    pub const SYSTEM: u8 = 1 << 3;
}

/// Источник листинга для [`walk_with`] (F01 §3.1).
pub trait DirLister: Sync {
    /// Перечислить `dir`; `out` вернул `false` — прекратить (отмена). Возвращает
    /// хэндл, который получат подкаталоги в [`DirRef::parent`] (или `None`).
    fn list(
        &self,
        dir: &DirRef,
        out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
    ) -> Result<Option<Parent>, VfsError>;
}

/// Колбэки [`walk_with`]: на запись и на каталог, с накопителем на каталог (сумма
/// без атомиков на каждый файл).
pub trait DirVisitor: Sync {
    type Acc: Default;
    /// Запись каталога `dir`. Для каталога (`EntryKind::Dir`): `Some(tag)` — войти
    /// (метка ребёнка).
    fn entry(&self, dir: &DirRef, acc: &mut Self::Acc, e: &RawEntry<'_>) -> Option<u32>;
    /// Каталог прочитан (`Err(Cancelled)` — листинг прерван отменой). Зовётся до
    /// того, как его подкаталоги попадут в очередь.
    fn done(&self, dir: &DirRef, acc: Self::Acc, res: Result<(), VfsError>);
}

/// Параметры [`walk_with`].
#[derive(Copy, Clone, Debug)]
pub struct WalkOpts {
    pub threads: usize,
    /// Первый `tick` — раньше периода (первые строки — быстро).
    pub first_tick: Duration,
    pub period: Duration,
}

/// Обойти деревья `roots` листером `lister`; отмена — `halted` (проверяется между
/// каталогами и каждые 256 записей). `tick` — в вызывающем потоке.
pub fn walk_with<V: DirVisitor>(
    lister: &dyn DirLister,
    roots: Vec<DirRef>,
    opts: WalkOpts,
    halted: &(dyn Fn() -> bool + Sync),
    visitor: &V,
    tick: impl FnMut(),
) -> WalkStats {
    let work = ListerWork {
        lister,
        visitor,
        halted,
        stats: WalkStats::default(),
    };
    run_pool(
        &work,
        roots,
        opts.threads,
        halted,
        tick,
        opts.first_tick,
        opts.period,
    );
    work.stats
}

struct ListerWork<'a, V> {
    lister: &'a dyn DirLister,
    visitor: &'a V,
    halted: &'a (dyn Fn() -> bool + Sync),
    stats: WalkStats,
}

impl<V: DirVisitor> Work for ListerWork<'_, V> {
    type Item = DirRef;
    type After = ();
    type Scratch = Vec<(u32, PathBuf)>;

    fn run(&self, dir: DirRef, kids: &mut Vec<(u32, PathBuf)>, children: &mut Vec<DirRef>) {
        kids.clear();
        let mut acc = V::Acc::default();
        let mut n = 0u32;
        let mut stopped = false;
        let res = self.lister.list(&dir, &mut |e| {
            n = n.wrapping_add(1);
            if n.is_multiple_of(HALT_EVERY) && (self.halted)() {
                stopped = true;
                return false;
            }
            if let Some(tag) = self.visitor.entry(&dir, &mut acc, e)
                && e.kind == EntryKind::Dir
            {
                kids.push((tag, dir.loc.path.join(e.name.to_os())));
            }
            true
        });
        let (parent, res) = match res {
            Ok(p) if stopped => (p, Err(VfsError::Cancelled)),
            Ok(p) => (p, Ok(())),
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                (None, Err(e))
            }
        };
        self.visitor.done(&dir, acc, res);
        self.stats.dirs.fetch_add(1, Ordering::Relaxed);
        children.extend(kids.drain(..).map(|(tag, path)| DirRef {
            loc: Location {
                backend: dir.loc.backend.clone(),
                path,
                outer: dir.loc.outer.clone(),
            },
            tag,
            parent: parent.clone(),
        }));
    }

    fn after(&self, (): ()) {}
}

// ---------------------------------------------------------------------------
// Общий планировщик

/// Работа над одним каталогом очереди.
trait Work: Sync {
    type Item: Send;
    /// Что сделать после того, как дети попали в очередь.
    type After;
    /// Рабочий буфер потока.
    type Scratch: Default;
    fn run(
        &self,
        item: Self::Item,
        scratch: &mut Self::Scratch,
        children: &mut Vec<Self::Item>,
    ) -> Self::After;
    fn after(&self, a: Self::After);
}

struct Queue<T> {
    items: Vec<T>,
    /// Каталоги в работе у потоков.
    busy: usize,
}

fn run_pool<W: Work>(
    work: &W,
    init: Vec<W::Item>,
    threads: usize,
    halted: &(dyn Fn() -> bool + Sync),
    mut tick: impl FnMut(),
    first: Duration,
    period: Duration,
) {
    let queue = Mutex::new(Queue {
        items: init,
        busy: 0,
    });
    let cv = Condvar::new();
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| worker(work, &queue, &cv, halted));
        }
        let mut next = Instant::now() + first;
        loop {
            let q = queue.lock().unwrap_or_else(PoisonError::into_inner);
            if (q.items.is_empty() && q.busy == 0) || halted() {
                cv.notify_all();
                break;
            }
            let wait = next
                .saturating_duration_since(Instant::now())
                .min(IDLE_CHECK);
            drop(
                cv.wait_timeout(q, wait)
                    .unwrap_or_else(PoisonError::into_inner),
            );
            if Instant::now() >= next {
                tick();
                next = Instant::now() + period;
            }
        }
    });
}

fn worker<W: Work>(
    work: &W,
    queue: &Mutex<Queue<W::Item>>,
    cv: &Condvar,
    halted: &(dyn Fn() -> bool + Sync),
) {
    let mut scratch = W::Scratch::default();
    loop {
        let item = {
            let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if halted() {
                    return;
                }
                if let Some(job) = q.items.pop() {
                    q.busy += 1;
                    break job;
                }
                if q.busy == 0 {
                    cv.notify_all();
                    return;
                }
                q = cv
                    .wait_timeout(q, IDLE_CHECK)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        };
        let mut children = Vec::new();
        let after = work.run(item, &mut scratch, &mut children);
        let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
        q.items.extend(children);
        q.busy -= 1;
        drop(q);
        cv.notify_all();
        work.after(after);
    }
}
