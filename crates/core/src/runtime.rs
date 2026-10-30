//! Пулы потоков (ADR-0003): полосы I/O `Interactive`/`Bulk`, глобальный rayon для CPU,
//! выделенные потоки для долгоживущих воркеров.

use std::any::Any;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::cancel::CancelToken;
use crate::msg::{self, MsgInbox, MsgSink, WakeFlag, Waker};
use crate::state::Level;
use crate::text::tr;

/// Полоса I/O-пула.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lane {
    /// Листинг, stat, превью: то, чего ждёт пользователь.
    Interactive,
    /// Только **короткие** задачи (запись сессии, мелкие шаги операций). Долгие
    /// воркеры — ops, watcher, PTY — запускаются через [`Runtime::spawn_dedicated`],
    /// а ops сам ограничивает число своих потоков `ops.max_threads` (S0 v2 №7).
    Bulk,
}

/// Размеры пулов и канала.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub interactive_threads: u8,
    pub bulk_threads: u8,
    /// 0 = число ядер − 1 (минимум 1).
    pub cpu_threads: u8,
    pub msg_capacity: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            interactive_threads: 4,
            bulk_threads: 2,
            cpu_threads: 0,
            msg_capacity: 1024,
        }
    }
}

/// То, что получает задача.
#[derive(Debug)]
pub struct TaskCx {
    pub cancel: CancelToken,
    pub sink: MsgSink,
}

type TaskFn = Box<dyn FnOnce(TaskCx) + Send>;

struct Task {
    name: &'static str,
    cancel: CancelToken,
    f: TaskFn,
}

/// Счётчик живых потоков runtime; shutdown ждёт его обнуления.
#[derive(Default)]
struct Alive {
    count: Mutex<usize>,
    cv: Condvar,
}

impl Alive {
    fn enter(self: &Arc<Self>) -> AliveGuard {
        *self.count.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        AliveGuard(self.clone())
    }

    fn count(&self) -> usize {
        *self.count.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ждёт обнуления не дольше `deadline`; возвращает оставшихся.
    fn wait_zero(&self, deadline: Instant) -> usize {
        let mut n = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        while *n > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            n = self
                .cv
                .wait_timeout(n, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *n
    }
}

/// Уменьшает счётчик при выходе потока, в том числе при раскрутке паники.
struct AliveGuard(Arc<Alive>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        *self.0.count.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.cv.notify_all();
    }
}

/// Пулы потоков и канал сообщений в UI-поток.
pub struct Runtime {
    interactive: Sender<Task>,
    bulk: Sender<Task>,
    sink: MsgSink,
    flag: Arc<WakeFlag>,
    root: CancelToken,
    stopping: Arc<AtomicBool>,
    alive: Arc<Alive>,
    /// Очередь физических устройств — одна на процесс (S5-19).
    devices: Arc<crate::DeviceGate>,
}

impl Runtime {
    /// Создаёт пулы. Вызывается в UI-потоке: [`MsgSink::send`] из этого потока
    /// запрещён. Приёмник сообщений (`MsgInbox`) забирает Core.
    ///
    /// Глобальный rayon настраивается при первом вызове в процессе (`cpu-N`);
    /// если он уже создан, настройки `cpu_threads` не применяются.
    pub fn new(cfg: &RuntimeConfig, waker: Arc<dyn Waker>) -> (Runtime, MsgInbox) {
        let flag = Arc::new(WakeFlag::new(waker));
        let (sink, inbox) = msg::channel(cfg.msg_capacity as usize, flag.clone());
        let stopping = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(Alive::default());

        let cpu = match cfg.cpu_threads {
            0 => thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1)),
            n => n.into(),
        };
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(cpu)
            .thread_name(|i| format!("cpu-{i}"))
            .build_global()
        {
            tracing::debug!(error = %e, "rayon global pool already initialized");
        }

        let pool = |prefix: &str, n: u8| {
            let (tx, rx) = crossbeam_channel::unbounded::<Task>();
            for i in 0..n.max(1) {
                let (rx, sink, stopping, guard) =
                    (rx.clone(), sink.clone(), stopping.clone(), alive.enter());
                thread::Builder::new()
                    .name(format!("{prefix}-{i}"))
                    .spawn(move || {
                        let _guard = guard;
                        worker(&rx, &sink, &stopping);
                    })
                    .expect("spawn pool thread");
            }
            tx
        };
        let interactive = pool("io-i", cfg.interactive_threads);
        let bulk = pool("io-b", cfg.bulk_threads);

        let rt = Runtime {
            interactive,
            bulk,
            sink,
            flag,
            root: CancelToken::new(),
            stopping,
            alive,
            devices: Arc::default(),
        };
        (rt, inbox)
    }

    /// Задача в полосе I/O-пула. Задача, чей `cancel` отменён до старта, не
    /// запускается. Паника перехватывается: поток пула живёт дальше. Долгие
    /// воркеры сюда не ставить — см. [`Lane::Bulk`] и [`Runtime::spawn_dedicated`].
    pub fn spawn(
        &self,
        lane: Lane,
        name: &'static str,
        cancel: CancelToken,
        f: impl FnOnce(TaskCx) + Send + 'static,
    ) {
        let tx = match lane {
            Lane::Interactive => &self.interactive,
            Lane::Bulk => &self.bulk,
        };
        let task = Task {
            name,
            cancel,
            f: Box::new(f),
        };
        // Получатели живут, пока жив Runtime: ошибка невозможна.
        let _ = tx.send(task);
    }

    /// CPU-задача в глобальном rayon (`rayon::spawn`): сортировка, хеши, поиск.
    pub fn spawn_cpu(
        &self,
        name: &'static str,
        cancel: CancelToken,
        f: impl FnOnce(TaskCx) + Send + 'static,
    ) {
        let task = Task {
            name,
            cancel,
            f: Box::new(f),
        };
        let sink = self.sink.clone();
        rayon::spawn(move || run(task, &sink));
    }

    /// Выделенный именованный поток для долгоживущих воркеров (ops, watcher,
    /// PTY-reader). Паника перехватывается. Shutdown ждёт и эти потоки.
    pub fn spawn_dedicated(
        &self,
        name: String,
        cancel: CancelToken,
        f: impl FnOnce(TaskCx) + Send + 'static,
    ) -> JoinHandle<()> {
        let sink = self.sink.clone();
        let guard = self.alive.enter();
        thread::Builder::new()
            .name(name)
            .spawn(move || {
                let _guard = guard;
                let task = Task {
                    name: "dedicated",
                    cancel,
                    f: Box::new(f),
                };
                run(task, &sink);
            })
            .expect("spawn dedicated thread")
    }

    pub fn sink(&self) -> MsgSink {
        self.sink.clone()
    }

    /// Тот же waker, что передан в `Runtime::new` (S0 v3 №7).
    pub fn waker(&self) -> Arc<dyn Waker> {
        self.flag.waker()
    }

    /// Общий `WakeFlag` для `CommandSender` шины.
    pub(crate) fn wake_flag(&self) -> Arc<WakeFlag> {
        self.flag.clone()
    }

    /// Токен, производный от корневого: shutdown его отменит. Токены задач
    /// следует выводить отсюда (`rt.token().child()` и т.д.).
    /// Общая очередь устройств: копирование, удаление, скан анализа места.
    pub fn devices(&self) -> Arc<crate::DeviceGate> {
        Arc::clone(&self.devices)
    }

    pub fn token(&self) -> CancelToken {
        self.root.child()
    }

    /// Завершение: отменяет корневой токен, закрывает канал сообщений
    /// (заблокированные `send` возвращают `Closed`), не запускает задачи из очередей
    /// и ждёт потоки пулов и выделенные не дольше `timeout`. Возвращает число
    /// потоков, не успевших завершиться: они отпускаются (detach).
    pub fn shutdown(self, inbox: MsgInbox, timeout: Duration) -> usize {
        let deadline = Instant::now() + timeout;
        self.flag.close();
        self.stopping.store(true, Ordering::SeqCst);
        self.root.cancel();
        drop(inbox);
        let Runtime {
            interactive,
            bulk,
            alive,
            ..
        } = self;
        drop((interactive, bulk));
        let stuck = alive.wait_zero(deadline);
        if stuck > 0 {
            tracing::warn!(stuck, "runtime shutdown timed out, detaching threads");
        }
        stuck
    }

    #[cfg(test)]
    fn live_threads(&self) -> usize {
        self.alive.count()
    }
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("live_threads", &self.alive.count())
            .finish_non_exhaustive()
    }
}

fn worker(rx: &Receiver<Task>, sink: &MsgSink, stopping: &AtomicBool) {
    while let Ok(task) = rx.recv() {
        if !stopping.load(Ordering::SeqCst) {
            run(task, sink);
        }
    }
}

fn run(task: Task, sink: &MsgSink) {
    if task.cancel.is_cancelled() {
        return;
    }
    let Task { name, cancel, f } = task;
    let cx = TaskCx {
        cancel,
        sink: sink.clone(),
    };
    if let Err(p) = catch_unwind(AssertUnwindSafe(move || f(cx))) {
        tracing::error!(task = name, panic = panic_text(p.as_ref()), "task panicked");
        // Полный канал или shutdown — тост теряется, запись в логе остаётся.
        let _ = sink.try_send(msg::msg("task.panic", move |cx| {
            cx.toast(Level::Error, tr("core-task-panicked", &[("task", name)]));
        }));
    }
}

fn panic_text(p: &(dyn Any + Send)) -> &str {
    p.downcast_ref::<&str>()
        .copied()
        .or_else(|| p.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic>")
}

#[cfg(test)]
mod tests {
    use super::{Lane, Runtime, RuntimeConfig};
    use crate::cancel::CancelToken;
    use crate::msg::{SendError, TrySendError, Waker, msg};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Счётчик пробуждений; `wait` блокирует до следующего `wake`.
    #[derive(Default)]
    struct TestWaker {
        wakes: AtomicUsize,
        pending: Mutex<bool>,
        cv: Condvar,
    }

    impl Waker for TestWaker {
        fn wake(&self) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            *self.pending.lock().unwrap() = true;
            self.cv.notify_all();
        }

        /// Отложенное пробуждение (как `request_repaint_after`): тесты качают pump сами.
        fn wake_after(&self, _d: Duration) {}
    }

    impl TestWaker {
        fn wakes(&self) -> usize {
            self.wakes.load(Ordering::SeqCst)
        }

        fn wait(&self, timeout: Duration) -> bool {
            let g = self.pending.lock().unwrap();
            let (mut g, _) = self.cv.wait_timeout_while(g, timeout, |p| !*p).unwrap();
            std::mem::take(&mut *g)
        }
    }

    fn rt_with(cfg: &RuntimeConfig) -> (Runtime, crate::msg::MsgInbox, Arc<TestWaker>) {
        let w = Arc::new(TestWaker::default());
        let (rt, inbox) = Runtime::new(cfg, w.clone());
        (rt, inbox, w)
    }

    fn rt() -> (Runtime, crate::msg::MsgInbox, Arc<TestWaker>) {
        rt_with(&RuntimeConfig::default())
    }

    #[test]
    fn panic_does_not_kill_pool_threads() {
        let (rt, inbox, _w) = rt();
        let live = rt.live_threads();
        assert_eq!(live, 6);
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..20 {
            let done = done.clone();
            rt.spawn(Lane::Interactive, "boom", rt.token(), move |_| {
                done.fetch_add(1, Ordering::SeqCst);
                panic!("test panic");
            });
        }
        // Четыре задачи держат все четыре потока полосы одновременно: значит, все живы.
        let (in_tx, in_rx) = crossbeam_channel::unbounded();
        let (rel_tx, rel_rx) = crossbeam_channel::unbounded::<()>();
        for _ in 0..4 {
            let (in_tx, rel_rx) = (in_tx.clone(), rel_rx.clone());
            rt.spawn(Lane::Interactive, "hold", rt.token(), move |_| {
                in_tx
                    .send(thread::current().name().unwrap().to_owned())
                    .unwrap();
                rel_rx.recv().unwrap();
            });
        }
        let mut names: Vec<String> = (0..4)
            .map(|_| in_rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["io-i-0", "io-i-1", "io-i-2", "io-i-3"]);
        drop(rel_tx);
        assert_eq!(done.load(Ordering::SeqCst), 20);
        assert_eq!(rt.live_threads(), live);
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn thread_names() {
        let (rt, inbox, _w) = rt();
        let (tx, rx) = crossbeam_channel::unbounded();
        let name = |tx: &crossbeam_channel::Sender<String>| {
            let tx = tx.clone();
            move |_| {
                tx.send(thread::current().name().unwrap_or("").to_owned())
                    .unwrap();
            }
        };
        rt.spawn(Lane::Interactive, "n", rt.token(), name(&tx));
        rt.spawn(Lane::Bulk, "n", rt.token(), name(&tx));
        rt.spawn_cpu("n", rt.token(), name(&tx));
        rt.spawn_dedicated("watcher".into(), rt.token(), name(&tx))
            .join()
            .unwrap();
        let mut got: Vec<String> = (0..4)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        got.sort();
        assert!(got[0].starts_with("cpu-"), "{got:?}");
        assert!(got[1].starts_with("io-b-"), "{got:?}");
        assert!(got[2].starts_with("io-i-"), "{got:?}");
        assert_eq!(got[3], "watcher");
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn cancelled_before_start_is_skipped() {
        let (rt, inbox, _w) = rt();
        let ran = Arc::new(AtomicBool::new(false));
        let token = rt.token();
        token.cancel();
        let r = ran.clone();
        rt.spawn(Lane::Bulk, "skip", token, move |_| {
            r.store(true, Ordering::SeqCst);
        });
        let (tx, rx) = crossbeam_channel::bounded(1);
        rt.spawn(Lane::Bulk, "after", rt.token(), move |_| {
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Обе задачи могли попасть в разные потоки полосы; ждём завершения через shutdown.
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn task_msgs_reach_inbox_and_wake() {
        let (rt, inbox, w) = rt();
        rt.spawn(Lane::Interactive, "listing", rt.token(), move |cx| {
            cx.sink.send(msg("listed", |_| {}), &cx.cancel).unwrap();
        });
        assert!(w.wait(Duration::from_secs(5)));
        inbox.begin_pump();
        assert_eq!(inbox.try_recv().map(|m| m.name()), Some("listed"));
        assert!(inbox.try_recv().is_none());
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn panic_sends_toast_msg() {
        let (rt, inbox, w) = rt();
        rt.spawn(Lane::Bulk, "boom", rt.token(), |_| panic!("test panic"));
        assert!(w.wait(Duration::from_secs(5)));
        inbox.begin_pump();
        assert_eq!(inbox.try_recv().map(|m| m.name()), Some("task.panic"));
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn wake_is_coalesced_across_10k_sends() {
        const N: usize = 10_000;
        let (rt, inbox, w) = rt();
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        rt.spawn(Lane::Bulk, "flood", rt.token(), move |cx| {
            for _ in 0..N {
                cx.sink.send(msg("m", |_| {}), &cx.cancel).unwrap();
            }
            done_tx.send(()).unwrap();
        });
        // UI: pump только по пробуждению, как в кадре egui.
        let (mut pumps, mut got) = (0usize, 0usize);
        while got < N {
            assert!(w.wait(Duration::from_secs(10)), "no wake, got {got}");
            pumps += 1;
            inbox.begin_pump();
            while inbox.try_recv().is_some() {
                got += 1;
            }
        }
        // Сообщение попадает в канал раньше, чем поднимается флаг: последнее
        // пробуждение может прийти уже после чтения. UI отработает его пустым кадром.
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if w.wait(Duration::ZERO) {
            pumps += 1;
            inbox.begin_pump();
            assert!(inbox.try_recv().is_none());
        }
        let wakes = w.wakes();
        assert!(wakes <= pumps, "wakes {wakes} > pumps {pumps}");
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn sends_between_two_pumps_wake_once() {
        let (rt, inbox, w) = rt();
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        // 1000 < ёмкости канала: отправитель не блокируется, pump не происходит.
        rt.spawn(Lane::Bulk, "burst", rt.token(), move |cx| {
            for _ in 0..1000 {
                cx.sink.send(msg("m", |_| {}), &cx.cancel).unwrap();
            }
            done_tx.send(()).unwrap();
        });
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(w.wakes(), 1);
        inbox.begin_pump();
        assert_eq!(inbox.len(), 1000);
        while inbox.try_recv().is_some() {}
        assert_eq!(w.wakes(), 1);
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn poke_is_coalesced_and_safe_after_shutdown() {
        let (rt, inbox, w) = rt();
        let sink = rt.sink();
        let t = thread::spawn(move || {
            for _ in 0..10_000 {
                sink.poke();
            }
            sink
        });
        let sink = t.join().unwrap();
        assert_eq!(w.wakes(), 1);
        inbox.begin_pump();
        sink.poke();
        assert_eq!(w.wakes(), 2);
        assert!(Arc::ptr_eq(&rt.waker(), &(w.clone() as Arc<dyn Waker>)));
        inbox.begin_pump();
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
        sink.poke();
        assert_eq!(w.wakes(), 2);
        assert!(matches!(
            sink.try_send(msg("late", |_| {})),
            Err(TrySendError::Closed(_))
        ));
    }

    /// Канал на 1 сообщение, заполненный; `send` в отдельном потоке блокируется.
    fn blocked_send(
        rt: &Runtime,
        cancel: CancelToken,
    ) -> thread::JoinHandle<(Result<(), SendError>, Instant)> {
        let sink = rt.sink();
        sink.try_send(msg("fill", |_| {})).unwrap();
        let h = thread::spawn(move || {
            let r = sink.send(msg("blocked", |_| {}), &cancel);
            (r, Instant::now())
        });
        thread::sleep(Duration::from_millis(150));
        assert!(!h.is_finished(), "send must block on a full channel");
        h
    }

    fn one_slot() -> RuntimeConfig {
        RuntimeConfig {
            msg_capacity: 1,
            ..RuntimeConfig::default()
        }
    }

    #[test]
    fn blocked_send_returns_on_cancel_within_60ms() {
        let (rt, inbox, _w) = rt_with(&one_slot());
        let token = rt.token();
        let h = blocked_send(&rt, token.clone());
        let t0 = Instant::now();
        token.cancel();
        let (r, t1) = h.join().unwrap();
        assert_eq!(r, Err(SendError::Cancelled));
        let dt = t1 - t0;
        assert!(dt <= Duration::from_millis(60), "unblocked after {dt:?}");
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn blocked_send_returns_on_shutdown() {
        let (rt, inbox, _w) = rt_with(&one_slot());
        let h = blocked_send(&rt, CancelToken::new());
        let t0 = Instant::now();
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
        let (r, t1) = h.join().unwrap();
        assert_eq!(r, Err(SendError::Closed));
        assert!(
            t1 - t0 <= Duration::from_millis(60),
            "unblocked after {:?}",
            t1 - t0
        );
    }

    #[test]
    fn shutdown_with_hung_task_respects_timeout() {
        let (rt, inbox, _w) = rt();
        let release = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let r = release.clone();
        // Зависшая задача игнорирует отмену.
        rt.spawn(Lane::Bulk, "hung", rt.token(), move |_| {
            started_tx.send(()).unwrap();
            while !r.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let t0 = Instant::now();
        let stuck = rt.shutdown(inbox, Duration::from_millis(200));
        let dt = t0.elapsed();
        assert_eq!(stuck, 1);
        assert!(dt >= Duration::from_millis(200), "{dt:?}");
        assert!(dt < Duration::from_millis(400), "{dt:?}");
        release.store(true, Ordering::SeqCst);
    }

    #[test]
    fn shutdown_cancels_root_tokens_and_skips_queue() {
        let (rt, inbox, _w) = rt_with(&RuntimeConfig {
            bulk_threads: 1,
            ..RuntimeConfig::default()
        });
        let token = rt.token();
        let child = token.child();
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (seen_tx, seen_rx) = crossbeam_channel::bounded(1);
        rt.spawn(Lane::Bulk, "long", child, move |cx| {
            started_tx.send(()).unwrap();
            while !cx.cancel.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            seen_tx.send(()).unwrap();
        });
        let queued_ran = Arc::new(AtomicBool::new(false));
        let q = queued_ran.clone();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        rt.spawn(Lane::Bulk, "queued", rt.token(), move |_| {
            q.store(true, Ordering::SeqCst);
        });
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
        seen_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(token.is_cancelled());
        assert!(!queued_ran.load(Ordering::SeqCst));
    }

    #[test]
    fn config_defaults_and_serde() {
        let c = RuntimeConfig::default();
        assert_eq!(
            (
                c.interactive_threads,
                c.bulk_threads,
                c.cpu_threads,
                c.msg_capacity
            ),
            (4, 2, 0, 1024)
        );
        let partial: RuntimeConfig = toml::from_str("bulk_threads = 3").unwrap();
        assert_eq!(partial.bulk_threads, 3);
        assert_eq!(partial.interactive_threads, 4);
    }
}
