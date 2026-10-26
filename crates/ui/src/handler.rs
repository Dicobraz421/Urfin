//! Handler namespace `ui` (S2 §3): меняет только [`UiState`] и фокус, сам без I/O.
//! Чтения ФС для breadcrumb (меню подкаталогов, проверка введённого пути) — задачами
//! на полосе Interactive (ADR-0014, `fs_tasks`).
//!
//! Тема, язык, масштаб, раздел панелей и колонки сохраняются в `config.toml`
//! (`urfin_core::settings`, S2-22) и берутся оттуда при загрузке конфига.

use std::sync::Arc;

use urfin_vfs::VfsRegistry;

use urfin_core::{
    ArgSpec, Args, CancelToken, CmdError, Command, CommandMeta, Config, Cx, Focus, Handler, Level,
    OverlayKind, PromptId, Row, Side, UiEvent, ValueKind, settings,
};

use crate::config::UiCfg;
use crate::fs_tasks;
use crate::state::{Draft, Subdirs, UiState};
use crate::theme_cmd::{self, ThemeEnv};

/// Команды фичи в своём модуле (ADR-0022 §2): список и обработчик (`None` — не её).
pub(crate) struct Part {
    pub commands: &'static [CommandMeta],
    pub handle: fn(&Command, &mut Cx<'_>) -> Option<Result<(), CmdError>>,
}

/// Фичи с командами `ui.*` — по строке на фичу.
const PARTS: &[Part] = &[
    crate::theme_cmd::PART,
    crate::settings_cmd::PART,
    crate::welcome_cmd::PART,
];

pub(crate) const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

pub(crate) const fn meta(
    id: &'static str,
    title: &'static str,
    args: &'static [ArgSpec],
) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "ui",
        args,
        repeatable: false,
    }
}

const SIDE: ArgSpec = arg("side", ValueKind::Str, false);
const ID: ArgSpec = arg("id", ValueKind::Int, false);

/// Ссылок каталога, читаемых для колонки и статуса, не больше.
const LINKS_MAX: usize = 2000;

/// Вариантов дополнения не больше.
const COMPLETE_MAX: usize = 12;

static COMMANDS: &[CommandMeta] = &[
    // Поля ввода (№70): запомнить введённое, дополнить (Tab, Alt+↓ — история), закрыть.
    meta(
        "ui.field.remember",
        "Remember field input",
        &[
            arg("kind", ValueKind::Str, true),
            arg("text", ValueKind::Str, true),
        ],
    ),
    meta(
        "ui.complete",
        "Complete field",
        &[
            arg("kind", ValueKind::Str, true),
            arg("text", ValueKind::Str, true),
            arg("history", ValueKind::Bool, false),
        ],
    ),
    meta("ui.complete.close", "Close completion", &[]),
    // «Мешочки» D-05: сортировка от заголовка колонки, история папок от пути.
    meta("ui.menu.sort", "Sort menu", &[SIDE]),
    meta("ui.history.dirs", "Folder history", &[]),
    // `menu=subdirs side path` и `menu=crumbs side` — меню breadcrumb (ADR-0014).
    meta(
        "ui.menu",
        "Menu",
        &[
            arg("menu", ValueKind::Str, false),
            SIDE,
            arg("path", ValueKind::Loc, false),
        ],
    ),
    meta(
        "ui.qsearch",
        "Quick search",
        &[arg("text", ValueKind::Str, false)],
    ),
    meta("ui.rename", "Rename in place", &[]),
    // С `path` — подтвердить ввод: проверка и `panel.go` или тост (ADR-0014).
    meta(
        "ui.path_edit",
        "Edit path",
        &[SIDE, arg("path", ValueKind::Str, false)],
    ),
    meta(
        "ui.draft",
        "Command argument",
        &[
            arg("command", ValueKind::Str, true),
            arg("arg", ValueKind::Str, true),
            arg("init", ValueKind::Str, false),
        ],
    ),
    meta("ui.cancel", "Close", &[]),
    meta("ui.prompt.hide", "Hide question", &[ID]),
    meta("ui.prompt.show", "Show question", &[ID]),
    meta("ui.jobs.toggle", "Expand tasks", &[]),
    // Диски, сеть и места (Alt+F1/F2, D-03).
    meta("ui.drives", "Drives", &[SIDE]),
    meta("ui.zoom", "Zoom", &[arg("delta", ValueKind::Int, false)]),
    meta(
        "ui.split",
        "Panel split",
        &[
            arg("delta", ValueKind::Int, false),
            arg("reset", ValueKind::Bool, false),
        ],
    ),
    meta("ui.lang", "Language", &[arg("lang", ValueKind::Str, true)]),
    meta("ui.columns", "Columns", &[arg("set", ValueKind::Str, true)]),
    // Имя — из S3 §3 (Ctrl+O), чтобы keymap не расходился с S2.
    // `show` — явно (автопоказ терминала S6-08), без него — переключить.
    meta(
        "ui.panels.toggle_all",
        "Hide or show panels",
        &[arg("show", ValueKind::Bool, false)],
    ),
    // Имена выделенных (или под курсором) — в буфер обмена, по строке (Far).
    meta(
        "ui.copy_names",
        "Copy names",
        &[arg("full", ValueKind::Bool, false)],
    ),
    // Дерево в панели (S2-18): клавиши — контекст keymap `panel && tree`.
    repeat("ui.tree.up", "Tree: up", &[]),
    repeat("ui.tree.down", "Tree: down", &[]),
    repeat("ui.tree.page_up", "Tree: page up", &[]),
    repeat("ui.tree.page_down", "Tree: page down", &[]),
    meta("ui.tree.home", "Tree: first", &[]),
    meta("ui.tree.end", "Tree: last", &[]),
    meta("ui.tree.expand", "Tree: expand", &[]),
    meta("ui.tree.collapse", "Tree: collapse", &[]),
    meta("ui.tree.enter", "Tree: open", &[]),
    meta(
        "ui.tree.select",
        "Tree: select",
        &[arg("n", ValueKind::Int, true)],
    ),
];

const fn repeat(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        repeatable: true,
        ..meta(id, title, args)
    }
}

/// Шаг `ui.zoom` и пределы, %.
const ZOOM_STEP: i64 = 10;
const ZOOM_RANGE: (i64, i64) = (50, 300);
/// Шаг `ui.split` и пределы доли левой панели.
const SPLIT_STEP: f32 = 0.05;
const SPLIT_RANGE: (f32, f32) = (0.1, 0.9);

/// Handler `ui`. [`UiState`] кладётся в `AppState.ext` в первом pump.
pub struct UiHandler {
    /// Чтения для меню и проверки пути. Свой реестр, пока архивы (S8) не общие.
    vfs: Arc<VfsRegistry>,
    /// Текущая фоновая задача меню или проверки пути — отменяется при закрытии.
    task: Option<CancelToken>,
    /// Чтения режимов панелей (S2-18).
    views: crate::views::ViewTasks,
    /// Последний виденный конфиг: новый (старт, `app.reload_config`) — `[ui]` в состояние.
    config: Option<Arc<Config>>,
    /// Каталог своих тем и политика — до первого pump, затем в [`UiState`].
    env: Option<ThemeEnv>,
}

impl Default for UiHandler {
    fn default() -> Self {
        Self {
            vfs: Arc::new(VfsRegistry::with_local()),
            task: None,
            views: crate::views::ViewTasks::default(),
            config: None,
            env: Some(ThemeEnv::default()),
        }
    }
}

impl std::fmt::Debug for UiHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiHandler").finish_non_exhaustive()
    }
}

impl UiHandler {
    pub fn new(env: ThemeEnv) -> UiHandler {
        UiHandler {
            env: Some(env),
            ..UiHandler::default()
        }
    }

    /// Отменить прежнюю задачу и выдать токен новой.
    fn new_task(&mut self, cx: &Cx<'_>) -> CancelToken {
        self.cancel_task();
        let t = cx.rt.token();
        self.task = Some(t.clone());
        t
    }

    fn cancel_task(&mut self) {
        if let Some(t) = self.task.take() {
            t.cancel();
        }
    }
}

impl Handler for UiHandler {
    fn namespace(&self) -> &'static str {
        "ui"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        static ALL: std::sync::OnceLock<Vec<CommandMeta>> = std::sync::OnceLock::new();
        ALL.get_or_init(|| {
            COMMANDS
                .iter()
                .chain(PARTS.iter().flat_map(|p| p.commands))
                .copied()
                .collect()
        })
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        self.scan_links(cx);
        if !self
            .config
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &cx.state.config))
        {
            let config = Arc::clone(&cx.state.config);
            // Ошибку секции уже показал баннер core.
            let (cfg, _) = config.section::<UiCfg>("ui");
            if let Some(env) = self.env.take() {
                let st = ui_state(cx);
                st.policy = env.policy;
                st.themes_dir = env.dir;
                st.fields_file.clone_from(&env.fields);
                st.welcome.clone_from(&env.welcome);
                st.config_dir.clone_from(&env.config_dir);
                st.log_dir.clone_from(&env.log_dir);
                // Первый запуск: опция быстрой корзины — в конфиг (№79).
                if matches!(env.welcome, Some(crate::state::Welcome::First)) {
                    settings::set(&["ops", "experimental_fast_trash"], true).apply(cx);
                }
                if let Some(file) = env.fields {
                    fs_tasks::spawn_fields_load(cx.rt, file);
                }
                theme_cmd::reload(cx, true);
            }
            seed(ui_state(cx), &cfg);
            cx.state.touch();
            self.config = Some(config);
        }
        let focus = cx.state.focus;
        let st = ui_state(cx);
        // Фокус ушёл из оверлея (клик в панель, вопрос core, быстрый поиск keymap) —
        // оверлей закрыт.
        let open = st.overlay;
        if open.is_some_and(|k| focus != Focus::Overlay(k)) {
            st.close_overlay();
            cx.state.touch();
            self.cancel_task();
        }
        crate::views::sync(cx, &self.vfs, &mut self.views);
        crate::status::sync(cx);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "диспетчер команд ui: ветка на команду"
    )]
    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        // Команды фич — в их модулях (ADR-0022 §2), по списку `PARTS`.
        if let Some(r) = PARTS.iter().find_map(|p| (p.handle)(cmd, cx)) {
            return r;
        }
        let a = &cmd.args;
        match cmd.id.as_str() {
            "ui.menu" | "ui.menu.sort" | "ui.history.dirs" => self.menu(cx, cmd.id.as_str(), a),
            "ui.field.remember" | "ui.complete" | "ui.complete.close" => {
                self.field_cmd(cx, cmd.id.as_str(), a);
            }
            // Быстрый поиск ведёт keymap (ADR-0016); здесь — синоним для меню.
            "ui.qsearch" => {
                let mut c = Command::new("keymap.qs.start");
                if let Some(t) = a.str("text") {
                    c = c.arg("text", t);
                }
                cx.dispatch(c);
                cx.keep_rev();
            }
            "ui.rename" => {
                let t = cx.state.active_tab();
                let Some(Row::Entry { entry, .. }) = t.row(t.cursor.row) else {
                    // На `..` и в пустом каталоге переименовывать нечего.
                    cx.keep_rev();
                    return Ok(());
                };
                let target = (t.id, entry.name.clone());
                open(cx, OverlayKind::InlineEdit, |st| st.rename = Some(target));
            }
            "ui.path_edit" => self.path_edit(cx, a),
            "ui.drives" => {
                self.cancel_task();
                let side = side_arg(cx, a);
                crate::drives::open(cx, side);
            }
            "ui.draft" => {
                let draft = Draft {
                    cmd: a.str("command").unwrap_or_default().into(),
                    arg: a.str("arg").unwrap_or_default().into(),
                    text: a.str("init").unwrap_or_default().into(),
                };
                open(cx, OverlayKind::Draft, |st| st.draft = Some(draft));
            }
            "ui.cancel" => {
                self.cancel_task();
                ui_state(cx).close_overlay();
                if matches!(cx.state.focus, Focus::Overlay(_)) {
                    set_focus(cx, Focus::Panel);
                }
            }
            "ui.prompt.hide" | "ui.prompt.show" => {
                let Some(id) = prompt_id(cx, a) else {
                    cx.keep_rev();
                    return Ok(());
                };
                let hidden = &mut ui_state(cx).prompt_hidden;
                hidden.retain(|h| *h != id);
                if cmd.id.as_str() == "ui.prompt.hide" {
                    hidden.push(id);
                }
            }
            "ui.jobs.toggle" => {
                let st = ui_state(cx);
                st.jobs_expanded = !st.jobs_expanded;
            }
            "ui.zoom" => {
                let st = ui_state(cx);
                zoom(st, a.int("delta"));
                let z = i64::from(st.zoom);
                settings::set(&["ui", "zoom"], z).apply(cx);
            }
            "ui.split" => {
                let st = ui_state(cx);
                split(st, a.bool("reset"), a.int("delta"));
                // Две цифры: в файле 0.55, а не 0.550000011920929.
                let s = (f64::from(st.split) * 100.0).round() / 100.0;
                settings::set(&["ui", "split"], s).apply(cx);
            }
            "ui.lang" => lang(cx, a.str("lang").unwrap_or_default()),
            "ui.panels.toggle_all" => {
                let st = ui_state(cx);
                let hidden = a.bool("show").map_or(!st.panels_hidden, |show| !show);
                let was = st.panels_hidden;
                st.panels_hidden = hidden;
                if hidden && !was {
                    // Показан терминал: shell — в каталог панели (S6 §5.1).
                    urfin_term::sync_cwd(cx);
                }
                // Вид «экран» (S6 §3.2): ввод — в терминал; панели вернулись — в панель.
                let screen = cx
                    .state
                    .ext
                    .get::<urfin_term::TermState>()
                    .is_some_and(urfin_term::TermState::screen_view);
                // Transcript — в командную строку: стрелки правят её, а не ходят по
                // невидимой панели.
                if hidden {
                    cx.state.focus = if screen {
                        Focus::Terminal
                    } else {
                        Focus::Cmdline
                    };
                } else if matches!(cx.state.focus, Focus::Terminal | Focus::Cmdline) {
                    cx.state.focus = Focus::Panel;
                }
            }
            id if id.starts_with("ui.tree.") => {
                crate::views::tree_command(cx, &self.vfs, &mut self.views, id, a.int("n"));
            }
            "ui.copy_names" => {
                let t = cx.state.active_tab();
                let mut names: Vec<String> = t
                    .selection
                    .names(&t.listing)
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                if names.is_empty()
                    && let Some(Row::Entry { entry, .. }) = t.current()
                {
                    names.push(entry.name.to_string());
                }
                if a.bool("full").unwrap_or(false) {
                    for n in &mut names {
                        *n = t.loc.path.join(&*n).display().to_string();
                    }
                }
                if !names.is_empty() {
                    cx.ui(UiEvent::Ext {
                        id: urfin_core::CommandId::from_static("clip.write"),
                        args: Args::default().with("text", names.join("\r\n")),
                    });
                }
            }
            "ui.columns" => {
                let set = a.str("set").unwrap_or_default();
                ui_state(cx).columns = Some(set.into());
                settings::set(&["ui", "columns"], set).apply(cx);
            }
            _ => return Err(CmdError::Unknown(cmd.id.as_str().into())),
        }
        Ok(())
    }
}

impl UiHandler {
    /// `ui.menu`: F9 (S2-14) или меню breadcrumb — подкаталоги (`›`) и свёрнутые
    /// сегменты (`…`).
    fn menu(&mut self, cx: &mut Cx<'_>, id: &str, a: &Args) {
        let side = side_arg(cx, a);
        // «Мешочки» D-05: сортировка от заголовка колонки панели, история папок.
        let menu = match id {
            "ui.menu.sort" if side == Side::Right => "sort-right",
            "ui.menu.sort" => "sort-left",
            "ui.history.dirs" => "history",
            _ => a.str("menu").unwrap_or_default(),
        };
        if menu == "subdirs" {
            let loc = a
                .loc("path")
                .unwrap_or_else(|| cx.state.panel(side).tab().loc.clone());
            let cancel = self.new_task(cx);
            let generation = ui_state(cx).bump();
            fs_tasks::spawn_subdirs(
                cx.rt,
                Arc::clone(&self.vfs),
                loc.clone(),
                generation,
                cancel,
            );
            open(cx, OverlayKind::Menu, |st| {
                st.menu = Some("subdirs".into());
                st.menu_side = Some(side);
                st.subdirs = Some(Subdirs {
                    side,
                    loc,
                    items: None,
                    generation,
                });
            });
            return;
        }
        self.cancel_task();
        let crumbs = menu == "crumbs";
        // F9 без аргумента — меню активной панели (как Far).
        let menu = match menu {
            "" if cx.state.active == Side::Left => "left".into(),
            "" => "right".into(),
            m => m.into(),
        };
        open(cx, OverlayKind::Menu, |st| {
            st.menu = Some(menu);
            st.menu_side = crumbs.then_some(side);
        });
    }

    /// Ссылки панелей (D-03): листинг с ссылками сменился — прочитать их цели и вид
    /// (junction или symlink) в Runtime; до ответа колонка показывает `<LINK>`.
    fn scan_links(&mut self, cx: &mut Cx<'_>) {
        for side in [Side::Left, Side::Right] {
            let t = cx.state.panel(side).tab();
            let generation = t.listing.generation.0;
            let i = usize::from(side == Side::Right);
            let known = cx
                .state
                .ext
                .get::<crate::UiState>()
                .map(|u| &u.links[i])
                .is_some_and(|l| l.loc.as_ref() == Some(&t.loc) && l.generation == generation);
            if known {
                continue;
            }
            let names: Vec<compact_str::CompactString> = t
                .listing
                .entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.kind,
                        urfin_core::EntryKind::LinkDir
                            | urfin_core::EntryKind::LinkFile
                            | urfin_core::EntryKind::LinkBroken
                    )
                })
                .take(LINKS_MAX)
                .map(|e| e.name.clone())
                .collect();
            let loc = t.loc.clone();
            let st = ui_state(cx);
            st.links[i] = crate::state::Links {
                loc: Some(loc.clone()),
                generation,
                items: std::collections::HashMap::new(),
            };
            cx.keep_rev();
            if !names.is_empty() && loc.is_local() {
                fs_tasks::spawn_links(cx.rt, Arc::clone(&self.vfs), side, loc, generation, names);
            }
        }
    }

    /// Поля ввода (№70): `ui.field.remember`, `ui.complete`, `ui.complete.close`.
    fn field_cmd(&mut self, cx: &mut Cx<'_>, id: &str, a: &Args) {
        use crate::field_history::{FieldKind, matching};
        use crate::state::{Cand, Complete};
        let kind = a.str("kind").and_then(FieldKind::parse);
        let text = a.str("text").unwrap_or_default().to_owned();
        match (id, kind) {
            ("ui.field.remember", Some(k)) => {
                let st = ui_state(cx);
                let save = st
                    .fields
                    .push(k, &text)
                    .then(|| st.fields_file.clone().map(|f| (f, st.fields.to_text())))
                    .flatten();
                if let Some((file, body)) = save {
                    fs_tasks::spawn_fields_save(cx.rt, file, body);
                }
                cx.keep_rev();
            }
            ("ui.complete", Some(k)) => {
                let history_only = a.bool("history").unwrap_or(false);
                let prefix = if history_only { "" } else { text.as_str() };
                let mut items: Vec<Cand> = matching(ui_state(cx).fields.get(k), prefix)
                    .map(|t| Cand {
                        text: t.clone(),
                        history: true,
                    })
                    .collect();
                if !history_only && k == FieldKind::Name {
                    // Имена в текущей папке, начинающиеся с набранного.
                    let low = text.to_lowercase();
                    let t = cx.state.active_tab();
                    items.extend(
                        t.listing
                            .order
                            .iter()
                            .filter_map(|&i| t.listing.entries.get(i as usize))
                            .filter(|e| {
                                let n = e.name.to_lowercase();
                                n.starts_with(&low) && n != low
                            })
                            .take(COMPLETE_MAX)
                            .map(|e| Cand {
                                text: e.name.to_string(),
                                history: false,
                            }),
                    );
                }
                let generation = ui_state(cx).bump();
                if !history_only && k == FieldKind::Path {
                    // Подкаталоги набранного родителя — в Runtime.
                    let base = cx.state.active_tab().loc.clone();
                    if let Some((dir, _)) = fs_tasks::split_path_prefix(&text, &base) {
                        let cancel = self.new_task(cx);
                        fs_tasks::spawn_complete_dirs(
                            cx.rt,
                            Arc::clone(&self.vfs),
                            dir,
                            text.clone(),
                            generation,
                            cancel,
                        );
                    }
                }
                items.truncate(COMPLETE_MAX);
                ui_state(cx).complete = Some(Complete {
                    generation,
                    kind: k,
                    text,
                    history_only,
                    items,
                });
            }
            ("ui.complete.close", _) => ui_state(cx).complete = None,
            _ => cx.keep_rev(),
        }
    }

    /// `ui.path_edit`: без `path` — открыть поле; с `path` — проверить и перейти.
    fn path_edit(&mut self, cx: &mut Cx<'_>, a: &Args) {
        let side = side_arg(cx, a);
        let Some(text) = a.str("path") else {
            self.cancel_task();
            open(cx, OverlayKind::InlineEdit, |st| st.path_edit = Some(side));
            return;
        };
        let base = cx.state.panel(side).tab().loc.clone();
        let Some(loc) = fs_tasks::resolve_input(text, &base) else {
            let mut args = urfin_i18n::FluentArgs::new();
            args.set("path", text.to_owned());
            args.set("error", "-");
            let msg = urfin_i18n::current().fmt("ui-path-not-found", &args);
            cx.toast(Level::Warn, msg);
            return;
        };
        let cancel = self.new_task(cx);
        let st = ui_state(cx);
        let generation = st.bump();
        st.path_check = Some(generation);
        if st.path_edit != Some(side) {
            st.close_overlay();
            st.path_check = Some(generation);
            st.overlay = Some(OverlayKind::InlineEdit);
            st.path_edit = Some(side);
            set_focus(cx, Focus::Overlay(OverlayKind::InlineEdit));
        }
        fs_tasks::spawn_path_check(cx.rt, Arc::clone(&self.vfs), side, loc, generation, cancel);
    }
}

/// [`UiState`] из `ext`; нет — создаётся.
pub(crate) fn ui_state<'c>(cx: &'c mut Cx<'_>) -> &'c mut UiState {
    if cx.state.ext.get::<UiState>().is_none() {
        cx.state.ext.insert(UiState::new());
    }
    cx.state.ext.get_mut::<UiState>().expect("inserted above")
}

/// Открыть оверлей: прежний закрывается, фокус — `Overlay(kind)`.
fn open(cx: &mut Cx<'_>, kind: OverlayKind, fill: impl FnOnce(&mut UiState)) {
    let st = ui_state(cx);
    st.close_overlay();
    st.overlay = Some(kind);
    fill(st);
    set_focus(cx, Focus::Overlay(kind));
}

fn set_focus(cx: &mut Cx<'_>, focus: Focus) {
    if cx.state.focus != focus {
        cx.state.focus = focus;
        cx.ui(UiEvent::FocusChanged(focus));
    }
}

fn side_arg(cx: &Cx<'_>, a: &Args) -> Side {
    use urfin_core::SideRef as S;
    let active = cx.state.active;
    match a.side("side") {
        None | Some(S::Active) => active,
        Some(S::Passive) => active.other(),
        Some(S::Left) => Side::Left,
        Some(S::Right) => Side::Right,
    }
}

fn zoom(st: &mut UiState, delta: Option<i64>) {
    st.zoom = match delta {
        Some(d) => {
            let z = (i64::from(st.zoom) + d.signum() * ZOOM_STEP).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
            u16::try_from(z).unwrap_or(100)
        }
        None => 100,
    };
}

fn split(st: &mut UiState, reset: Option<bool>, delta: Option<i64>) {
    st.split = match (reset, delta) {
        (Some(true), _) | (_, None) => 0.5,
        (_, Some(d)) => {
            let step = if d < 0 { -SPLIT_STEP } else { SPLIT_STEP };
            (st.split + step).clamp(SPLIT_RANGE.0, SPLIT_RANGE.1)
        }
    };
}

/// `[ui]` конфига — в состояние; значения вне пределов приводятся к ним.
fn seed(st: &mut UiState, cfg: &UiCfg) {
    theme_cmd::seed(st, &cfg.theme);
    st.density = cfg.density.px();
    st.icons = cfg.icons;
    st.zoom = u16::try_from(i64::from(cfg.zoom).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1)).unwrap_or(100);
    st.split = if cfg.split.is_finite() {
        cfg.split.clamp(SPLIT_RANGE.0, SPLIT_RANGE.1)
    } else {
        0.5
    };
    st.columns = cfg.columns.as_deref().map(Into::into);
}

/// Язык на лету: `epoch` растёт, кэши текста сбрасываются (S2 §7). Доступный язык
/// пишется в `[general] language`.
fn lang(cx: &mut Cx<'_>, lang: &str) {
    let mut known = true;
    for w in urfin_i18n::set_language(lang) {
        if let urfin_i18n::I18nWarning::UnknownLanguage { requested, .. } = w {
            known = false;
            let mut args = urfin_i18n::FluentArgs::new();
            args.set("lang", requested);
            let text = urfin_i18n::current().fmt("ui-lang-unavailable", &args);
            cx.toast(Level::Warn, text);
        }
    }
    if known {
        settings::set(&["general", "language"], lang.trim()).apply(cx);
    }
}

/// `id` или активный вопрос.
fn prompt_id(cx: &Cx<'_>, a: &Args) -> Option<PromptId> {
    match a.int("id") {
        Some(id) => u64::try_from(id).ok().map(PromptId),
        None => cx.state.prompts.active().map(|p| p.id),
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
