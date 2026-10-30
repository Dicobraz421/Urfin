//! Движок job (S5 §3, S5-04): координатор и воркеры на выделенных потоках
//! (`Runtime::spawn_dedicated`, S0 v2 №7), ограниченная очередь работ с
//! бэкпрешером, пауза и отмена через `JobHandle::checkpoint`, прогресс с фазой
//! (S0 v2 №9), Finalizer каталогов в post-order, отчёт и `panel.refresh`
//! затронутых табов не чаще 1 раза в секунду (S0 v2 №14).
//!
//! Операцию описывает [`OpExec`]: `plan` в потоке координатора ставит работы в
//! очередь, воркеры выполняют `exec`, `finalize_dir` закрывает каталог после
//! всех его детей. Обход (S5-05) и сами операции (S5-06…) — поверх движка;
//! число воркеров по носителю и AIMD — S5-19, здесь N фиксировано на job.
//! Проблемы (конфликт имени и т.п.) паркуются и задаются вопросом без
//! остановки воркеров — [`park`] (S5-07).
//!
//! Потоки: координатор `ops-job-N`, поток вопросов `ops-ask-N` и воркеры
//! `ops-wN-i`; все завершаются вместе с job, воркеров всех job не больше
//! `ops.max_threads` ([`ThreadBudget`]).

pub(crate) mod aimd;
mod finalize;
mod job;
mod park;
mod sched;
pub(crate) mod walker;
mod worker;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use urfin_core::{Cancelled, Cx, JobId, JobRegistry, JobSpec, JobSummary, Level, Location};

use crate::api::{FailedItem, Issue, Report, Resolution};

pub(crate) use finalize::DirNode;
pub(crate) use job::{ExecCx, Plan};
pub(crate) use park::Resolved;
pub(crate) use sched::ThreadBudget;
pub(crate) use urfin_core::device::{JobCtl, Slot};

/// Ёмкость очереди работ (S5 §0: бэкпрешер, память ограничена при любом дереве).
pub(crate) const QUEUE_CAP: usize = 64 * 1024;

/// Операция для движка. Реализация — `Sync`: `exec` зовут воркеры параллельно.
pub(crate) trait OpExec: Send + Sync + Sized + 'static {
    /// Работа воркера: файл или пачка мелких файлов.
    type Item: Send + 'static;
    /// Данные каталога для Finalizer (имя, исходные times/attrs и т.п.).
    type Dir: Send + Sync + 'static;

    /// План в потоке координатора: обход и постановка работ ([`Plan::push`]
    /// блокируется на полной очереди). `Err` — job `Failed` (или `Cancelled`).
    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError>;

    /// Одна работа в воркере. Байты — [`ExecCx::add_bytes`] по ходу;
    /// [`ExecCx::checkpoint`] — между чанками.
    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult;

    /// Каталог после всех детей (post-order, поток координатора).
    /// [`DirNode::clean`] — среди детей нет пропущенных и неудачных.
    fn finalize_dir(&self, dir: &DirNode<Self::Dir>, cx: &ExecCx<'_>) -> ItemResult;

    /// Ответ на запаркованную проблему (поток вопросов или план; I/O — только
    /// короткий, вроде удаления temp). По умолчанию — пропуск.
    fn resolve(&self, _item: Self::Item, _issue: &Issue, _r: &Resolution) -> Resolved<Self::Item> {
        Resolved::Done(ItemResult::Skipped)
    }

    /// Работа осталась без ответа (отмена job): убрать следы (temp).
    fn discard(&self, _item: Self::Item, _issue: &Issue) {}

    /// Раз в 500 мс из координатора (журнал: сброс, счётчики; AIMD воркеров).
    fn tick(&self, _cx: &ExecCx<'_>) {}

    /// Как повторить упавшее (`ops.retry`): хранится с отчётом job.
    fn retry(&self) -> Option<Retry> {
        None
    }

    /// Итог для пользователя (тост) по отчёту; поток координатора, после всех
    /// работ. `outcome` — то, с чем job завершится.
    fn finished(&self, _report: &Report, _outcome: &Result<JobSummary, String>) -> Option<Note> {
        None
    }
}

/// Тост по итогу job.
pub(crate) type Note = (Level, String);

/// Данные операции для `ops.retry` (тип знает сама операция).
pub(crate) type Retry = Arc<dyn std::any::Any + Send + Sync>;

/// Работа в очереди: элемент и каталог, которому он принадлежит.
pub(crate) struct Work<E: OpExec> {
    pub item: E::Item,
    pub parent: Option<Arc<DirNode<E::Dir>>>,
}

/// Итог работы воркера или Finalizer.
#[derive(Debug)]
pub(crate) enum ItemResult {
    Done,
    Skipped,
    Failed(FailedItem),
    /// Прервано отменой посреди работы: не считается, воркер выходит.
    Cancelled,
    /// Проблема (S5 §5): работа паркуется до ответа, воркер идёт дальше.
    /// Байты — сколько из `bytes_total` не перенесётся при пропуске.
    Park(Box<Issue>, u64),
}

/// Ошибка плана.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PlanError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(Arc<str>),
}

impl From<Cancelled> for PlanError {
    fn from(_: Cancelled) -> Self {
        PlanError::Cancelled
    }
}

/// Запуск job: общий потолок потоков и счётчик живых потоков движка.
/// Живёт в handler `ops` (UI-поток).
#[derive(Clone, Debug)]
pub(crate) struct JobEngine {
    budget: Arc<ThreadBudget>,
    live: Arc<AtomicUsize>,
    /// Job на физическое устройство (S5-19).
    gate: Arc<urfin_core::DeviceGate>,
    /// Приоритет и «запустить сейчас» живых job.
    ctls: Arc<std::sync::Mutex<std::collections::HashMap<JobId, Arc<JobCtl>>>>,
}

impl JobEngine {
    /// `gate` — общая очередь устройств процесса ([`urfin_core::Core::devices`]).
    pub(crate) fn new(max_threads: u16, gate: Arc<urfin_core::DeviceGate>) -> JobEngine {
        JobEngine {
            budget: ThreadBudget::new(max_threads),
            live: Arc::default(),
            gate,
            ctls: Arc::default(),
        }
    }

    fn ctl(&self, id: JobId) -> Option<Arc<JobCtl>> {
        self.ctls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .cloned()
    }

    /// `ops.priority`: приоритет в очереди устройства (не вытесняет запущенные).
    pub(crate) fn set_priority(&self, id: JobId, prio: u8) -> bool {
        let Some(c) = self.ctl(id) else {
            return false;
        };
        c.prio.store(prio, Ordering::Relaxed);
        self.gate.poke();
        true
    }

    /// `ops.run_now`: мимо очереди устройства.
    pub(crate) fn run_now(&self, id: JobId) -> bool {
        let Some(c) = self.ctl(id) else {
            return false;
        };
        c.run_now.store(true, Ordering::Relaxed);
        self.gate.poke();
        true
    }

    /// Из handler (UI-поток, без I/O): регистрирует job и запускает координатор
    /// и `workers` воркеров (сколько даст потолок; 0 свободных — job `Queued`
    /// до освобождения).
    pub(crate) fn start<E: OpExec>(
        &self,
        cx: &mut Cx<'_>,
        spec: JobSpec,
        workers: u16,
        exec: E,
    ) -> JobId {
        job::start(self, cx, spec, workers.max(1), exec)
    }

    /// Живых потоков движка (координаторы и воркеры всех job).
    #[cfg_attr(not(test), allow(dead_code, reason = "тесты; выход с job — S5-12"))]
    pub(crate) fn live_threads(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

/// Отчёты последних завершённых job (`AppState.ext`, S0 v2 №10): столько же,
/// сколько завершённых хранит `JobRegistry` (S1 §7.1), — для `ops.retry` и истории.
#[derive(Default)]
pub(crate) struct Reports(VecDeque<(JobId, Arc<Report>, Option<Retry>)>);

impl std::fmt::Debug for Reports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reports")
            .field("len", &self.0.len())
            .finish()
    }
}

impl Reports {
    #[cfg(test)]
    pub(crate) fn get(&self, id: JobId) -> Option<&Arc<Report>> {
        self.0.iter().find(|(j, ..)| *j == id).map(|(_, r, _)| r)
    }

    /// Отчёт и данные повтора job.
    pub(crate) fn retry(&self, id: JobId) -> Option<(Arc<Report>, Retry)> {
        self.0
            .iter()
            .find(|(j, ..)| *j == id)
            .and_then(|(_, r, t)| Some((Arc::clone(r), Arc::clone(t.as_ref()?))))
    }

    fn put(&mut self, id: JobId, r: Arc<Report>, retry: Option<Retry>) {
        if self.0.len() == JobRegistry::KEEP_FINISHED {
            self.0.pop_front();
        }
        self.0.push_back((id, r, retry));
    }
}

/// UI-поток: `panel.refresh` табов, показывающих затронутые каталоги.
fn refresh_tabs(cx: &mut Cx<'_>, dirs: &[Location]) {
    let tabs: Vec<i64> = cx
        .state
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter())
        .filter(|t| dirs.contains(&t.loc))
        .map(|t| i64::from(t.id.0))
        .collect();
    for id in tabs {
        cx.dispatch(urfin_core::Command::new("panel.refresh").arg("tab", id));
    }
}
