//! Движок job с настоящими `Core` и `Runtime` (приёмка S5-04): фиктивная
//! операция на 100k элементов — пауза ≤ 50 мс, отмена ≤ 100 мс, потоков движка
//! после job 0; паника в воркере — job `Failed`, остальное живо.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use urfin_core::fake::FakeListing;
use urfin_core::{
    Boot, CmdError, Command, CommandMeta, Core, Cx, Handler, JobId, JobSpec, JobState, Lane,
    Location, Services, Waker,
};

use super::{DirNode, ExecCx, ItemResult, JobEngine, OpExec, Plan, PlanError, Reports, Work};
use crate::api::{FailedItem, IoClass, ItemOp};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

type Run = Box<dyn FnOnce(&mut Cx<'_>) + Send>;

/// Handler `test.run`: выполняет переданное замыкание в UI-потоке (там есть `Runtime`).
struct RunHandler(Receiver<Run>);

static RUN: [CommandMeta; 1] = [CommandMeta {
    id: "test.run",
    title: "Run",
    category: "test",
    args: &[],
    repeatable: false,
}];

impl Handler for RunHandler {
    fn namespace(&self) -> &'static str {
        "test"
    }
    fn commands(&self) -> &'static [CommandMeta] {
        &RUN
    }
    fn handle(&mut self, _cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        while let Ok(f) = self.0.try_recv() {
            f(cx);
        }
        Ok(())
    }
}

pub(crate) struct H {
    pub(crate) core: Core,
    tx: Sender<Run>,
    pub(crate) eng: Arc<JobEngine>,
    listing: Arc<FakeListing>,
    /// Сторож зависания `pump_until`, не бюджет: тестам с массовым I/O на
    /// диске (10k переименований) нужно больше — под антивирусом и соседями
    /// по nextest один `MoveFileExW` стоит около миллисекунды.
    pub(crate) deadline: Duration,
}

fn dir() -> Location {
    Location::local(if cfg!(windows) {
        r"C:\urfin-fake"
    } else {
        "/urfin-fake"
    })
}

impl H {
    pub(crate) fn new(max_threads: u16) -> H {
        let listing = Arc::new(FakeListing::default());
        listing.set_dir(dir(), Vec::new());
        let services = Services {
            listing: listing.clone(),
            open_resolvers: Vec::new(),
        };
        let mut core = Core::new(Boot::new(dir(), dir()), services, Arc::new(NoWake));
        let (tx, rx) = crossbeam_channel::unbounded();
        core.register(Box::new(RunHandler(rx)));
        let gate = core.devices();
        H {
            core,
            tx,
            eng: Arc::new(JobEngine::new(max_threads, gate)),
            listing,
            deadline: Duration::from_secs(30),
        }
    }

    pub(crate) fn start<E: OpExec>(&mut self, workers: u16, exec: E) -> JobId {
        let (itx, irx) = crossbeam_channel::bounded(1);
        let eng = Arc::clone(&self.eng);
        let spec = JobSpec {
            kind: "fake",
            title: "fake".into(),
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "ops",
        };
        self.tx
            .send(Box::new(move |cx| {
                itx.send(eng.start(cx, spec, workers, exec)).unwrap();
            }))
            .unwrap();
        self.core.dispatch(Command::new("test.run"));
        irx.recv().unwrap()
    }

    pub(crate) fn pump(&mut self) {
        self.core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }

    pub(crate) fn pump_until(&mut self, what: &str, done: impl Fn(&H) -> bool) {
        let t0 = Instant::now();
        while !done(self) {
            assert!(t0.elapsed() < self.deadline, "timeout: {what}");
            self.pump();
        }
    }

    pub(crate) fn state(&self, id: JobId) -> JobState {
        self.core.state().jobs.get(id).unwrap().state.clone()
    }

    fn items_done(&self, id: JobId) -> u64 {
        let j = self.core.state().jobs.get(id).unwrap();
        j.progress.items_done.load(Ordering::Relaxed)
    }

    pub(crate) fn cmd(&mut self, name: &'static str, id: JobId) {
        self.core
            .dispatch(Command::new(name).arg("id", i64::try_from(id.0).unwrap()));
    }

    /// Ждёт конца job и выхода всех потоков движка.
    pub(crate) fn wait_done(&mut self, id: JobId) -> JobState {
        self.pump_until("job end and zero engine threads", |h| {
            !h.state(id).is_active() && h.eng.live_threads() == 0
        });
        self.state(id)
    }

    pub(crate) fn report(&self, id: JobId) -> Arc<crate::api::Report> {
        let r = self.core.state().ext.get::<Reports>().unwrap();
        Arc::clone(r.get(id).unwrap())
    }
}

/// Плоская фиктивная операция: `n` работ, каждая крутится `spin`.
struct Fake {
    n: u64,
    spin: Duration,
    panic_at: Option<u64>,
    fail_every: Option<u64>,
    /// План падает после стольких работ.
    plan_fails_at: Option<u64>,
    touch: Option<Location>,
    base: Instant,
    /// Время старта последней работы, нс от `base`.
    last_start: AtomicU64,
    /// Работ сейчас и максимум одновременно (общие для нескольких job).
    conc: Arc<AtomicUsize>,
    max_conc: Arc<AtomicUsize>,
}

impl Fake {
    fn new(n: u64, spin: Duration) -> Fake {
        Fake {
            n,
            spin,
            panic_at: None,
            fail_every: None,
            plan_fails_at: None,
            touch: None,
            base: Instant::now(),
            last_start: AtomicU64::new(0),
            conc: Arc::default(),
            max_conc: Arc::default(),
        }
    }

    fn since_base(&self, t: Instant) -> Duration {
        t.saturating_duration_since(self.base)
    }
}

#[derive(Clone)]
struct FakeOp(Arc<Fake>);

impl OpExec for FakeOp {
    type Item = u64;
    type Dir = ();

    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        for i in 0..self.0.n {
            if self.0.plan_fails_at == Some(i) {
                return Err(PlanError::Failed("fake plan error".into()));
            }
            p.push(None, i, 1)?;
        }
        Ok(())
    }

    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult {
        let f = &*self.0;
        let t0 = Instant::now();
        f.last_start
            .fetch_max(f.since_base(t0).as_nanos() as u64, Ordering::Relaxed);
        let c = f.conc.fetch_add(1, Ordering::SeqCst) + 1;
        f.max_conc.fetch_max(c, Ordering::SeqCst);
        if f.panic_at == Some(work.item) {
            f.conc.fetch_sub(1, Ordering::SeqCst);
            panic!("fake panic at {}", work.item);
        }
        while t0.elapsed() < f.spin {
            std::hint::spin_loop();
        }
        // Как между чанками: пауза ждёт здесь, отмена прерывает работу.
        if cx.checkpoint().is_err() {
            f.conc.fetch_sub(1, Ordering::SeqCst);
            return ItemResult::Cancelled;
        }
        if let Some(d) = &f.touch {
            cx.touch(d);
        }
        cx.add_bytes(1);
        f.conc.fetch_sub(1, Ordering::SeqCst);
        match f.fail_every {
            Some(k) if work.item.is_multiple_of(k) => ItemResult::Failed(FailedItem {
                path: PathBuf::from(work.item.to_string()),
                op: ItemOp::Copy,
                error: "fake".into(),
                class: IoClass::Other,
                pair: None,
            }),
            _ => ItemResult::Done,
        }
    }

    fn finalize_dir(&self, _dir: &DirNode<()>, _cx: &ExecCx<'_>) -> ItemResult {
        unreachable!("flat plan has no directories")
    }
}

const N: u64 = 100_000;
const SPIN: Duration = Duration::from_micros(50);

/// Приёмка: пауза за ≤ 50 мс (после `job.pause` ни одна работа не стартует позже
/// 50 мс), продолжение доводит job до конца, потоков движка после job 0.
#[test]
fn pause_takes_effect_within_50ms() {
    let mut h = H::new(64);
    let fake = Arc::new(Fake::new(N, SPIN));
    let id = h.start(4, FakeOp(Arc::clone(&fake)));
    assert_eq!(
        h.eng.live_threads(),
        6,
        "coordinator, questions + 4 workers"
    );
    h.pump_until("progress", |h| h.items_done(id) >= 2_000);
    let t0 = Instant::now();
    h.cmd("job.pause", id);
    assert_eq!(h.state(id), JobState::Paused);
    let t_pause = fake.since_base(t0);
    let paused_at = h.items_done(id);
    std::thread::sleep(Duration::from_millis(300));
    let last = Duration::from_nanos(fake.last_start.load(Ordering::Relaxed));
    let lag = last.saturating_sub(t_pause);
    eprintln!("pause: last work started {lag:?} after job.pause");
    assert!(lag <= Duration::from_millis(50), "pause lag {lag:?}");
    let frozen = h.items_done(id);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.items_done(id), frozen, "progress must stop while paused");
    assert!(frozen < N, "job must not finish before pause ({paused_at})");
    assert_eq!(h.eng.live_threads(), 6, "paused threads stay");

    h.cmd("job.resume", id);
    let st = h.wait_done(id);
    let JobState::Done(s) = st else {
        panic!("{st:?}")
    };
    assert_eq!((s.items, s.bytes, s.errors), (N, N, 0));
    let j = h.core.state().jobs.get(id).unwrap().progress.snapshot();
    assert_eq!((j.items_total, j.items_done), (N, N));
    assert!(j.total_final);
    assert_eq!(j.phase, urfin_core::Phase::Finalize);
    assert_eq!(h.report(id).summary, s);
    assert_eq!(h.eng.budget.used(), 0);
}

/// Приёмка: отмена за ≤ 100 мс — job `Cancelled` и все потоки движка вышли.
#[test]
fn cancel_takes_effect_within_100ms() {
    let mut h = H::new(64);
    let fake = Arc::new(Fake::new(N, SPIN));
    let id = h.start(4, FakeOp(Arc::clone(&fake)));
    h.pump_until("progress", |h| h.items_done(id) >= 2_000);
    let t0 = Instant::now();
    h.cmd("job.cancel", id);
    let st = h.wait_done(id);
    let dt = t0.elapsed();
    eprintln!("cancel: job Cancelled and 0 engine threads after {dt:?}");
    assert_eq!(st, JobState::Cancelled);
    assert!(dt <= Duration::from_millis(100), "cancel took {dt:?}");
    assert!(h.items_done(id) < N);
    assert_eq!(h.eng.budget.used(), 0);
}

/// Накладные расходы движка: 100k пустых работ (4 воркера), без проверки
/// времени — число в выводе теста.
#[test]
fn noop_100k_overhead() {
    let mut h = H::new(64);
    let t0 = Instant::now();
    let id = h.start(4, FakeOp(Arc::new(Fake::new(N, Duration::ZERO))));
    let JobState::Done(s) = h.wait_done(id) else {
        panic!("{:?}", h.state(id))
    };
    eprintln!(
        "noop 100k: {:?} (report {:?})",
        t0.elapsed(),
        h.report(id).elapsed
    );
    assert_eq!(s.items, N);
}

/// Отмена на паузе: потоки, ждущие в `checkpoint`, выходят.
#[test]
fn cancel_while_paused() {
    let mut h = H::new(64);
    let id = h.start(4, FakeOp(Arc::new(Fake::new(N, SPIN))));
    h.pump_until("progress", |h| h.items_done(id) >= 1_000);
    h.cmd("job.pause", id);
    std::thread::sleep(Duration::from_millis(50));
    let t0 = Instant::now();
    h.cmd("job.cancel", id);
    assert_eq!(h.wait_done(id), JobState::Cancelled);
    let dt = t0.elapsed();
    eprintln!("cancel while paused: {dt:?}");
    assert!(dt <= Duration::from_millis(100), "cancel took {dt:?}");
}

/// Приёмка: паника в воркере — job `Failed`, параллельная job и runtime живы.
#[test]
fn worker_panic_fails_job_only() {
    let mut h = H::new(64);
    let mut f = Fake::new(N, SPIN);
    f.panic_at = Some(500);
    let bad = h.start(4, FakeOp(Arc::new(f)));
    let good = h.start(2, FakeOp(Arc::new(Fake::new(20_000, Duration::ZERO))));
    let st = h.wait_done(bad);
    let JobState::Failed(e) = &st else {
        panic!("{st:?}")
    };
    assert!(e.contains("panicked"), "{e}");
    let JobState::Done(s) = h.wait_done(good) else {
        panic!("{:?}", h.state(good))
    };
    assert_eq!(s.items, 20_000);
    // Runtime и движок продолжают работать.
    let again = h.start(4, FakeOp(Arc::new(Fake::new(1_000, Duration::ZERO))));
    assert!(matches!(h.wait_done(again), JobState::Done(_)));
    assert_eq!(h.eng.budget.used(), 0);
}

/// Ошибка плана — job `Failed` с её текстом; поставленное успевает выполниться
/// или отбрасывается, потоки выходят.
#[test]
fn plan_error_fails_job() {
    let mut h = H::new(64);
    let mut f = Fake::new(N, Duration::ZERO);
    f.plan_fails_at = Some(1_000);
    let id = h.start(2, FakeOp(Arc::new(f)));
    assert_eq!(h.wait_done(id), JobState::Failed("fake plan error".into()));
    assert!(
        !h.core
            .state()
            .jobs
            .get(id)
            .unwrap()
            .progress
            .snapshot()
            .total_final
    );
}

/// Ошибки элементов не роняют job: счётчик, список `Report.failed`.
#[test]
fn item_errors_go_to_report() {
    let mut h = H::new(64);
    let mut f = Fake::new(1_000, Duration::ZERO);
    f.fail_every = Some(100);
    let id = h.start(3, FakeOp(Arc::new(f)));
    let JobState::Done(s) = h.wait_done(id) else {
        panic!("{:?}", h.state(id))
    };
    assert_eq!((s.items, s.errors), (990, 10));
    let r = h.report(id);
    assert_eq!(r.failed.len(), 10);
    assert!(r.failed.iter().all(|f| f.error.as_ref() == "fake"));
}

/// Потолок потоков на все job: лишняя job ждёт в `Queued`, одновременно
/// работ не больше `max_threads`.
#[test]
fn thread_budget_is_shared_and_queues() {
    let mut h = H::new(3);
    let conc = Arc::new(AtomicUsize::new(0));
    let max_conc = Arc::new(AtomicUsize::new(0));
    let fake = |n| {
        let mut f = Fake::new(n, Duration::from_micros(200));
        f.conc = Arc::clone(&conc);
        f.max_conc = Arc::clone(&max_conc);
        FakeOp(Arc::new(f))
    };
    let a = h.start(2, fake(5_000));
    let b = h.start(4, fake(5_000));
    let c = h.start(2, fake(500));
    assert_eq!(h.eng.budget.used(), 3, "a: 2, b: 1, c: none");
    h.pump_until("c queued", |h| h.state(c) == JobState::Queued);
    for id in [a, b, c] {
        let st = h.wait_done(id);
        assert!(matches!(st, JobState::Done(_)), "{id:?}: {st:?}");
    }
    assert!(max_conc.load(Ordering::SeqCst) <= 3);
    assert_eq!(h.eng.budget.used(), 0);
}

/// Отмена job, ждущей потоков в `Queued`.
#[test]
fn cancel_queued_job() {
    let mut h = H::new(1);
    let a = h.start(1, FakeOp(Arc::new(Fake::new(N, SPIN))));
    let b = h.start(1, FakeOp(Arc::new(Fake::new(10, Duration::ZERO))));
    h.pump_until("queued", |h| h.state(b) == JobState::Queued);
    h.cmd("job.cancel", b);
    h.pump_until("b cancelled", |h| h.state(b) == JobState::Cancelled);
    h.cmd("job.cancel", a);
    assert_eq!(h.wait_done(a), JobState::Cancelled);
}

/// Затронутый каталог → `panel.refresh` его табов не чаще раза в секунду и
/// в конце job.
#[test]
fn touched_refresh_is_throttled() {
    let mut h = H::new(64);
    h.pump_until("initial listing", |h| h.listing.loads().len() >= 2);
    let before = h.listing.loads().len();
    let mut f = Fake::new(3_000, Duration::from_micros(500));
    f.touch = Some(dir());
    let id = h.start(1, FakeOp(Arc::new(f)));
    let t0 = Instant::now();
    assert!(matches!(h.wait_done(id), JobState::Done(_)));
    let elapsed = t0.elapsed();
    h.pump_until("final refresh", |h| h.listing.loads().len() > before);
    for _ in 0..20 {
        h.pump();
    }
    // Две панели на одном каталоге: каждый сброс — два refresh.
    let flushes = (h.listing.loads().len() - before) / 2;
    eprintln!("touched: {flushes} refresh rounds in {elapsed:?}");
    assert!(flushes >= 2, "{flushes}");
    // Раз в секунду плюс финальный.
    let max = elapsed.as_millis().div_ceil(1000) as usize + 1;
    assert!(flushes <= max, "{flushes} in {elapsed:?}");
}

/// Дерево каталогов: каждый каталог финализируется один раз, после всех своих
/// файлов и подкаталогов; пропуск файла делает «грязными» каталог и предков.
#[derive(Default)]
struct TreeLog {
    seq: AtomicU64,
    /// (каталог, seq) завершения файлов.
    files: Mutex<Vec<(u32, u64)>>,
    /// (каталог, seq, clean) финализации.
    dirs: Mutex<Vec<(u32, u64, bool)>>,
    parents: Mutex<Vec<(u32, Option<u32>)>>,
}

struct TreeOp(Arc<TreeLog>);

const FANOUT: u32 = 3;
const DEPTH: u32 = 3;
const FILES: u32 = 4;
/// Файл, который пропускается (каталог 5).
const SKIP: (u32, u32) = (5, 1);

impl TreeOp {
    fn walk(
        &self,
        p: &mut Plan<'_, Self>,
        parent: Option<&Arc<DirNode<u32>>>,
        depth: u32,
        next: &mut u32,
    ) -> Result<(), PlanError> {
        let id = *next;
        *next += 1;
        self.0
            .parents
            .lock()
            .unwrap()
            .push((id, parent.map(|d| d.data)));
        let d = p.dir(parent, id)?;
        if depth > 0 {
            for _ in 0..FANOUT {
                self.walk(p, Some(&d), depth - 1, next)?;
            }
        }
        for f in 0..FILES {
            p.push(Some(&d), (id, f), 10)?;
        }
        p.close(&d);
        Ok(())
    }
}

impl OpExec for TreeOp {
    type Item = (u32, u32);
    type Dir = u32;

    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        let mut next = 0;
        self.walk(p, None, DEPTH, &mut next)
    }

    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult {
        assert_eq!(work.parent.as_ref().map(|d| d.data), Some(work.item.0));
        cx.add_bytes(10);
        let s = self.0.seq.fetch_add(1, Ordering::SeqCst);
        self.0.files.lock().unwrap().push((work.item.0, s));
        if work.item == SKIP {
            ItemResult::Skipped
        } else {
            ItemResult::Done
        }
    }

    fn finalize_dir(&self, dir: &DirNode<u32>, _cx: &ExecCx<'_>) -> ItemResult {
        let s = self.0.seq.fetch_add(1, Ordering::SeqCst);
        self.0.dirs.lock().unwrap().push((dir.data, s, dir.clean()));
        ItemResult::Done
    }
}

#[test]
fn finalizer_runs_post_order() {
    let mut h = H::new(64);
    let log = Arc::new(TreeLog::default());
    let id = h.start(4, TreeOp(Arc::clone(&log)));
    let JobState::Done(s) = h.wait_done(id) else {
        panic!("{:?}", h.state(id))
    };
    let parents: Vec<(u32, Option<u32>)> = log.parents.lock().unwrap().clone();
    let ndirs = parents.len();
    assert_eq!(ndirs, 40, "1 + 3 + 9 + 27");
    assert_eq!(s.items, ndirs as u64 * u64::from(FILES) - 1);
    assert_eq!(s.skipped, 1);
    let dirs = log.dirs.lock().unwrap().clone();
    let files = log.files.lock().unwrap().clone();
    assert_eq!(dirs.len(), ndirs, "each directory finalized once");
    let fin = |d: u32| dirs.iter().find(|x| x.0 == d).unwrap();
    // Предки пропущенного файла: каталог 5 и вверх до корня.
    let mut dirty = vec![SKIP.0];
    while let Some(&(_, Some(p))) = parents.iter().find(|x| x.0 == *dirty.last().unwrap()) {
        dirty.push(p);
    }
    for &(d, parent) in &parents {
        let (_, seq, clean) = *fin(d);
        assert_eq!(clean, !dirty.contains(&d), "dir {d}");
        for &(fd, fs) in &files {
            if fd == d {
                assert!(fs < seq, "file of {d} after its finalize");
            }
        }
        if let Some(p) = parent {
            assert!(seq < fin(p).1, "dir {d} finalized after parent {p}");
        }
    }
}
