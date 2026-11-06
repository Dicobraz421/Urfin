//! Job поиска (S8 §2.2): выделенный поток, из него — потоки обхода. Строки идут в
//! `SearchStore` мимо очереди `Msg`, UI будит `MsgSink::poke`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_core::search::SearchStore;
use urfin_core::{
    Banner, CancelToken, Cx, DeviceLease, JobHandle, JobId, JobSpec, JobSummary, Lane, Level,
    Location, TaskCx, msg,
};
use urfin_i18n::FluentArgs;
use urfin_vfs::VfsRegistry;

use crate::SearchCfg;
use crate::query::{QueryError, SearchQuery};
use crate::walk::{Outcome, TreeSearch, search_tree};

/// Прогресс job — не чаще 30 раз в секунду.
const PROGRESS_EVERY: Duration = Duration::from_millis(34);

/// Что нужно job.
pub(crate) struct Spawn {
    pub reg: Arc<VfsRegistry>,
    pub query: SearchQuery,
    pub cfg: SearchCfg,
    pub store: Arc<SearchStore>,
    pub denied: Arc<Mutex<Vec<Location>>>,
}

/// Очередь устройств. Вопрос В6 = А (решение 2026-11-06 «s8-answers»): поиск её не занимает,
/// на HDD у него и так один поток. Поменять решение можно здесь и только здесь: вернуть
/// слот, как `du::scan::device_slot`, — job держит его на весь обход.
fn device_lease(_job: &JobHandle, _roots: &[Location]) -> Option<DeviceLease> {
    None
}

/// Запускает поиск: job в реестре, поток — выделенный.
pub(crate) fn start(cx: &mut Cx<'_>, s: Spawn) -> JobId {
    let job = cx.state.jobs.start(
        JobSpec {
            kind: "search",
            title: s.query.to_string(),
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "search",
        },
        cx.rt.sink(),
    );
    let id = job.id();
    cx.rt
        .spawn_dedicated("search".into(), cx.rt.token(), move |tcx| {
            run(&s, job, &tcx);
        });
    id
}

fn run(s: &Spawn, job: JobHandle, tcx: &TaskCx) {
    let _span = tracing::info_span!("search.job").entered();
    let t0 = Instant::now();
    let _lease = device_lease(&job, &s.query.roots);
    let halted = || tcx.cancel.is_cancelled() || job.checkpoint().is_err();
    let poke = || tcx.sink.poke();
    let tree = TreeSearch {
        query: &s.query,
        cfg: &s.cfg,
        reg: &s.reg,
        store: &s.store,
        denied: &s.denied,
        halted: &halted,
        poke: &poke,
    };
    let info = &s.store.info;
    let mut told = t0;
    let res = search_tree(&tree, || {
        if told.elapsed() >= PROGRESS_EVERY {
            told = Instant::now();
            let files = info.files.load(Ordering::Relaxed);
            job.progress().items_done.store(files, Ordering::Relaxed);
            poke();
        }
    });
    let stopped = job.cancel_token().is_cancelled();
    let files = info.files.load(Ordering::Relaxed);
    job.progress().items_done.store(files, Ordering::Relaxed);
    s.store
        .finish(u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX));
    let summary = JobSummary {
        items: info.found.load(Ordering::Relaxed),
        bytes: 0,
        skipped: 0,
        errors: info.denied.load(Ordering::Relaxed),
    };
    job.finish(match &res {
        Ok(_) if stopped => Err("cancelled".to_owned()),
        Ok(_) => Ok(summary),
        Err(e) => Err(e.to_string()),
    });
    let limit = s.cfg.max_hits;
    // Свой токен: итог должен дойти и у отменённого поиска.
    let done = msg("search.done", move |cx| report(cx, res, limit));
    let _ = tcx.sink.send(done, &CancelToken::new());
    poke();
}

/// Текст по ключу с одним аргументом.
pub(crate) fn tr(key: &str, name: &str, value: String) -> String {
    let mut args = FluentArgs::new();
    args.set(name, value);
    urfin_i18n::current().fmt(key, &args)
}

/// Что сказать об ошибке запроса.
pub(crate) fn query_error(e: &QueryError) -> String {
    match e {
        QueryError::BadMask(m) => tr("ui-search-bad-mask", "mask", m.clone()),
        QueryError::BadWhere(p) => tr("ui-search-bad-where", "path", p.clone()),
        QueryError::BadRegex(r) => tr("ui-search-bad-regex", "error", r.clone()),
    }
}

#[allow(
    clippy::default_trait_access,
    reason = "smallvec — не зависимость крейта"
)]
fn report(cx: &mut Cx<'_>, res: Result<Outcome, QueryError>, limit: u64) {
    let out = match res {
        Ok(out) => out,
        Err(e) => {
            cx.toast(Level::Warn, query_error(&e));
            return;
        }
    };
    if out.capped {
        cx.state.banner = Some(Banner {
            level: Level::Warn,
            text: tr("ui-search-max-hits", "limit", limit.to_string()),
            actions: Default::default(), // SmallVec: крейт от него не зависит.
        });
    }
    for root in out.failed_roots {
        let path = root.path.display().to_string();
        cx.toast(Level::Warn, tr("ui-search-bad-where", "path", path));
    }
}
