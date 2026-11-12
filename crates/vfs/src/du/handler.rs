//! Handler `du` (F01 §2.4): команды анализа места. Работает в UI-потоке, без I/O;
//! скан, чтение и запись кэша — задачами Runtime.
//!
//! `du.open` — анализ каталога активной панели в новой вкладке `du:` (из кэша — сразу,
//! с датой подсчёта; без кэша — скан); `du.refresh` — пересчёт; `du.stop` (Esc) —
//! остановка, посчитанное остаётся «≥ X»; `du.filter` (Ctrl+1…6) — вид вкладки,
//! Enter на группе «Типов» — раскрыть; `du.old_age` — порог «Старые» без пересчёта;
//! `du.map` (Ctrl+Tab) — карта во второй панели; «Дубликаты» (Ctrl+5) — группы по
//! размеру и частичный хэш задачей `du.dups`, `du.verify_hash` (Ctrl+H) — полный хэш,
//! совпавшие группы отмечаются (все копии, кроме первой); с `group` — только группа под
//! курсором. `du.select_group` — отметить (или снять) все копии группы под курсором;
//! `du.goto_file` — из строки вида фильтра в её папку «По папкам», курсор на ней (F01-15). `du.elevate` — досчёт
//! недоступных папок тем же `urfin.exe` через UAC (ADR-0021, [`super::elevated`]): итоги
//! подмешиваются в снимок, отказ в UAC — тост, снимок не меняется; Esc — отмена досчёта,
//! без задач — закрыть баннер «N папок без доступа».
//!
//! Пока у анализа есть вкладка и он посчитан, его поддерево наблюдается
//! ([`super::fresh::Live`], F01 §3.3): изменения перелистываются и попадают в снимок;
//! последняя вкладка закрыта — наблюдение снято, снимок записан в кэш.
//!
//! Запись изменённого снимка (F01-14): пока вкладка открыта — не чаще раза в 30 с;
//! вкладка закрыта — сразу; выход (`Core::shutdown` роняет handler) — синхронно в
//! `Drop`, задачи Runtime к этому моменту остановлены.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use urfin_core::{
    ArgSpec, CmdError, Command, CommandMeta, Cx, DeviceJobs, Handler, JobSpec, JobSummary, Lane,
    ListingStatus, Location, ValueKind, msg,
};

use super::DuCfg;
use super::cache::DuCache;
use super::dvfs::{self, DuRegistry, DuState, SnapId};
use super::elevated::{self, Elevation, Fail, Outcome};
use super::filters::{self, View};
use super::model::{DuFilter, DuSnapshot};
use super::scan::{OnScan, ScanCfg, ScanEvent};
use crate::api::Vfs;

/// Запись изменённого снимка в кэш — не чаще (F01 §2.2).
const STORE_EVERY: Duration = Duration::from_secs(30);

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "du",
        args,
        repeatable: false,
    }
}

static COMMANDS: [CommandMeta; 12] = [
    // Меню «Утилиты → Анализ места…» (Ctrl+F9, №83).
    meta(
        "du.open",
        "Disk usage",
        &[
            arg("loc", ValueKind::Loc, false),
            arg("drive", ValueKind::Bool, false),
        ],
    ),
    meta(
        "du.refresh",
        "Refresh disk usage",
        &[arg("full", ValueKind::Bool, false)],
    ),
    meta("du.stop", "Stop disk usage scan", &[]),
    // `kind`: dirs | large | types | old | dups | temp; `group` — раскрыть или свернуть
    // группу «Типов» (Enter на ней).
    meta(
        "du.filter",
        "Disk usage filter",
        &[
            arg("kind", ValueKind::Str, true),
            arg("group", ValueKind::Str, false),
        ],
    ),
    meta("du.map", "Disk usage map", &[]),
    // `group` — только группа под курсором (F01-15).
    meta(
        "du.verify_hash",
        "Verify duplicates by hash",
        &[arg("group", ValueKind::Bool, false)],
    ),
    // «Отметить группу» / «К папке файла» (F01-15; привязки — F01-19).
    meta("du.select_group", "Select duplicate group", &[]),
    meta("du.goto_file", "Go to file folder", &[]),
    // Досчёт недоступных папок с правами администратора (ADR-0021, F01-12).
    meta("du.elevate", "Count as administrator", &[]),
    meta(
        "du.old_age",
        "Old files age",
        &[arg("age", ValueKind::Str, true)],
    ),
    // «Временные»: отметить все строки (кроме Корзины) и очистить отмеченное — с
    // подтверждением и сводкой, в Корзину путём F8 (решение 2026-11-02 «du-filters» №3).
    meta("du.clean_select", "Select all temporary files", &[]),
    meta("du.clean", "Clean up selected temporary files", &[]),
];

/// Общее для задач и сообщений handler.
struct Env {
    reg: Arc<DuRegistry>,
    cfg: DuCfg,
    cache: Option<DuCache>,
    /// Backend обхода (локальная ФС).
    fs: Arc<dyn Vfs>,
    /// Досчёт с правами (F01-12): `None` — недоступен.
    elevation: Option<Elevation>,
}

/// Handler `du`.
pub struct DuHandler {
    env: Arc<Env>,
    stored: Option<Instant>,
    /// Открытый вопрос «Очистить временные?»: ответ — `ops.delete` в Корзину.
    clean: Option<crossbeam_channel::Receiver<urfin_core::Reply>>,
    /// Порог «Старые» (`[du] old_age`, затем последний выбранный чип).
    age: compact_str::CompactString,
    /// Наблюдения за поддеревьями посчитанных анализов с открытыми вкладками.
    live: FxHashMap<SnapId, super::fresh::Live>,
}

impl std::fmt::Debug for DuHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuHandler")
            .field("cfg", &self.env.cfg)
            .field("cache", &self.env.cache)
            .finish_non_exhaustive()
    }
}

impl DuHandler {
    /// Свой реестр, без кэша (тесты).
    pub fn new(cfg: DuCfg) -> DuHandler {
        DuHandler::with(cfg, DuRegistry::new(), None)
    }

    /// `reg` — общий с backend `du:` ([`dvfs::DuVfs`]); `cache` — каталог снимков
    /// (`None` — без кэша: `--smoke`, тесты).
    pub fn with(cfg: DuCfg, reg: Arc<DuRegistry>, cache: Option<DuCache>) -> DuHandler {
        let age = if filters::age_days(&cfg.old_age).is_some() {
            cfg.old_age.clone()
        } else {
            "1y".into()
        };
        reg.set_dup_min(super::parse_size(&cfg.dup_min).unwrap_or(1 << 20));
        DuHandler {
            age,
            env: Arc::new(Env {
                reg,
                cfg,
                cache,
                fs: Arc::new(crate::LocalFs::new()),
                elevation: None,
            }),
            stored: None,
            clean: None,
            live: FxHashMap::default(),
        }
    }

    /// Досчёт с правами администратора (F01-12): запуск через UAC и каталог ответов.
    #[must_use]
    pub fn with_elevation(mut self, elevation: Option<Elevation>) -> DuHandler {
        if let Some(env) = Arc::get_mut(&mut self.env) {
            env.elevation = elevation;
        }
        self
    }

    fn open(&self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let a = &cmd.args;
        let base = a
            .loc("loc")
            .unwrap_or_else(|| cx.state.active_tab().loc.clone());
        // Из вкладки анализа — тот же каталог на диске.
        let base = dvfs::localize(cx.state, &base).unwrap_or(base);
        if !base.is_local() {
            return Err(CmdError::Rejected(text("du-local-only", &[]).into()));
        }
        let root = if a.bool("drive").unwrap_or(false) {
            drive_root(&base.path)
        } else {
            base.path.clone()
        };
        if let Some(id) = self.env.reg.find(&root) {
            open_tab(cx, &self.env, id);
            return Ok(());
        }
        let Some(cache) = self.env.cache.clone() else {
            let id = start(cx, &self.env, root, None);
            open_tab(cx, &self.env, id);
            return Ok(());
        };
        // Кэш читается вне UI-потока (≤ 100 мс), затем — вкладка из него или скан.
        let env = Arc::clone(&self.env);
        cx.rt.spawn(
            Lane::Interactive,
            "du.cache_load",
            cx.rt.token(),
            move |tcx| {
                let t0 = Instant::now();
                let serial = crate::volume::of(&root)
                    .ok()
                    .map_or(0, |v| u32::try_from(v.id & 0xFFFF_FFFF).unwrap_or(0));
                let snap = cache.load(serial, &root);
                tracing::info!(name: "du.cache_load", target: "vfs.du", ms = t0.elapsed().as_millis(), hit = snap.is_some());
                let m = msg("du.cache_load", move |cx| {
                    // Пока читали, анализ мог открыться повторной командой.
                    let id = match env.reg.find(&root) {
                        Some(id) => id,
                        None => match snap {
                            Some(s) => {
                                let id = env.reg.open_snapshot(s, true);
                                recheck(cx, &env, id);
                                id
                            }
                            None => start(cx, &env, root, None),
                        },
                    };
                    open_tab(cx, &env, id);
                });
                let _ = tcx.sink.send(m, &tcx.cancel);
            },
        );
        Ok(())
    }

    /// Пересчёт анализа активной вкладки: есть «●» — перелистать только их (Ctrl+R),
    /// иначе или `full` (Ctrl+Shift+R) — полный скан.
    fn refresh(&self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = active_snap(cx)?;
        if self.env.reg.job(id).is_some() || self.env.reg.elev_job(id).is_some() {
            cx.keep_rev();
            return Ok(());
        }
        if !cmd.args.bool("full").unwrap_or(false)
            && self.env.reg.watchable(id)
            && !self.env.reg.changed(id).is_empty()
        {
            relist_changed(cx, &self.env, id);
            return Ok(());
        }
        let root = self
            .env
            .reg
            .root(id)
            .ok_or_else(|| CmdError::Rejected(text("du-not-du", &[]).into()))?;
        start(cx, &self.env, root, Some(id));
        refresh_tabs(cx, id);
        Ok(())
    }

    /// Вид вкладки (Ctrl+1…6): фильтры — у корня анализа, по папкам — корень. С
    /// `group` — раскрыть или свернуть группу «Типов».
    fn filter(&self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (id, rel, cur) = active_view(cx)?;
        let kind = cmd.args.str("kind").unwrap_or_default();
        let f = DuFilter::from_id(kind).ok_or_else(|| bad_arg("kind", "unknown filter"))?;
        if let Some(g) = cmd.args.str("group") {
            let ty =
                filters::TypeGroup::from_id(g).ok_or_else(|| bad_arg("group", "unknown type"))?;
            self.env.reg.toggle_group(id, ty);
            refresh_tabs(cx, id);
            return Ok(());
        }
        if f == cur.filter && (f != DuFilter::Dirs || rel.is_empty()) {
            cx.keep_rev();
            return Ok(());
        }
        let v = View {
            filter: f,
            age: self.age.clone(),
        };
        go_view(cx, id, &v);
        // Дубликаты: группы по размеру — при показе вида, частичный хэш — сразу задачей
        // (часть явного действия «открыть фильтр», F01 §3.6).
        if f == DuFilter::Dups {
            hash_dups(cx, &self.env, id, false, None);
        }
        Ok(())
    }

    /// «Отметить все» во «Временных»: строки совпадений, кроме Корзины. Отмечает только
    /// по команде пользователя (сами правила ничего не отмечают).
    fn clean_select(cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (_, _, v) = active_view(cx)?;
        let Some(reg) = dvfs::registry(cx.state).cloned() else {
            cx.keep_rev();
            return Ok(());
        };
        if v.filter != DuFilter::Temp {
            cx.keep_rev();
            return Ok(());
        }
        let loc = cx.state.active_tab().loc.clone();
        let hints = reg.hints(&loc);
        let t = cx.state.panel_mut(cx.state.active).tab_mut();
        let l = t.listing.clone();
        t.selection.select_where(&l, true, |e| {
            e.kind != urfin_core::EntryKind::Other && hints.get(&e.name).is_some_and(|h| !h.recycle)
        });
        t.touch();
        Ok(())
    }

    /// «Очистить выбранное…»: вопрос со сводкой — сколько, сколько освободится, что
    /// пересоздастся и что потеряется; «В Корзину» — `ops.delete` (путь F8), «Отмена» —
    /// ничего. Корзина из отметки снимается: её очищают в самой Корзине.
    fn clean(&mut self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (_, _, v) = active_view(cx)?;
        let reg = dvfs::registry(cx.state).cloned();
        let (Some(reg), true) = (reg, v.filter == DuFilter::Temp) else {
            cx.keep_rev();
            return Ok(());
        };
        let loc = cx.state.active_tab().loc.clone();
        let hints = reg.hints(&loc);
        let side = cx.state.active;
        let t = cx.state.panel_mut(side).tab_mut();
        let l = t.listing.clone();
        t.selection
            .select_where(&l, false, |e| hints.get(&e.name).is_some_and(|h| h.recycle));
        let picked: Vec<(u64, Option<super::Hint>)> = t
            .selection
            .iter()
            .filter_map(|i| l.entries.get(i as usize))
            .map(|e| (e.size, hints.get(&e.name).cloned()))
            .collect();
        if picked.is_empty() {
            cx.keep_rev();
            return Ok(());
        }
        if self.clean.is_some() {
            // Вопрос уже открыт.
            cx.keep_rev();
            return Ok(());
        }
        let spec = clean_prompt(side, &picked);
        let (_, rx) = cx.state.prompts.open(spec, None);
        self.clean = Some(rx);
        Ok(())
    }

    /// Ответ на «Очистить временные?»: «В Корзину» — удаление отмеченного путём F8.
    fn poll_clean(&mut self, cx: &mut Cx<'_>) {
        let Some(rx) = &self.clean else {
            return;
        };
        let reply = match rx.try_recv() {
            Ok(r) => Some(r),
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => None,
        };
        self.clean = None;
        let temp = dvfs::filter_of(&cx.state.active_tab().loc) == Some(DuFilter::Temp);
        if reply.is_some_and(|r| r.choice == "trash") && temp {
            cx.dispatch(Command::new("ops.delete").arg("confirmed", true));
        }
    }

    /// Ctrl+H: полный хэш непроверенных групп вида «Дубликаты»; `group` — только
    /// группы под курсором (F01-15).
    fn verify_hash(&self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (id, _, v) = active_view(cx)?;
        if v.filter != DuFilter::Dups || self.env.reg.dup_job(id).is_some() {
            cx.keep_rev();
            return Ok(());
        }
        let only = if cmd.args.bool("group").unwrap_or(false) {
            let Some((gi, _)) = cursor_group(cx.state.active_tab()) else {
                cx.keep_rev();
                return Ok(());
            };
            Some(gi)
        } else {
            None
        };
        hash_dups(cx, &self.env, id, true, only);
        Ok(())
    }

    /// «Отметить группу» (F01-15): все копии группы под курсором (заголовок или копия);
    /// отмечены все — отметка снимается.
    fn select_group(cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (_, _, v) = active_view(cx)?;
        let t = cx.state.active_tab();
        let group = (v.filter == DuFilter::Dups)
            .then(|| cursor_group(t))
            .flatten();
        let Some((_, rows)) = group.filter(|(_, rows)| !rows.is_empty()) else {
            cx.keep_rev();
            return Ok(());
        };
        let on = !rows.iter().all(|&i| t.selection.contains(i));
        let t = cx.state.panel_mut(cx.state.active).tab_mut();
        let l = t.listing.clone();
        for i in rows {
            if let Some(e) = l.entries.get(i as usize) {
                t.selection.set(i, on, e);
            }
        }
        t.touch();
        Ok(())
    }

    /// «К папке файла» (F01-15): строка вида фильтра → её каталог «По папкам», курсор
    /// на ней. Без I/O: адрес — из пути строки от корня анализа.
    fn goto_file(cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (id, _, v) = active_view(cx)?;
        let target = match cx.state.active_tab().current() {
            Some(urfin_core::Row::Entry { entry, .. })
                if v.filter != DuFilter::Dirs && entry.kind != urfin_core::EntryKind::Other =>
            {
                Some(entry.name.clone())
            }
            _ => None,
        };
        let Some(rel) = target else {
            cx.keep_rev();
            return Ok(());
        };
        let (dir, name) = rel.rsplit_once('\\').unwrap_or(("", rel.as_str()));
        let loc = if dir.is_empty() {
            dvfs::location(id, Path::new(""))
        } else {
            dvfs::location(id, &dvfs::join_name(Path::new(""), dir))
        };
        cx.dispatch(Command::new("panel.go").arg("path", loc.clone()));
        by_size(cx, &loc);
        cx.dispatch(Command::new("panel.cursor.to").arg("name", name));
        Ok(())
    }

    fn old_age(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let age = cmd.args.str("age").unwrap_or_default();
        if filters::age_days(age).is_none() {
            return Err(bad_arg("age", "expected 6m, 1y, 2y, 5y"));
        }
        self.age = age.into();
        match active_view(cx) {
            Ok((id, _, v)) if v.filter == DuFilter::Old && v.age != age => {
                go_view(
                    cx,
                    id,
                    &View {
                        filter: DuFilter::Old,
                        age: self.age.clone(),
                    },
                );
            }
            _ => cx.keep_rev(),
        }
        Ok(())
    }

    /// Карта во второй панели (Ctrl+Tab): показать или убрать.
    fn map(cx: &mut Cx<'_>) -> Result<(), CmdError> {
        active_snap(cx)?;
        match cx.state.ext.get_mut::<DuState>() {
            Some(d) => d.map = !d.map,
            None => cx.keep_rev(),
        }
        Ok(())
    }

    /// Esc: остановить подсчёт, хэширование дубликатов или досчёт с правами; без задач —
    /// закрыть баннер «N папок без доступа».
    fn stop(&self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = active_snap(cx)?;
        let reg = &self.env.reg;
        match reg
            .job(id)
            .or_else(|| reg.dup_job(id))
            .or_else(|| reg.elev_job(id))
        {
            Some(job) => cx.state.jobs.cancel(job),
            None if reg.close_banner(id) => {}
            None => cx.keep_rev(),
        }
        Ok(())
    }

    /// «Досчитать с правами администратора» (F01-12, ADR-0021): недоступные папки
    /// анализа — `urfin.exe --du-elevated` через UAC задачей `du.elevate`; итоги — в
    /// снимок. Политика `DuElevatedHelper` = 0 — отказ.
    fn elevate(&self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = active_snap(cx)?;
        let Some(el) = self.env.elevation.clone() else {
            return Err(CmdError::Rejected(
                text("du-elevate-unavailable", &[]).into(),
            ));
        };
        if elevate_forbidden(cx.state) {
            return Err(CmdError::Rejected(text("du-elevate-policy", &[]).into()));
        }
        let reg = &self.env.reg;
        if reg.job(id).is_some() || reg.elev_job(id).is_some() {
            cx.keep_rev();
            return Ok(());
        }
        let roots = reg.denied_paths(id);
        if roots.is_empty() {
            return Err(CmdError::Rejected(text("du-elevate-nothing", &[]).into()));
        }
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("count", roots.len());
        let job = cx.state.jobs.start(
            JobSpec {
                kind: "du.elevate",
                title: urfin_i18n::current().fmt("du-elevate-job", &a),
                lane: Lane::Bulk,
                priority: 0,
                owner_ns: "du",
            },
            cx.rt.sink(),
        );
        reg.set_elev_job(id, Some(job.id()));
        let reg = Arc::clone(reg);
        tracing::info!(target: "vfs.du", roots = roots.len(), "du.elevate: asking UAC");
        cx.rt
            .spawn_dedicated("disk-usage-elevate".into(), cx.rt.token(), move |tcx| {
                let p = job.progress();
                p.set_current(&text("du-elevate-uac", &[]));
                let t0 = Instant::now();
                let halted = || tcx.cancel.is_cancelled() || job.checkpoint().is_err();
                let out = elevated::collect(&el, roots, &halted, &|| {
                    p.set_current(&text("du-elevate-running", &[]));
                });
                tracing::info!(
                    target: "vfs.du",
                    ms = t0.elapsed().as_millis(),
                    outcome = outcome_kind(&out),
                    "du.elevate"
                );
                match &out {
                    Outcome::Done(a) => job.finish(Ok(JobSummary {
                        items: a.roots.iter().filter(|r| r.ok).count() as u64,
                        ..JobSummary::default()
                    })),
                    Outcome::Cancelled => job.finish(Err("cancelled".into())),
                    Outcome::Declined => job.finish(Err(text("du-elevate-declined", &[]))),
                    Outcome::Failed(f) => job.finish(Err(fail_text(f))),
                }
                let m = msg("du.elevate", move |cx| {
                    reg.set_elev_job(id, None);
                    elevated_done(cx, &reg, id, out);
                });
                let _ = tcx.sink.send(m, &tcx.cancel);
            });
        Ok(())
    }

    /// Наблюдение — у посчитанных анализов с открытыми вкладками (F01 §3.3): новые —
    /// начать, без вкладок или на пересчёте — снять; без вкладок снимок — в кэш сразу.
    fn sync_live(&mut self, cx: &mut Cx<'_>, open: &rustc_hash::FxHashSet<SnapId>) {
        let reg = &self.env.reg;
        let stale: Vec<SnapId> = self
            .live
            .keys()
            .copied()
            .filter(|id| !open.contains(id) || !reg.watchable(*id))
            .collect();
        for id in stale {
            self.live.remove(&id);
            if !open.contains(&id)
                && let Some(snap) = reg.take_dirty(id)
            {
                store(cx, &self.env, snap);
            }
        }
        for &id in open {
            if self.live.contains_key(&id) || !reg.watchable(id) {
                continue;
            }
            let Some(root) = reg.root(id) else {
                continue;
            };
            let (sink, token) = (cx.rt.sink(), cx.rt.token());
            let notify = Box::new(move || {
                let _ = sink.send(msg("du.live", move |cx| refresh_tabs(cx, id)), &token);
            });
            match super::fresh::Live::start(
                Arc::clone(reg),
                id,
                root,
                Arc::clone(&self.env.fs),
                super::fresh::LiveCfg::new(ScanCfg::from_du(&self.env.cfg)),
                notify,
            ) {
                Ok(l) => {
                    self.live.insert(id, l);
                }
                Err(e) => tracing::warn!(target: "vfs.du", error = %e, "live watch thread"),
            }
        }
    }

    /// Изменённые после скана снимки (вычтено удалённое) — в кэш: с открытой вкладкой
    /// не чаще раза в 30 с, без вкладки — сразу (закрыта вкладка, F01-14).
    fn store_dirty(&mut self, cx: &mut Cx<'_>, open: &rustc_hash::FxHashSet<SnapId>) {
        if self.env.cache.is_none() {
            return;
        }
        let dirty = self.env.reg.dirty();
        if dirty.is_empty() {
            return;
        }
        let due = self.stored.is_none_or(|t| t.elapsed() >= STORE_EVERY);
        for id in dirty {
            let shown = open.contains(&id);
            if shown && !due {
                continue;
            }
            if let Some(snap) = self.env.reg.take_dirty(id) {
                if shown {
                    self.stored = Some(Instant::now());
                }
                store(cx, &self.env, snap);
            }
        }
    }

    /// Все изменённые снимки — в кэш сейчас, в этом потоке (выход, F01-14). Сколько
    /// записано.
    pub fn flush(&self) -> usize {
        let Some(cache) = &self.env.cache else {
            return 0;
        };
        let mut n = 0;
        for id in self.env.reg.dirty() {
            let Some(snap) = self.env.reg.take_dirty(id) else {
                continue;
            };
            match cache.store(&snap) {
                Ok(_) => n += 1,
                Err(e) => {
                    tracing::warn!(target: "vfs.du", error = %e, root = %snap.root.display(), "cache store on exit");
                }
            }
        }
        n
    }
}

/// Выход: `Core::shutdown` останавливает Runtime и роняет handlers — изменённое после
/// последней записи не теряется (F01-14).
impl Drop for DuHandler {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Анализы с открытыми вкладками `du:`.
fn open_snaps(cx: &Cx<'_>) -> rustc_hash::FxHashSet<SnapId> {
    cx.state
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter())
        .filter_map(|t| dvfs::parse(&t.loc).map(|(id, _)| id))
        .collect()
}

impl Handler for DuHandler {
    fn namespace(&self) -> &'static str {
        "du"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "du.open" => self.open(cmd, cx),
            "du.refresh" => self.refresh(cmd, cx),
            "du.stop" => self.stop(cx),
            "du.filter" => self.filter(cmd, cx),
            "du.old_age" => self.old_age(cmd, cx),
            "du.map" => Self::map(cx),
            "du.verify_hash" => self.verify_hash(cmd, cx),
            "du.select_group" => Self::select_group(cx),
            "du.goto_file" => Self::goto_file(cx),
            "du.clean_select" => Self::clean_select(cx),
            "du.clean" => self.clean(cx),
            "du.elevate" => self.elevate(cx),
            id if COMMANDS.iter().any(|m| m.id == id) => {
                tracing::debug!(target: "vfs.du", cmd = id, "not implemented");
                Err(CmdError::Rejected(text("du-not-yet", &[]).into()))
            }
            id => Err(CmdError::Unknown(id.into())),
        }
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if cx.state.ext.get::<DuState>().is_none() {
            cx.state.ext.insert(DuState {
                reg: Arc::clone(&self.env.reg),
                map: false,
            });
        }
        for loc in self.env.reg.take_opened() {
            by_size(cx, &loc);
        }
        apply_marks(cx, &self.env.reg);
        self.poll_clean(cx);
        // Каталоги «Временных» для карты — обход снимка вне UI-потока.
        for id in self.env.reg.take_temp_wanted() {
            let reg = Arc::clone(&self.env.reg);
            cx.rt
                .spawn(Lane::Bulk, "du.temp_dirs", cx.rt.token(), move |tcx| {
                    reg.compute_temp(id);
                    // Разбудить UI: карта перерисуется с пометками.
                    let _ = tcx.sink.send(msg("du.temp_dirs", |_| {}), &tcx.cancel);
                });
        }
        let open = open_snaps(cx);
        self.sync_live(cx, &open);
        self.store_dirty(cx, &open);
    }
}

/// Политика организации запрещает досчёт с правами (`DuElevatedHelper` = 0).
pub fn elevate_forbidden(s: &urfin_core::AppState) -> bool {
    s.ext
        .get::<urfin_core::PolicySet>()
        .and_then(|p| p.bool(urfin_core::PolicyKey::DuElevatedHelper))
        == Some(false)
}

/// Итог досчёта — в UI-потоке: итоги в снимок и тост; отказ UAC — тост, снимок прежний.
fn elevated_done(cx: &mut Cx<'_>, reg: &DuRegistry, id: SnapId, out: Outcome) {
    let i18n = urfin_i18n::current();
    match out {
        Outcome::Done(a) => {
            let (n, bytes) = reg.merge_elevated(id, &a.roots);
            let left = reg.denied_paths(id).len();
            let mut f = urfin_i18n::FluentArgs::new();
            f.set("count", n);
            f.set("size", urfin_i18n::fmt::bytes_with(&i18n, bytes));
            f.set("left", left);
            cx.toast(urfin_core::Level::Info, i18n.fmt("du-elevate-done", &f));
            refresh_tabs(cx, id);
        }
        Outcome::Declined => {
            cx.toast(urfin_core::Level::Warn, i18n.get("du-elevate-declined"));
        }
        Outcome::Cancelled => cx.keep_rev(),
        Outcome::Failed(Fail::Exit(c)) if c == u32::from(elevated::exit::POLICY) => {
            cx.toast(urfin_core::Level::Warn, i18n.get("du-elevate-policy"));
        }
        Outcome::Failed(f) => {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("reason", fail_text(&f));
            cx.toast(urfin_core::Level::Error, i18n.fmt("du-elevate-failed", &a));
        }
    }
}

/// Причина неудачи досчёта для тоста и журнала задач.
fn fail_text(f: &Fail) -> String {
    match f {
        Fail::Exit(c) => text("du-elevate-exit", &[("code", &c.to_string())]),
        Fail::Io(e) | Fail::Result(e) => e.clone(),
    }
}

/// Итог досчёта для журнала (без путей и чисел).
fn outcome_kind(o: &Outcome) -> &'static str {
    match o {
        Outcome::Done(_) => "done",
        Outcome::Declined => "declined",
        Outcome::Cancelled => "cancelled",
        Outcome::Failed(_) => "failed",
    }
}

fn text(key: &str, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

/// Анализ активной вкладки `du:`.
fn active_snap(cx: &Cx<'_>) -> Result<SnapId, CmdError> {
    dvfs::parse(&cx.state.active_tab().loc)
        .map(|(id, _)| id)
        .ok_or_else(|| CmdError::Rejected(text("du-not-du", &[]).into()))
}

fn bad_arg(name: &str, msg: &str) -> CmdError {
    CmdError::Arg {
        name: name.into(),
        msg: msg.into(),
    }
}

/// Анализ, путь и вид активной вкладки `du:`.
fn active_view(cx: &Cx<'_>) -> Result<(SnapId, Vec<compact_str::CompactString>, View), CmdError> {
    dvfs::view(&cx.state.active_tab().loc)
        .ok_or_else(|| CmdError::Rejected(text("du-not-du", &[]).into()))
}

/// Вкладка — на вид `v` анализа `id`. Порядок: «По папкам», «Крупные», «Старые» — по
/// размеру ↓; «Типы», «Временные» и «Дубликаты» — как построены (группы и разделы).
fn go_view(cx: &mut Cx<'_>, id: SnapId, v: &View) {
    let loc = dvfs::location_view(id, v);
    cx.dispatch(Command::new("panel.go").arg("path", loc));
    cx.state
        .panel_mut(cx.state.active)
        .tab_mut()
        .sort
        .dirs_first = false;
    let sort = match v.filter {
        DuFilter::Types | DuFilter::Temp | DuFilter::Dups => {
            Command::new("panel.sort").arg("key", "unsorted")
        }
        _ => Command::new("panel.sort")
            .arg("key", "size")
            .arg("desc", true),
    };
    cx.dispatch(sort);
}

/// Корень диска пути (`C:\Dev\x` → `C:\`, `\\srv\sh\x` → `\\srv\sh\`).
fn drive_root(p: &Path) -> PathBuf {
    p.ancestors()
        .last()
        .map_or_else(|| p.to_path_buf(), Path::to_path_buf)
}

/// Новая вкладка анализа `id` в активной панели.
fn open_tab(cx: &mut Cx<'_>, env: &Env, id: SnapId) {
    if dvfs::parse(&cx.state.active_tab().loc).is_some_and(|(cur, _)| cur == id) {
        cx.keep_rev();
        return;
    }
    let loc = dvfs::location(id, Path::new(""));
    cx.dispatch(Command::new("tab.new"));
    cx.dispatch(Command::new("panel.go").arg("path", loc.clone()));
    env.reg.opened(loc);
}

/// Вкладка анализа — по размеру ↓, файлы и папки вперемешку (`screen-dulist`).
pub(crate) fn by_size(cx: &mut Cx<'_>, loc: &Location) {
    let t = cx.state.panel_mut(cx.state.active).tab_mut();
    if t.loc != *loc {
        return;
    }
    t.sort.dirs_first = false;
    cx.dispatch(
        Command::new("panel.sort")
            .arg("key", "size")
            .arg("desc", true),
    );
}

/// `panel.refresh` вкладок анализа `id`.
fn refresh_tabs(cx: &mut Cx<'_>, id: SnapId) {
    let tabs: Vec<i64> = cx
        .state
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter())
        .filter(|t| dvfs::parse(&t.loc).is_some_and(|(s, _)| s == id))
        .map(|t| i64::from(t.id.0))
        .collect();
    for tab in tabs {
        cx.dispatch(Command::new("panel.refresh").arg("tab", tab));
    }
}

/// Скан корня `root`: новый анализ (или пересчёт `again`). Тики — перечитывание вкладок,
/// конец — снимок в реестр и кэш.
fn start(cx: &mut Cx<'_>, env: &Arc<Env>, root: PathBuf, again: Option<SnapId>) -> SnapId {
    let cell: Arc<OnceLock<SnapId>> = Arc::new(OnceLock::new());
    let on: OnScan = {
        let (env, cell) = (Arc::clone(env), Arc::clone(&cell));
        Arc::new(move |cx, ev| {
            let Some(&id) = cell.get() else {
                return;
            };
            match ev {
                ScanEvent::Progress(_) => env.reg.touch(),
                ScanEvent::Done(snap) => {
                    let snap = Arc::try_unwrap(snap).unwrap_or_else(|a| (*a).clone());
                    store(cx, &env, snap.clone());
                    env.reg.finish(id, snap);
                }
            }
            refresh_tabs(cx, id);
        })
    };
    let title = text("du-job", &[("path", &root.to_string_lossy())]);
    let (job, tree) = super::scan::start(
        cx,
        Arc::clone(&env.fs),
        Location::local(&root),
        ScanCfg::from_du(&env.cfg),
        title,
        on,
    );
    let id = match again {
        Some(id) => {
            env.reg.restart(id, tree, job);
            id
        }
        None => env.reg.open_live(root, tree, job),
    };
    let _ = cell.set(id);
    id
}

/// Вопрос «Очистить временные?» (решение 2026-11-02 «du-filters» №3): сколько и
/// сколько освободится; что пересоздастся и что потеряется — по пояснениям правил,
/// с числом строк и размером.
fn clean_prompt(
    side: urfin_core::Side,
    picked: &[(u64, Option<super::Hint>)],
) -> urfin_core::PromptSpec {
    use super::filters::Effect;
    let i18n = urfin_i18n::current();
    let bytes: u64 = picked.iter().map(|(n, _)| n).sum();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", picked.len());
    a.set("size", urfin_i18n::fmt::bytes_with(&i18n, bytes));
    let mut body = i18n.fmt("du-clean-body", &a);
    // Пояснения по последствиям: сначала потери, потом то, что пересоздастся.
    let mut notes: Vec<(Effect, String, usize, u64)> = Vec::new();
    for (size, h) in picked {
        let Some(h) = h.as_ref().filter(|h| h.effect != Effect::Safe) else {
            continue;
        };
        match notes.iter_mut().find(|n| n.0 == h.effect && n.1 == h.after) {
            Some(n) => {
                n.2 += 1;
                n.3 += size;
            }
            None => notes.push((h.effect, h.after.clone(), 1, *size)),
        }
    }
    notes.sort_by(|a, b| b.0.cmp(&a.0).then(b.3.cmp(&a.3)));
    for (effect, after, count, size) in &notes {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("effect", i18n.get(effect.key()));
        a.set("after", after.clone());
        a.set("count", *count);
        a.set("size", urfin_i18n::fmt::bytes_with(&i18n, *size));
        body.push('\n');
        body.push_str(&i18n.fmt("du-clean-note", &a));
    }
    body.push('\n');
    body.push_str(&i18n.get("du-clean-busy"));
    let choice = |id: &str, key: &str, k: char| urfin_core::Choice {
        id: id.into(),
        label: i18n.get(key).into(),
        key: k,
    };
    urfin_core::PromptSpec {
        anchor: urfin_core::Anchor::Panel(side),
        title: i18n.get("du-clean-title"),
        body,
        choices: [
            choice("trash", "du-clean-trash", 't'),
            choice("cancel", "du-clean-cancel", 'c'),
        ]
        .into_iter()
        .collect(),
        default: 0,
        input: None,
        allow_all: false,
        detail: None,
        secret: false,
    }
}

/// Группа «Дубликатов» под курсором (F01-15): номер (`ctime` заголовка) и индексы
/// строк её копий в листинге — от заголовка над курсором до следующего.
fn cursor_group(t: &urfin_core::Tab) -> Option<(usize, Vec<u32>)> {
    use urfin_core::{EntryKind, Row};
    let head = (0..=t.cursor.row).rev().find_map(|r| match t.row(r)? {
        Row::Entry { entry, .. } if entry.kind == EntryKind::Other => Some(r),
        _ => None,
    })?;
    let Some(Row::Entry { entry, .. }) = t.row(head) else {
        return None;
    };
    let gi = usize::try_from(entry.ctime).ok()?;
    let rows = (head + 1..t.rows())
        .map_while(|r| match t.row(r)? {
            Row::Entry { idx, entry } if entry.kind != EntryKind::Other => Some(idx),
            _ => None,
        })
        .collect();
    Some((gi, rows))
}

/// Отметки после полного хэша (все копии, кроме первой): ставятся во вкладке вида
/// «Дубликаты», когда её листинг загружен; другая вкладка или вид — ждут.
fn apply_marks(cx: &mut Cx<'_>, reg: &DuRegistry) {
    for id in reg.pending_marks() {
        let target = [urfin_core::Side::Left, urfin_core::Side::Right]
            .into_iter()
            .find(|&side| {
                let t = cx.state.panel(side).tab();
                let ours = dvfs::view(&t.loc)
                    .is_some_and(|(s, _, v)| s == id && v.filter == DuFilter::Dups);
                ours && t.listing.status == ListingStatus::Complete
            });
        let Some(side) = target else {
            continue;
        };
        let names: rustc_hash::FxHashSet<compact_str::CompactString> =
            reg.take_marks(id).into_iter().collect();
        let t = cx.state.panel_mut(side).tab_mut();
        let l = t.listing.clone();
        t.selection.select_where(&l, true, |e| {
            !e.kind.is_dir_like() && names.contains(&e.name)
        });
        t.touch();
    }
}

/// Хэши дубликатов задачей `du.dups` (очередь устройства, как скан): `full` — полный
/// xxh3-128 непроверенных групп (Ctrl+H), иначе — частичный xxh3-64 первых 64 KiB
/// (один раз). Результат — в реестр, вкладки перечитываются; отмена (Esc) — без
/// изменений групп.
fn hash_dups(cx: &mut Cx<'_>, env: &Arc<Env>, id: SnapId, full: bool, only: Option<usize>) {
    if env.reg.dup_job(id).is_some() || env.reg.job(id).is_some() {
        return;
    }
    let title = text(
        if full {
            "du-dups-job-full"
        } else {
            "du-dups-job"
        },
        &[],
    );
    let job = cx.state.jobs.start(
        JobSpec {
            kind: "du.dups",
            title,
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "du",
        },
        cx.rt.sink(),
    );
    env.reg.set_dup_job(id, Some(job.id()));
    let env = Arc::clone(env);
    let (gate, jobs) = (cx.rt.devices(), DeviceJobs::from_config(&cx.state.config));
    cx.rt
        .spawn_dedicated("disk-usage-dups".into(), cx.rt.token(), move |tcx| {
            let done = || {
                let _ = tcx
                    .sink
                    .send(msg("du.dups", move |cx| refresh_tabs(cx, id)), &tcx.cancel);
            };
            let Some((root, plan)) = env.reg.dup_plan_in(id, full, only) else {
                env.reg.set_dup_job(id, None);
                job.finish(Ok(JobSummary::default()));
                return;
            };
            let p = job.progress();
            p.items_total
                .store(plan.len() as u64, std::sync::atomic::Ordering::Relaxed);
            let _lease =
                super::scan::device_slot(&gate, jobs, &job, &Location::local(&root), &tcx.cancel);
            let halted = || tcx.cancel.is_cancelled() || job.checkpoint().is_err();
            let mut hashes: FxHashMap<u32, u128> = FxHashMap::default();
            let mut errors = 0u64;
            for (i, rel) in &plan {
                if halted() {
                    break;
                }
                let path = dvfs::join_name(&root, rel);
                let h = if full {
                    super::dups::full_hash(&path, job.cancel_token(), &mut |n| {
                        p.bytes_done
                            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                    })
                } else {
                    super::dups::head_hash(&path).map(u128::from)
                };
                match h {
                    Ok(h) => {
                        hashes.insert(*i, h);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => break,
                    Err(e) => {
                        errors += 1;
                        tracing::debug!(target: "vfs.du", error = %e, "dup hash");
                    }
                }
                p.items_done
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let bytes = p.bytes_done.load(std::sync::atomic::Ordering::Relaxed);
            if halted() {
                env.reg.set_dup_job(id, None);
                job.finish(Err("cancelled".into()));
            } else {
                env.reg.dup_apply_in(id, &hashes, full, only);
                job.finish(Ok(JobSummary {
                    items: hashes.len() as u64,
                    bytes,
                    skipped: 0,
                    errors,
                }));
                // Перечитать вид: новые группы и уровни.
                done();
            }
        });
}

/// Открыт из кэша: дочитать изменения (USN-журнал или `mtime`, F01 §3.3) задачей
/// Runtime; итог — «●» или перелистанные каталоги, вкладки перечитываются.
fn recheck(cx: &mut Cx<'_>, env: &Arc<Env>, id: SnapId) {
    if !env.reg.watchable(id) {
        return;
    }
    let env = Arc::clone(env);
    let cfg = ScanCfg::from_du(&env.cfg);
    cx.rt
        .spawn(Lane::Bulk, "du.recheck", cx.rt.token(), move |tcx| {
            let halted = || tcx.cancel.is_cancelled();
            let t0 = Instant::now();
            let r = super::fresh::check(&env.reg, id, &*env.fs, &cfg, &halted);
            tracing::info!(target: "vfs.du", ms = t0.elapsed().as_millis(), ?r, "recheck");
            let _ = tcx.sink.send(
                msg("du.recheck", move |cx| refresh_tabs(cx, id)),
                &tcx.cancel,
            );
        });
}

/// Ctrl+R при «●»: перелистать изменённые каталоги задачей `du` (job-бар, Esc —
/// стоп); «●» больше половины снимка — полный скан.
fn relist_changed(cx: &mut Cx<'_>, env: &Arc<Env>, id: SnapId) {
    let Some(root) = env.reg.root(id) else {
        return;
    };
    let job = cx.state.jobs.start(
        JobSpec {
            kind: "du",
            title: text("du-job-changed", &[("path", &root.to_string_lossy())]),
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "du",
        },
        cx.rt.sink(),
    );
    let env = Arc::clone(env);
    let cfg = ScanCfg::from_du(&env.cfg);
    cx.rt
        .spawn_dedicated("disk-usage-relist".into(), cx.rt.token(), move |tcx| {
            let halted = || tcx.cancel.is_cancelled() || job.checkpoint().is_err();
            let done = super::fresh::relist_changed(&env.reg, id, &*env.fs, &cfg, &halted);
            let stopped = halted();
            let full = done.is_none() && !stopped;
            job.finish(if stopped {
                Err("cancelled".into())
            } else {
                Ok(JobSummary {
                    items: done.unwrap_or(0) as u64,
                    ..JobSummary::default()
                })
            });
            let _ = tcx.sink.send(
                msg("du.relist", move |cx| {
                    if full && env.reg.job(id).is_none() {
                        start(cx, &env, root, Some(id));
                    }
                    refresh_tabs(cx, id);
                }),
                &tcx.cancel,
            );
        });
}

/// Снимок — в кэш (`Lane::Bulk`).
fn store(cx: &mut Cx<'_>, env: &Arc<Env>, snap: DuSnapshot) {
    let Some(cache) = env.cache.clone() else {
        return;
    };
    cx.rt
        .spawn(Lane::Bulk, "du.cache_store", cx.rt.token(), move |_| {
            if let Err(e) = cache.store(&snap) {
                tracing::warn!(target: "vfs.du", error = %e, root = %snap.root.display(), "cache store");
            }
        });
}
