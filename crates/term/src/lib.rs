//! Urfin — терминал и командная строка (S6).
//!
//! Handlers `term` и `cmdline` с полным реестром команд (S6 §9; имена `cmdline.*` —
//! как в пресетах keymap S3), секция `[term]`, состояние в `AppState.ext`. Ещё не
//! сделанные команды отвечают «ещё не сделано» (карточки `docs/tasks/S6-*.md`).

mod blocks;
pub mod complete;
pub mod config;
pub mod history;
pub mod integ;
pub mod line;
mod line_cmds;
pub mod manager;
pub mod menu;
pub mod open;
pub mod pwsh_server;
pub mod quote;
mod raw;
pub mod session;
pub mod shells;
pub mod sys;
pub mod vt;

pub use config::{CompleteConfig, HistoryConfig, ShellConfig, TermConfig};

use std::path::PathBuf;
use std::time::{Duration, Instant};

use urfin_core::{
    AppState, ArgSpec, Args, CmdError, Command, CommandId, CommandMeta, Core, Cx, Handler,
    QuitDecision, SideRef, UiEvent, ValueKind,
};

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(
    id: &'static str,
    title: &'static str,
    category: &'static str,
    args: &'static [ArgSpec],
    repeatable: bool,
) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category,
        args,
        repeatable,
    }
}

const NONE: &[ArgSpec] = &[];

/// `term.*` (S6 §9).
static TERM_COMMANDS: [CommandMeta; 25] = [
    meta("term.toggle", "Show or hide panels", "term", NONE, false),
    meta(
        "term.shell",
        "Switch shell",
        "term",
        &[arg("id", ValueKind::Str, true)],
        false,
    ),
    meta("term.shell_menu", "Shell menu", "term", NONE, false),
    meta(
        "term.new",
        "New session",
        "term",
        &[arg("shell", ValueKind::Str, false)],
        false,
    ),
    meta("term.next", "Next session", "term", NONE, false),
    meta("term.prev", "Previous session", "term", NONE, false),
    meta(
        "term.close",
        "Close session",
        "term",
        &[arg("id", ValueKind::Int, false)],
        false,
    ),
    meta("term.restart", "Restart session", "term", NONE, false),
    meta("term.kill", "Kill process", "term", NONE, false),
    meta("term.focus", "Focus terminal", "term", NONE, false),
    meta(
        "term.input",
        "Send text",
        "term",
        &[arg("text", ValueKind::Str, true)],
        false,
    ),
    meta(
        "term.key",
        "Send key",
        "term",
        &[
            arg("key", ValueKind::Str, true),
            arg("mods", ValueKind::Str, false),
        ],
        true,
    ),
    meta(
        "term.scroll",
        "Scroll output",
        "term",
        &[
            arg("lines", ValueKind::Int, false),
            arg("pages", ValueKind::Int, false),
        ],
        true,
    ),
    meta("term.copy", "Copy selection", "term", NONE, false),
    meta(
        "term.paste",
        "Paste",
        "term",
        &[arg("text", ValueKind::Str, false)],
        false,
    ),
    meta("term.select_all", "Select all output", "term", NONE, false),
    meta("term.clear", "Clear output", "term", NONE, false),
    meta(
        "term.open",
        "Open file",
        "term",
        &[
            arg("path", ValueKind::Str, false),
            arg("verb", ValueKind::Str, false),
        ],
        false,
    ),
    meta(
        "term.exec",
        "Run command",
        "term",
        &[
            arg("cmd", ValueKind::Str, true),
            arg("cwd", ValueKind::Str, false),
        ],
        false,
    ),
    // Мышь над сеткой (S6-07): шлёт окно; ячейка экрана, кнопка, модификаторы.
    meta(
        "term.mouse",
        "Mouse in terminal",
        "term",
        &[
            arg("kind", ValueKind::Str, true),
            arg("button", ValueKind::Str, false),
            arg("row", ValueKind::Int, false),
            arg("col", ValueKind::Int, false),
            arg("right", ValueKind::Bool, false),
            arg("mods", ValueKind::Str, false),
            arg("clicks", ValueKind::Int, false),
            arg("lines", ValueKind::Int, false),
        ],
        true,
    ),
    // Область терминала окна (S6-08): шлёт окно при раскладке, не пользователь.
    meta(
        "term.resize",
        "Terminal area size",
        "term",
        &[
            arg("cols", ValueKind::Int, true),
            arg("rows", ValueKind::Int, true),
        ],
        false,
    ),
    // Блоки команд (S6-21): переходы по промптам, вывод последней команды, поиск.
    meta("term.prev_prompt", "Previous prompt", "term", NONE, true),
    meta("term.next_prompt", "Next prompt", "term", NONE, true),
    meta(
        "term.copy_output",
        "Copy last command output",
        "term",
        NONE,
        false,
    ),
    meta(
        "term.find",
        "Find in output",
        "term",
        &[
            arg("pattern", ValueKind::Str, false),
            arg("back", ValueKind::Bool, false),
        ],
        true,
    ),
];

/// `cmdline.*` (S6 §9; имена — как в пресетах keymap S3).
static CMDLINE_COMMANDS: [CommandMeta; 35] = [
    meta(
        "cmdline.insert",
        "Insert text",
        "cmdline",
        &[arg("text", ValueKind::Str, true)],
        false,
    ),
    meta("cmdline.cursor.left", "Cursor left", "cmdline", NONE, true),
    meta(
        "cmdline.cursor.right",
        "Cursor right",
        "cmdline",
        NONE,
        true,
    ),
    meta("cmdline.word.left", "Word left", "cmdline", NONE, true),
    meta("cmdline.word.right", "Word right", "cmdline", NONE, true),
    meta("cmdline.home", "Line start", "cmdline", NONE, false),
    meta("cmdline.end", "Line end", "cmdline", NONE, false),
    meta("cmdline.backspace", "Delete left", "cmdline", NONE, true),
    meta("cmdline.delete", "Delete right", "cmdline", NONE, true),
    meta(
        "cmdline.del.word_left",
        "Delete word left",
        "cmdline",
        NONE,
        true,
    ),
    meta(
        "cmdline.del.word_right",
        "Delete word right",
        "cmdline",
        NONE,
        true,
    ),
    meta(
        "cmdline.del.to_end",
        "Delete to end",
        "cmdline",
        NONE,
        false,
    ),
    meta("cmdline.clear", "Clear line", "cmdline", NONE, false),
    meta("cmdline.undo", "Undo", "cmdline", NONE, true),
    meta("cmdline.redo", "Redo", "cmdline", NONE, true),
    meta(
        "cmdline.exec",
        "Run command",
        "cmdline",
        &[arg("detached", ValueKind::Bool, false)],
        false,
    ),
    meta("cmdline.history", "Command history", "cmdline", NONE, false),
    meta(
        "cmdline.history.prev",
        "Previous command",
        "cmdline",
        NONE,
        true,
    ),
    meta(
        "cmdline.history.next",
        "Next command",
        "cmdline",
        NONE,
        true,
    ),
    meta("cmdline.complete", "Complete", "cmdline", NONE, false),
    meta(
        "cmdline.complete_native",
        "Shell completion",
        "cmdline",
        NONE,
        false,
    ),
    meta(
        "cmdline.insert_name",
        "Insert name",
        "cmdline",
        &[
            arg("what", ValueKind::Str, true),
            arg("side", ValueKind::Str, false),
        ],
        false,
    ),
    meta(
        "cmdline.insert_selection",
        "Insert selected names",
        "cmdline",
        NONE,
        false,
    ),
    meta(
        "cmdline.apply",
        "Apply to selection",
        "cmdline",
        NONE,
        false,
    ),
    meta("cmdline.copy", "Copy command line", "cmdline", NONE, false),
    // Список над cmdline: история Alt+F8 и варианты комплита (S6-16, S6-17).
    meta("cmdline.menu.up", "List: up", "cmdline", NONE, true),
    meta("cmdline.menu.down", "List: down", "cmdline", NONE, true),
    meta(
        "cmdline.menu.page_up",
        "List: page up",
        "cmdline",
        NONE,
        true,
    ),
    meta(
        "cmdline.menu.page_down",
        "List: page down",
        "cmdline",
        NONE,
        true,
    ),
    meta(
        "cmdline.menu.accept",
        "List: insert",
        "cmdline",
        &[arg("exec", ValueKind::Bool, false)],
        false,
    ),
    meta(
        "cmdline.menu.delete",
        "List: delete",
        "cmdline",
        NONE,
        false,
    ),
    meta("cmdline.menu.lock", "List: pin", "cmdline", NONE, false),
    meta(
        "cmdline.menu.scope",
        "List: all shells",
        "cmdline",
        NONE,
        false,
    ),
    meta("cmdline.menu.close", "List: close", "cmdline", NONE, false),
    meta(
        "cmdline.paste",
        "Paste",
        "cmdline",
        &[arg("text", ValueKind::Str, false)],
        false,
    ),
];

/// Состояние терминала в `AppState.ext` (S6 §1).
#[allow(clippy::struct_excessive_bools, reason = "независимые признаки")]
#[derive(Debug, Default)]
pub struct TermState {
    pub config: TermConfig,
    /// Пул сеансов (S6-10).
    pub sessions: manager::Manager,
    /// Сетка, показанная явно вместо активного сеанса (сцены `--screens`, S6-06).
    pub preview: Option<std::sync::Arc<vt::shared::TermShared>>,
    /// Строка ввода (S6-15).
    pub line: line::LineEditor,
    /// Панели спрятал автопоказ (S6-08): по концу команды или выходу из alt-screen —
    /// вернуть.
    pub(crate) auto_hidden: bool,
    /// Отправленная команда ждёт автопоказа (S6 §3.3): момент submit.
    pub(crate) submitted: Option<Instant>,
    /// Альтернативный экран активного сеанса на прошлом pump.
    pub(crate) alt: bool,
    /// Вид «экран» на прошлом pump ([`TermState::screen_view`]).
    pub(crate) screen: bool,
    /// Размер области терминала ждёт дебаунса (S6 §3.1).
    pub(crate) resize: Option<(sys::conpty::PtySize, Instant)>,
    /// Поиск в выводе (S6-21): шаблон и прошлое совпадение.
    pub(crate) find: Option<blocks::Find>,
    /// Истории команд по shell (S6-16).
    pub history: history::Store,
    /// Список над cmdline: Alt+F8 или варианты Tab (S6-16, S6-17).
    pub menu: Option<menu::Menu>,
    /// Ctrl+E/Ctrl+X: набранное до листания и номер показанной записи.
    pub(crate) hist_pos: Option<(String, usize)>,
    /// Листинг последнего каталога комплита (S6-17).
    pub(crate) dir_cache: Option<complete::DirCache>,
    /// Команды PATH; `None` — ещё не сканированы.
    pub(crate) path_cmds: Option<complete::PathCommands>,
    pub(crate) path_scan: bool,
    /// Tab ждёт листинга каталога: строка и курсор в момент Tab.
    pub(crate) complete_wait: Option<(String, usize)>,
    /// Сервер комплита PowerShell (S6-18): программа и живой процесс.
    pub(crate) pwsh: Option<(PathBuf, pwsh_server::Server)>,
    /// Сервер не стартовал (нет pwsh, CLM) — до перезапуска не пробовать.
    pub(crate) pwsh_off: bool,
    pub(crate) next_native: u64,
    /// Ждём ответ сервера комплита.
    pub(crate) native_wait: Option<line_cmds::NativeWait>,
    /// Многострочная вставка ждёт ответа «Вставить?» (shell без bracketed paste).
    pub(crate) paste: Option<(
        urfin_core::PromptId,
        crossbeam_channel::Receiver<urfin_core::Reply>,
        String,
    )>,
}

impl TermState {
    /// Вид «экран» за спрятанными панелями (S6 §3.2): alt-screen или сеанс без
    /// интеграции — сетка во всю область, cmdline скрыт, ввод raw. Показанная явно
    /// сетка (`preview`) — тоже экран.
    pub fn screen_view(&self) -> bool {
        self.preview.is_some()
            || self.sessions.active().is_some_and(|s| {
                !s.integrated || s.running || s.raw || s.shared().is_some_and(|sh| sh.alt_screen())
            })
    }

    /// Промпт shell для cmdline (S6 §3.2): последняя строка промпта — ячейки от `A`
    /// (или начала строки) до `B`; `None` — нет интеграции или команда идёт.
    pub fn prompt_cells(&self, d: &vt::grid::Defaults) -> Option<Vec<vt::grid::Cell>> {
        let slot = self
            .sessions
            .active()
            .filter(|s| s.integrated && !s.running)?;
        let (a, b) = slot.prompt?;
        slot.shared()?.prompt_cells(a, b?, d)
    }

    /// Shell активного сеанса (ключ истории).
    pub fn shell_key(&self) -> Option<compact_str::CompactString> {
        self.sessions.active().map(|s| s.shell.clone())
    }

    /// Ghost-текст (S6 §6.2): хвост строки истории за набранным; курсор в конце.
    pub fn ghost(&self) -> Option<&str> {
        if !self.config.complete.ghost
            || self.menu.is_some()
            || self.line.cursor() != self.line.text().len()
        {
            return None;
        }
        let h = self.history.get(&self.shell_key()?)?;
        complete::ghost(h, self.line.text())
    }

    /// Строка начала промпта на экране: transcript рисует строки выше неё (S6 §3.2).
    pub fn prompt_row(&self) -> Option<usize> {
        let slot = self
            .sessions
            .active()
            .filter(|s| s.integrated && !s.running)?;
        let (a, _) = slot.prompt?;
        a.screen_row(slot.shared()?.history_size())
    }
}

/// Терминал показан (Ctrl+O): shell — в каталог активной панели, тихо (S6 §5.1).
/// Без хуков (cmd) — до submit: видимый cd на каждый Ctrl+O был бы мусором в выводе.
pub fn sync_cwd(cx: &mut Cx<'_>) {
    let want = manager::session_cwd(&cx.state.active_tab().loc);
    let rt = cx.rt;
    if let Some(st) = cx.state.ext.get_mut::<TermState>() {
        let TermState {
            config, sessions, ..
        } = st;
        sessions.sync_quiet(config, rt, want);
    }
}

/// Флаг keymap `cmdline_empty` (S3: Enter на пустой строке — `panel.enter`).
pub fn cmdline_empty(st: &AppState) -> bool {
    st.ext.get::<TermState>().is_none_or(|t| t.line.is_empty())
}

/// Флаг keymap `cmdline_menu`: открыт список над cmdline (Alt+F8, варианты Tab) —
/// стрелки, Enter, Del, Ins, Esc идут в него.
pub fn cmdline_menu(st: &AppState) -> bool {
    st.ext.get::<TermState>().is_some_and(|t| t.menu.is_some())
}

/// Флаг keymap `cmdline_ghost`: есть ghost-текст — → и End принимают его.
pub fn cmdline_ghost(st: &AppState) -> bool {
    st.ext
        .get::<TermState>()
        .is_some_and(|t| t.ghost().is_some())
}

/// Кто открывает файлы по Enter (S6-19): регистрируется в `Services.open_resolvers`.
pub fn open_resolver() -> std::sync::Arc<dyn urfin_core::OpenResolver> {
    std::sync::Arc::new(open::TermOpen)
}

/// Просьба к окну показать (`true`) или спрятать панели (`term.panels`): видимость
/// панелей — состояние UI.
pub(crate) fn panels(cx: &mut Cx<'_>, show: bool) {
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("term.panels"),
        args: Args::default().with("show", show),
    });
}

/// Дебаунс ресайза (S6 §3.1): `ConPTY` перерисовывает экран на каждый.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(50);

fn not_yet(cmd: &Command) -> CmdError {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("what", cmd.id.as_str().to_owned());
    CmdError::Rejected(urfin_i18n::current().fmt("term-not-yet", &a).into())
}

/// Handler `term`.
#[derive(Debug, Default)]
pub struct TermHandler {
    /// Конфиг до первого pump: тогда уходит в `TermState` (у `Core` нет доступа
    /// к состоянию до цикла).
    pending: Option<(TermConfig, Option<PathBuf>)>,
}

impl TermHandler {
    /// `term_dir` — `state_dir/term` (кэш shells, статистика выбора); `None` — без диска.
    pub fn new(config: TermConfig, term_dir: Option<PathBuf>) -> TermHandler {
        TermHandler {
            pending: Some((config, term_dir)),
        }
    }
}

impl Handler for TermHandler {
    fn namespace(&self) -> &'static str {
        "term"
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some((config, dir)) = self.pending.take() {
            let st = TermState {
                history: history::Store {
                    dir: dir.as_ref().map(|d| d.join("history")),
                    ..history::Store::default()
                },
                config,
                sessions: manager::Manager::new(dir),
                ..TermState::default()
            };
            st.history.spawn_load(cx.rt, st.config.history.max as usize);
            cx.state.ext.insert(st);
        }
        let rt = cx.rt;
        let now = Instant::now();
        let home = manager::session_cwd(&cx.state.active_tab().loc);
        let Some(st) = cx.state.ext.get_mut::<TermState>() else {
            return;
        };
        st.sessions.pump(&st.config, rt, home);
        // Нативные истории (S6 §6.1) — когда shells известны; PATH — для комплита.
        if !st.history.imported && st.history.dir.is_some() && !st.sessions.shells().is_empty() {
            if st.config.history.import_native {
                let shells = st.sessions.shells().to_vec();
                st.history
                    .spawn_import(rt, &shells, st.config.history.max as usize);
            } else {
                st.history.imported = true;
            }
            line_cmds::scan_path(st, rt);
        }
        if let Some((size, at)) = st.resize
            && now >= at
        {
            st.resize = None;
            st.sessions.resize(size);
        }
        // Автопоказ (S6 §3.3): команда идёт дольше `auto_show_ms` (видно только с
        // интеграцией) или включила alt-screen — панели прячутся; alt-screen выключен —
        // спрятанные им панели возвращаются.
        let mut show = None;
        if let Some(at) = st.submitted
            && now.duration_since(at) >= Duration::from_millis(u64::from(st.config.auto_show_ms))
        {
            st.submitted = None;
            if st
                .sessions
                .active()
                .is_some_and(|s| s.integrated && s.running)
            {
                show = Some(false);
            }
        }
        let alt = st
            .sessions
            .active()
            .and_then(manager::Slot::shared)
            .is_some_and(|s| s.alt_screen());
        if alt != st.alt {
            st.alt = alt;
            if alt {
                show = Some(false);
            } else if st.auto_hidden && st.config.return_to_panels {
                show = Some(true);
            }
        }
        match show {
            Some(false) if !st.auto_hidden => {
                st.auto_hidden = true;
                panels(cx, false);
            }
            Some(true) => {
                st.auto_hidden = false;
                panels(cx, true);
            }
            _ => {}
        }
        // Вид сменился (команда пошла или кончилась, alt-screen, интеграция ответила):
        // окно переоценит фокус (экран — ввод в терминал, transcript — в cmdline).
        if let Some(st) = cx.state.ext.get_mut::<TermState>() {
            let screen = st.screen_view();
            if screen != st.screen {
                st.screen = screen;
                cx.ui(UiEvent::Ext {
                    id: CommandId::from_static("term.panels"),
                    args: Args::default().with("refocus", true),
                });
            }
        }
        let silent = cx
            .state
            .ext
            .get_mut::<TermState>()
            .map(|st| st.sessions.silent_integrations())
            .unwrap_or_default();
        for shell in silent {
            cx.toast(
                urfin_core::Level::Warn,
                manager::tr("term-integ-silent", &[("shell", &shell)]),
            );
        }
        poll_paste(cx);
    }

    fn on_quit(&mut self, cx: &mut Cx<'_>) -> QuitDecision {
        match cx.state.ext.get_mut::<TermState>() {
            Some(st) => st.sessions.quit_decision(),
            None => QuitDecision::Allow,
        }
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &TERM_COMMANDS
    }

    #[allow(clippy::too_many_lines, reason = "таблица команд `term.*`")]
    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        if cx.state.ext.get::<TermState>().is_none() {
            return Err(not_yet(cmd));
        }
        // Нужен весь `cx` (буфер обмена у окна, вопросы, задачи).
        match cmd.id.as_str() {
            "term.copy" => {
                raw::copy(cx);
                return Ok(());
            }
            "term.paste" => return raw::paste(cx, cmd.args.str("text")),
            "term.mouse" => return raw::mouse(cx, &cmd.args),
            "term.open" => return open::handle(cx, &cmd.args),
            "term.copy_output" => {
                blocks::copy_output(cx);
                return Ok(());
            }
            // Ctrl+C с выделением копирует и снимает его, без выделения — `^C` (S6 §3.4).
            "term.key"
                if cmd
                    .args
                    .str("key")
                    .is_some_and(|k| k.eq_ignore_ascii_case("c"))
                    && cmd.args.str("mods") == Some("ctrl")
                    && raw::copy(cx) =>
            {
                return Ok(());
            }
            _ => {}
        }
        let cwd = manager::session_cwd(&cx.state.active_tab().loc);
        let rt = cx.rt;
        let Some(st) = cx.state.ext.get_mut::<TermState>() else {
            return Err(not_yet(cmd));
        };
        match cmd.id.as_str() {
            "term.prev_prompt" | "term.next_prompt" => {
                blocks::jump(st, cmd.id.as_str() == "term.prev_prompt");
                return Ok(());
            }
            "term.find" => return blocks::find(st, &cmd.args),
            "term.select_all" => {
                raw::select_all(st);
                return Ok(());
            }
            "term.scroll" => {
                raw::scroll(st, &cmd.args);
                return Ok(());
            }
            "term.input" | "term.key" => raw::before_input(st),
            _ => {}
        }
        let TermState {
            config,
            sessions,
            resize,
            ..
        } = st;
        let a = &cmd.args;
        match cmd.id.as_str() {
            "term.shell" => sessions.open(config, rt, a.str("id"), true, cwd),
            "term.new" => sessions.open(config, rt, a.str("shell"), false, cwd),
            "term.next" => sessions.cycle(config, 1),
            "term.prev" => sessions.cycle(config, -1),
            "term.close" => {
                let id = a
                    .int("id")
                    .map(|i| session::SessionId::try_from(i).unwrap_or(0));
                sessions.close(config, id)
            }
            "term.restart" => sessions.restart(config, rt),
            "term.kill" => sessions.kill(),
            "term.input" => {
                let text = a.str("text").unwrap_or_default();
                sessions.write(config, rt, text.as_bytes().to_vec(), false)
            }
            "term.key" => {
                let key = a.str("key").unwrap_or_default();
                let bytes = manager::key_bytes(key, a.str("mods"), sessions.input_modes())
                    .ok_or_else(|| CmdError::Arg {
                        name: "key".into(),
                        msg: "unknown key".into(),
                    })?;
                // Enter после «процесс завершён» только перезапускает.
                sessions.write(config, rt, bytes, key.eq_ignore_ascii_case("enter"))
            }
            "term.resize" => {
                let dim = |k: &str| {
                    a.int(k)
                        .and_then(|v| u16::try_from(v).ok())
                        .filter(|v| *v >= 2)
                        .ok_or_else(|| CmdError::Arg {
                            name: k.into(),
                            msg: "2..65535".into(),
                        })
                };
                let size = sys::conpty::PtySize {
                    cols: dim("cols")?,
                    rows: dim("rows")?,
                };
                if sessions.area() == Some(size) {
                    *resize = None;
                } else if sessions.area().is_none() {
                    // Первая раскладка окна — сразу: сеанс прогрева стартует уже в
                    // своём размере.
                    sessions.resize(size);
                } else {
                    *resize = Some((size, Instant::now() + RESIZE_DEBOUNCE));
                    rt.waker().wake_after(RESIZE_DEBOUNCE);
                }
                Ok(())
            }
            "term.toggle" => {
                cx.ui(UiEvent::Ext {
                    id: CommandId::from_static("term.panels"),
                    args: Args::default().with("toggle", true),
                });
                Ok(())
            }
            _ => Err(not_yet(cmd)),
        }
    }
}

/// Handler `cmdline`.
#[derive(Debug, Default)]
pub struct CmdlineHandler;

impl Handler for CmdlineHandler {
    fn namespace(&self) -> &'static str {
        "cmdline"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &CMDLINE_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        // История, комплит, список над cmdline (S6-16, S6-17).
        if let Some(r) = line_cmds::handle(cmd, cx) {
            return r;
        }
        let a = &cmd.args;
        // Имена из панелей — до заимствования состояния терминала.
        let names = match cmd.id.as_str() {
            "cmdline.insert_name" => Some(insert_name(cx.state, a)?),
            "cmdline.insert_selection" => Some(insert_selection(cx.state)),
            _ => None,
        };
        // Каталог активной панели — для ленивой синхронизации cwd при submit.
        let want = manager::session_cwd(&cx.state.active_tab().loc);
        let in_cmdline = cx.state.focus == urfin_core::Focus::Cmdline;
        let rt = cx.rt;
        let Some(st) = cx.state.ext.get_mut::<TermState>() else {
            return Err(not_yet(cmd));
        };
        let line = &mut st.line;
        match cmd.id.as_str() {
            "cmdline.insert" => line.insert(&printable(a.str("text").unwrap_or_default())),
            "cmdline.cursor.left" => line.left(),
            "cmdline.cursor.right" => line.right(),
            "cmdline.word.left" => line.word_left(),
            "cmdline.word.right" => line.word_right(),
            "cmdline.home" => line.home(),
            "cmdline.end" => line.end(),
            "cmdline.backspace" => line.backspace(),
            "cmdline.delete" => line.delete(),
            "cmdline.del.word_left" => line.delete_word_left(),
            "cmdline.del.word_right" => line.delete_word_right(),
            "cmdline.del.to_end" => line.delete_to_end(),
            "cmdline.clear" => line.clear(),
            "cmdline.undo" => line.undo(),
            "cmdline.redo" => line.redo(),
            "cmdline.insert_name" | "cmdline.insert_selection" => {
                let text = names.unwrap_or_default();
                if !text.is_empty() {
                    // Как в Far: имя и пробел после него.
                    line.insert(&format!("{text} "));
                }
            }
            "cmdline.paste" => match a.str("text") {
                Some(t) => {
                    let t = t.trim_end_matches(['\r', '\n']);
                    if t.contains('\n') {
                        return paste_lines(cx, t);
                    }
                    line.insert(&printable(t));
                }
                None => cx.ui(UiEvent::Ext {
                    id: CommandId::from_static("clip.read"),
                    args: Args::default().with("then", "cmdline.paste"),
                }),
            },
            "cmdline.copy" => {
                let text = line.text().to_owned();
                cx.ui(UiEvent::Ext {
                    id: CommandId::from_static("clip.write"),
                    args: Args::default().with("text", text),
                });
            }
            "cmdline.exec" if a.bool("detached").unwrap_or(false) => {
                exec_detached(cx, want);
                return Ok(());
            }
            "cmdline.exec" => {
                if line.is_empty() {
                    // За спрятанными панелями пустой Enter — новый промпт shell (Far).
                    if in_cmdline {
                        st.sessions.write(&st.config, rt, b"\r".to_vec(), false)?;
                    }
                    return Ok(());
                }
                // Нет сеанса — строка остаётся набранной.
                let text = line.text().to_owned();
                run_line(st, rt, &text, want)?;
                st.line.take();
            }
            _ => return Err(not_yet(cmd)),
        }
        Ok(())
    }
}

/// Строку — в активный сеанс как команду (S6 §6): ленивая синхронизация cwd,
/// `текст\r`, «выполняется», таймер автопоказа, история.
pub(crate) fn run_line(
    st: &mut TermState,
    rt: &urfin_core::Runtime,
    text: &str,
    want: Option<PathBuf>,
) -> Result<(), CmdError> {
    let TermState {
        config,
        sessions,
        submitted,
        ..
    } = st;
    let bytes = submit_bytes(text, sessions.input_modes());
    sessions.submit(config, rt, bytes, want)?;
    sessions.mark_running(Some(text.to_owned()));
    *submitted = Some(Instant::now());
    rt.waker()
        .wake_after(Duration::from_millis(u64::from(config.auto_show_ms)));
    if let Some(shell) = st.shell_key() {
        history::record(st, rt, &shell, text);
    }
    Ok(())
}

/// Enter по консольной программе или скрипту (S6 §7): в активном сеансе, из каталога
/// файла. Сеанса нет — запустить shell по умолчанию.
pub(crate) fn run_file(
    cx: &mut Cx<'_>,
    name: &str,
    kind: open::Kind,
    pwsh: &str,
    dir: Option<PathBuf>,
) -> Result<(), CmdError> {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    if st.sessions.active().is_none() {
        st.sessions.open(&st.config, rt, None, true, dir.clone())?;
    }
    let shell = st
        .sessions
        .active_spec()
        .map(|s| s.kind.clone())
        .ok_or_else(|| CmdError::Rejected(manager::tr("term-no-session", &[]).into()))?;
    let text = open::exec_line(name, kind, &shell, pwsh);
    run_line(st, rt, &text, dir)
}

/// Shift+Enter в cmdline (S6 §6): строка — в новом окне консоли ОС тем же shell, сеанс
/// свободен.
fn exec_detached(cx: &mut Cx<'_>, want: Option<PathBuf>) {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    if st.line.is_empty() {
        return;
    }
    let (program, kind) = st.sessions.active_spec().map_or_else(
        || (PathBuf::from("cmd.exe"), shells::ShellKind::Cmd),
        |s| (s.program.clone(), s.kind.clone()),
    );
    let text = st.line.take();
    if let Some(shell) = st.shell_key() {
        history::record(st, rt, &shell, &text);
    }
    let spec = open::detached_spec(&program, &kind, &text, want);
    open::spawn_shell(cx, spec, &program);
}

/// Многострочная вставка (S6 §3.4): не в однострочную cmdline, а в терминал — панели
/// гаснут, текст показывает и правит сам shell. Shell без bracketed paste (cmd)
/// выполнил бы каждую строку сразу — сначала вопрос.
fn paste_lines(cx: &mut Cx<'_>, text: &str) -> Result<(), CmdError> {
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    let shell = st
        .sessions
        .active()
        .map(|s| s.shell.to_string())
        .ok_or_else(|| CmdError::Rejected(manager::tr("term-no-session", &[]).into()))?;
    if st.sessions.input_modes().bracketed_paste {
        return paste_now(cx, text);
    }
    if let Some((id, ..)) = st.paste.take() {
        cx.state.prompts.cancel(id);
    }
    let i18n = urfin_i18n::current();
    let n = text.lines().count().to_string();
    let choice = |id: &str, key: &str, k: char| urfin_core::Choice {
        id: id.into(),
        label: i18n.get(key).into(),
        key: k,
    };
    let spec = urfin_core::PromptSpec {
        anchor: urfin_core::Anchor::Cmdline,
        title: manager::tr("term-paste-title", &[("n", &n), ("shell", &shell)]),
        body: manager::tr("term-paste-body", &[("shell", &shell)]),
        choices: [
            choice("paste", "term-paste-insert", 'p'),
            choice("cancel", "term-paste-cancel", 'c'),
        ]
        .into_iter()
        .collect(),
        default: 0,
        input: None,
        allow_all: false,
        detail: None,
        secret: false,
    };
    let (id, rx) = cx.state.prompts.open(spec, None);
    if let Some(st) = cx.state.ext.get_mut::<TermState>() {
        st.paste = Some((id, rx, text.to_owned()));
    }
    Ok(())
}

/// Вставить строки в PTY (в скобках вставки, если shell их включил) и показать
/// терминал.
fn paste_now(cx: &mut Cx<'_>, text: &str) -> Result<(), CmdError> {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    let modes = st.sessions.input_modes();
    let body = text.replace("\r\n", "\n").replace('\n', "\r");
    let mut bytes = Vec::with_capacity(body.len() + 12);
    if modes.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(body.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
    } else {
        bytes.extend_from_slice(body.as_bytes());
    }
    st.sessions.write(&st.config, rt, bytes, false)?;
    panels(cx, false);
    Ok(())
}

/// Ответ на «Вставить строки?»: да — в терминал, нет или закрыто — забыть.
fn poll_paste(cx: &mut Cx<'_>) {
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    let Some((_, rx, _)) = &st.paste else {
        return;
    };
    let reply = match rx.try_recv() {
        Ok(r) => Some(r),
        Err(crossbeam_channel::TryRecvError::Empty) => return,
        Err(crossbeam_channel::TryRecvError::Disconnected) => None,
    };
    let Some((_, _, text)) = st.paste.take() else {
        return;
    };
    if reply.is_some_and(|r| r.choice == "paste")
        && let Err(e) = paste_now(cx, &text)
    {
        cx.toast(urfin_core::Level::Error, e.to_string());
    }
}

/// Байты submit (S6 §6): текст и `\r`; многострочный при `BRACKETED_PASTE` —
/// в скобках вставки, чтобы shell не выполнял строки по одной.
fn submit_bytes(text: &str, modes: vt::input::Modes) -> Vec<u8> {
    let multi = text.contains('\n');
    let body = text.replace("\r\n", "\n");
    let mut out = Vec::with_capacity(body.len() + 16);
    if multi && modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(body.replace('\n', "\r").as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(body.replace('\n', "\r").as_bytes());
    }
    out.push(b'\r');
    out
}

/// Набранный текст без управляющих символов (Tab и переводы строк — пробел).
fn printable(t: &str) -> String {
    t.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

fn side_of(st: &AppState, a: &Args) -> urfin_core::Side {
    match a.side("side") {
        Some(SideRef::Left) => urfin_core::Side::Left,
        Some(SideRef::Right) => urfin_core::Side::Right,
        Some(SideRef::Passive) => st.active.other(),
        Some(SideRef::Active) | None => st.active,
    }
}

/// `cmdline.insert_name what=name|full|path` (S6 §6.3): имя под курсором, полный путь
/// к нему или путь панели — в кавычках, если нужно.
fn insert_name(st: &AppState, a: &Args) -> Result<String, CmdError> {
    let tab = st.panel(side_of(st, a)).tab();
    let entry = match tab.current() {
        Some(urfin_core::Row::Entry { entry, .. }) => Some(entry.name.as_str()),
        _ => None,
    };
    let path = tab.loc.path.display().to_string();
    let text = match a.str("what").unwrap_or("name") {
        "name" => entry.map(str::to_owned).unwrap_or_default(),
        "full" => entry.map_or(path, |n| tab.loc.path.join(n).display().to_string()),
        "path" => {
            // Как в Far: путь каталога с разделителем на конце.
            if path.ends_with(['\\', '/']) {
                path
            } else {
                format!("{path}{}", std::path::MAIN_SEPARATOR)
            }
        }
        other => {
            return Err(CmdError::Arg {
                name: "what".into(),
                msg: format!("name|full|path, not {other}").into(),
            });
        }
    };
    Ok(if text.is_empty() {
        text
    } else {
        line::quote(&text)
    })
}

/// `cmdline.insert_selection`: выделенные имена через пробел; без выделения — имя
/// под курсором.
fn insert_selection(st: &AppState) -> String {
    let tab = st.active_tab();
    let names = tab.selection.names(&tab.listing);
    if names.is_empty() {
        return match tab.current() {
            Some(urfin_core::Row::Entry { entry, .. }) => line::quote(&entry.name),
            _ => String::new(),
        };
    }
    names
        .iter()
        .map(|n| line::quote(n))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Регистрирует handlers `term` и `cmdline`, состояние — в `AppState.ext`.
/// Конфиг — из `config.section("term")`. Без каталога состояния: тесты.
pub fn register(core: &mut Core, config: TermConfig) {
    register_with(core, config, None);
}

/// Как [`register`], с каталогом `state_dir/term` (кэш shells, статистика выбора).
pub fn register_with(core: &mut Core, config: TermConfig, term_dir: Option<PathBuf>) {
    core.register(Box::new(TermHandler::new(config, term_dir)));
    core.register(Box::new(CmdlineHandler));
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_wraps_multiline_in_bracketed_paste() {
        let plain = vt::input::Modes::default();
        assert_eq!(submit_bytes("dir /b", plain), b"dir /b\r");
        assert_eq!(submit_bytes("a\nb", plain), b"a\rb\r");
        let bp = vt::input::Modes {
            bracketed_paste: true,
            ..plain
        };
        assert_eq!(submit_bytes("a\r\nb", bp), b"\x1b[200~a\rb\x1b[201~\r");
        assert_eq!(printable("a\tb\u{7}c"), "a bc");
    }

    #[test]
    fn version_smoke() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert_ne!(super::version(), "");
    }

    /// Приёмка S6-01: реестр видит все команды §9, `validate` проходит.
    #[test]
    fn registry_has_all_commands() {
        let mut reg = urfin_core::CommandRegistry::new();
        reg.register(&TERM_COMMANDS).unwrap();
        reg.register(&CMDLINE_COMMANDS).unwrap();
        for c in [
            "term.shell id=pwsh",
            "term.key key=Up mods=ctrl",
            "cmdline.insert text=dir",
            "cmdline.insert_name what=path side=left",
            "cmdline.exec detached=true",
        ] {
            let c = Command::parse(c).unwrap();
            assert_eq!(reg.validate(&c), Ok(()), "{c}");
        }
        for id in [
            "term.toggle",
            "term.exec",
            "term.key",
            "cmdline.exec",
            "cmdline.insert_name",
            "cmdline.del.to_end",
        ] {
            assert!(reg.get(id).is_some(), "{id}");
        }
    }
}
