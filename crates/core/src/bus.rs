//! Шина Core (ADR-0001, ADR-0002): владеет состоянием в UI-потоке, роутит команды
//! по namespace, применяет `Msg` в `pump` с бюджетом времени.

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::cmd::{CmdError, Command, CommandMeta, CommandRegistry};
use crate::config::{Config, ConfigWarning};
use crate::handlers::{
    AppHandler, JobHandler, PanelHandler, PromptHandler, SelHandler, TabHandler, job_id,
};
use crate::ids::TabId;
use crate::loc::Location;
use crate::msg::{Cx, MsgInbox, WakeFlag, Waker};
use crate::nav;
use crate::paths::{Paths, write_atomic};
use crate::prompt::PromptSpec;
use crate::runtime::{Lane, Runtime};
use crate::services::{Services, TabEvent};
use crate::session::{Session, WindowGeom};
use crate::settings::{self, Writer};
use crate::state::{AppState, Focus, Level, Panel, UiEvent};
use crate::tab::Tab;
use crate::text::tr;

/// Обработчик namespace. Живёт в UI-потоке, не делает I/O и не блокируется.
pub trait Handler: 'static {
    fn namespace(&self) -> &'static str;
    /// Команды handler; id каждой — в его namespace.
    fn commands(&self) -> &'static [CommandMeta];
    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError>;

    /// Опрос перед выходом (S0 v2 №13); `app.quit` опрашивает все handlers до shutdown.
    fn on_quit(&mut self, _cx: &mut Cx<'_>) -> QuitDecision {
        QuitDecision::Allow
    }

    /// Начало каждого pump, до команд и `Msg` — например, разбор `ViewInbox` S7
    /// (S0 v5 №4). `rev` шина здесь не поднимает: при изменениях — `cx.state.touch()`.
    fn on_pump(&mut self, _cx: &mut Cx<'_>) {}
}

/// Ответ handler на выход.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "форма из S0 v2 №13; значение возвращается раз за сеанс, при выходе"
)]
pub enum QuitDecision {
    Allow,
    /// Нельзя выйти молча: показать запрос (например, «идёт копирование»).
    Veto(PromptSpec),
}

/// Начальные данные Core.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Boot {
    pub paths: Paths,
    pub config: Config,
    /// Предупреждения `Config::load`; показываются баннером.
    pub warnings: Vec<ConfigWarning>,
    /// Сессия прошлого запуска; применяется при `general.restore_session`.
    pub session: Option<Session>,
    /// Crash-файл прошлого запуска (маркер падения, S1 §2.9): баннер с кнопками.
    pub crashed_last_time: Option<PathBuf>,
    /// Табы, если сессии нет.
    pub left: Location,
    pub right: Location,
}

impl Boot {
    /// Для тестов и `--smoke`: конфиг по умолчанию, без сессии, каталоги —
    /// во временном каталоге (профиль пользователя не трогается). Приложение
    /// собирает `Boot` из `Paths::detect()`, `Config::load` и `Session::load`.
    pub fn new(left: Location, right: Location) -> Self {
        Boot {
            paths: Paths::in_dir(std::env::temp_dir().join("urfin-ephemeral")),
            config: Config::default(),
            warnings: Vec::new(),
            session: None,
            crashed_last_time: None,
            left,
            right,
        }
    }
}

/// Итог `pump`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Pump {
    /// Бюджет кончился, а работа осталась: нужен ещё кадр.
    pub more: bool,
    /// `state.rev` изменился.
    pub changed: bool,
}

/// Канал закрыт: Core завершён.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("command channel closed")]
pub struct Closed;

/// Отправка команд из не-UI потоков (term, watcher, IPC). Clone + Send.
#[derive(Clone)]
pub struct CommandSender {
    tx: Sender<Command>,
    flag: Arc<WakeFlag>,
}

impl CommandSender {
    /// Не блокирует; будит UI через общий с `MsgSink` флаг.
    pub fn send(&self, c: Command) -> Result<(), Closed> {
        self.tx.send(c).map_err(|_| Closed)?;
        self.flag.raise();
        Ok(())
    }
}

impl fmt::Debug for CommandSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSender")
            .field("queued", &self.tx.len())
            .finish_non_exhaustive()
    }
}

/// Handler дольше этого в debug-сборке даёт `tracing::warn!` (S1 §0).
const SLOW_HANDLER: Duration = Duration::from_millis(1);

/// Состояние и шина. Живёт в UI-потоке.
pub struct Core {
    state: AppState,
    rt: Runtime,
    inbox: MsgInbox,
    services: Services,
    handlers: Vec<Box<dyn Handler>>,
    by_ns: FxHashMap<&'static str, usize>,
    registry: CommandRegistry,
    cmd_tx: Sender<Command>,
    cmd_rx: Receiver<Command>,
    queue: VecDeque<Command>,
    paths: Paths,
    window: Option<WindowGeom>,
    /// Время последнего изменения, попадающего в сессию (ещё не записанного).
    session_changed: Option<Instant>,
    session_debounce: Duration,
    /// Последний записанный номер версии сессии: писатель единственный, старые
    /// версии пропускает.
    session_writer: Arc<Mutex<u64>>,
    session_seq: u64,
    #[cfg(test)]
    pub(crate) session_writes: u32,
    /// Запись настроек из UI (S2-22).
    settings: Arc<Writer>,
    pub(crate) settings_debounce: Duration,
    #[cfg(test)]
    pub(crate) settings_flushes: u32,
}

/// Сессия пишется через столько после последнего изменения (S1 §2.8).
const SESSION_DEBOUNCE: Duration = Duration::from_secs(2);

impl Core {
    /// Создаёт Runtime, состояние с одним табом на панель, регистрирует handlers
    /// core (`panel`, `sel`, `tab`, `app`, `job`, `prompt`) и начинает загрузку
    /// обоих табов.
    /// Вызывать в UI-потоке.
    pub fn new(boot: Boot, services: Services, waker: Arc<dyn Waker>) -> Self {
        let (rt, inbox) = Runtime::new(&boot.config.runtime, waker);
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let config = Arc::new(boot.config);
        let mut state = AppState::new(
            Panel::new(Tab::with_config(TabId(1), boot.left.clone(), &config)),
            Panel::new(Tab::with_config(TabId(2), boot.right.clone(), &config)),
        );
        let restore = config.general.restore_session;
        state.set_config(config, &boot.warnings);
        let window = boot.session.as_ref().and_then(|s| s.window);
        if let Some(s) = boot.session.filter(|_| restore) {
            s.apply(&mut state);
        }
        if let Some(file) = &boot.crashed_last_time {
            state.set_crash_banner(file);
        }
        let settings = Arc::new(Writer::new(boot.paths.config_file()));
        let mut core = Core {
            state,
            rt,
            inbox,
            services,
            handlers: Vec::new(),
            by_ns: FxHashMap::default(),
            registry: CommandRegistry::new(),
            cmd_tx,
            cmd_rx,
            queue: VecDeque::new(),
            paths: boot.paths.clone(),
            window,
            session_changed: None,
            session_debounce: SESSION_DEBOUNCE,
            session_writer: Arc::default(),
            session_seq: 0,
            #[cfg(test)]
            session_writes: 0,
            settings: Arc::clone(&settings),
            settings_debounce: settings::DEBOUNCE,
            #[cfg(test)]
            settings_flushes: 0,
        };
        core.register(Box::new(PanelHandler));
        core.register(Box::new(SelHandler));
        core.register(Box::new(TabHandler));
        core.register(Box::new(AppHandler::new(boot.paths, settings)));
        core.register(Box::new(JobHandler));
        core.register(Box::new(PromptHandler));
        let tabs: Vec<_> = core
            .state
            .panels
            .iter_mut()
            .flat_map(|p| {
                let active = p.active;
                p.tabs.iter_mut().enumerate().map(move |(i, t)| {
                    let focus = t.pending_focus.take();
                    (
                        t.id,
                        t.loc.clone(),
                        focus,
                        t.cursor.top,
                        t.restore_up > 0,
                        i == active,
                    )
                })
            })
            .collect();
        for (id, loc, focus, top, restored, shown) in tabs {
            if shown {
                core.services.listing.tab_event(TabEvent::Shown(id));
            }
            let mut cx = Cx::new(&mut core.state, &core.rt, &core.services, &mut core.queue);
            if restored {
                nav::navigate_restored(&mut cx, id, loc, focus, top);
            } else {
                nav::navigate(&mut cx, id, loc, focus, top, false);
            }
        }
        // Стартовые загрузки — не изменение сессии.
        core.state.session.dirty = false;
        core
    }

    /// Паника при дубле namespace, невалидном или повторном id команды и при
    /// команде не из своего namespace — это ошибки сборки приложения.
    pub fn register(&mut self, h: Box<dyn Handler>) {
        let ns = h.namespace();
        assert!(
            !self.by_ns.contains_key(ns),
            "handler namespace '{ns}' registered twice"
        );
        if let Some(m) = h
            .commands()
            .iter()
            .find(|m| m.id.split_once('.').map(|(n, _)| n) != Some(ns))
        {
            panic!("command '{}' is outside handler namespace '{ns}'", m.id);
        }
        if let Err(e) = self.registry.register(h.commands()) {
            panic!("handler '{ns}': {e}");
        }
        self.by_ns.insert(ns, self.handlers.len());
        self.handlers.push(h);
    }

    /// Общая очередь устройств ([`Runtime::devices`]) — для handler'ов при регистрации.
    pub fn devices(&self) -> Arc<crate::DeviceGate> {
        self.rt.devices()
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    #[cfg(test)]
    pub(crate) fn rt(&self) -> &Runtime {
        &self.rt
    }

    #[cfg(test)]
    pub(crate) fn state_mut_for_tests(&mut self) -> &mut AppState {
        &mut self.state
    }

    pub fn sender(&self) -> CommandSender {
        CommandSender {
            tx: self.cmd_tx.clone(),
            flag: self.rt.wake_flag(),
        }
    }

    /// Синхронно выполняет команду и всё, что handlers поставили через
    /// `cx.dispatch` (FIFO). Ошибка — тост, не паника.
    pub fn dispatch(&mut self, cmd: Command) {
        self.queue.push_back(cmd);
        while let Some(c) = self.queue.pop_front() {
            self.exec(&c, None);
        }
    }

    /// Кадр: хуки `on_pump`, затем команды (из `CommandSender` и `cx.dispatch`),
    /// затем `Msg` — пока не кончится работа или `budget` (хотя бы один шаг
    /// выполняется всегда). `panel.refresh` с одним и тем же `tab` выполняется за
    /// pump один раз (S0 v2 №14).
    pub fn pump(&mut self, budget: Duration) -> Pump {
        let start = Instant::now();
        let rev0 = self.state.rev;
        // Сброс WakeFlag до чтения каналов: всё, что придёт позже, разбудит снова.
        self.inbox.begin_pump();
        for h in &mut self.handlers {
            h.on_pump(&mut Cx::new(
                &mut self.state,
                &self.rt,
                &self.services,
                &mut self.queue,
            ));
        }
        self.queue.extend(self.cmd_rx.try_iter());
        let mut refreshed = FxHashSet::default();
        loop {
            if let Some(c) = self.queue.pop_front() {
                self.exec(&c, Some(&mut refreshed));
            } else if let Some(m) = self.inbox.try_recv() {
                let name = m.name();
                let mut cx = Cx::new(&mut self.state, &self.rt, &self.services, &mut self.queue);
                m.apply(&mut cx);
                let kept = cx.rev_kept();
                if !kept {
                    self.state.touch();
                }
                tracing::trace!(msg = name, "applied");
            } else {
                break;
            }
            if start.elapsed() >= budget {
                break;
            }
        }
        nav::swap_overdue(&mut Cx::new(
            &mut self.state,
            &self.rt,
            &self.services,
            &mut self.queue,
        ));
        self.session_tick();
        self.settings_tick();
        Pump {
            more: !self.queue.is_empty() || !self.inbox.is_empty() || !self.cmd_rx.is_empty(),
            changed: self.state.rev != rev0,
        }
    }

    /// Последняя геометрия окна (из сессии или `set_window`).
    pub fn window(&self) -> Option<WindowGeom> {
        self.window
    }

    /// Геометрия окна для сессии.
    pub fn set_window(&mut self, g: WindowGeom) {
        if self.window != Some(g) {
            self.window = Some(g);
            self.state.mark_session_dirty();
        }
    }

    /// Запись сессии через `session_debounce` после последнего изменения:
    /// снимок — здесь (микросекунды), запись — на Bulk-полосе.
    fn session_tick(&mut self) {
        let now = Instant::now();
        if std::mem::take(&mut self.state.session.dirty) {
            self.session_changed = Some(now);
        }
        let force = std::mem::take(&mut self.state.session.force);
        match self.session_changed {
            Some(t) if !force && now - t < self.session_debounce => {
                self.rt
                    .waker()
                    .wake_after(self.session_debounce.saturating_sub(now - t));
            }
            Some(_) => self.write_session(),
            None if force => self.write_session(),
            None => {}
        }
    }

    /// Настройки из UI — в файл через `settings_debounce` после последнего изменения:
    /// очередь — здесь, запись — на Bulk-полосе.
    fn settings_tick(&mut self) {
        let Some(at) = self.state.settings.changed else {
            return;
        };
        let left = self.settings_debounce.saturating_sub(at.elapsed());
        if !left.is_zero() {
            self.rt.waker().wake_after(left);
            return;
        }
        self.settings.enqueue(self.state.settings.take());
        #[cfg(test)]
        {
            self.settings_flushes += 1;
        }
        let w = Arc::clone(&self.settings);
        self.rt
            .spawn(Lane::Bulk, "settings.save", self.rt.token(), move |tcx| {
                if let Err(e) = w.flush() {
                    let _ = tcx.sink.send(settings::failed(w.file(), &e), &tcx.cancel);
                }
            });
    }

    fn write_session(&mut self) {
        self.session_changed = None;
        let text = match toml::to_string(&Session::capture(&self.state, self.window)) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "cannot serialize session");
                return;
            }
        };
        self.session_seq += 1;
        let (seq, writer, file) = (
            self.session_seq,
            self.session_writer.clone(),
            self.paths.session_file(),
        );
        #[cfg(test)]
        {
            self.session_writes += 1;
        }
        self.rt
            .spawn(Lane::Bulk, "session.save", self.rt.token(), move |_| {
                let mut last = writer.lock().unwrap_or_else(PoisonError::into_inner);
                if *last >= seq {
                    return;
                }
                match write_atomic(&file, text.as_bytes()) {
                    Ok(()) => *last = seq,
                    Err(e) => {
                        tracing::warn!(error = %e, file = %file.display(), "cannot save session");
                    }
                }
            });
    }

    pub fn take_ui_events(&mut self) -> Vec<UiEvent> {
        self.state.take_ui_events()
    }

    /// Опрос handlers перед выходом: первый `Veto` в порядке регистрации.
    pub fn quit_decision(&mut self) -> QuitDecision {
        for h in &mut self.handlers {
            let mut cx = Cx::new(&mut self.state, &self.rt, &self.services, &mut self.queue);
            if let QuitDecision::Veto(p) = h.on_quit(&mut cx) {
                return QuitDecision::Veto(p);
            }
        }
        QuitDecision::Allow
    }

    /// Отменяет задачи, останавливает Runtime (см. [`Runtime::shutdown`];
    /// `CommandSender` после этого получает `Closed`) и синхронно пишет сессию и
    /// отложенные настройки. Возвращает записанную сессию.
    pub fn shutdown(mut self, timeout: Duration) -> Session {
        // Токены задач не производны от корня Runtime: отменить явно.
        self.state.jobs.cancel_all();
        let session = Session::capture(&self.state, self.window);
        let pending = self.state.settings.take();
        let Core {
            rt,
            inbox,
            paths,
            session_writer,
            settings,
            ..
        } = self;
        let stuck = rt.shutdown(inbox, timeout);
        if stuck > 0 {
            tracing::warn!(stuck, "threads still running after shutdown");
        }
        // Ждём фоновую запись, если она идёт, и пишем поверх неё.
        let _guard = session_writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Err(e) = session.save(&paths) {
            tracing::error!(error = %e, "cannot save session on exit");
        }
        // Отложенные настройки — сейчас; задача записи, если идёт, допишет первой.
        settings.enqueue(pending);
        if let Err(e) = settings.flush() {
            tracing::error!(error = %e, "cannot save settings on exit");
        }
        session
    }

    fn exec(&mut self, cmd: &Command, refreshed: Option<&mut FxHashSet<Option<i64>>>) {
        if cmd.id.as_str() == "panel.refresh"
            && let Some(seen) = refreshed
            && !seen.insert(cmd.args.int("tab"))
        {
            return;
        }
        if let Err(e) = self.registry.validate(cmd) {
            self.error_toast(cmd, &e);
            return;
        }
        let i = match self.route(cmd) {
            Ok(i) => i,
            Err(e) => {
                self.error_toast(cmd, &e);
                return;
            }
        };
        let t0 = Instant::now();
        let mut cx = Cx::new(&mut self.state, &self.rt, &self.services, &mut self.queue);
        let r = self.handlers[i].handle(cmd, &mut cx);
        let kept = cx.rev_kept();
        let dt = t0.elapsed();
        if cfg!(debug_assertions) && dt > SLOW_HANDLER {
            tracing::warn!(
                cmd = cmd.id.as_str(),
                ?dt,
                "slow handler (> 1 ms): move work to Runtime"
            );
        }
        match r {
            Ok(()) if !kept => self.state.touch(),
            Ok(()) => {}
            Err(e) => self.error_toast(cmd, &e),
        }
        if cmd.id.as_str() == "app.quit" {
            self.finish_quit();
        }
    }

    /// Handler команды: по namespace; `job.retry`/`job.priority` — handler
    /// `owner_ns` задачи (S0 v2 №8).
    fn route(&mut self, cmd: &Command) -> Result<usize, CmdError> {
        let ns = match cmd.id.as_str() {
            "job.retry" | "job.priority" => {
                let id = job_id(cmd)?;
                let owner = self
                    .state
                    .jobs
                    .get(id)
                    .ok_or_else(|| CmdError::arg("id", "no such job"))?
                    .owner_ns;
                if let Some(p) = cmd.args.int("p") {
                    let p = u8::try_from(p).map_err(|_| CmdError::arg("p", "expected 0..255"))?;
                    self.state.jobs.set_priority(id, p);
                }
                owner
            }
            _ => cmd.id.namespace(),
        };
        // Реестр знает только команды зарегистрированных handlers; владелец
        // задачи мог не зарегистрироваться.
        self.by_ns
            .get(ns)
            .copied()
            .ok_or_else(|| CmdError::Unknown(cmd.id.as_str().into()))
    }

    /// `app.quit`: опросить handlers; без вето — `state.quit`. Вето открывает
    /// запрос с фокусом; ответ `choice = "quit"` выходит, отменяя задачи.
    fn finish_quit(&mut self) {
        match self.quit_decision() {
            QuitDecision::Allow => {
                self.state.quit = true;
                self.state.touch();
            }
            QuitDecision::Veto(spec) => {
                if let Some(old) = self.state.quit_prompt.take() {
                    self.state.prompts.cancel(old);
                }
                let (id, _rx) = self.state.prompts.open(spec, None);
                self.state.quit_prompt = Some(id);
                self.state.focus = Focus::Prompt(id);
                self.state.push_ui(UiEvent::FocusChanged(Focus::Prompt(id)));
            }
        }
    }

    fn error_toast(&mut self, cmd: &Command, e: &CmdError) {
        let id = cmd.id.as_str();
        let text = match e {
            CmdError::Unknown(_) => tr("core-unknown-command", &[("id", id)]),
            CmdError::Rejected(msg) => msg.to_string(),
            e => tr(
                "core-command-failed",
                &[("id", id), ("error", &e.to_string())],
            ),
        };
        tracing::debug!(cmd = id, error = %e, "command failed");
        let level = if matches!(e, CmdError::Rejected(_)) {
            Level::Warn
        } else {
            Level::Error
        };
        self.state.push_ui(UiEvent::Toast { level, text });
    }
}

impl fmt::Debug for Core {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Core")
            .field("handlers", &self.by_ns.keys().collect::<Vec<_>>())
            .field("queued", &self.queue.len())
            .field("rev", &self.state.rev)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{Boot, Core, Handler, Pump, QuitDecision};
    use crate::cmd::{ArgSpec, CmdError, Command, CommandMeta, ValueKind};
    use crate::ids::Side;
    use crate::loc::Location;
    use crate::msg::{Cx, Waker, msg};
    use crate::prompt::{Anchor, PromptSpec};
    use crate::services::Services;
    use crate::state::{Level, UiEvent};
    use crate::tab::ListingStatus;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct TestWaker {
        wakes: AtomicUsize,
        pending: Mutex<bool>,
        cv: Condvar,
    }

    impl Waker for TestWaker {
        fn wake(&self) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            *self.pending.lock().unwrap() = true;
            self.cv.notify_all();
        }
        /// Отложенное пробуждение (как `request_repaint_after`): тесты качают pump сами.
        fn wake_after(&self, _d: Duration) {}
    }

    impl TestWaker {
        fn wait(&self, timeout: Duration) -> bool {
            let g = self.pending.lock().unwrap();
            let (mut g, _) = self.cv.wait_timeout_while(g, timeout, |p| !*p).unwrap();
            std::mem::take(&mut *g)
        }
    }

    const fn meta(id: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
        CommandMeta {
            id,
            title: id,
            category: "test",
            args,
            repeatable: false,
        }
    }

    static TEST: [CommandMeta; 4] = [
        meta(
            "test.log",
            &[ArgSpec {
                name: "n",
                kind: ValueKind::Int,
                required: false,
            }],
        ),
        meta("test.chain", &[]),
        meta("test.fail", &[]),
        meta(
            "test.refresh",
            &[ArgSpec {
                name: "tab",
                kind: ValueKind::Int,
                required: false,
            }],
        ),
    ];

    /// Пишет id выполненных команд в общий журнал.
    struct Recorder {
        log: Rc<RefCell<Vec<String>>>,
        veto: bool,
    }

    impl Handler for Recorder {
        fn namespace(&self) -> &'static str {
            "test"
        }
        fn commands(&self) -> &'static [CommandMeta] {
            &TEST
        }
        fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
            let entry = match cmd.args.int("n").or(cmd.args.int("tab")) {
                Some(n) => format!("{}:{n}", cmd.id),
                None => cmd.id.to_string(),
            };
            self.log.borrow_mut().push(entry);
            match cmd.id.as_str() {
                "test.chain" => {
                    cx.dispatch(Command::new("test.log").arg("n", 1));
                    cx.dispatch(Command::new("test.log").arg("n", 2));
                    Ok(())
                }
                "test.fail" => Err(CmdError::Rejected("not now".into())),
                _ => Ok(()),
            }
        }
        fn on_quit(&mut self, _cx: &mut Cx<'_>) -> QuitDecision {
            if self.veto {
                QuitDecision::Veto(PromptSpec {
                    anchor: Anchor::JobBar,
                    title: String::new(),
                    body: String::new(),
                    choices: smallvec::SmallVec::new(),
                    default: 0,
                    input: None,
                    allow_all: false,
                    detail: None,
                    secret: false,
                })
            } else {
                QuitDecision::Allow
            }
        }
    }

    struct Other;
    static OTHER: [CommandMeta; 1] = [meta("other.keep", &[])];
    impl Handler for Other {
        fn namespace(&self) -> &'static str {
            "other"
        }
        fn commands(&self) -> &'static [CommandMeta] {
            &OTHER
        }
        fn handle(&mut self, _: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
            cx.keep_rev();
            Ok(())
        }
    }

    fn core() -> (Core, Arc<TestWaker>, Rc<RefCell<Vec<String>>>) {
        let w = Arc::new(TestWaker::default());
        let p = if cfg!(windows) { r"C:\" } else { "/" };
        let mut c = Core::new(
            Boot::new(Location::local(p), Location::local(p)),
            Services::null(),
            w.clone(),
        );
        let log = Rc::new(RefCell::new(Vec::new()));
        c.register(Box::new(Recorder {
            log: log.clone(),
            veto: false,
        }));
        c.register(Box::new(Other));
        settle(&mut c, &w);
        (c, w, log)
    }

    /// Дождаться стартовой загрузки обоих табов и сбросить счётчики пробуждений.
    fn settle(c: &mut Core, w: &TestWaker) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !c
            .state()
            .panels
            .iter()
            .all(|p| p.tab().listing.status == ListingStatus::Complete)
        {
            assert!(Instant::now() < deadline, "initial load did not finish");
            w.wait(Duration::from_millis(50));
            c.pump(Duration::from_secs(1));
        }
        // Как следующий кадр UI: сбросить WakeFlag, поднятый сообщением во время
        // последнего pump, иначе следующая отправка не разбудит.
        *w.pending.lock().unwrap() = false;
        c.pump(Duration::from_secs(1));
        w.wakes.store(0, Ordering::SeqCst);
    }

    fn toasts(c: &mut Core) -> Vec<(Level, String)> {
        c.take_ui_events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Toast { level, text } => Some((level, text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_namespace_panics() {
        let (mut c, _, log) = core();
        c.register(Box::new(Recorder { log, veto: false }));
    }

    #[test]
    #[should_panic(expected = "outside handler namespace")]
    fn foreign_command_panics() {
        struct Bad;
        static BAD: [CommandMeta; 1] = [meta("test.x", &[])];
        impl Handler for Bad {
            fn namespace(&self) -> &'static str {
                "ops"
            }
            fn commands(&self) -> &'static [CommandMeta] {
                &BAD
            }
            fn handle(&mut self, _: &Command, _: &mut Cx<'_>) -> Result<(), CmdError> {
                Ok(())
            }
        }
        core().0.register(Box::new(Bad));
    }

    #[test]
    fn unknown_command_is_a_toast() {
        let (mut c, _, log) = core();
        let rev = c.state().rev;
        c.dispatch(Command::new("ops.copy"));
        c.dispatch(Command::new("test.log").arg("n", "x"));
        let t = toasts(&mut c);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0], (Level::Error, "Unknown command: ops.copy".to_owned()));
        assert!(t[1].1.starts_with("test.log: arg n"), "{t:?}");
        assert!(log.borrow().is_empty());
        assert!(c.state().rev > rev, "toast must trigger a repaint");
    }

    #[test]
    fn rejected_is_a_warning_toast() {
        let (mut c, _, _) = core();
        c.dispatch(Command::new("test.fail"));
        assert_eq!(toasts(&mut c), [(Level::Warn, "not now".to_owned())]);
    }

    #[test]
    fn cx_dispatch_is_fifo_in_same_dispatch_and_pump() {
        let (mut c, _, log) = core();
        c.dispatch(Command::new("test.chain"));
        c.dispatch(Command::new("test.log").arg("n", 3));
        assert_eq!(
            *log.borrow(),
            ["test.chain", "test.log:1", "test.log:2", "test.log:3"]
        );
        log.borrow_mut().clear();
        let s = c.sender();
        s.send(Command::new("test.chain")).unwrap();
        s.send(Command::new("test.log").arg("n", 3)).unwrap();
        let p = c.pump(Duration::from_secs(1));
        assert_eq!(
            p,
            Pump {
                more: false,
                changed: true
            }
        );
        assert_eq!(
            *log.borrow(),
            ["test.chain", "test.log:3", "test.log:1", "test.log:2"]
        );
    }

    #[test]
    fn rev_bumps_unless_kept() {
        let (mut c, _, _) = core();
        let r0 = c.state().rev;
        c.dispatch(Command::new("test.log"));
        assert_eq!(c.state().rev, r0 + 1);
        c.dispatch(Command::new("other.keep"));
        assert_eq!(c.state().rev, r0 + 1);
        assert_eq!(
            c.pump(Duration::from_secs(1)),
            Pump {
                more: false,
                changed: false
            }
        );
    }

    #[test]
    fn pump_stops_on_budget_and_reports_more() {
        let (mut c, _, log) = core();
        let s = c.sender();
        for n in 0..5 {
            s.send(Command::new("test.log").arg("n", n)).unwrap();
        }
        let p = c.pump(Duration::ZERO);
        assert_eq!(
            p,
            Pump {
                more: true,
                changed: true
            }
        );
        assert_eq!(log.borrow().len(), 1);
        let mut pumps = 1;
        while c.pump(Duration::ZERO).more {
            pumps += 1;
        }
        assert_eq!(log.borrow().len(), 5);
        assert_eq!(pumps, 4);
    }

    #[test]
    fn commands_before_msgs_and_msgs_can_dispatch() {
        let (mut c, w, log) = core();
        let sink = c.rt.sink();
        let t = std::thread::spawn(move || {
            sink.try_send(msg("listed", |cx| {
                cx.dispatch(Command::new("test.log").arg("n", 9));
            }))
            .unwrap();
        });
        t.join().unwrap();
        c.sender()
            .send(Command::new("test.log").arg("n", 1))
            .unwrap();
        assert!(w.wait(Duration::from_secs(1)));
        c.pump(Duration::from_secs(1));
        assert_eq!(*log.borrow(), ["test.log:1", "test.log:9"]);
    }

    #[test]
    fn sender_from_other_thread_wakes_and_delivers() {
        let (mut c, w, log) = core();
        let s = c.sender();
        std::thread::spawn(move || {
            for n in 0..100 {
                s.send(Command::new("test.log").arg("n", n)).unwrap();
            }
        })
        .join()
        .unwrap();
        assert!(w.wait(Duration::from_secs(1)));
        assert_eq!(w.wakes.load(Ordering::SeqCst), 1);
        assert!(!c.pump(Duration::from_secs(1)).more);
        assert_eq!(log.borrow().len(), 100);
        let s = c.sender();
        let _ = c.shutdown(Duration::from_secs(5));
        assert!(s.send(Command::new("test.log")).is_err());
    }

    #[test]
    fn on_pump_runs_first_without_rev_bump() {
        struct Inbox(Rc<RefCell<Vec<String>>>);
        static VIEW: [CommandMeta; 0] = [];
        impl Handler for Inbox {
            fn namespace(&self) -> &'static str {
                "viewer"
            }
            fn commands(&self) -> &'static [CommandMeta] {
                &VIEW
            }
            fn handle(&mut self, _: &Command, _: &mut Cx<'_>) -> Result<(), CmdError> {
                Ok(())
            }
            fn on_pump(&mut self, _cx: &mut Cx<'_>) {
                self.0.borrow_mut().push("on_pump".into());
            }
        }
        let (mut c, _, log) = core();
        c.register(Box::new(Inbox(log.clone())));
        c.sender().send(Command::new("test.log")).unwrap();
        c.pump(Duration::from_secs(1));
        assert_eq!(*log.borrow(), ["on_pump", "test.log"]);
        assert!(!c.pump(Duration::from_secs(1)).changed);
    }

    #[test]
    fn quit_decision_polls_handlers() {
        let (mut c, _, _) = core();
        assert_eq!(c.quit_decision(), QuitDecision::Allow);
        let w = Arc::new(TestWaker::default());
        let mut c = Core::new(
            Boot::new(Location::local("/"), Location::local("/")),
            Services::null(),
            w,
        );
        c.register(Box::new(Other));
        c.register(Box::new(Recorder {
            log: Rc::default(),
            veto: true,
        }));
        assert!(matches!(c.quit_decision(), QuitDecision::Veto(_)));
    }

    #[test]
    fn task_panic_becomes_toast() {
        let (mut c, w, _) = core();
        c.rt.spawn(crate::runtime::Lane::Bulk, "boom", c.rt.token(), |_| {
            panic!("test panic")
        });
        assert!(w.wait(Duration::from_secs(5)));
        c.pump(Duration::from_secs(1));
        let t = toasts(&mut c);
        assert_eq!(
            t,
            [(Level::Error, "Task boom failed unexpectedly".to_owned())]
        );
    }

    #[test]
    fn initial_state() {
        let (c, _, _) = core();
        assert_eq!(c.state().active, Side::Left);
        assert_eq!(c.state().panel(Side::Right).tab().id, crate::ids::TabId(2));
        // panel 29 + sel 9 + tab 7 + app 4 + job 5 + prompt 2 + тестовые 5.
        assert_eq!(c.registry().iter().count(), 61);
    }
}
