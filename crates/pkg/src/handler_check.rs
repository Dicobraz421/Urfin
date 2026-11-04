//! Фоновая проверка и расписание handler'а `pkg` (F02 §4, F02-11).
//!
//! - Баннер «Доступно N обновлений · M важных»: итог `check.json` (его пишет
//!   `urfin.exe --pkg-check` по расписанию или `pkg.check_now`) — через [`STARTUP_DELAY`]
//!   после старта, по звонку [`crate::CheckBell`] (проверка по расписанию закончилась,
//!   пока Урфин открыт — IPC единственного экземпляра) и после `pkg.check_now`.
//!   «Показать» / «Позже» закрывают его до следующей проверки (`dismissed_at`).
//! - `pkg.schedule on at?` — задача Планировщика и `[pkg.schedule]` конфига; без `on` —
//!   сверка задачи (путь `urfin.exe`, раздел настроек F02-12).
//!
//! Таймеров опроса нет: файл читается по событию, а старт — один отложенный будильник.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use urfin_core::pkg::{PkgModel, Task};
use urfin_core::{Level, msg, settings};

use super::{Arc, CACHE_READY, CmdError, Command, Cx, Env, Filter, Lane, load, location};
use crate::check::{CHECK_FILE, CheckError, CheckReport};
use crate::schedule::{self, Tasks};

/// Итог прошлой проверки читается не в первом кадре: полоса Interactive на старте —
/// листингу панелей.
pub(super) const STARTUP_DELAY: Duration = Duration::from_millis(500);

fn tr(key: &str, pairs: &[(&str, String)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in pairs {
        a.set((*k).to_owned(), v.clone());
    }
    urfin_i18n::current().fmt(key, &a)
}

/// Когда читать `check.json` при старте.
#[derive(Debug, Default)]
pub(super) enum Startup {
    /// Первый pump ещё не прошёл.
    #[default]
    New,
    /// Будильник поставлен.
    Waiting(Instant),
    Done,
}

/// Каждый pump: отложенное чтение итога при старте и звонок от `--pkg-check`.
pub(super) fn pump(cx: &mut Cx<'_>, env: &Arc<Env>, startup: &mut Startup) {
    if env.dir.is_none() {
        return;
    }
    match startup {
        Startup::New => {
            *startup = Startup::Waiting(Instant::now());
            cx.rt.waker().wake_after(STARTUP_DELAY);
        }
        Startup::Waiting(at) if at.elapsed() >= STARTUP_DELAY => {
            *startup = Startup::Done;
            read_report(cx, env, false);
        }
        Startup::Waiting(_) | Startup::Done => {}
    }
    if env.bell.take() {
        *startup = Startup::Done;
        read_report(cx, env, true);
    }
}

/// `check.json` → баннер (задача Interactive); `fresh` — проверка только что
/// закончилась: открытая вкладка перечитывает кэш.
fn read_report(cx: &mut Cx<'_>, env: &Arc<Env>, fresh: bool) {
    let Some(dir) = env.dir.clone() else {
        return;
    };
    let env = Arc::clone(env);
    cx.rt
        .spawn(Lane::Interactive, "pkg.check_read", cx.rt.token(), move |tcx| {
            let report = CheckReport::load(&dir.join(CHECK_FILE));
            let m = msg("pkg.check_read", move |cx| {
                if let Some(r) = report {
                    apply(cx, &env, &r);
                }
                if fresh {
                    reload(cx, &env);
                }
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Баннер по итогу: обновления есть, баннер этой проверки не закрыт и их не видно уже
/// в открытых «Обновлениях» (активная вкладка панели на данных не старше проверки).
pub(super) fn apply(cx: &mut Cx<'_>, env: &Env, r: &CheckReport) {
    let notice = r.notice().filter(|n| {
        let shown = cx
            .state
            .panels
            .iter()
            .any(|p| p.tab().loc == location(Filter::Updates));
        let fresh = env
            .book
            .info()
            .checked_at
            .is_some_and(|at| at >= n.checked_at);
        !(shown && fresh)
    });
    set_notice(cx, notice);
}

fn set_notice(cx: &mut Cx<'_>, notice: Option<urfin_core::pkg::Notice>) {
    if let Some(m) = cx.state.ext.get_mut::<PkgModel>()
        && m.notice != notice
    {
        m.notice = notice;
        cx.state.touch();
    }
}

fn set_task(cx: &mut Cx<'_>, task: Task) {
    if let Some(m) = cx.state.ext.get_mut::<PkgModel>()
        && m.task != task
    {
        m.task = task;
        cx.state.touch();
    }
}

/// Кэш вкладки переписан проверкой: прочитанный раньше — перечитать (не во время
/// своей проверки).
fn reload(cx: &mut Cx<'_>, env: &Arc<Env>) {
    if env.cache.load(Ordering::Acquire) == CACHE_READY && !env.book.info().busy() {
        load(cx, env, None);
    }
}

/// `pkg.banner.show` / `pkg.banner.later`: баннер скрыт до следующей проверки —
/// `dismissed_at` в `check.json` (задача Bulk).
pub(super) fn close(cx: &mut Cx<'_>, env: &Env) {
    let Some(n) = urfin_core::pkg::notice(cx.state).cloned() else {
        return;
    };
    set_notice(cx, None);
    let Some(dir) = env.dir.clone() else {
        return;
    };
    cx.rt
        .spawn(Lane::Bulk, "pkg.banner_dismiss", cx.rt.token(), move |_| {
            if let Err(e) = crate::check::dismiss(&dir, n.checked_at) {
                tracing::warn!(target: "pkg", error = %e, "pkg: banner dismiss");
            }
        });
}

/// `pkg.check_now`: проверка как по расписанию, в своём потоке; итог — баннер, без
/// обновлений — тост.
pub(super) fn check_now(cx: &mut Cx<'_>, env: &Arc<Env>) {
    let Some(dir) = env.dir.clone() else {
        cx.toast(
            Level::Warn,
            tr("pkg-check-failed", &[("error", "no state directory".into())]),
        );
        return;
    };
    if env.checking.swap(true, Ordering::AcqRel) {
        cx.toast(Level::Info, tr("pkg-check-busy", &[]));
        return;
    }
    let env = Arc::clone(env);
    let _ = cx
        .rt
        .spawn_dedicated("pkg.check_now".into(), cx.rt.token(), move |tcx| {
            let res = crate::check::run(&dir, &env.config, &*env.source, &tcx.cancel, true);
            let m = msg("pkg.checked", move |cx| {
                env.checking.store(false, Ordering::Release);
                match res {
                    Ok(r) => {
                        if r.updates.is_empty() {
                            cx.toast(Level::Info, tr("pkg-check-none", &[]));
                        }
                        apply(cx, &env, &r);
                        reload(cx, &env);
                    }
                    Err(CheckError::Busy) => cx.toast(Level::Info, tr("pkg-check-busy", &[])),
                    Err(e) => cx.toast(
                        Level::Warn,
                        tr("pkg-check-failed", &[("error", e.to_string())]),
                    ),
                }
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// `pkg.schedule on at?`: создать задачу (время — `at` или из конфига) или удалить её,
/// затем сверить и записать `[pkg.schedule]`; без `on` — только сверить.
pub(super) fn schedule_cmd(cmd: &Command, cx: &mut Cx<'_>, env: &Arc<Env>) -> Result<(), CmdError> {
    let on = cmd.args.bool("on");
    let at = cmd
        .args
        .str("at")
        .map(str::to_owned)
        .or_else(|| {
            cx.state
                .ext
                .get::<crate::PkgState>()
                .map(|p| p.config.schedule.at.to_string())
        })
        .unwrap_or_else(|| env.config.schedule.at.to_string());
    if on == Some(true) && schedule::parse_at(&at).is_none() {
        return Err(CmdError::Arg {
            name: "at".into(),
            msg: tr("pkg-schedule-bad-at", &[]).into(),
        });
    }
    set_task(cx, Task::Busy);
    let tasks = Arc::clone(&env.tasks);
    let _ = cx
        .rt
        .spawn_dedicated("pkg.schedule".into(), cx.rt.token(), move |tcx| {
            let res = apply_task(&*tasks, on, &at);
            let m = msg("pkg.schedule", move |cx| scheduled(cx, on, &at, res));
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
    Ok(())
}

/// Создание или удаление задачи и сверка: состояние задачи или ошибка `schtasks`.
fn apply_task(tasks: &dyn Tasks, on: Option<bool>, at: &str) -> Result<Task, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    match on {
        Some(true) => {
            let user = schedule::current_user();
            let xml = schedule::task_xml(&exe, at, user.as_deref())
                .ok_or_else(|| tr("pkg-schedule-bad-at", &[]))?;
            tasks
                .create(&xml)
                .map_err(|e| schedule::reason(&e).to_string())?;
        }
        Some(false) => tasks
            .delete()
            .map_err(|e| schedule::reason(&e).to_string())?,
        None => {}
    }
    let xml = tasks
        .query()
        .map_err(|e| schedule::reason(&e).to_string())?;
    Ok(schedule::status(xml.as_deref(), &exe))
}

/// Итог `pkg.schedule`: состояние задачи в модель, `[pkg.schedule]` — в конфиг, тост.
fn scheduled(cx: &mut Cx<'_>, on: Option<bool>, at: &str, res: Result<Task, String>) {
    match res {
        Ok(task) => {
            set_task(cx, task);
            let Some(on) = on else {
                return;
            };
            settings::set(&["pkg", "schedule", "enabled"], on).apply(cx);
            if on {
                settings::set(&["pkg", "schedule", "at"], at).apply(cx);
            }
            if let Some(p) = cx.state.ext.get_mut::<crate::PkgState>() {
                p.config.schedule.enabled = on;
                if on {
                    p.config.schedule.at = at.into();
                }
            }
            let text = if on {
                tr("pkg-schedule-on", &[("at", at.to_owned())])
            } else {
                tr("pkg-schedule-off", &[])
            };
            cx.toast(Level::Info, text);
        }
        Err(e) => {
            set_task(cx, Task::Failed(e.as_str().into()));
            cx.toast(Level::Warn, tr("pkg-schedule-failed", &[("error", e)]));
        }
    }
}
