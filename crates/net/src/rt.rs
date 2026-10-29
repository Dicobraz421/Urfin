//! Рантайм крейта (F03 §2.1, S8 §5): один tokio current-thread на выделенном потоке
//! Runtime (`spawn_dedicated`). Наружу — только отправка задач; UI-поток в сокеты
//! не ходит, результаты задачи шлют через `MsgSink`.
//!
//! В простое поток не просыпается (F03-01b): он ждёт задачу из канала или конец одной из
//! своих задач — без таймеров и опроса. Живёт, пока есть задачи или очередь не пуста;
//! подключение — задача на весь сеанс (задачи из `tokio::spawn` внутри поток не держат).
//! Потом выходит сам, следующая задача запускает его заново. Явный выход — [`NetRt::stop`]:
//! отмену Runtime поток не смотрит, а `Runtime::shutdown` его ждёт.

use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::Pin;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinSet};
use urfin_core::Runtime;

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Общее для ручки и потока.
#[derive(Default)]
struct Shared {
    /// Канал живого потока; `None` — потока нет. Под замком: поток уходит, только если
    /// очередь пуста, — задача не теряется между проверкой и выходом.
    tx: Mutex<Option<mpsc::UnboundedSender<Job>>>,
    /// Итерации цикла потока: в простое не растут.
    #[cfg(test)]
    turns: AtomicU64,
}

impl Shared {
    fn tx(&self) -> MutexGuard<'_, Option<mpsc::UnboundedSender<Job>>> {
        self.tx.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Ручка рантайма: поток запускается первой задачей и завершается сам, когда задач нет,
/// или по [`NetRt::stop`] (в том числе при уничтожении ручки).
#[derive(Default)]
pub struct NetRt {
    shared: Arc<Shared>,
}

impl NetRt {
    pub fn new() -> NetRt {
        NetRt::default()
    }

    /// Задача в рантайме крейта; поток запускается, если его нет. `false` — Runtime
    /// остановлен или tokio не создался.
    pub fn spawn(&self, rt: &Runtime, f: impl Future<Output = ()> + Send + 'static) -> bool {
        let mut tx = self.shared.tx();
        if tx.as_ref().is_some_and(mpsc::UnboundedSender::is_closed) {
            *tx = None;
        }
        tx.get_or_insert_with(|| start(rt, &self.shared))
            .send(Box::pin(f))
            .is_ok()
    }

    /// Явный выход потока (выход приложения): канал закрывается, бегущие задачи
    /// прерываются. Следующая [`NetRt::spawn`] запустит поток заново.
    pub fn stop(&self) {
        drop(self.shared.tx().take());
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.shared.tx().is_some()
    }

    #[cfg(test)]
    fn turns(&self) -> u64 {
        self.shared.turns.load(Ordering::SeqCst)
    }
}

impl Drop for NetRt {
    fn drop(&mut self) {
        self.stop();
    }
}

impl fmt::Debug for NetRt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetRt")
            .field("running", &self.shared.tx().is_some())
            .finish()
    }
}

fn start(rt: &Runtime, shared: &Arc<Shared>) -> mpsc::UnboundedSender<Job> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    let shared = Arc::clone(shared);
    rt.spawn_dedicated("net".into(), rt.token(), move |_| {
        let tokio = match tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(target: "net", error = %e, "cannot start tokio runtime");
                return;
            }
        };
        tokio.block_on(serve(&mut rx, &shared));
    });
    tx
}

/// Что разбудило поток.
enum Wake {
    Job(Job),
    Closed,
    Done(Result<(), JoinError>),
}

async fn serve(rx: &mut mpsc::UnboundedReceiver<Job>, shared: &Shared) {
    let mut jobs = JoinSet::new();
    loop {
        #[cfg(test)]
        shared.turns.fetch_add(1, Ordering::SeqCst);
        match poll_fn(|cx| next(cx, rx, &mut jobs)).await {
            Wake::Job(job) => drop(jobs.spawn(job)),
            // Бегущие задачи прерываются вместе с `jobs`.
            Wake::Closed => return,
            Wake::Done(r) => {
                if let Err(e) = r
                    && e.is_panic()
                {
                    tracing::error!(target: "net", "net task panicked");
                }
                if jobs.is_empty() && idle(rx, shared) {
                    return;
                }
            }
        }
    }
}

fn next(
    cx: &mut Context<'_>,
    rx: &mut mpsc::UnboundedReceiver<Job>,
    jobs: &mut JoinSet<()>,
) -> Poll<Wake> {
    match rx.poll_recv(cx) {
        Poll::Ready(Some(job)) => return Poll::Ready(Wake::Job(job)),
        Poll::Ready(None) => return Poll::Ready(Wake::Closed),
        Poll::Pending => {}
    }
    match jobs.poll_join_next(cx) {
        Poll::Ready(Some(r)) => Poll::Ready(Wake::Done(r)),
        // Задач нет: ждём только канал.
        Poll::Ready(None) | Poll::Pending => Poll::Pending,
    }
}

/// Задач нет: под замком канала — выходить ли. При пустой очереди канал забирается,
/// и следующая задача запустит новый поток.
fn idle(rx: &mpsc::UnboundedReceiver<Job>, shared: &Shared) -> bool {
    let mut tx = shared.tx();
    if !rx.is_empty() {
        return false;
    }
    // Закрытый канал уже забрал `stop`: в слоте может быть канал нового потока.
    if !rx.is_closed() {
        *tx = None;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc as std_mpsc;
    use std::time::{Duration, Instant};

    use tokio::sync::oneshot;
    use urfin_core::{MsgInbox, RuntimeConfig};

    use super::*;

    struct NoWake;

    impl urfin_core::Waker for NoWake {
        fn wake(&self) {}
        fn wake_after(&self, _d: Duration) {}
    }

    fn runtime() -> (Runtime, MsgInbox) {
        Runtime::new(&RuntimeConfig::default(), Arc::new(NoWake))
    }

    /// Задача «подключение»: запущена к возврату и держит поток, пока не закроют ответ.
    fn connection(net: &NetRt, rt: &Runtime) -> oneshot::Sender<()> {
        let (close, closed) = oneshot::channel::<()>();
        let (opened_tx, opened) = std_mpsc::channel();
        assert!(net.spawn(rt, async move {
            opened_tx.send(()).unwrap();
            let _ = closed.await;
        }));
        opened.recv_timeout(Duration::from_secs(5)).unwrap();
        close
    }

    fn wait_stopped(net: &NetRt) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while net.is_running() {
            assert!(Instant::now() < deadline, "net thread still running");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn runs_jobs_on_net_thread() {
        let (rt, inbox) = runtime();
        let net = NetRt::new();
        assert!(!net.is_running());
        let (tx, rx) = std_mpsc::channel();
        assert!(net.spawn(&rt, async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            tx.send(std::thread::current().name().map(str::to_owned))
                .unwrap();
        }));
        let name = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(name.as_deref(), Some("net"));
        wait_stopped(&net);
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn stops_when_channel_closed() {
        let (rt, inbox) = runtime();
        let net = NetRt::new();
        // Подключение не закрыто: поток выходит только по `stop`, прерывая задачу.
        let _close = connection(&net, &rt);
        assert!(net.is_running());
        net.stop();
        assert!(!net.is_running());
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn drop_stops_thread() {
        let (rt, inbox) = runtime();
        let net = NetRt::new();
        let _close = connection(&net, &rt);
        drop(net);
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn stops_without_connections_and_restarts() {
        let (rt, inbox) = runtime();
        let net = NetRt::new();
        let a = connection(&net, &rt);
        let b = connection(&net, &rt);
        a.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        // Одно подключение ещё открыто.
        assert!(net.is_running());
        b.send(()).unwrap();
        wait_stopped(&net);
        // Следующее действие — новый поток.
        let c = connection(&net, &rt);
        assert!(net.is_running());
        c.send(()).unwrap();
        wait_stopped(&net);
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }

    #[test]
    fn no_wakeups_while_idle() {
        let (rt, inbox) = runtime();
        let net = NetRt::new();
        let close = connection(&net, &rt);
        let before = net.turns();
        // Дольше прежнего периода опроса (200 мс).
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(net.turns(), before);
        close.send(()).unwrap();
        wait_stopped(&net);
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
    }
}
