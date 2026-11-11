//! Handler `search` (S8 §2.5). Живёт в UI-потоке: файлы читают job и фоновые задачи, модель
//! экрана — `SearchModel` в `AppState.ext`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use compact_str::{CompactString, ToCompactString};
use rustc_hash::{FxHashMap, FxHashSet};
use urfin_core::search::{Hit, PastQuery, SearchId, SearchModel};
use urfin_core::{
    ArgSpec, Args, CancelToken, CmdError, Command, CommandMeta, Cx, Focus, Handler, JobId, Lane,
    Level, Location, OverlayKind, Row, UiEvent, ValueKind, msg,
};
use urfin_vfs::VfsRegistry;
use urfin_vfs::name::join_os;
use urfin_vfs::walk::child;

use crate::job::{self, Spawn};
use crate::life::{self, Fate, Holds, Recent};
use crate::query::SearchQuery;
use crate::svfs::{self, SearchStores, View};
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

/// `search.to_panel`: стор (по умолчанию — экрана); `cmp` — это сравнение
/// `ops.compare { recursive }` (режим `quick`/`content`, его job — `job`), панель `cmp:/id`;
/// `replace` — в активный таб вместо нового (Ctrl+R).
const TO_PANEL: [ArgSpec; 4] = [
    arg("id", ValueKind::Int, false),
    arg("job", ValueKind::Int, false),
    arg("cmp", ValueKind::Str, false),
    arg("replace", ValueKind::Bool, false),
];

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
    meta("search.to_panel", "Results to panel", &TO_PANEL),
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
    /// Аргументы `search.run`: для `search.again`; у сравнения — `mode` для `ops.compare`.
    args: Args,
    past: PastQuery,
    /// Сравнение `cmp:` (стор пишет ops).
    cmp: bool,
}

/// Handler `search`.
pub struct SearchHandler {
    reg: Arc<VfsRegistry>,
    /// Каталог истории; `None` — без диска.
    dir: Option<PathBuf>,
    runs: FxHashMap<SearchId, Run>,
    /// Сторы для `search:` (общий с `SearchVfs`).
    stores: Arc<SearchStores>,
    /// Отпущенные сторы: время жизни — `life.rs`.
    recent: Recent,
    /// Последний поиск экрана и курсор его списка: Alt+F7 из обычной панели возвращает его.
    last: Option<(SearchId, u32)>,
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

/// Номер поиска панели на `search:` (любой вид).
fn panel_id(loc: &Location) -> Option<SearchId> {
    svfs::parse(loc).map(|a| a.id)
}

/// Номер стора панели `search:` или `cmp:`.
fn any_panel_id(loc: &Location) -> Option<SearchId> {
    panel_id(loc).or_else(|| svfs::parse_cmp(loc).map(|a| a.id))
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
    /// Handler со своим реестром сторов: панель `search:` их не увидит (тесты, `--smoke`).
    pub fn new(reg: Arc<VfsRegistry>, dir: Option<PathBuf>) -> SearchHandler {
        SearchHandler::with_stores(reg, dir, SearchStores::new())
    }

    /// Handler со сторами, общими с `SearchVfs` в реестре листинга.
    pub fn with_stores(
        reg: Arc<VfsRegistry>,
        dir: Option<PathBuf>,
        stores: Arc<SearchStores>,
    ) -> SearchHandler {
        stores.bind(&reg);
        SearchHandler {
            reg,
            dir,
            runs: FxHashMap::default(),
            stores,
            recent: Recent::default(),
            last: None,
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

    /// Поиск команды: `id`, иначе открытый экран, иначе панель `search:`, иначе стор экрана.
    fn target(cmd: &Command, cx: &mut Cx<'_>) -> Option<SearchId> {
        let id = cmd
            .args
            .int("id")
            .and_then(|v| u32::try_from(v).ok())
            .map(SearchId);
        let on_screen = cx.state.focus == Focus::Overlay(OverlayKind::Search);
        let screen = model(cx).current().map(|s| s.id);
        id.or(screen.filter(|_| on_screen))
            .or_else(|| any_panel_id(&cx.state.active_tab().loc))
            .or(screen)
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

    /// Экран освобождается: поиск отменён, найденное остаётся последним поиском экрана;
    /// что со стором дальше — решает `life.rs`.
    fn drop_current(&mut self, cx: &mut Cx<'_>) {
        let Some(id) = model(cx).current().map(|s| s.id) else {
            return;
        };
        self.cancel(cx, id);
        let m = model(cx);
        self.last = Some((id, m.cursor()));
        m.show(None);
        if life::on_screen_closed() == Fate::Drop {
            self.evict(cx, id);
        }
    }

    /// Стор больше не нужен: из модели, реестра панели и недавних.
    fn evict(&mut self, cx: &mut Cx<'_>, id: SearchId) {
        self.runs.remove(&id);
        self.recent.forget(id);
        self.stores.remove(id);
        model(cx).remove(id);
    }

    /// Каждый pump: кто держит сторы (экран, вкладки, job) — по `life.rs`; отпущенные — в
    /// недавние, лишние недавние — вон. Закрытие последней вкладки (`TabEvent::Closed`)
    /// handler видит здесь: вкладок с `search:/id` не осталось.
    fn sweep(&mut self, cx: &mut Cx<'_>) {
        if self.runs.is_empty() {
            return;
        }
        let tabs: FxHashSet<SearchId> = cx
            .state
            .panels
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| any_panel_id(&t.loc))
            .collect();
        let screen = model(cx).current().map(|s| s.id);
        let mut ids: Vec<SearchId> = self.runs.keys().copied().collect();
        // Отпущенные за один pump встают в очередь по возрасту поиска.
        ids.sort_unstable();
        for id in ids {
            let job = self.runs[&id].job;
            let holds = Holds {
                screen: screen == Some(id),
                tab: tabs.contains(&id),
                job: cx.state.jobs.get(job).is_some_and(|j| j.state.is_active()),
            };
            match life::fate(holds) {
                Fate::Keep => self.recent.forget(id),
                Fate::Recent => {
                    if !self.recent.contains(id) {
                        self.recent.push(id, self.stores.approx_bytes(id));
                    }
                }
                Fate::Drop => self.evict(cx, id),
            }
        }
        for id in life::trim(&mut self.recent) {
            tracing::debug!(target: "search", id = id.0, "store released");
            self.evict(cx, id);
        }
    }

    /// `panel.refresh` готового поиска: перепроверка попаданий фоном; у сравнения —
    /// сравнить заново.
    fn recheck(&self, cx: &mut Cx<'_>) {
        for id in self.stores.take_wanted() {
            svfs::spawn_recheck(&self.stores, id, cx.rt);
        }
        for id in self.stores.take_again() {
            self.compare_again(cx, id);
        }
    }

    /// Сравнить заново (Ctrl+R в `cmp:`, `search.again`): `ops.compare` с теми же сторонами
    /// и режимом; новый стор займёт активный таб, если он показывает этот.
    fn compare_again(&self, cx: &mut Cx<'_>, id: SearchId) {
        let Some(run) = self.runs.get(&id).filter(|r| r.cmp) else {
            return;
        };
        let Some(store) = model(cx).get(id).cloned() else {
            return;
        };
        let [left, right] = &*store.roots else {
            return;
        };
        let here = svfs::parse_cmp(&cx.state.active_tab().loc).map(|a| a.id) == Some(id);
        cx.dispatch(
            Command::new("ops.compare")
                .arg("recursive", true)
                .arg("mode", run.args.str("mode").unwrap_or("quick"))
                .arg("left", left.clone())
                .arg("right", right.clone())
                .arg("replace", here),
        );
    }

    /// Сравнение `ops.compare { recursive }` — в панель `cmp:/id` (новый таб или, с
    /// `replace`, активный): стор — в реестр панели, время жизни — как у поиска.
    fn show_cmp(&mut self, cx: &mut Cx<'_>, cmd: &Command, mode: &str) {
        let a = &cmd.args;
        let Some(id) = a
            .int("id")
            .and_then(|v| u32::try_from(v).ok())
            .map(SearchId)
        else {
            return;
        };
        let Some(store) = model(cx).get(id).cloned() else {
            return;
        };
        self.stores.insert_cmp(&store);
        let job = JobId(
            a.int("job")
                .and_then(|v| u64::try_from(v).ok())
                .unwrap_or(0),
        );
        let run = Run {
            job,
            args: Args::default().with("mode", mode),
            past: PastQuery::default(),
            cmp: true,
        };
        self.runs.insert(id, run);
        if !a.bool("replace").unwrap_or(false) {
            cx.dispatch(Command::new("tab.new"));
        }
        cx.dispatch(Command::new("panel.go").arg("path", svfs::cmp_location(id)));
    }

    /// Последний поиск экрана — снова на экран, если стор жив и вид тот же (файлы,
    /// каталоги): его запрос для формы.
    fn restore_last(&self, cx: &mut Cx<'_>, kinds: &str) -> Option<PastQuery> {
        let (id, cursor) = self.last?;
        let run = self.runs.get(&id).filter(|r| !r.cmp)?;
        if run.args.str("kinds").unwrap_or("files") != kinds {
            return None;
        }
        let m = model(cx);
        if m.current().is_some() || m.get(id).is_none() {
            return None;
        }
        m.show(Some(id));
        m.set_cursor(cursor);
        Some(run.past.clone())
    }

    /// Форма экрана: `search.files`, `search.dirs`. Из панели `search:` — её запрос; из
    /// обычной панели — последний поиск экрана с найденным, если он ещё жив.
    fn open_form(&mut self, cx: &mut Cx<'_>, kinds: &'static str) {
        let panel = panel_id(&cx.state.active_tab().loc);
        let form = panel
            .and_then(|id| self.runs.get(&id))
            .map(|r| r.past.clone());
        let form = match (form, panel) {
            (Some(f), _) => Some(f),
            (None, None) => self.restore_last(cx, kinds),
            (None, Some(_)) => None,
        };
        // Запрос поиска (панели или последнего): и его «В».
        let own = form.is_some();
        let last = model(cx).history().front().cloned();
        let form = form.or(last).unwrap_or(PastQuery {
            recursive: true,
            ..PastQuery::default()
        });
        let place = if own {
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
        let text = query.text.is_some();
        self.stores.insert(&store, &denied, text);
        let spawn = Spawn {
            reg: Arc::clone(&self.reg),
            query,
            cfg,
            store,
            denied,
        };
        let job = job::start(cx, spawn);
        let run = Run {
            job,
            args: cmd.args.clone(),
            past,
            cmp: false,
        };
        self.runs.insert(id, run);
        self.last = Some((id, 0));
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

    /// Стор — в новую вкладку; поиск продолжается. Запрос — окну: заголовок панели и имя
    /// вкладки (`search.panel`).
    fn show_in_panel(&mut self, cx: &mut Cx<'_>, id: SearchId) {
        if let Some(run) = self.runs.get(&id) {
            let args = Args::default()
                .with("id", i64::from(id.0))
                .with("name", run.past.name.as_str())
                .with("text", run.past.text.as_str())
                .with("where", run.past.place.as_str())
                .with("kinds", run.args.str("kinds").unwrap_or("files"));
            let ev = Command::new("search.panel").id;
            cx.ui(UiEvent::Ext { id: ev, args });
        }
        model(cx).show(None);
        cx.dispatch(Command::new("tab.new"));
        cx.dispatch(Command::new("panel.go").arg("path", svfs::location(id, View::Files)));
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

    /// Клавиши панели `search:` или `cmp:`, а не экрана: экран не в фокусе, активная
    /// вкладка — поиск или сравнение.
    fn on_panel(cx: &Cx<'_>) -> bool {
        cx.state.focus != Focus::Overlay(OverlayKind::Search)
            && any_panel_id(&cx.state.active_tab().loc).is_some()
    }

    /// Строка под курсором панели `search:`: настоящий адрес, строка первого совпадения и
    /// каталог ли это.
    fn panel_row(&self, cx: &Cx<'_>) -> Option<(Location, u32, bool)> {
        let t = cx.state.active_tab();
        let Some(Row::Entry { entry, .. }) = t.current() else {
            return None;
        };
        let (loc, line) = self.stores.locate(&t.loc, entry)?;
        Some((loc, line, entry.kind.is_dir_like()))
    }

    /// F3, F4 в панели `search:`: файл на строке первого совпадения через `resolve`.
    fn open_row(&self, cx: &mut Cx<'_>, id: &'static str) {
        if let Some((loc, line, false)) = self.panel_row(cx) {
            let mut c = Command::new(id).arg("path", loc);
            if line > 0 {
                c = c.arg("line", line);
            }
            cx.dispatch(c);
        }
    }

    /// Ctrl+PgUp в панели `search:`: в папку файла (каталога), курсор — на него.
    fn goto_row(&self, cx: &mut Cx<'_>) {
        if let Some((loc, _, _)) = self.panel_row(cx)
            && let (Some(dir), Some(name)) = (loc.parent(), loc.file_name())
        {
            let name = name.to_string_lossy().to_compact_string();
            cx.dispatch(Command::new("panel.go").arg("path", dir));
            cx.dispatch(Command::new("panel.cursor.to").arg("name", name));
        }
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
                if let Some(mode) = cmd.args.str("cmp") {
                    let mode = mode.to_owned();
                    self.show_cmp(cx, cmd, &mode);
                    return Ok(());
                }
                let id = cmd.args.int("id").and_then(|v| u32::try_from(v).ok());
                let id = id
                    .map(SearchId)
                    .filter(|id| self.runs.get(id).is_some_and(|r| !r.cmp));
                if let Some(id) = id.or_else(|| model(cx).current().map(|s| s.id)) {
                    self.show_in_panel(cx, id);
                }
            }
            "search.cursor" => return Self::cursor(cmd, cx),
            "search.view" | "search.edit" => {
                let id = if cmd.id.as_str() == "search.view" {
                    "viewer.open"
                } else {
                    "editor.open"
                };
                if Self::on_panel(cx) {
                    self.open_row(cx, id);
                } else {
                    Self::open_file(cx, id);
                }
            }
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
                let id = Self::target(cmd, cx);
                if let Some(id) = id.filter(|id| self.runs.get(id).is_some_and(|r| r.cmp)) {
                    self.compare_again(cx, id);
                    return Ok(());
                }
                let run = id.and_then(|id| self.runs.get(&id));
                if let Some(run) = run {
                    let mut c = Command::new("search.run");
                    c.args = run.args.clone();
                    cx.dispatch(c);
                }
            }
            "search.goto" if Self::on_panel(cx) => self.goto_row(cx),
            "search.goto" => Self::goto(cx),
            // Каталоги без доступа — панель `search:/id?f=errors` (вопрос В5).
            "search.errors" => {
                let id = cmd.args.int("id").and_then(|v| u32::try_from(v).ok());
                if let Some(id) = id.map(SearchId).filter(|id| self.stores.contains(*id)) {
                    let loc = svfs::location(id, View::Errors);
                    cx.dispatch(Command::new("panel.go").arg("path", loc));
                }
            }
            // `job.retry`, `job.priority`: очереди у поиска нет, повтор — `search.again`.
            "job.retry" | "job.priority" => {}
            other => return Err(CmdError::Unknown(other.into())),
        }
        Ok(())
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        self.install(cx);
        self.sweep(cx);
        self.recheck(cx);
    }
}
