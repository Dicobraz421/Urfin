//! Handler `du` (F01 §2.4): команды анализа места. Работает в UI-потоке, без I/O;
//! скан, чтение и запись кэша — задачами Runtime.
//!
//! `du.open` — анализ каталога активной панели в новой вкладке `du:` (из кэша — сразу,
//! с датой подсчёта; без кэша — скан); `du.refresh` — пересчёт; `du.stop` (Esc) —
//! остановка, посчитанное остаётся «≥ X». Фильтры, карта, дубликаты и досчёт с
//! правами — следующие задачи F01 (пока отказ).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use urfin_core::{
    ArgSpec, CmdError, Command, CommandMeta, Cx, Handler, Lane, Location, ValueKind, msg,
};

use super::DuCfg;
use super::cache::DuCache;
use super::dvfs::{self, DuRegistry, DuState, SnapId};
use super::model::DuSnapshot;
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

static COMMANDS: [CommandMeta; 8] = [
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
    // `kind`: dirs | large | types | old | dups | temp.
    meta(
        "du.filter",
        "Disk usage filter",
        &[arg("kind", ValueKind::Str, true)],
    ),
    meta("du.map", "Disk usage map", &[]),
    meta("du.verify_hash", "Verify duplicates by hash", &[]),
    // Выключен до принятия ADR-0021 (S0 v6 №10).
    meta("du.elevate", "Count as administrator", &[]),
    meta(
        "du.old_age",
        "Old files age",
        &[arg("age", ValueKind::Str, true)],
    ),
];

/// Общее для задач и сообщений handler.
struct Env {
    reg: Arc<DuRegistry>,
    cfg: DuCfg,
    cache: Option<DuCache>,
    /// Backend обхода (локальная ФС).
    fs: Arc<dyn Vfs>,
}

/// Handler `du`.
pub struct DuHandler {
    env: Arc<Env>,
    stored: Option<Instant>,
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
        DuHandler {
            env: Arc::new(Env {
                reg,
                cfg,
                cache,
                fs: Arc::new(crate::LocalFs::new()),
            }),
            stored: None,
        }
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
                            Some(s) => env.reg.open_snapshot(s, true),
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

    /// Пересчёт анализа активной вкладки.
    fn refresh(&self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = active_snap(cx)?;
        if self.env.reg.job(id).is_some() {
            cx.keep_rev();
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

    fn stop(&self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = active_snap(cx)?;
        match self.env.reg.job(id) {
            Some(job) => cx.state.jobs.cancel(job),
            None => cx.keep_rev(),
        }
        Ok(())
    }

    /// Изменённые после скана снимки (вычтено удалённое) — в кэш, не чаще раза в 30 с.
    fn store_dirty(&mut self, cx: &mut Cx<'_>) {
        if self.env.cache.is_none() || self.stored.is_some_and(|t| t.elapsed() < STORE_EVERY) {
            return;
        }
        let dirty = self.env.reg.dirty();
        if dirty.is_empty() {
            return;
        }
        self.stored = Some(Instant::now());
        for id in dirty {
            if let Some(snap) = self.env.reg.take_dirty(id) {
                store(cx, &self.env, snap);
            }
        }
    }
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
            "du.refresh" => self.refresh(cx),
            "du.stop" => self.stop(cx),
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
            });
        }
        for loc in self.env.reg.take_opened() {
            by_size(cx, &loc);
        }
        self.store_dirty(cx);
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
