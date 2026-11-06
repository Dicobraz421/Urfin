//! Handler `search` (S8 §2.5). Живёт в UI-потоке: файлы читают job и фоновые задачи, модель
//! экрана — `SearchModel` в `AppState.ext`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use compact_str::{CompactString, ToCompactString};
use rustc_hash::FxHashMap;
use urfin_core::search::{Hit, PastQuery, SearchId, SearchModel};
use urfin_core::{
    ArgSpec, Args, CancelToken, CmdError, Command, CommandMeta, Cx, Handler, JobId, Lane, Level,
    Location, UiEvent, ValueKind, msg,
};
use urfin_vfs::VfsRegistry;
use urfin_vfs::name::join_os;
use urfin_vfs::walk::child;

use crate::job::{self, Spawn};
use crate::query::SearchQuery;
use crate::{SearchCfg, content, history};

/// Строк на страницу: окна экрана handler не знает, у команды только аргумент `to`.
const PAGE: i64 = 20;

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
        category: "search",
        args,
        repeatable: false,
    }
}

const ID: [ArgSpec; 1] = [arg("id", ValueKind::Int, false)];

/// Аргументы `search.run`: первые восемь — поля формы (§2.5), остальные — вопрос В3.
const RUN: [ArgSpec; 20] = [
    arg("name", ValueKind::Str, false),
    arg("text", ValueKind::Str, false),
    arg("where", ValueKind::List, false),
    arg("case", ValueKind::Bool, false),
    arg("regex", ValueKind::Bool, false),
    arg("recursive", ValueKind::Bool, false),
    arg("kinds", ValueKind::Str, false),
    arg("target", ValueKind::Str, false),
    arg("hex", ValueKind::Bool, false),
    arg("word", ValueKind::Bool, false),
    arg("encoding", ValueKind::Str, false),
    arg("binary", ValueKind::Bool, false),
    arg("invert", ValueKind::Bool, false),
    arg("hidden", ValueKind::Bool, false),
    arg("skip_offline", ValueKind::Bool, false),
    arg("size_min", ValueKind::Int, false),
    arg("size_max", ValueKind::Int, false),
    arg("date_from", ValueKind::Str, false),
    arg("date_to", ValueKind::Str, false),
    arg("depth", ValueKind::Int, false),
];

/// Команды §2.5.
pub static COMMANDS: [CommandMeta; 12] = [
    meta("search.files", "Find files", &[]),
    meta("search.dirs", "Find folders", &[]),
    meta("search.run", "Start search", &RUN),
    meta("search.to_panel", "Results to panel", &[]),
    CommandMeta {
        repeatable: true,
        ..meta(
            "search.cursor",
            "Move cursor in results",
            &[arg("to", ValueKind::Str, true)],
        )
    },
    meta("search.view", "View file", &[]),
    meta("search.edit", "Edit file", &[]),
    meta("search.close", "Close search", &[]),
    meta("search.cancel", "Stop search", &ID),
    meta("search.again", "Repeat search", &ID),
    meta("search.goto", "Go to file", &[]),
    meta(
        "search.errors",
        "Folders without access",
        &[arg("id", ValueKind::Int, true)],
    ),
];

/// Запущенный поиск: чем отменить, повторить и показать.
struct Run {
    job: JobId,
    /// Аргументы `search.run`: для `search.again`.
    args: Args,
    past: PastQuery,
    /// Стор виден в панели: новый поиск на экране его не убирает.
    in_panel: bool,
    /// Недоступные каталоги: читает `search:/id?f=errors` (S8-10).
    denied: Arc<Mutex<Vec<Location>>>,
}

/// Handler `search`.
pub struct SearchHandler {
    reg: Arc<VfsRegistry>,
    /// Каталог истории; `None` — без диска.
    dir: Option<PathBuf>,
    runs: FxHashMap<SearchId, Run>,
    installed: bool,
    /// Номер последнего снимка истории: старый снимок не пишется поверх свежего.
    saved: Arc<Mutex<u64>>,
    seq: Arc<AtomicU64>,
}

impl std::fmt::Debug for SearchHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchHandler")
            .field("runs", &self.runs.len())
            .finish_non_exhaustive()
    }
}

fn model<'a>(cx: &'a mut Cx<'_>) -> &'a mut SearchModel {
    if cx.state.ext.get::<SearchModel>().is_none() {
        cx.state.ext.insert(SearchModel::default());
    }
    cx.state.ext.get_mut().expect("model is inserted above")
}

/// Открыть или закрыть экран: это дело фичи окна (S8-07).
fn screen(cx: &mut Cx<'_>, args: Args) {
    let id = Command::new("search.screen").id;
    cx.ui(UiEvent::Ext { id, args });
}

fn close_screen(cx: &mut Cx<'_>) {
    screen(cx, Args::default().with("close", true));
}

/// Адрес панели `search:/id`.
fn panel_loc(id: SearchId) -> Location {
    Location {
        backend: "search".into(),
        authority: None,
        path: format!("/{}", id.0).into(),
        outer: None,
    }
}

/// Номер поиска панели на `search:`.
fn panel_id(loc: &Location) -> Option<SearchId> {
    if loc.backend != "search" {
        return None;
    }
    let s = loc.path.to_str()?;
    let n = s.trim_start_matches(['/', '\\']);
    n.parse().ok().map(SearchId)
}

/// Часть поля «В»: путь с `;` — в кавычках.
fn place_of(loc: &Location) -> String {
    let s = if loc.is_local() {
        loc.path.display().to_string()
    } else {
        loc.to_uri()
    };
    if s.contains(';') {
        format!("\"{s}\"")
    } else {
        s
    }
}

/// Поле «В»: отмеченные каталоги панели, иначе её папка.
fn panel_place(cx: &Cx<'_>) -> CompactString {
    let tab = cx.state.active_tab();
    let marked: Vec<String> = tab
        .targets()
        .filter(|t| t.from_selection)
        .map(|t| {
            t.items
                .iter()
                .filter(|e| e.kind.is_dir_like())
                .map(|e| {
                    place_of(&Location {
                        path: join_os(&t.base, e),
                        ..t.base.clone()
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if marked.is_empty() {
        place_of(&tab.loc).into()
    } else {
        marked.join(";").into()
    }
}

impl SearchHandler {
    pub fn new(reg: Arc<VfsRegistry>, dir: Option<PathBuf>) -> SearchHandler {
        SearchHandler {
            reg,
            dir,
            runs: FxHashMap::default(),
            installed: false,
            saved: Arc::default(),
            seq: Arc::default(),
        }
    }

    /// Первый pump: модель в `ext`, чтение истории — фоном.
    fn install(&mut self, cx: &mut Cx<'_>) {
        if std::mem::replace(&mut self.installed, true) {
            return;
        }
        content::mark_ui_thread();
        model(cx);
        cx.state.touch();
        let Some(dir) = self.dir.clone() else {
            return;
        };
        cx.rt
            .spawn(Lane::Bulk, "search.history", cx.rt.token(), move |tcx| {
                let list = history::load(&dir);
                if list.is_empty() {
                    return;
                }
                let done = msg("search.history", move |cx| {
                    let m = model(cx);
                    for q in list.into_iter().rev() {
                        m.remember(q);
                    }
                });
                let _ = tcx.sink.send(done, &tcx.cancel);
            });
    }

    /// Снимок истории — на диск фоном; из двух снимков выживает свежий.
    fn save_history(&self, cx: &mut Cx<'_>) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let list: Vec<PastQuery> = model(cx).history().iter().cloned().collect();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let saved = Arc::clone(&self.saved);
        // Свой токен: запись идёт и при выходе, runtime дождётся её.
        cx.rt.spawn(
            Lane::Bulk,
            "search.history",
            CancelToken::new(),
            move |_| {
                let mut newest = saved.lock().unwrap_or_else(PoisonError::into_inner);
                if seq > *newest {
                    *newest = seq;
                    if let Err(e) = history::save(&dir, list) {
                        tracing::warn!(target: "search", error = %e, "history not saved");
                    }
                }
            },
        );
    }

    /// Поиск команды: `id`, иначе панель `search:`, иначе экран.
    fn target(cmd: &Command, cx: &mut Cx<'_>) -> Option<SearchId> {
        cmd.args
            .int("id")
            .and_then(|v| u32::try_from(v).ok())
            .map(SearchId)
            .or_else(|| panel_id(&cx.state.active_tab().loc))
            .or_else(|| model(cx).current().map(|s| s.id))
    }

    fn cancel(&self, cx: &mut Cx<'_>, id: SearchId) {
        if let Some(run) = self.runs.get(&id)
            && cx
                .state
                .jobs
                .get(run.job)
                .is_some_and(|j| j.state.is_active())
        {
            cx.state.jobs.cancel(run.job);
        }
    }

    /// Экран освобождается: поиск отменён, стор убран, если он не показан в панели.
    fn drop_current(&mut self, cx: &mut Cx<'_>) {
        let Some(id) = model(cx).current().map(|s| s.id) else {
            return;
        };
        self.cancel(cx, id);
        model(cx).show(None);
        if !self.runs.get(&id).is_some_and(|r| r.in_panel) {
            self.runs.remove(&id);
            model(cx).remove(id);
        }
    }

    /// Форма экрана: `search.files`, `search.dirs`.
    fn open_form(&mut self, cx: &mut Cx<'_>, kinds: &'static str) {
        let panel = panel_id(&cx.state.active_tab().loc);
        let form = panel
            .and_then(|id| self.runs.get(&id))
            .map(|r| r.past.clone());
        let in_panel = form.is_some();
        let last = model(cx).history().front().cloned();
        let form = form.or(last).unwrap_or(PastQuery {
            recursive: true,
            ..PastQuery::default()
        });
        let place = if in_panel {
            form.place.clone()
        } else {
            panel_place(cx)
        };
        let args = Args::default()
            .with("name", form.name)
            .with("text", form.text)
            .with("where", place)
            .with("case", form.case)
            .with("regex", form.regex)
            .with("recursive", form.recursive)
            .with("kinds", kinds);
        screen(cx, args);
    }

    fn start(&mut self, cmd: &Command, cx: &mut Cx<'_>) {
        let (cfg, warning) = cx.state.config.section::<SearchCfg>("search");
        if let Some(w) = warning {
            tracing::warn!(target: "search", warning = ?w, "[search] ignored");
        }
        let base = Self::base(cx);
        let query = match SearchQuery::from_args(&cmd.args, &cfg, &base) {
            Ok(q) => q,
            Err(e) => {
                cx.toast(Level::Warn, job::query_error(&e));
                return;
            }
        };
        // Второй запуск отменяет первый на том же экране.
        self.drop_current(cx);
        let a = &cmd.args;
        let past = PastQuery {
            name: a.str("name").unwrap_or_default().into(),
            text: a.str("text").unwrap_or_default().into(),
            place: query
                .roots
                .iter()
                .map(place_of)
                .collect::<Vec<_>>()
                .join(";")
                .into(),
            case: a.bool("case").unwrap_or(false),
            regex: a.bool("regex").unwrap_or(false),
            recursive: query.depth.is_none(),
        };
        let m = model(cx);
        m.remember(past.clone());
        let store = m.open(query.roots.clone());
        m.show(Some(store.id));
        self.save_history(cx);

        let id = store.id;
        let denied = Arc::<Mutex<Vec<Location>>>::default();
        let spawn = Spawn {
            reg: Arc::clone(&self.reg),
            query,
            cfg,
            store,
            denied: Arc::clone(&denied),
        };
        let job = job::start(cx, spawn);
        let run = Run {
            job,
            args: cmd.args.clone(),
            past,
            in_panel: false,
            denied,
        };
        self.runs.insert(id, run);
        if cmd.args.str("target") == Some("panel") {
            self.show_in_panel(cx, id);
        }
    }

    /// Корень по умолчанию: папка панели, у панели `search:` — первый корень её поиска.
    fn base(cx: &mut Cx<'_>) -> Location {
        let loc = cx.state.active_tab().loc.clone();
        panel_id(&loc)
            .and_then(|id| model(cx).get(id).and_then(|s| s.roots.first().cloned()))
            .unwrap_or(loc)
    }

    /// Стор — в новую вкладку; поиск продолжается.
    fn show_in_panel(&mut self, cx: &mut Cx<'_>, id: SearchId) {
        if let Some(run) = self.runs.get_mut(&id) {
            run.in_panel = true;
        }
        model(cx).show(None);
        cx.dispatch(Command::new("tab.new"));
        cx.dispatch(Command::new("panel.go").arg("path", panel_loc(id)));
        close_screen(cx);
    }

    /// Строка под курсором экрана и её корень.
    fn current_hit(cx: &mut Cx<'_>) -> Option<(Hit, Location)> {
        let m = model(cx);
        let row = m.cursor() as usize;
        let store = m.current()?;
        let hit = store.rows(row..row + 1).iter().next().cloned()?;
        let root = store.roots.get(usize::from(hit.root))?.clone();
        Some((hit, root))
    }

    /// F3, F4: файл на строке совпадения.
    fn open_file(cx: &mut Cx<'_>, id: &'static str) {
        let Some((hit, root)) = Self::current_hit(cx) else {
            return;
        };
        if hit.e.kind.is_dir_like() {
            return;
        }
        let mut c = Command::new(id).arg("path", child(&root, &hit.e));
        if hit.line > 0 {
            c = c.arg("line", hit.line);
        }
        cx.dispatch(c);
    }

    /// Панель — в каталог файла, курсор — на имя; каталог открывается сам.
    fn goto(cx: &mut Cx<'_>) {
        let Some((hit, root)) = Self::current_hit(cx) else {
            return;
        };
        let loc = child(&root, &hit.e);
        if hit.e.kind.is_dir_like() {
            cx.dispatch(Command::new("panel.go").arg("path", loc));
        } else if let Some(dir) = loc.parent() {
            let name = hit.e.name.rsplit(['\\', '/']).next().unwrap_or_default();
            let name = name.to_compact_string();
            cx.dispatch(Command::new("panel.go").arg("path", dir));
            cx.dispatch(Command::new("panel.cursor.to").arg("name", name));
        }
        close_screen(cx);
    }

    fn cursor(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let to = cmd.args.str("to").unwrap_or_default();
        // Стрелки привязаны все четыре, а список вертикальный.
        if matches!(to, "left" | "right") {
            cx.keep_rev();
            return Ok(());
        }
        let m = model(cx);
        let len = m.current().map_or(0, |s| s.rows_len());
        let len = i64::try_from(len).unwrap_or(i64::MAX);
        let cur = i64::from(m.cursor());
        let next = match to {
            "up" => cur - 1,
            "down" => cur + 1,
            "page_up" => cur - PAGE,
            "page_down" => cur + PAGE,
            "home" => 0,
            "end" => len - 1,
            other => {
                return Err(CmdError::Arg {
                    name: "to".into(),
                    msg: format!("unknown: {other}").into(),
                });
            }
        };
        m.set_cursor(u32::try_from(next.clamp(0, (len - 1).max(0))).unwrap_or(0));
        Ok(())
    }
}

impl Handler for SearchHandler {
    fn namespace(&self) -> &'static str {
        "search"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        self.install(cx);
        match cmd.id.as_str() {
            "search.files" => self.open_form(cx, "files"),
            "search.dirs" => self.open_form(cx, "dirs"),
            "search.run" => self.start(cmd, cx),
            "search.to_panel" => {
                if let Some(id) = model(cx).current().map(|s| s.id) {
                    self.show_in_panel(cx, id);
                }
            }
            "search.cursor" => return Self::cursor(cmd, cx),
            "search.view" => Self::open_file(cx, "viewer.open"),
            "search.edit" => Self::open_file(cx, "editor.open"),
            "search.close" => {
                self.drop_current(cx);
                close_screen(cx);
            }
            "search.cancel" => {
                if let Some(id) = Self::target(cmd, cx) {
                    self.cancel(cx, id);
                }
            }
            "search.again" => {
                let run = Self::target(cmd, cx).and_then(|id| self.runs.get(&id));
                if let Some(run) = run {
                    let mut c = Command::new("search.run");
                    c.args = run.args.clone();
                    cx.dispatch(c);
                }
            }
            "search.goto" => Self::goto(cx),
            // Список `search:/id?f=errors` — S8-10.
            "search.errors" => {
                let id = cmd.args.int("id").and_then(|v| u32::try_from(v).ok());
                let run = id.and_then(|v| self.runs.get(&SearchId(v)));
                let n = run.map_or(0, |r| {
                    r.denied
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .len()
                });
                tracing::debug!(target: "search", n, "denied folders");
            }
            // `job.retry`, `job.priority`: очереди у поиска нет, повтор — `search.again`.
            "job.retry" | "job.priority" => {}
            other => return Err(CmdError::Unknown(other.into())),
        }
        Ok(())
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        self.install(cx);
    }
}
