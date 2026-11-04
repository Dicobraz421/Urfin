//! Handler `pkg` (F02 §2.5): вкладка `pkg:` — открыть (`pkg.open`), фильтр
//! (`pkg.filter`, Ctrl+1/2), проверка (`pkg.refresh`, Ctrl+R). Работает в UI-потоке без
//! I/O: кэш, обнаружение и запросы менеджеров — задачи Runtime.
//!
//! Открытие показывает кэш `state/pkg/cache.json` без запуска процессов; менеджеры
//! спрашиваются только по Ctrl+R (и при первом открытии, когда кэша ещё нет). Каждый
//! менеджер — свой поток: установленные, затем обновления; ответ одного сразу виден,
//! ошибка одного не мешает остальным.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use urfin_core::{
    CancelToken, CmdError, Command, CommandMeta, Cx, Handler, Lane, Location, Side, msg,
};

use crate::api::{Detected, PackageManager, RunCx};
use crate::arp::ArpEntry;
use crate::book::{CACHE_FILE, Filter, PkgBook, PkgCache};
use crate::config::PkgConfig;
use crate::detect::{MANAGERS_FILE, Managers};
use crate::jobs::Gates;
use crate::schedule::{NoTasks, Tasks};
use crate::CheckBell;
use crate::vfs::{filter_of, location};

/// Откуда берутся менеджеры: система или подстановка в тестах.
pub trait PkgSource: Send + Sync + 'static {
    /// Найденные менеджеры, без процессов (F02 §2.2); `cached` — `managers.toml`
    /// (версии не перезапрашиваются).
    fn detect(&self, cached: &Managers) -> Vec<Detected>;
    /// Записи ARP для издателей.
    fn arp(&self) -> Vec<ArpEntry>;
    /// Адаптер найденного менеджера.
    fn adapter(&self, found: &Detected, cfg: &PkgConfig) -> Arc<dyn PackageManager>;
}

/// Менеджеры системы: PATH, переменные окружения, ARP из реестра.
#[derive(Debug, Default)]
pub struct SystemSource;

impl PkgSource for SystemSource {
    fn detect(&self, cached: &Managers) -> Vec<Detected> {
        let mut m = cached.clone();
        m.refresh(crate::detect::detect_all());
        m.managers
    }

    fn arp(&self) -> Vec<ArpEntry> {
        #[cfg(windows)]
        {
            crate::arp::Arp::load()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }

    #[cfg(windows)]
    fn adapter(&self, found: &Detected, cfg: &PkgConfig) -> Arc<dyn PackageManager> {
        use crate::api::Mgr;
        let d = Some(found.clone());
        match found.mgr {
            Mgr::Winget => Arc::new(crate::winget::WingetAdapter::new(cfg, d)),
            Mgr::Scoop => Arc::new(crate::scoop::ScoopAdapter::new(cfg, d, None)),
            Mgr::Choco => Arc::new(crate::choco::ChocoAdapter::new(d)),
        }
    }

    #[cfg(not(windows))]
    fn adapter(&self, found: &Detected, _cfg: &PkgConfig) -> Arc<dyn PackageManager> {
        Arc::new(Absent(found.mgr))
    }
}

/// Без менеджеров: `--smoke`, `--screens` (без каталога состояния) процессов не
/// запускают.
#[derive(Debug, Default)]
pub struct NoSource;

impl PkgSource for NoSource {
    fn detect(&self, _cached: &Managers) -> Vec<Detected> {
        Vec::new()
    }

    fn arp(&self) -> Vec<ArpEntry> {
        Vec::new()
    }

    fn adapter(&self, found: &Detected, cfg: &PkgConfig) -> Arc<dyn PackageManager> {
        SystemSource.adapter(found, cfg)
    }
}

/// Менеджер вне Windows: процессов нет.
#[cfg(not(windows))]
#[derive(Debug)]
struct Absent(crate::api::Mgr);

#[cfg(not(windows))]
impl PackageManager for Absent {
    fn mgr(&self) -> crate::api::Mgr {
        self.0
    }
    fn detect(&self) -> Option<Detected> {
        None
    }
    fn version(&self, _cx: &RunCx) -> Result<compact_str::CompactString, crate::PkgError> {
        Err(crate::PkgError::NotFound)
    }
    fn list_installed(&self, _cx: &RunCx) -> Result<Vec<crate::Package>, crate::PkgError> {
        Err(crate::PkgError::NotFound)
    }
    fn list_updates(&self, _cx: &RunCx) -> Result<Vec<crate::Update>, crate::PkgError> {
        Err(crate::PkgError::NotFound)
    }
    fn search(&self, _q: &str, _cx: &RunCx) -> Result<Vec<crate::Hit>, crate::PkgError> {
        Err(crate::PkgError::NotFound)
    }
    fn action(
        &self,
        _op: crate::PkgOp,
        _id: &crate::PkgId,
        _cx: &RunCx,
    ) -> Result<crate::ActionPlan, crate::PkgError> {
        Err(crate::PkgError::NotFound)
    }
}

/// Кэш вкладки: ещё не читали, читается, прочитан.
const CACHE_NONE: u8 = 0;
const CACHE_LOADING: u8 = 1;
const CACHE_READY: u8 = 2;

/// Общее для handler, задач и сообщений.
struct Env {
    book: Arc<PkgBook>,
    source: Arc<dyn PkgSource>,
    config: PkgConfig,
    /// `state_dir/pkg`; `None` — без кэша (`--smoke`, `--screens`, тесты).
    dir: Option<PathBuf>,
    cache: AtomicU8,
    /// Менеджеров, ещё не ответивших в текущей проверке.
    left: AtomicUsize,
    /// Только что открытые вкладки: порядок строк модели (без сортировки панели)
    /// ставится в следующем pump, когда `tab.new` и `panel.go` уже выполнились.
    opened: Mutex<Vec<Location>>,
    /// Отмена процессов текущего поиска: новый запрос отменяет прежний.
    search: Mutex<CancelToken>,
    /// Очереди действий по менеджерам.
    gates: Gates,
    /// Планировщик заданий (`pkg.schedule`).
    tasks: Arc<dyn Tasks>,
    /// Звонок от `--pkg-check`: итог проверки по расписанию готов.
    bell: Arc<CheckBell>,
    /// Идёт `pkg.check_now`.
    checking: AtomicBool,
}

/// Handler `pkg`.
pub struct PkgHandler {
    env: Arc<Env>,
    installed: bool,
    /// Чтение итога прошлой проверки при старте.
    startup: check::Startup,
}

impl std::fmt::Debug for PkgHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkgHandler")
            .field("config", &self.env.config)
            .field("dir", &self.env.dir)
            .finish_non_exhaustive()
    }
}

impl PkgHandler {
    /// Менеджеры системы, своя модель, без кэша.
    pub fn new(config: PkgConfig) -> PkgHandler {
        let book = PkgBook::new(&config);
        PkgHandler::with(config, book, Arc::new(SystemSource), None)
    }

    /// `book` — общая с backend `pkg:` ([`crate::PkgVfs`]); `dir` — `state_dir/pkg`.
    pub fn with(
        config: PkgConfig,
        book: Arc<PkgBook>,
        source: Arc<dyn PkgSource>,
        dir: Option<PathBuf>,
    ) -> PkgHandler {
        PkgHandler::with_tasks(
            config,
            book,
            source,
            dir,
            Arc::new(NoTasks),
            Arc::default(),
        )
    }

    /// Как [`PkgHandler::with`], с Планировщиком `tasks` и звонком `bell` от
    /// `--pkg-check` (IPC единственного экземпляра).
    pub fn with_tasks(
        config: PkgConfig,
        book: Arc<PkgBook>,
        source: Arc<dyn PkgSource>,
        dir: Option<PathBuf>,
        tasks: Arc<dyn Tasks>,
        bell: Arc<CheckBell>,
    ) -> PkgHandler {
        book.configure(&config);
        PkgHandler {
            env: Arc::new(Env {
                book,
                source,
                config,
                dir,
                cache: AtomicU8::new(CACHE_NONE),
                left: AtomicUsize::new(0),
                opened: Mutex::new(Vec::new()),
                search: Mutex::new(CancelToken::new()),
                gates: Gates::default(),
                tasks,
                bell,
                checking: AtomicBool::new(false),
            }),
            installed: false,
            startup: check::Startup::default(),
        }
    }

    /// Фильтр из аргумента.
    fn filter_arg(cmd: &Command, name: &str) -> Result<Option<Filter>, CmdError> {
        match cmd.args.str(name) {
            None => Ok(None),
            Some(s) => Filter::parse(s).map(Some).ok_or_else(|| CmdError::Arg {
                name: name.into(),
                msg: "installed | updates | search".into(),
            }),
        }
    }
}

impl Handler for PkgHandler {
    fn namespace(&self) -> &'static str {
        "pkg"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &urfin_core::pkg::COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "pkg.open" => {
                let f = Self::filter_arg(cmd, "filter")?.unwrap_or(Filter::Installed);
                if f == Filter::Search {
                    open_search(cx, &self.env);
                } else {
                    open(cx, &self.env, f);
                }
                Ok(())
            }
            "pkg.filter" => {
                let f = Self::filter_arg(cmd, "name")?.unwrap_or(Filter::Installed);
                if f == Filter::Search {
                    open_search(cx, &self.env);
                } else if filter_of(&cx.state.active_tab().loc).is_some() {
                    go(cx, &self.env, f);
                } else {
                    open(cx, &self.env, f);
                }
                Ok(())
            }
            "pkg.search" => search_cmd(cmd, cx, &self.env),
            "pkg.search_in" => {
                cx.ui(urfin_core::UiEvent::Ext {
                    id: urfin_core::CommandId::from_static("pkg.search_in"),
                    args: urfin_core::Args::default(),
                });
                Ok(())
            }
            "pkg.pick" => {
                pick(cx, &self.env);
                Ok(())
            }
            "pkg.install" | "pkg.upgrade" | "pkg.upgrade_all" | "pkg.uninstall" => {
                action_cmd(cmd, cx, &self.env);
                Ok(())
            }
            "pkg.check_now" => {
                check::check_now(cx, &self.env);
                Ok(())
            }
            "pkg.schedule" => check::schedule_cmd(cmd, cx, &self.env),
            "pkg.banner.show" => {
                check::close(cx, &self.env);
                open(cx, &self.env, Filter::Updates);
                Ok(())
            }
            "pkg.banner.later" => {
                check::close(cx, &self.env);
                Ok(())
            }
            "pkg.refresh" => {
                if self.env.book.info().busy() {
                    cx.keep_rev();
                } else {
                    refresh(cx, &self.env);
                }
                Ok(())
            }
            _ => Err(crate::not_yet(cmd)),
        }
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if !self.installed {
            self.installed = true;
            cx.state.ext.insert(urfin_core::pkg::PkgModel {
                order: self.env.config.order.clone(),
                feed: Arc::clone(self.env.book.feed()),
                fixture: Arc::clone(&self.env.book) as Arc<dyn urfin_core::pkg::PkgFixture>,
                notice: None,
                task: urfin_core::pkg::Task::Unknown,
            });
            cx.state.ext.insert(crate::PkgState {
                config: self.env.config.clone(),
                book: Arc::clone(&self.env.book),
            });
        }
        let opened = std::mem::take(
            &mut *self
                .env
                .opened
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for loc in opened {
            unsorted(cx, &loc);
        }
        // Вкладка, открытая адресом `pkg:/…` (не `pkg.open`): кэш — без запросов к
        // менеджерам (F02 §2.5).
        if self.env.cache.load(Ordering::Acquire) == CACHE_NONE && any_tab(cx) {
            load(cx, &self.env, None);
        }
        check::pump(cx, &self.env, &mut self.startup);
    }
}

/// Есть вкладка `pkg:` в какой-нибудь панели.
fn any_tab(cx: &Cx<'_>) -> bool {
    cx.state
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter())
        .any(|t| filter_of(&t.loc).is_some())
}

/// Открыть вкладку с фильтром `f`: кэш прочитан — сразу, иначе — после чтения кэша (нет
/// кэша — ещё и проверка).
fn open(cx: &mut Cx<'_>, env: &Arc<Env>, f: Filter) {
    match env.cache.load(Ordering::Acquire) {
        CACHE_NONE => load(cx, env, Some(f)),
        _ => open_tab(cx, env, f),
    }
}

/// Кэш — в модель (задача Interactive), затем вкладка `then` и, без кэша, проверка.
fn load(cx: &mut Cx<'_>, env: &Arc<Env>, then: Option<Filter>) {
    env.cache.store(CACHE_LOADING, Ordering::Release);
    let Some(dir) = env.dir.clone() else {
        env.book.load(None);
        env.cache.store(CACHE_READY, Ordering::Release);
        if let Some(f) = then {
            open_tab(cx, env, f);
        }
        return;
    };
    let env = Arc::clone(env);
    cx.rt
        .spawn(Lane::Interactive, "pkg.cache_load", cx.rt.token(), move |tcx| {
            let t0 = std::time::Instant::now();
            let cache = PkgCache::load(&dir.join(CACHE_FILE));
            let hit = cache.is_some();
            env.book.load(cache);
            tracing::info!(name: "pkg.cache_load", target: "pkg", ms = t0.elapsed().as_millis(), hit);
            let m = msg("pkg.cache_load", move |cx| {
                env.cache.store(CACHE_READY, Ordering::Release);
                refresh_tabs(cx);
                if let Some(f) = then {
                    open_tab(cx, &env, f);
                    if !hit {
                        refresh(cx, &env);
                    }
                }
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Вкладка `pkg:` — активировать имеющуюся (активная панель, затем другая) или открыть
/// новую в активной панели; фильтр — `f`.
fn open_tab(cx: &mut Cx<'_>, env: &Env, f: Filter) {
    let active = cx.state.active;
    let found = [active, active.other()].into_iter().find_map(|side| {
        cx.state
            .panel(side)
            .tabs
            .iter()
            .position(|t| filter_of(&t.loc).is_some())
            .map(|i| (side, i))
    });
    let name = |s: Side| if s == Side::Left { "left" } else { "right" };
    match found {
        Some((side, i)) => {
            if side != active {
                cx.dispatch(Command::new("panel.focus").arg("side", name(side)));
            }
            if cx.state.panel(side).active != i {
                cx.dispatch(
                    Command::new("tab.goto")
                        .arg("n", i64::try_from(i + 1).unwrap_or(i64::MAX))
                        .arg("side", name(side)),
                );
            }
            if filter_of(&cx.state.panel(side).tabs[i].loc) == Some(f) {
                return;
            }
        }
        None => cx.dispatch(Command::new("tab.new")),
    }
    go(cx, env, f);
}

/// Активная вкладка `pkg:` — на фильтр `f`.
fn go(cx: &mut Cx<'_>, env: &Env, f: Filter) {
    let loc = location(f);
    cx.dispatch(Command::new("panel.go").arg("path", loc.clone()));
    env.opened
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(loc);
}

/// Порядок строк модели («важные» сверху), без сортировки панели.
fn unsorted(cx: &mut Cx<'_>, loc: &Location) {
    let t = cx.state.panel_mut(cx.state.active).tab_mut();
    if t.loc != *loc {
        return;
    }
    t.sort.dirs_first = false;
    cx.dispatch(
        Command::new("panel.sort")
            .arg("key", "unsorted")
            .arg("desc", false),
    );
}

/// `panel.refresh` всех вкладок `pkg:`.
fn refresh_tabs(cx: &mut Cx<'_>) {
    let tabs: Vec<i64> = cx
        .state
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter())
        .filter(|t| filter_of(&t.loc).is_some())
        .map(|t| i64::from(t.id.0))
        .collect();
    for tab in tabs {
        cx.dispatch(Command::new("panel.refresh").arg("tab", tab));
    }
}

/// Проверка (Ctrl+R): обнаружение и ARP (Interactive), затем поток на менеджер.
fn refresh(cx: &mut Cx<'_>, env: &Arc<Env>) {
    env.book.start();
    env.cache.store(CACHE_READY, Ordering::Release);
    let env = Arc::clone(env);
    cx.rt
        .spawn(Lane::Interactive, "pkg.detect", cx.rt.token(), move |tcx| {
            let mut cached = Managers {
                managers: env.book.detected(),
            };
            if cached.managers.is_empty()
                && let Some(dir) = &env.dir
            {
                cached = Managers::load(&dir.join(MANAGERS_FILE)).unwrap_or_default();
            }
            let found = env.source.detect(&cached);
            env.book.found(found.clone(), env.source.arp());
            tracing::info!(target: "pkg", managers = found.len(), "pkg: detected");
            let m = msg("pkg.detected", move |cx| {
                refresh_tabs(cx);
                env.left.store(found.len(), Ordering::Release);
                if found.is_empty() {
                    env.book.finish(Timestamp::now());
                    store(cx, &env);
                }
                for d in &found {
                    query(cx, &env, d);
                }
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Запросы менеджера `found` в своём потоке: установленные (сразу в модель), затем
/// обновления. Последний ответивший заканчивает проверку и пишет кэш.
fn query(cx: &mut Cx<'_>, env: &Arc<Env>, found: &Detected) {
    let adapter = env.source.adapter(found, &env.config);
    let env = Arc::clone(env);
    let mgr = found.mgr;
    let _ = cx
        .rt
        .spawn_dedicated(format!("pkg.query.{mgr}"), cx.rt.token(), move |tcx| {
            let t = env.config.timeouts;
            let rc = |s: u32| RunCx {
                cancel: tcx.cancel.clone(),
                timeout: Some(Duration::from_secs(u64::from(s))),
                lines: None,
            };
            let res = adapter.list_installed(&rc(t.list_s)).and_then(|list| {
                env.book.set_installed(mgr, list);
                let _ = tcx
                    .sink
                    .send(msg("pkg.installed", refresh_tabs), &tcx.cancel);
                adapter.list_updates(&rc(t.updates_s))
            });
            match res {
                Ok(ups) => env.book.set_updates(mgr, ups, adapter.detect()),
                Err(e) => {
                    tracing::warn!(target: "pkg", %mgr, error = %e, "pkg: query");
                    env.book.failed(mgr, e.to_string().into());
                }
            }
            let last = env.left.fetch_sub(1, Ordering::AcqRel) == 1;
            if last {
                env.book.finish(Timestamp::now());
            }
            let m = msg("pkg.answer", move |cx| {
                refresh_tabs(cx);
                if last {
                    store(cx, &env);
                }
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Кэш и `managers.toml` — на диск (`Lane::Bulk`).
fn store(cx: &mut Cx<'_>, env: &Arc<Env>) {
    let Some(dir) = env.dir.clone() else {
        return;
    };
    let book = Arc::clone(&env.book);
    cx.rt
        .spawn(Lane::Bulk, "pkg.cache_store", cx.rt.token(), move |_| {
            if let Err(e) = book.cache().save(&dir.join(CACHE_FILE)) {
                tracing::warn!(target: "pkg", error = %e, "pkg: cache store");
            }
            let m = Managers {
                managers: book.detected(),
            };
            if let Err(e) = m.save(&dir.join(MANAGERS_FILE)) {
                tracing::warn!(target: "pkg", error = %e, "pkg: managers store");
            }
        });
}

#[path = "handler_act.rs"]
mod act;
#[path = "handler_check.rs"]
mod check;
use act::{action_cmd, open_search, pick, search_cmd};

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
