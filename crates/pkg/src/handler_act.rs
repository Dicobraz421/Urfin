//! Поиск (F02-09) и действия (F02-10) handler'а `pkg`.
//!
//! Поиск: строка «Найти пакет» — черновик `ui.draft command=pkg.search` (поле с
//! историей, №70); запрос отменяет процессы прежнего; ответы менеджеров — по мере
//! готовности, уже показанные группы выше курсора не переставляются.
//!
//! Действия: install/upgrade/uninstall — job на менеджер (пакеты внутри —
//! последовательно), очередь одна на менеджер; ход — из строк вывода в строке пакета и
//! в job-баре; после — перечитываются только затронутые менеджеры.

use std::sync::atomic::Ordering;
use std::time::Duration;

use compact_str::CompactString;
use urfin_core::{
    Anchor, Banner, CancelToken, Choice, JobSpec, Level, PromptSpec, Row, Value, msg,
};

use super::{
    Arc, CmdError, Command, Cx, Detected, Env, Filter, Lane, MANAGERS_FILE, Managers, Mutex,
    PackageManager, PoisonError, RunCx, filter_of, go, open, refresh_tabs, store,
};
use crate::api::{ActionDone, Elevation, Mgr, PkgError, PkgId, PkgOp};
use crate::book::Busy;
use crate::config::SearchIn;

fn i18n_args(pairs: &[(&str, String)]) -> urfin_i18n::FluentArgs<'static> {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in pairs {
        a.set((*k).to_owned(), v.clone());
    }
    a
}

fn tr(key: &str, pairs: &[(&str, String)]) -> String {
    urfin_i18n::current().fmt(key, &i18n_args(pairs))
}

// ---------------------------------------------------------------- поиск

/// Вкладка на поиске (Esc вернёт к прежнему фильтру) и строка «Найти пакет» с прежним
/// запросом.
pub(super) fn open_search(cx: &mut Cx<'_>, env: &Arc<Env>) {
    to_search(cx, env);
    let q = env.book.view().search.info.q.to_string();
    cx.dispatch(
        Command::new("ui.draft")
            .arg("command", "pkg.search")
            .arg("arg", "q")
            .arg("init", q),
    );
}

/// Активная вкладка — на `pkg:/search`; прежний фильтр запоминается для Esc.
fn to_search(cx: &mut Cx<'_>, env: &Arc<Env>) {
    let cur = filter_of(&cx.state.active_tab().loc);
    if cur != Some(Filter::Search) {
        env.book.search_back(cur.unwrap_or(Filter::Installed));
        if cur.is_some() {
            go(cx, env, Filter::Search);
        } else {
            open(cx, env, Filter::Search);
        }
    }
}

fn scope_arg(s: &str) -> Option<SearchIn> {
    Some(match s {
        "all" => SearchIn::All,
        _ => match Mgr::parse(s)? {
            Mgr::Winget => SearchIn::Winget,
            Mgr::Scoop => SearchIn::Scoop,
            Mgr::Choco => SearchIn::Choco,
        },
    })
}

/// `pkg.search q? in?`: без запроса — строка поиска; `in` — область (с прежним
/// запросом — сразу заново).
pub(super) fn search_cmd(cmd: &Command, cx: &mut Cx<'_>, env: &Arc<Env>) -> Result<(), CmdError> {
    let scope = match cmd.args.str("in") {
        Some(s) => Some(scope_arg(s).ok_or_else(|| CmdError::Arg {
            name: "in".into(),
            msg: "all | winget | scoop | choco".into(),
        })?),
        None => None,
    };
    if let Some(s) = scope {
        env.book.search_scope(s);
    }
    let mut q = cmd.args.str("q").map_or("", str::trim).to_owned();
    if q.is_empty() && scope.is_some() {
        q = env.book.view().search.info.q.to_string();
    }
    if q.is_empty() {
        if scope.is_none() {
            open_search(cx, env);
        } else {
            refresh_tabs(cx);
        }
        return Ok(());
    }
    to_search(cx, env);
    run_search(cx, env, q);
    Ok(())
}

/// Новый запрос: процессы прежнего отменяются; без найденных менеджеров — сначала
/// обнаружение (без процессов).
fn run_search(cx: &mut Cx<'_>, env: &Arc<Env>, q: String) {
    let token = cx.rt.token().child();
    {
        let mut cur = env.search.lock().unwrap_or_else(PoisonError::into_inner);
        cur.cancel();
        *cur = token.clone();
    }
    if !env.book.detected().is_empty() {
        start_search(cx, env, &q, &token);
        return;
    }
    let env = Arc::clone(env);
    cx.rt
        .spawn(Lane::Interactive, "pkg.detect", token.clone(), move |tcx| {
            let cached = env
                .dir
                .as_ref()
                .and_then(|d| Managers::load(&d.join(MANAGERS_FILE)))
                .unwrap_or_default();
            let found = env.source.detect(&cached);
            env.book.known(found);
            let m = msg("pkg.detected", move |cx| start_search(cx, &env, &q, &token));
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Найденные менеджеры в порядке `[pkg].order`.
fn ordered(env: &Env) -> Vec<Detected> {
    let mut found = env.book.detected();
    let order = &env.config.order;
    found.sort_by_key(|d| {
        order
            .iter()
            .position(|m| *m == d.mgr)
            .unwrap_or(order.len())
    });
    found
}

/// Запрос ко всем менеджерам области параллельно: поток на менеджер.
fn start_search(cx: &mut Cx<'_>, env: &Arc<Env>, q: &str, token: &CancelToken) {
    if token.is_cancelled() {
        return;
    }
    let scope = env.book.view().search.info.scope;
    let found: Vec<Detected> = ordered(env)
        .into_iter()
        .filter(|d| scope.mgr().is_none_or(|m| m == d.mgr))
        .collect();
    let mgrs: Vec<Mgr> = found.iter().map(|d| d.mgr).collect();
    let generation = env.book.search_start(q, &mgrs);
    refresh_tabs(cx);
    tracing::info!(target: "pkg", generation, managers = mgrs.len(), "pkg: search");
    for d in &found {
        let adapter = env.source.adapter(d, &env.config);
        let env = Arc::clone(env);
        let q = q.to_owned();
        let mgr = d.mgr;
        let _ = cx
            .rt
            .spawn_dedicated(format!("pkg.search.{mgr}"), token.clone(), move |tcx| {
                let rc = RunCx {
                    cancel: tcx.cancel.clone(),
                    timeout: Some(Duration::from_secs(u64::from(env.config.timeouts.search_s))),
                    lines: None,
                };
                let res = adapter.search(&q, &rc);
                if tcx.cancel.is_cancelled() {
                    return;
                }
                let m = msg("pkg.search_answer", move |cx| {
                    let keep = keep_of(cx, &env);
                    if env.book.search_answer(generation, mgr, res, keep) {
                        refresh_tabs(cx);
                    }
                });
                let _ = tcx.sink.send(m, &tcx.cancel);
            });
    }
}

/// Групп выше курсора и под ним во вкладке поиска (их порядок не меняется).
fn keep_of(cx: &Cx<'_>, env: &Env) -> usize {
    let view = env.book.view();
    cx.state
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter())
        .find(|t| filter_of(&t.loc) == Some(Filter::Search))
        .and_then(|t| match t.current() {
            Some(Row::Entry { entry, .. }) => Some(view.search.keep_for(&entry.name)),
            _ => None,
        })
        .unwrap_or(0)
}

/// `pkg.pick` (Пробел): на группе поиска — свернуть или раскрыть варианты, иначе —
/// отметка строки, как Пробел в панели.
pub(super) fn pick(cx: &mut Cx<'_>, env: &Arc<Env>) {
    let t = cx.state.active_tab();
    if filter_of(&t.loc) == Some(Filter::Search)
        && let Some(Row::Entry { entry, .. }) = t.current()
        && let Some(r) = env.book.view().search.row(&entry.name)
        && r.is_group()
    {
        env.book.search_toggle(r.group);
        refresh_tabs(cx);
        return;
    }
    cx.dispatch(Command::new("sel.toggle").arg("advance", true));
}

// ---------------------------------------------------------------- действия

/// Цели: аргумент `targets` (`mgr:id`), иначе отмеченные строки или строка курсора.
/// В поиске группа — её рекомендуемый вариант; «вне менеджера» обновлять нельзя.
fn targets(cmd: &Command, cx: &Cx<'_>, env: &Env, op: PkgOp) -> Vec<PkgId> {
    if let Some(list) = cmd.args.list("targets") {
        return list
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => s.parse().ok(),
                _ => None,
            })
            .collect();
    }
    let t = cx.state.panel(cx.state.active).tab();
    let Some(f) = filter_of(&t.loc) else {
        return Vec::new();
    };
    let names: Vec<CompactString> = t
        .targets()
        .map(|tg| tg.items.iter().map(|e| e.name.clone()).collect())
        .unwrap_or_default();
    let view = env.book.view();
    let mut out: Vec<PkgId> = Vec::new();
    for n in &names {
        let id = if f == Filter::Search {
            view.search.row(n).map(|r| r.target().clone())
        } else {
            view.row(f, n)
                .filter(|r| op == PkgOp::Uninstall || !r.outside)
                .filter(|r| op != PkgOp::Upgrade || f == Filter::Updates || r.available.is_some())
                .map(|r| r.pkg.id.clone())
        };
        if let Some(id) = id
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
}

/// `pkg.install` (Enter), `pkg.upgrade` (F5), `pkg.upgrade_all` (Shift+Enter),
/// `pkg.uninstall` (F8 — подтверждение одной строкой). В поиске Enter и F5 ставят, в
/// «Обновлениях» — обновляют.
pub(super) fn action_cmd(cmd: &Command, cx: &mut Cx<'_>, env: &Arc<Env>) {
    let f = filter_of(&cx.state.active_tab().loc);
    let op = match (cmd.id.as_str(), f) {
        ("pkg.uninstall", _) => PkgOp::Uninstall,
        ("pkg.install" | "pkg.upgrade", Some(Filter::Search)) | ("pkg.install", None) => {
            PkgOp::Install
        }
        ("pkg.install", Some(Filter::Installed)) if cmd.args.list("targets").is_none() => {
            cx.keep_rev();
            return;
        }
        _ => PkgOp::Upgrade,
    };
    let ids = if cmd.id.as_str() == "pkg.upgrade_all" {
        let view = env.book.view();
        view.rows(Filter::Updates)
            .iter()
            .filter(|r| !r.pinned)
            .map(|r| r.pkg.id.clone())
            .collect()
    } else {
        targets(cmd, cx, env, op)
    };
    if ids.is_empty() {
        cx.keep_rev();
        return;
    }
    if op == PkgOp::Uninstall {
        confirm_uninstall(cx, env, ids);
    } else {
        start(cx, env, op, &ids);
    }
}

/// Подтверждение удаления одной строкой у панели: «Удалить 7-Zip (winget)?».
fn confirm_uninstall(cx: &mut Cx<'_>, env: &Arc<Env>, ids: Vec<PkgId>) {
    let i18n = urfin_i18n::current();
    let title = if ids.len() == 1 {
        tr(
            "pkg-uninstall-one",
            &[
                ("name", ids[0].id.to_string()),
                ("mgr", ids[0].mgr.as_str().to_owned()),
            ],
        )
    } else {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("count", ids.len());
        i18n.fmt("pkg-uninstall-n", &a)
    };
    let spec = PromptSpec {
        anchor: Anchor::Panel(cx.state.active),
        title,
        body: String::new(),
        choices: [
            Choice {
                id: "uninstall".into(),
                label: i18n.get("pkg-uninstall-yes").into(),
                key: 'y',
            },
            Choice {
                id: "cancel".into(),
                label: i18n.get("pkg-cancel").into(),
                key: 'n',
            },
        ]
        .into_iter()
        .collect(),
        default: 0,
        input: None,
        allow_all: false,
        detail: None,
        secret: false,
    };
    let (_, rx) = cx.state.prompts.open(spec, None);
    cx.state.touch();
    let env = Arc::clone(env);
    let _ = cx
        .rt
        .spawn_dedicated("pkg.confirm".into(), cx.rt.token(), move |tcx| {
            let reply = loop {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(r) => break Some(r),
                    Err(e) if e.is_disconnected() => break None,
                    Err(_) => {}
                }
                if tcx.cancel.is_cancelled() {
                    break None;
                }
            };
            if reply.is_some_and(|r| r.choice == "uninstall") {
                let m = msg("pkg.uninstall", move |cx| {
                    start(cx, &env, PkgOp::Uninstall, &ids);
                });
                let _ = tcx.sink.send(m, &tcx.cancel);
            }
        });
}

/// Job на менеджер: пакеты `ids` по очереди.
pub(super) fn start(cx: &mut Cx<'_>, env: &Arc<Env>, op: PkgOp, ids: &[PkgId]) {
    let found = env.book.detected();
    for mgr in Mgr::ALL {
        let mine: Vec<PkgId> = ids.iter().filter(|i| i.mgr == mgr).cloned().collect();
        if mine.is_empty() {
            continue;
        }
        let Some(d) = found.iter().find(|d| d.mgr == mgr) else {
            cx.toast(
                Level::Error,
                tr(
                    "pkg-failed",
                    &[
                        ("mgr", mgr.as_str().to_owned()),
                        ("error", PkgError::NotFound.to_string()),
                    ],
                ),
            );
            continue;
        };
        if mgr == Mgr::Choco {
            // choco меняет систему только с правами администратора (F02 §8).
            cx.toast(
                Level::Info,
                urfin_i18n::current().get(crate::choco::UAC_NOTICE),
            );
        }
        run_job(cx, env, d, op, mine);
    }
}

fn verb(op: PkgOp) -> &'static str {
    match op {
        PkgOp::Install => "install",
        PkgOp::Upgrade => "upgrade",
        PkgOp::Uninstall => "uninstall",
    }
}

/// Итог job.
#[derive(Default)]
struct Outcome {
    done: usize,
    reboot: bool,
    declined: bool,
    errors: Vec<String>,
}

#[allow(
    clippy::too_many_lines,
    reason = "очередь, шаги, перечитывание и итог одной job"
)]
fn run_job(cx: &mut Cx<'_>, env: &Arc<Env>, found: &Detected, op: PkgOp, ids: Vec<PkgId>) {
    let mgr = found.mgr;
    let title = if ids.len() == 1 {
        format!("{mgr} {} {}", verb(op), ids[0].id)
    } else {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("cmd", format!("{mgr} {}", verb(op)));
        a.set("count", ids.len());
        urfin_i18n::current().fmt("pkg-job-many", &a)
    };
    let kind = match op {
        PkgOp::Install => "pkg_install",
        PkgOp::Upgrade => "pkg_upgrade",
        PkgOp::Uninstall => "pkg_uninstall",
    };
    let job = cx.state.jobs.start(
        JobSpec {
            kind,
            title,
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "pkg",
        },
        cx.rt.sink(),
    );
    for id in &ids {
        env.book.set_busy(
            id,
            Some(Busy {
                op,
                queued: true,
                pct: None,
                uac: false,
            }),
        );
    }
    refresh_tabs(cx);
    let adapter = env.source.adapter(found, &env.config);
    let env = Arc::clone(env);
    let _ = cx
        .rt
        .spawn_dedicated(format!("pkg.action.{mgr}"), cx.rt.token(), move |tcx| {
            let p = job.progress();
            p.items_total.store(ids.len() as u64, Ordering::Relaxed);
            p.total_final.store(true, Ordering::Relaxed);
            let (turn, first) = env.gates.enter(mgr, job.id());
            if !first {
                job.set_state(urfin_core::JobState::Queued);
            }
            let cancel = job.cancel_token().clone();
            let mut out = Outcome::default();
            if env.gates.wait(&turn, &cancel) {
                job.set_state(urfin_core::JobState::Running);
                for id in &ids {
                    if cancel.is_cancelled() || out.declined {
                        break;
                    }
                    one(
                        &env,
                        &*adapter,
                        &job,
                        (op, id),
                        &cancel,
                        &mut out,
                        &tcx.sink,
                    );
                    p.items_done.fetch_add(1, Ordering::Relaxed);
                }
            }
            drop(turn);
            for id in &ids {
                env.book.set_busy(id, None);
            }
            // Перечитать только затронутый менеджер.
            if out.done > 0 && !tcx.cancel.is_cancelled() {
                requery(&env, &*adapter, mgr, &tcx.cancel);
            }
            let declined = out.declined;
            let reboot = out.reboot;
            let errors = out.errors.clone();
            let m = msg("pkg.action_done", move |cx| {
                refresh_tabs(cx);
                if let Some(first) = errors.first() {
                    cx.toast(Level::Error, first.clone());
                }
                if reboot {
                    reboot_banner(cx);
                }
                store(cx, &env);
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
            let summary = urfin_core::JobSummary {
                items: out.done as u64,
                errors: out.errors.len() as u64,
                ..Default::default()
            };
            if declined {
                p.set_current(&urfin_i18n::current().get(crate::choco::UAC_DECLINED));
                cancel.cancel();
                job.finish(Err(urfin_i18n::current().get(crate::choco::UAC_DECLINED)));
            } else if cancel.is_cancelled() {
                job.finish(Err("cancelled".into()));
            } else if out.errors.is_empty() || out.done > 0 {
                job.finish(Ok(summary));
            } else {
                job.finish(Err(out.errors.join("\n")));
            }
        });
}

/// Одно действие: план, запуск, ход из вывода (строка пакета и job-бар).
fn one(
    env: &Arc<Env>,
    adapter: &dyn PackageManager,
    job: &urfin_core::JobHandle,
    step: (PkgOp, &PkgId),
    cancel: &CancelToken,
    out: &mut Outcome,
    sink: &urfin_core::MsgSink,
) {
    let (op, id) = step;
    let p = job.progress();
    p.set_current(&id.id);
    p.bytes_done.store(0, Ordering::Relaxed);
    p.bytes_total.store(0, Ordering::Relaxed);
    let base = RunCx {
        cancel: cancel.clone(),
        timeout: None,
        lines: None,
    };
    let plan = match adapter.action(op, id, &base) {
        Ok(p) => p,
        Err(e) => {
            out.errors.push(format!("{id}: {e}"));
            return;
        }
    };
    let busy = Busy {
        op,
        queued: false,
        pct: None,
        uac: plan.elevate,
    };
    env.book.set_busy(id, Some(busy));
    if plan.elevate {
        p.set_current(&urfin_i18n::current().get("pkg-busy-uac"));
    }
    let _ = sink.try_send(msg("pkg.progress", refresh_tabs));
    // Ход строки — из строк вывода, без опоры на слова (F02 §3.2); байты — job-бару.
    let bytes = Arc::new(Mutex::new(None::<(u64, u64)>));
    let lines: crate::api::LineSink = {
        let last = Mutex::new(busy);
        let env = Arc::clone(env);
        let id = id.clone();
        let sink = sink.clone();
        let bytes = Arc::clone(&bytes);
        Arc::new(move |line: &str| {
            tracing::debug!(target: "pkg", "{line}");
            let lp = crate::jobs::progress_of(line);
            if let Some(b) = lp.bytes {
                *bytes.lock().unwrap_or_else(PoisonError::into_inner) = Some(b);
            }
            let mut b = last.lock().unwrap_or_else(PoisonError::into_inner);
            let next = Busy {
                pct: lp.pct.or(b.pct),
                uac: false,
                ..*b
            };
            if next != *b {
                *b = next;
                env.book.set_busy(&id, Some(next));
                let _ = sink.try_send(msg("pkg.progress", refresh_tabs));
            }
        })
    };
    let rc = RunCx {
        lines: Some(lines),
        ..base
    };
    let done = std::sync::atomic::AtomicBool::new(false);
    let res = std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if let Some((d, t)) = *bytes.lock().unwrap_or_else(PoisonError::into_inner) {
                    p.bytes_done.store(d, Ordering::Relaxed);
                    p.bytes_total.store(t, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let r = adapter.run(&plan, &rc);
        done.store(true, Ordering::Release);
        r
    });
    match res {
        Ok(ActionDone::Ok) => out.done += 1,
        Ok(ActionDone::Reboot) => {
            out.done += 1;
            out.reboot = true;
        }
        Err(PkgError::Elevation(Elevation::Declined)) => out.declined = true,
        Err(PkgError::Cancelled) => {}
        Err(e) => out.errors.push(format!("{id}: {e}")),
    }
}

/// Списки менеджера после действия: установленные, затем обновления.
fn requery(env: &Env, adapter: &dyn PackageManager, mgr: Mgr, cancel: &CancelToken) {
    let t = env.config.timeouts;
    let rc = |s: u32| RunCx {
        cancel: cancel.clone(),
        timeout: Some(Duration::from_secs(u64::from(s))),
        lines: None,
    };
    let res = adapter.list_installed(&rc(t.list_s)).and_then(|list| {
        env.book.set_installed(mgr, list);
        adapter.list_updates(&rc(t.updates_s))
    });
    match res {
        Ok(ups) => env.book.set_updates(mgr, ups, None),
        Err(e) => {
            tracing::warn!(target: "pkg", %mgr, error = %e, "pkg: requery");
            env.book.failed(mgr, e.to_string().into());
        }
    }
}

/// «Нужна перезагрузка» (коды 1641/3010 и winget): баннер с «Позже».
fn reboot_banner(cx: &mut Cx<'_>) {
    let i18n = urfin_i18n::current();
    cx.state.banner = Some(Banner {
        level: Level::Warn,
        text: i18n.get("pkg-reboot"),
        actions: [(
            i18n.get("pkg-later").into(),
            Command::new("app.dismiss_banner"),
        )]
        .into_iter()
        .collect(),
    });
    cx.state.touch();
}
