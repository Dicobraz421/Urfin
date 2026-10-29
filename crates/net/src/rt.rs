//! Рантайм крейта (F03 §2.1, S8 §5): один tokio current-thread на выделенном потоке
//! Runtime (`spawn_dedicated`). Наружу — только отправка задач; UI-поток в сокеты
//! не ходит, результаты задачи шлют через `MsgSink`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::mpsc;
use urfin_core::Runtime;

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Как часто поток проверяет отмену Runtime: `CancelToken` — только опрос, а shutdown
/// ждёт выделенные потоки. Поток живёт лишь после первого сетевого действия.
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// Ручка рантайма: поток завершается, когда ручка уничтожена или Runtime отменён.
pub struct NetRt {
    tx: mpsc::UnboundedSender<Job>,
}

impl NetRt {
    pub fn start(rt: &Runtime) -> NetRt {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        rt.spawn_dedicated("net".into(), rt.token(), move |tcx| {
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
            tokio.block_on(async move {
                loop {
                    match tokio::time::timeout(CANCEL_POLL, rx.recv()).await {
                        Ok(Some(job)) => drop(tokio::spawn(job)),
                        Ok(None) => break,
                        Err(_) if tcx.cancel.is_cancelled() => break,
                        Err(_) => {}
                    }
                }
            });
        });
        NetRt { tx }
    }

    /// Задача в рантайме крейта. `false` — рантайм уже остановлен.
    pub fn spawn(&self, f: impl Future<Output = ()> + Send + 'static) -> bool {
        self.tx.send(Box::pin(f)).is_ok()
    }
}

impl fmt::Debug for NetRt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetRt")
            .field("closed", &self.tx.is_closed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc as std_mpsc;

    use urfin_core::RuntimeConfig;

    use super::*;

    struct NoWake;

    impl urfin_core::Waker for NoWake {
        fn wake(&self) {}
        fn wake_after(&self, _d: Duration) {}
    }

    #[test]
    fn runs_jobs_and_stops_on_shutdown() {
        let (rt, inbox) = Runtime::new(&RuntimeConfig::default(), Arc::new(NoWake));
        let net = NetRt::start(&rt);
        let (tx, rx) = std_mpsc::channel();
        assert!(net.spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            tx.send(std::thread::current().name().map(str::to_owned))
                .unwrap();
        }));
        let name = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(name.as_deref(), Some("net"));
        // Ручка жива: поток выходит по отмене Runtime.
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
        drop(net);
    }
}
