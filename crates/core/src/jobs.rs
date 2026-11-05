//! Задачи (каркас для S5, S1 §2.7 + S0 v2 №8, №9): реестр в UI-потоке, ручка
//! `JobHandle` в воркере, прогресс атомиками.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::cancel::{CancelToken, Cancelled};
use crate::ids::{JobId, PromptId};
use crate::msg::{MsgSink, msg};
use crate::prompt::{PromptSpec, Reply, next_prompt_id};
use crate::runtime::Lane;

/// Как часто ожидание (пауза, ответ на запрос) перепроверяет отмену.
const POLL: Duration = Duration::from_millis(20);

/// Описание задачи.
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub kind: &'static str,
    pub title: String,
    pub lane: Lane,
    /// Больше — раньше (очереди устройств S5).
    pub priority: u8,
    /// Namespace handler, которому уходят `job.retry` и `job.priority` (S0 v2 №8).
    pub owner_ns: &'static str,
}

/// Состояние задачи. `Waiting` — «есть открытый вопрос», прогресс при этом может
/// идти; переходы `Waiting ↔ Running` повторяемы (S0 v2 №8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Waiting(PromptId),
    Done(JobSummary),
    Failed(Arc<str>),
    Cancelled,
}

impl JobState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            JobState::Queued | JobState::Running | JobState::Paused | JobState::Waiting(_)
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobSummary {
    pub items: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub errors: u64,
}

/// Фаза задачи (S0 v2 №9).
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Scan,
    Transfer,
    Verify,
    Finalize,
    Recover,
    /// Задача давно молчит (менеджер пакетов без вывода, F02 §2.3): текущий элемент —
    /// подпись, в job-баре «Прервать».
    Stalled,
}

impl Phase {
    fn from_u8(v: u8) -> Phase {
        match v {
            1 => Phase::Transfer,
            2 => Phase::Verify,
            3 => Phase::Finalize,
            4 => Phase::Recover,
            5 => Phase::Stalled,
            _ => Phase::Scan,
        }
    }
}

/// Прогресс: пишут воркеры, UI читает снапшотом в кадре.
#[derive(Default)]
pub struct Progress {
    pub bytes_done: AtomicU64,
    pub bytes_total: AtomicU64,
    pub items_done: AtomicU64,
    pub items_total: AtomicU64,
    phase: AtomicU8,
    /// Итоги посчитаны полностью (иначе UI показывает «≥»).
    pub total_final: AtomicBool,
    /// Элементов отложено до ответа на вопрос.
    pub parked: AtomicU32,
    current: Mutex<Option<Arc<str>>>,
}

/// Снимок прогресса для UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressSnap {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub items_done: u64,
    pub items_total: u64,
    pub current: Option<Arc<str>>,
    pub phase: Phase,
    pub total_final: bool,
    pub parked: u32,
}

impl Progress {
    /// Текущий элемент (имя файла и т.п.).
    pub fn set_current(&self, s: &str) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(s.into());
    }

    pub fn set_phase(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Relaxed))
    }

    /// Relaxed-чтения: значения могут быть из разных моментов, для UI это нормально.
    pub fn snapshot(&self) -> ProgressSnap {
        ProgressSnap {
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            items_done: self.items_done.load(Ordering::Relaxed),
            items_total: self.items_total.load(Ordering::Relaxed),
            current: self
                .current
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            phase: self.phase(),
            total_final: self.total_final.load(Ordering::Relaxed),
            parked: self.parked.load(Ordering::Relaxed),
        }
    }
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Progress").field(&self.snapshot()).finish()
    }
}

/// Управление задачей из UI: пауза (Condvar) и отмена.
#[derive(Debug, Default)]
pub(crate) struct JobControl {
    paused: Mutex<bool>,
    cv: Condvar,
    cancel: CancelToken,
}

impl JobControl {
    fn set_paused(&self, on: bool) {
        *self.paused.lock().unwrap_or_else(PoisonError::into_inner) = on;
        self.cv.notify_all();
    }

    fn cancel(&self) {
        self.cancel.cancel();
        self.cv.notify_all();
    }
}

/// Задача в реестре.
#[derive(Debug)]
pub struct Job {
    pub id: JobId,
    pub kind: &'static str,
    pub title: String,
    pub state: JobState,
    pub progress: Arc<Progress>,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub priority: u8,
    pub owner_ns: &'static str,
    control: Arc<JobControl>,
}

/// Задачи приложения. Живёт в `AppState`, меняется только в UI-потоке.
#[derive(Debug, Default)]
pub struct JobRegistry {
    jobs: BTreeMap<JobId, Job>,
    next: u64,
}

impl JobRegistry {
    /// Регистрирует задачу (`Running`) и отдаёт ручку для воркера. Вызывается из
    /// handler; воркер запускает сам handler (`spawn_dedicated` для долгих).
    pub fn start(&mut self, spec: JobSpec, sink: MsgSink) -> JobHandle {
        self.next += 1;
        let id = JobId(self.next);
        let control = Arc::new(JobControl::default());
        let progress = Arc::new(Progress::default());
        self.jobs.insert(
            id,
            Job {
                id,
                kind: spec.kind,
                title: spec.title,
                state: JobState::Running,
                progress: progress.clone(),
                started: Instant::now(),
                finished: None,
                priority: spec.priority,
                owner_ns: spec.owner_ns,
                control: control.clone(),
            },
        );
        JobHandle {
            id,
            progress,
            control,
            sink,
            finished: false,
        }
    }

    pub fn get(&self, id: JobId) -> Option<&Job> {
        self.jobs.get(&id)
    }

    /// По возрастанию id (порядок запуска).
    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    pub fn any_active(&self) -> bool {
        self.jobs.values().any(|j| j.state.is_active())
    }

    pub fn cancel(&mut self, id: JobId) {
        if let Some(j) = self.jobs.get(&id) {
            j.control.cancel();
        }
    }

    pub fn pause(&mut self, id: JobId) {
        if let Some(j) = self.jobs.get_mut(&id)
            && matches!(
                j.state,
                JobState::Running | JobState::Queued | JobState::Waiting(_)
            )
        {
            j.control.set_paused(true);
            j.state = JobState::Paused;
        }
    }

    pub fn resume(&mut self, id: JobId) {
        if let Some(j) = self.jobs.get_mut(&id)
            && j.state == JobState::Paused
        {
            j.control.set_paused(false);
            j.state = JobState::Running;
        }
    }

    pub fn set_priority(&mut self, id: JobId, priority: u8) {
        if let Some(j) = self.jobs.get_mut(&id) {
            j.priority = priority;
        }
    }

    /// Убрать завершённые, кроме `keep_finished` последних.
    /// Сколько завершённых задач хранится (для job-бара и истории S5); старые
    /// убираются при завершении следующей.
    pub const KEEP_FINISHED: usize = 64;

    pub fn retire(&mut self, keep_finished: usize) {
        let finished: Vec<JobId> = self
            .jobs
            .values()
            .filter(|j| !j.state.is_active())
            .map(|j| j.id)
            .collect();
        let drop_n = finished.len().saturating_sub(keep_finished);
        for id in &finished[..drop_n] {
            self.jobs.remove(id);
        }
    }

    pub(crate) fn cancel_all(&mut self) {
        for j in self.jobs.values() {
            j.control.cancel();
        }
    }

    /// Состояние от воркера (через `Msg`). Завершённую задачу не оживляет; пауза
    /// пользователя важнее `Running`/`Queued` от воркера.
    pub(crate) fn update(&mut self, id: JobId, st: JobState) {
        let Some(j) = self.jobs.get_mut(&id) else {
            return;
        };
        if !j.state.is_active() {
            return;
        }
        if j.state == JobState::Paused && matches!(st, JobState::Running | JobState::Queued) {
            return;
        }
        let finished = !st.is_active();
        if finished {
            j.finished = Some(Instant::now());
        }
        j.state = st;
        if finished {
            self.retire(Self::KEEP_FINISHED);
        }
    }

    /// Ответ на вопрос задачи пришёл: `Waiting(prompt)` → `Running`.
    pub(crate) fn answered(&mut self, id: JobId, prompt: PromptId) {
        if let Some(j) = self.jobs.get_mut(&id)
            && j.state == JobState::Waiting(prompt)
        {
            j.state = JobState::Running;
        }
    }
}

/// Ручка задачи в воркере. Send. Drop без `finish` → `Failed("aborted")`
/// (при панике — `Failed("panicked")`), при отмене — `Cancelled`.
pub struct JobHandle {
    id: JobId,
    progress: Arc<Progress>,
    control: Arc<JobControl>,
    sink: MsgSink,
    finished: bool,
}

impl JobHandle {
    pub fn id(&self) -> JobId {
        self.id
    }

    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    pub fn cancel_token(&self) -> &CancelToken {
        &self.control.cancel
    }

    /// Точка проверки: ждёт на паузе, `Err` при отмене.
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        let mut paused = self
            .control
            .paused
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            self.control.cancel.check()?;
            if !*paused {
                return Ok(());
            }
            // Отмена от родителя (shutdown) не будит Condvar — поэтому с таймаутом.
            paused = self
                .control
                .cv
                .wait_timeout(paused, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Очередь устройства S5 (S0 v2 №8): `Queued` или `Running`.
    pub fn set_state(&self, st: JobState) {
        debug_assert!(matches!(st, JobState::Queued | JobState::Running));
        self.post(st);
    }

    /// Inline-запрос: открывает Prompt в UI, задача — `Waiting`, ждёт ответа.
    /// `prompt.cancel` или отмена задачи → `Err(Cancelled)`.
    pub fn ask(&self, p: PromptSpec) -> Result<Reply, Cancelled> {
        self.control.cancel.check()?;
        let pid = next_prompt_id();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let id = self.id;
        let open = msg("job.ask", move |cx| {
            cx.state.prompts.open_with(pid, p, Some(id), tx);
            cx.state.jobs.update(id, JobState::Waiting(pid));
        });
        self.sink
            .send(open, &self.control.cancel)
            .map_err(|_| Cancelled)?;
        loop {
            match rx.recv_timeout(POLL) {
                Ok(r) => return Ok(r),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Err(Cancelled),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
            if self.control.cancel.is_cancelled() {
                let _ = self.sink.try_send(msg("job.ask.cancel", move |cx| {
                    cx.state.prompts.cancel(pid);
                }));
                return Err(Cancelled);
            }
        }
    }

    /// Итог задачи. Ошибка при отменённой задаче даёт `Cancelled`.
    pub fn finish(mut self, r: Result<JobSummary, String>) {
        self.finished = true;
        let st = match r {
            Ok(s) => JobState::Done(s),
            Err(_) if self.control.cancel.is_cancelled() => JobState::Cancelled,
            Err(e) => JobState::Failed(e.into()),
        };
        self.post(st);
    }

    fn post(&self, st: JobState) {
        let id = self.id;
        // Свой токен: финальное состояние должно дойти и у отменённой задачи.
        let _ = self.sink.send(
            msg("job.state", move |cx| cx.state.jobs.update(id, st)),
            &CancelToken::new(),
        );
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let st = if self.control.cancel.is_cancelled() {
            JobState::Cancelled
        } else if thread::panicking() {
            JobState::Failed("panicked".into())
        } else {
            JobState::Failed("aborted".into())
        };
        self.post(st);
    }
}

impl fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
