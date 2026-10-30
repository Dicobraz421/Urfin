//! Координатор job (S5 §3): план, Finalizer, итог, троттлинг затронутых
//! каталогов. Свой выделенный поток; воркеры — [`super::worker`].

use std::any::Any;
use std::collections::HashSet;
use std::fmt;
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, SendTimeoutError, Sender, TrySendError};
use urfin_core::{
    CancelToken, Cancelled, Cx, JobHandle, JobId, JobSpec, JobState, JobSummary, Location, MsgSink,
    Phase, Progress, msg,
};

use super::finalize::DirNode;
use super::park::{self, Park, Resolved};
use super::sched::{Lease, ThreadBudget};
use super::{ItemResult, JobEngine, OpExec, PlanError, QUEUE_CAP, Reports, Work, worker};
use crate::api::{FailedItem, Issue, Report};
use urfin_core::device::{DeviceGate, DeviceLease, JobCtl, Slot};

/// Шаг ожиданий координатора и воркеров: отмена и пауза видны не позже.
pub(super) const POLL: Duration = Duration::from_millis(10);
/// Не чаще — `panel.refresh` затронутых табов (S0 v2 №14).
const TOUCH_EVERY: Duration = Duration::from_secs(1);
/// Шаг [`OpExec::tick`]: замер AIMD (S5 §3.3) и сброс журнала.
const TICK_EVERY: Duration = Duration::from_millis(500);
/// Раз в столько работ план проверяет паузу и обслуживает Finalizer.
const PLAN_TICK: u64 = 64;
/// Окно замера скорости для `Report.peak_bps`.
const RATE_WINDOW: Duration = Duration::from_millis(500);

/// Событие воркера для координатора.
pub(super) enum Event<D> {
    /// Все дети каталога готовы — в Finalizer.
    DirReady(Arc<DirNode<D>>),
    /// Воркер вышел.
    Exit,
}

/// Общее для координатора и воркеров, не зависящее от операции.
pub(super) struct Ctl {
    pub(super) job: JobHandle,
    /// Остановка движка: shutdown, паника, ошибка плана. Отмена пользователем —
    /// токен job (`job.cancel`).
    pub(super) stop: CancelToken,
    fatal: Mutex<Option<Arc<str>>>,
    touched: Mutex<HashSet<Location>>,
    done: AtomicU64,
    skipped: AtomicU64,
    errors: AtomicU64,
    failed: Mutex<Vec<FailedItem>>,
    pub(super) span: tracing::Span,
}

impl Ctl {
    pub(super) fn stopped(&self) -> bool {
        self.stop.is_cancelled() || self.job.cancel_token().is_cancelled()
    }

    /// Ошибка всей job: первая причина остаётся, движок останавливается.
    pub(super) fn fail(&self, why: &str) {
        tracing::error!(error = why, "job failed");
        self.fatal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| why.into());
        self.stop.cancel();
    }

    /// Учесть итог работы. `false` — работа прервана отменой.
    pub(super) fn record<D>(&self, r: ItemResult, parent: Option<&Arc<DirNode<D>>>) -> bool {
        match r {
            ItemResult::Done => {
                self.done.fetch_add(1, Ordering::Relaxed);
            }
            ItemResult::Skipped => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                parent.inspect(|p| p.mark_dirty());
            }
            ItemResult::Failed(f) => {
                self.errors.fetch_add(1, Ordering::Relaxed);
                parent.inspect(|p| p.mark_dirty());
                let mut v = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
                if v.len() < Report::FAILED_MAX {
                    v.push(f);
                }
            }
            ItemResult::Cancelled => return false,
            ItemResult::Park(..) => tracing::error!("parked work recorded as a result"),
        }
        true
    }
}

/// Контекст работы воркера и Finalizer.
pub(crate) struct ExecCx<'a> {
    pub(super) ctl: &'a Ctl,
}

impl ExecCx<'_> {
    /// Между файлами и чанками: ждёт на паузе, `Err` при отмене и остановке.
    pub(crate) fn checkpoint(&self) -> Result<(), Cancelled> {
        self.ctl.stop.check()?;
        self.ctl.job.checkpoint()
    }

    /// Токен отмены job (листинг каталогов между порциями).
    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.ctl.job.cancel_token().clone()
    }

    pub(crate) fn progress(&self) -> &Progress {
        self.ctl.job.progress()
    }

    pub(crate) fn add_bytes(&self, n: u64) {
        self.progress().bytes_done.fetch_add(n, Ordering::Relaxed);
    }

    /// Ошибка всей job (`ErrorPolicy::Abort`): движок останавливается.
    pub(crate) fn abort(&self, why: &str) {
        self.ctl.fail(why);
    }

    /// Каталог изменился: табы с ним обновятся (не чаще раза в секунду).
    pub(crate) fn touch(&self, dir: &Location) {
        let mut set = self
            .ctl
            .touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !set.contains(dir) {
            set.insert(dir.clone());
        }
    }
}

impl fmt::Debug for ExecCx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecCx")
            .field("job", &self.ctl.job.id())
            .finish_non_exhaustive()
    }
}

/// Общее с воркерами.
pub(super) struct Shared<E: OpExec> {
    pub(super) ctl: Ctl,
    pub(super) exec: E,
    pub(super) events: Sender<Event<E::Dir>>,
    pub(super) park: Park<E>,
}

/// Уменьшает счётчик живых потоков движка при выходе (и при раскрутке паники).
pub(super) struct LiveGuard(Arc<AtomicUsize>);

impl LiveGuard {
    /// Вызывается до запуска потока: счётчик не пропускает ещё не стартовавший.
    pub(super) fn enter(live: &Arc<AtomicUsize>) -> LiveGuard {
        live.fetch_add(1, Ordering::SeqCst);
        LiveGuard(Arc::clone(live))
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Останавливает воркеров, если координатор выходит не штатно.
struct StopOnDrop(CancelToken);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(super) fn panic_text(p: &(dyn Any + Send)) -> &str {
    p.downcast_ref::<&str>()
        .copied()
        .or_else(|| p.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic>")
}

/// UI-поток: регистрирует job, берёт потоки, запускает воркеров и координатора.
pub(super) fn start<E: OpExec>(
    eng: &JobEngine,
    cx: &mut Cx<'_>,
    spec: JobSpec,
    want: u16,
    exec: E,
) -> JobId {
    let (kind, spec_prio) = (spec.kind, spec.priority);
    let job = cx.state.jobs.start(spec, cx.rt.sink());
    let id = job.id();
    let span = tracing::info_span!("ops.job", job = id.0, kind);
    let (tx, rx) = crossbeam_channel::bounded(QUEUE_CAP);
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
    let sh = Arc::new(Shared {
        ctl: Ctl {
            job,
            stop: cx.rt.token(),
            fatal: Mutex::new(None),
            touched: Mutex::default(),
            done: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            failed: Mutex::default(),
            span,
        },
        exec,
        events: ev_tx,
        park: Park::new(),
    });
    let ctl = Arc::new(JobCtl::default());
    ctl.prio.store(spec_prio, Ordering::Relaxed);
    eng.ctls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id, Arc::clone(&ctl));
    let lease = eng.budget.try_take(want);
    let handles = worker::spawn(cx.rt, &eng.live, &sh, &rx, lease.n);
    let asker = {
        let (sh, live) = (Arc::clone(&sh), LiveGuard::enter(&eng.live));
        cx.rt
            .spawn_dedicated(format!("ops-ask-{}", id.0), CancelToken::new(), move |_| {
                let _live = live;
                park::ask_loop(&sh);
            })
    };
    let co = Coord {
        sh,
        tx: Some(tx),
        rx,
        events: ev_rx,
        handles,
        asker: Some(asker),
        gate: Arc::clone(&eng.gate),
        ctl,
        ctls: Arc::clone(&eng.ctls),
        device: None,
        exited: 0,
        sink: cx.rt.sink(),
        live: Arc::clone(&eng.live),
        last_touch: None,
        last_tick: Instant::now(),
        next_tick: Instant::now(),
        rate: (Instant::now(), 0),
        peak_bps: 0,
        started: Instant::now(),
        transfer: false,
        pushed: 0,
    };
    let (budget, live) = (Arc::clone(&eng.budget), LiveGuard::enter(&eng.live));
    // Свой токен: поток стартует и при shutdown, чтобы закрыть job и очередь.
    cx.rt
        .spawn_dedicated(format!("ops-job-{}", id.0), CancelToken::new(), move |_| {
            let _live = live;
            co.run(lease, want, &budget);
        });
    id
}

/// Управление живыми job (общая таблица движка).
type Ctls = Arc<Mutex<std::collections::HashMap<JobId, Arc<JobCtl>>>>;

/// Состояние координатора.
struct Coord<E: OpExec> {
    sh: Arc<Shared<E>>,
    /// Очередь работ; `None` — план закончен, воркеры доедают остаток.
    tx: Option<Sender<Work<E>>>,
    rx: Receiver<Work<E>>,
    events: Receiver<Event<E::Dir>>,
    handles: Vec<JoinHandle<()>>,
    /// Поток вопросов (S5-07).
    asker: Option<JoinHandle<()>>,
    /// Очередь устройства (S5-19) и слоты, занятые job.
    gate: Arc<DeviceGate>,
    ctl: Arc<JobCtl>,
    ctls: Ctls,
    device: Option<DeviceLease>,
    exited: usize,
    sink: MsgSink,
    live: Arc<AtomicUsize>,
    last_touch: Option<Instant>,
    /// Последний [`OpExec::tick`].
    last_tick: Instant,
    /// Не раньше — троттлинг и замер скорости (`service` зовётся на каждую работу плана).
    next_tick: Instant,
    /// Начало окна скорости и `bytes_done` в нём.
    rate: (Instant, u64),
    peak_bps: u64,
    started: Instant,
    transfer: bool,
    /// Работ поставлено планом.
    pushed: u64,
}

impl<E: OpExec> Coord<E> {
    fn cx(&self) -> ExecCx<'_> {
        ExecCx { ctl: &self.sh.ctl }
    }

    fn run(mut self, lease: Lease, want: u16, budget: &Arc<ThreadBudget>) {
        let span = self.sh.ctl.span.clone();
        let _enter = span.enter();
        let _stop = StopOnDrop(self.sh.ctl.stop.clone());
        self.sh.ctl.job.progress().set_phase(Phase::Scan);
        let lease = if lease.n > 0 {
            Some(lease)
        } else {
            drop(lease);
            self.queued(want, budget)
        };
        if lease.is_some() {
            tracing::debug!(workers = self.handles.len(), "job started");
            self.plan();
        }
        drop(self.tx.take());
        // Воркеры доедают очередь; Finalizer — по ходу.
        while self.exited < self.handles.len() {
            self.service(POLL);
        }
        self.close_park();
        self.sh.ctl.job.progress().set_phase(Phase::Finalize);
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev);
        }
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
        drop(lease);
        self.finish();
    }

    /// Воркеры вышли: поток вопросов — завершить, неразобранное (отмена) — убрать.
    fn close_park(&mut self) {
        let sh = &self.sh;
        sh.park.close();
        if let Some(h) = self.asker.take() {
            if sh.ctl.stopped() && sh.park.asking() {
                // Остановка движка (ошибка плана, паника) не видна `ask`: он ждёт
                // только отмены job. Открытый вопрос держал бы поток вопросов.
                sh.ctl.job.cancel_token().cancel();
            }
            let _ = h.join();
        }
        let left = sh.park.drain();
        if !left.is_empty() {
            tracing::info!(n = left.len(), "parked work not processed");
        }
        for p in left {
            sh.ctl.skipped.fetch_add(1, Ordering::Relaxed);
            sh.exec.discard(p.work.item, &p.issue);
        }
    }

    /// Свободных потоков нет: `Queued`, пока не освободятся; воркеров
    /// запускает UI-поток (`Runtime` есть только там).
    fn queued(&mut self, want: u16, budget: &Arc<ThreadBudget>) -> Option<Lease> {
        let ctl = &self.sh.ctl;
        ctl.job.set_state(JobState::Queued);
        let lease = budget
            .take_wait(want, ctl.job.cancel_token(), &ctl.stop)
            .ok()?;
        ctl.job.set_state(JobState::Running);
        let (htx, hrx) = crossbeam_channel::bounded(1);
        let (sh, rx, live, n) = (
            Arc::clone(&self.sh),
            self.rx.clone(),
            Arc::clone(&self.live),
            lease.n,
        );
        let m = msg("ops.workers", move |cx| {
            let _ = htx.send(worker::spawn(cx.rt, &live, &sh, &rx, n));
        });
        self.sink.send(m, &ctl.stop).ok()?;
        loop {
            match hrx.recv_timeout(POLL) {
                Ok(h) => {
                    self.handles = h;
                    return Some(lease);
                }
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) if ctl.stopped() => return None,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn plan(&mut self) {
        let sh = Arc::clone(&self.sh);
        let r = catch_unwind(AssertUnwindSafe(|| sh.exec.plan(&mut Plan { co: self })));
        match r {
            Ok(Ok(())) => {
                let p = sh.ctl.job.progress();
                p.total_final.store(true, Ordering::Relaxed);
                if !self.transfer {
                    p.set_phase(Phase::Transfer);
                }
            }
            Ok(Err(PlanError::Cancelled)) => {}
            Ok(Err(PlanError::Failed(e))) => sh.ctl.fail(&e),
            Err(p) => sh
                .ctl
                .fail(&format!("plan panicked: {}", panic_text(p.as_ref()))),
        }
    }

    /// Очередь полна (бэкпрешер): ждём место, обслуживая Finalizer.
    fn push_full(&mut self, mut w: Work<E>) -> Result<(), Cancelled> {
        let Some(tx) = self.tx.clone() else {
            return Err(Cancelled);
        };
        loop {
            self.cx().checkpoint()?;
            self.service(Duration::ZERO);
            match tx.send_timeout(w, POLL) {
                Ok(()) => return Ok(()),
                Err(SendTimeoutError::Timeout(back)) => w = back,
                Err(SendTimeoutError::Disconnected(_)) => return Err(Cancelled),
            }
        }
    }

    /// События воркеров (до `wait`), троттлинг `Touched`, замер скорости.
    fn service(&mut self, wait: Duration) {
        let first = if wait.is_zero() {
            self.events.try_recv().ok()
        } else {
            self.events.recv_timeout(wait).ok()
        };
        if let Some(ev) = first {
            self.on_event(ev);
            while let Ok(ev) = self.events.try_recv() {
                self.on_event(ev);
            }
        }
        let now = Instant::now();
        if now < self.next_tick {
            return;
        }
        self.next_tick = now + POLL;
        if self.last_touch.is_none_or(|t| now - t >= TOUCH_EVERY) {
            self.flush_touched(now);
        }
        if now - self.last_tick >= TICK_EVERY {
            self.last_tick = now;
            self.sh.exec.tick(&self.cx());
        }
        let dt = now - self.rate.0;
        if dt >= RATE_WINDOW {
            let bytes = self
                .sh
                .ctl
                .job
                .progress()
                .bytes_done
                .load(Ordering::Relaxed);
            let bps = u128::from(bytes - self.rate.1) * 1000 / dt.as_millis();
            self.peak_bps = self.peak_bps.max(u64::try_from(bps).unwrap_or(u64::MAX));
            self.rate = (now, bytes);
        }
    }

    fn on_event(&mut self, ev: Event<E::Dir>) {
        match ev {
            Event::DirReady(d) => self.finalize(d),
            Event::Exit => self.exited += 1,
        }
    }

    /// Finalizer: каталог, затем родители, у которых он был последним ребёнком.
    fn finalize(&self, dir: Arc<DirNode<E::Dir>>) {
        let ctl = &self.sh.ctl;
        let cx = self.cx();
        let mut next = Some(dir);
        while let Some(d) = next.take() {
            if cx.checkpoint().is_err() {
                return;
            }
            let ok = match catch_unwind(AssertUnwindSafe(|| self.sh.exec.finalize_dir(&d, &cx))) {
                Ok(ItemResult::Done) => true,
                Ok(r) => {
                    if !ctl.record(r, d.parent.as_ref()) {
                        return;
                    }
                    false
                }
                Err(p) => {
                    ctl.fail(&format!("finalize panicked: {}", panic_text(p.as_ref())));
                    return;
                }
            };
            if let Some(p) = &d.parent {
                if !ok || !d.clean() {
                    p.mark_dirty();
                }
                if p.complete_child() {
                    next = Some(Arc::clone(p));
                }
            }
        }
    }

    /// Затронутые каталоги → `panel.refresh` их табов (UI-поток).
    fn flush_touched(&mut self, now: Instant) {
        let set = mem::take(
            &mut *self
                .sh
                .ctl
                .touched
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        if set.is_empty() {
            return;
        }
        self.last_touch = Some(now);
        let dirs: Vec<Location> = set.into_iter().collect();
        let m = msg("ops.touched", move |cx| super::refresh_tabs(cx, &dirs));
        let _ = self.sink.send(m, &CancelToken::new());
    }

    /// Отчёт и итог job. Затронутые каталоги — всегда, даже раньше секунды:
    /// панели должны увидеть результат.
    fn finish(mut self) {
        self.flush_touched(Instant::now());
        drop(self.device.take());
        self.ctls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.sh.ctl.job.id());
        let ctl = &self.sh.ctl;
        let p = ctl.job.progress();
        let summary = JobSummary {
            items: ctl.done.load(Ordering::Relaxed),
            bytes: p.bytes_done.load(Ordering::Relaxed),
            skipped: ctl.skipped.load(Ordering::Relaxed),
            errors: ctl.errors.load(Ordering::Relaxed),
        };
        let fatal = ctl
            .fatal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let failed = mem::take(&mut *ctl.failed.lock().unwrap_or_else(PoisonError::into_inner));
        let elapsed = self.started.elapsed();
        let report = Arc::new(Report {
            summary: summary.clone(),
            failed,
            elapsed,
            peak_bps: self.peak_bps,
            ..Report::default()
        });
        let outcome = match (&fatal, ctl.stopped()) {
            (Some(e), _) => Err(e.to_string()),
            (None, true) => Err("cancelled".to_owned()),
            (None, false) => Ok(summary),
        };
        tracing::info!(
            items = report.summary.items,
            bytes = report.summary.bytes,
            skipped = report.summary.skipped,
            errors = report.summary.errors,
            elapsed_ms = elapsed.as_millis() as u64,
            ok = outcome.is_ok(),
            "job finished"
        );
        let id = ctl.job.id();
        let note = catch_unwind(AssertUnwindSafe(|| {
            self.sh.exec.finished(&report, &outcome)
        }))
        .ok()
        .flatten();
        let retry = self.sh.exec.retry();
        let m = msg("ops.finished", move |cx| {
            if cx.state.ext.get::<Reports>().is_none() {
                cx.state.ext.insert(Reports::default());
            }
            if let Some(r) = cx.state.ext.get_mut::<Reports>() {
                r.put(id, report, retry);
            }
            if let Some((level, text)) = note {
                cx.toast(level, text);
            }
        });
        let _ = self.sink.send(m, &CancelToken::new());
        let Coord { sh, .. } = self;
        if let Ok(sh) = Arc::try_unwrap(sh) {
            sh.ctl.job.finish(outcome);
        } else {
            // Воркер не отпустил job (поток не завершился): итог отправит drop ручки.
            tracing::warn!("job handle still shared, finishing on drop");
        }
    }
}

/// План в потоке координатора: каталоги и работы.
pub(crate) struct Plan<'a, E: OpExec> {
    co: &'a mut Coord<E>,
}

impl<E: OpExec> Plan<'_, E> {
    /// Открывает каталог (ребёнок `parent`); закрыть — [`Plan::close`], когда все
    /// его дети поставлены. Пауза и отмена — здесь же.
    pub(crate) fn dir(
        &mut self,
        parent: Option<&Arc<DirNode<E::Dir>>>,
        data: E::Dir,
    ) -> Result<Arc<DirNode<E::Dir>>, Cancelled> {
        self.cx().checkpoint()?;
        Ok(DirNode::open(parent, data))
    }

    /// Ставит работу в очередь (`bytes` — в `bytes_total`). На полной очереди
    /// ждёт, обслуживая Finalizer; пауза и отмена — здесь же и раз в
    /// [`PLAN_TICK`] работ.
    pub(crate) fn push(
        &mut self,
        parent: Option<&Arc<DirNode<E::Dir>>>,
        item: E::Item,
        bytes: u64,
    ) -> Result<(), Cancelled> {
        let co = &mut *self.co;
        co.pushed += 1;
        if co.pushed.is_multiple_of(PLAN_TICK) {
            co.cx().checkpoint()?;
            co.service(Duration::ZERO);
        }
        let p = co.sh.ctl.job.progress();
        p.items_total.fetch_add(1, Ordering::Relaxed);
        p.bytes_total.fetch_add(bytes, Ordering::Relaxed);
        if !co.transfer {
            co.transfer = true;
            p.set_phase(Phase::Transfer);
        }
        if let Some(d) = parent {
            d.add_child();
        }
        let w = Work {
            item,
            parent: parent.cloned(),
        };
        let Some(tx) = &co.tx else {
            return Err(Cancelled);
        };
        match tx.try_send(w) {
            Ok(()) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err(Cancelled),
            Err(TrySendError::Full(w)) => co.push_full(w),
        }
    }

    /// Проблема до работы (S5 §5): работа паркуется, план идёт дальше; есть
    /// ответ «для всех» — применяется сразу.
    pub(crate) fn park(
        &mut self,
        parent: Option<&Arc<DirNode<E::Dir>>>,
        item: E::Item,
        bytes: u64,
        issue: Issue,
    ) -> Result<(), Cancelled> {
        let sh = Arc::clone(&self.co.sh);
        if let Some(r) = sh.park.decided(issue.kind) {
            return match sh.exec.resolve(item, &issue, &r) {
                Resolved::Run(item) => self.push(parent, item, bytes),
                Resolved::Done(res) => {
                    self.record(parent, res);
                    Ok(())
                }
            };
        }
        let p = sh.ctl.job.progress();
        p.items_total.fetch_add(1, Ordering::Relaxed);
        p.bytes_total.fetch_add(bytes, Ordering::Relaxed);
        if let Some(d) = parent {
            d.add_child();
        }
        let work = Work {
            item,
            parent: parent.cloned(),
        };
        park::put(&sh, work, issue, bytes);
        Ok(())
    }

    /// Слоты устройств (S5-19): ждёт очереди устройства (`Queued`), пока на нём
    /// идёт столько job, сколько позволяет `[ops.device_jobs]`; держит до конца job.
    pub(crate) fn device_slot(&mut self, slots: &[Slot]) -> Result<(), Cancelled> {
        let co = &mut *self.co;
        let job = &co.sh.ctl.job;
        let mut queued = false;
        let lease = co.gate.acquire(
            job.id(),
            slots,
            &co.ctl,
            job.cancel_token(),
            &co.sh.ctl.stop,
            &mut || {
                queued = true;
                job.set_state(JobState::Queued);
            },
        )?;
        if queued {
            job.set_state(JobState::Running);
        }
        co.device = Some(lease);
        Ok(())
    }

    /// Итог, решённый планом без работы воркера (пропуск существующего,
    /// ошибка листинга): считается как итог работы, каталог `parent` — «грязный»
    /// при пропуске и ошибке.
    pub(crate) fn record(&mut self, parent: Option<&Arc<DirNode<E::Dir>>>, r: ItemResult) {
        self.co.sh.ctl.record(r, parent);
    }

    /// Все дети каталога поставлены; пустой каталог уходит в Finalizer сразу.
    pub(crate) fn close(&mut self, dir: &Arc<DirNode<E::Dir>>) {
        if dir.complete_child() {
            self.co.finalize(Arc::clone(dir));
        }
    }

    pub(crate) fn cx(&self) -> ExecCx<'_> {
        self.co.cx()
    }
}

impl<E: OpExec> fmt::Debug for Plan<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plan")
            .field("job", &self.co.sh.ctl.job.id())
            .finish_non_exhaustive()
    }
}
