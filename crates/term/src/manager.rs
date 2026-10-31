//! Менеджер сеансов (S6 §4.2, S6-10): пул сеансов по shells, активный — один,
//! остальные живут в фоне с урезанным скроллбэком.
//!
//! Тяжёлое — не в UI-потоке: поиск shells и запуск процесса (`CreateProcess`,
//! `ConPTY`) идут задачами полосы Interactive, готовый [`Session`] приходит `Msg`.
//! Shell по умолчанию прогревается через [`WARMUP`] после первого pump — уже после
//! первого кадра. Выбор по умолчанию (№52): настройка → статистика выбора → pwsh →
//! cmd.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use compact_str::CompactString;
use urfin_core::{
    Anchor, CancelToken, Choice, CmdError, Cx, Lane, Level, Location, Msg, PromptSpec,
    QuitDecision, Runtime,
};

use crate::TermState;
use crate::config::TermConfig;
use crate::session::{CoreSink, Mark, OscEvent, Session, SessionEvent, SessionId, SessionOpts};
use crate::shells::{self, ShellSpec, SystemProbe};
use crate::sys::conpty::{PtySize, SpawnSpec};
use crate::vt::input::{self, Mods};
use crate::vt::shared::TermShared;

/// Через сколько после первого pump прогреть shell по умолчанию.
pub const WARMUP: Duration = Duration::from_millis(500);

/// Ждать маркеров интеграции после старта (S6-12: Clink перетёр `PROMPT`).
pub const INTEG_TIMEOUT: Duration = Duration::from_secs(5);

/// Размер PTY до первой раскладки окна (S6-08 ресайзит под область терминала).
const START_SIZE: PtySize = PtySize {
    cols: 120,
    rows: 30,
};

/// Состояние слота.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Starting,
    Running,
    /// Процесс вышел; транскрипт остаётся, ввод перезапускает.
    Exited(Option<u32>),
    Failed,
}

/// Готовность активного сеанса к `term.exec`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecReady {
    Wait,
    Go,
    /// Сеанс не запустился — команда отменяется.
    Gone,
}

/// Итог события сеанса.
#[derive(Debug, Default)]
pub(crate) struct Effect {
    /// У активного сеанса закончилась команда (`D`).
    pub finished: bool,
    /// Shell активного сеанса сам перешёл в другой каталог (S6 §5.1 shell → панель).
    pub cwd: Option<PathBuf>,
    /// OSC 52 от активного сеанса: текст в буфер обмена (S6 §3.4, только запись).
    pub clip: Option<String>,
    /// Строка команды из `633;E` (raw-ввод, nonce проверен) — в историю (S6 §6.1).
    pub command: Option<(CompactString, String)>,
}

/// Сеанс в пуле.
#[allow(clippy::struct_excessive_bools, reason = "независимые признаки сеанса")]
#[derive(Debug)]
pub struct Slot {
    pub id: SessionId,
    pub shell: CompactString,
    pub state: SlotState,
    /// Заголовок от процесса (OSC 0/2).
    pub title: Option<String>,
    session: Option<Session>,
    /// Сетка живёт и после выхода процесса.
    shared: Option<Arc<TermShared>>,
    /// Ввод до старта процесса.
    queued: Vec<u8>,
    /// Shell прислал маркер промпта `A`: интеграция работает (S6 §5); нет — вид
    /// «экран» и raw-ввод (S6 §3.2).
    pub integrated: bool,
    /// Команда выполняется: отправлена или `C`, до `D` (S6 §3.3).
    pub running: bool,
    /// Каталог shell (путь Windows) по последнему `FnCwd` (S6 §5.1).
    pub cwd: Option<PathBuf>,
    /// Тихие хуки работают; `633;P;FnHook=0` (CLM, нет `PSReadLine`) — видимый cd.
    pub hooks: bool,
    /// Корень автомонтирования WSL (`633;P;FnMountRoot`).
    pub mount_root: Option<String>,
    /// Промпт на экране: метки `A` и `B` (S6 §3.2); `None` — команда идёт.
    pub prompt: Option<(Mark, Option<Mark>)>,
    path_style: crate::shells::PathStyle,
    started: Instant,
    /// Запущен с инъекцией интеграции: маркеры ждутся (cmd с Clink — нет, S6 §5).
    expects_marks: bool,
    /// Код последней команды из `D` (`Some(None)` — shell его не сообщает, cmd).
    pub last_exit: Option<Option<i32>>,
    /// Каталог нашего cd, ответ на который ещё не пришёл (S6 §5.1 `pending_sync`).
    pending_sync: Option<PathBuf>,
    /// Первый `FnCwd` уже был (стартовый каталог — не переход shell).
    cwd_seen: bool,
    /// Строка последнего submit: её `633;E` в историю второй раз не пишется.
    pub(crate) submitted_line: Option<String>,
    /// Блоки команд (S6-21): метки `A`, `B`, `D` последних [`BLOCKS`] промптов.
    pub blocks: std::collections::VecDeque<Block>,
    /// Строка отдана shell для его родного комплита (`cmdline.complete_native`): вид
    /// «экран» и ввод в PTY до следующего промпта (S6 §6.2).
    pub raw: bool,
}

/// Блок команды: промпт (`A`), начало ввода (`B`), конец команды (`D`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub a: Mark,
    pub b: Option<Mark>,
    pub d: Option<Mark>,
}

/// Сколько блоков помнить (приёмка S6-21 — 100 последних промптов).
pub const BLOCKS: usize = 200;

impl Slot {
    pub fn shared(&self) -> Option<&Arc<TermShared>> {
        self.shared.as_ref()
    }

    /// Кроме shell есть процессы (без интеграции — по job object).
    pub fn busy(&self) -> bool {
        self.session.as_ref().is_some_and(Session::busy)
    }
}

/// Отложенный до готовности списка shells запуск.
#[derive(Debug)]
enum Deferred {
    /// `term.shell`/`term.new`: id shell (`None` — по умолчанию), выбор пользователя.
    Start {
        shell: Option<CompactString>,
        reuse: bool,
        cwd: Option<PathBuf>,
    },
}

/// Пул сеансов.
#[derive(Debug, Default)]
pub struct Manager {
    shells: Option<Vec<ShellSpec>>,
    slots: Vec<Slot>,
    active: Option<SessionId>,
    next_id: SessionId,
    /// Сколько раз пользователь выбирал shell (№52), `state_dir/term/usage.toml`.
    usage: BTreeMap<CompactString, u32>,
    term_dir: Option<PathBuf>,
    born: Option<Instant>,
    /// Каталог активной панели на последнем pump.
    home: Option<PathBuf>,
    /// Сколько cd в shell отправила синхронизация (приёмка S6-14: ровно один).
    pub(crate) syncs: u32,
    /// Лишних промптов (`D`) даст cd последнего submit до самой команды: видимый cd —
    /// свой промпт, тихий в PowerShell — `InvokePrompt` (bash, zsh, fish — нет).
    pub(crate) cd_prompts: u32,
    /// Размер области терминала окна (S6-08); `None` — окно ещё не разложено.
    area: Option<PtySize>,
    discovering: bool,
    warmed: bool,
    deferred: Vec<Deferred>,
}

impl Manager {
    pub fn new(term_dir: Option<PathBuf>) -> Manager {
        Manager {
            term_dir,
            next_id: 1,
            ..Manager::default()
        }
    }

    pub fn shells(&self) -> &[ShellSpec] {
        self.shells.as_deref().unwrap_or_default()
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn active(&self) -> Option<&Slot> {
        let id = self.active?;
        self.slots.iter().find(|s| s.id == id)
    }

    fn slot_mut(&mut self, id: SessionId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == id)
    }

    /// Shell по умолчанию (№52): `term.default_shell`, иначе чаще всего
    /// выбираемый, иначе pwsh 7, иначе cmd, иначе первый найденный.
    pub fn default_shell(&self, cfg: &TermConfig) -> Option<&ShellSpec> {
        let shells = self.shells();
        let by_id = |id: &str| shells.iter().find(|s| s.id == id);
        if cfg.default_shell != "auto"
            && let Some(s) = by_id(&cfg.default_shell)
        {
            return Some(s);
        }
        let used = shells
            .iter()
            .filter_map(|s| self.usage.get(&s.id).map(|n| (*n, s)))
            .filter(|(n, _)| *n > 0)
            // Равные — первый в списке shells.
            .fold(None::<(u32, &ShellSpec)>, |best, (n, s)| match best {
                Some((b, _)) if b >= n => best,
                _ => Some((n, s)),
            });
        used.map(|(_, s)| s)
            .or_else(|| by_id("pwsh"))
            .or_else(|| by_id("cmd"))
            .or_else(|| shells.first())
    }

    /// Каждый pump: в первый — таймер прогрева, через [`WARMUP`] — поиск shells и старт shell по умолчанию. Без каталога
    /// состояния (тесты, `--smoke`, `--screens`) — ничего: shells ищутся по первой
    /// команде, прогрева нет.
    pub(crate) fn pump(&mut self, cfg: &TermConfig, rt: &Runtime, home: Option<PathBuf>) {
        // Прогрев — в каталоге активной панели (S6 §4.2), не процесса.
        self.home = home;
        if self.term_dir.is_none() {
            return;
        }
        let Some(born) = self.born else {
            // Поиск shells — тоже после первого кадра: полоса Interactive нужна
            // листингу на старте.
            self.born = Some(Instant::now());
            rt.waker().wake_after(WARMUP);
            return;
        };
        if born.elapsed() >= WARMUP {
            self.discover(cfg, rt);
        }
        self.maybe_warm(cfg, rt);
    }

    fn discover(&mut self, cfg: &TermConfig, rt: &Runtime) {
        if !self.discovering {
            self.discovering = true;
            spawn_discovery(rt, cfg, self.term_dir.clone());
        }
    }

    fn maybe_warm(&mut self, cfg: &TermConfig, rt: &Runtime) {
        if self.warmed || self.shells.is_none() || self.born.is_none_or(|b| b.elapsed() < WARMUP) {
            return;
        }
        self.warmed = true;
        if self.slots.is_empty()
            && let Some(id) = self.default_shell(cfg).map(|s| s.id.clone())
        {
            let _ = self.start(cfg, rt, &id, self.home.clone(), true);
        }
    }

    /// Список shells и статистика готовы.
    fn on_shells(
        &mut self,
        cfg: &TermConfig,
        rt: &Runtime,
        shells: Vec<ShellSpec>,
        usage: BTreeMap<CompactString, u32>,
    ) -> Vec<String> {
        self.shells = Some(shells);
        for (k, v) in usage {
            *self.usage.entry(k).or_default() += v;
        }
        let mut errors = Vec::new();
        for d in std::mem::take(&mut self.deferred) {
            let Deferred::Start { shell, reuse, cwd } = d;
            if let Err(e) = self.open(cfg, rt, shell.as_deref(), reuse, cwd) {
                errors.push(e.to_string());
            }
        }
        self.maybe_warm(cfg, rt);
        errors
    }

    /// `term.shell` (`reuse`) / `term.new`: сделать активным сеанс shell, запустив
    /// при необходимости. До готовности списка shells — отложить.
    pub(crate) fn open(
        &mut self,
        cfg: &TermConfig,
        rt: &Runtime,
        shell: Option<&str>,
        reuse: bool,
        cwd: Option<PathBuf>,
    ) -> Result<(), CmdError> {
        if self.shells.is_none() {
            self.deferred.push(Deferred::Start {
                shell: shell.map(Into::into),
                reuse,
                cwd,
            });
            self.discover(cfg, rt);
            return Ok(());
        }
        let id: CompactString = match shell {
            Some(s) => s.into(),
            None => self
                .default_shell(cfg)
                .map(|s| s.id.clone())
                .ok_or_else(|| CmdError::Rejected(tr("term-no-shells", &[]).into()))?,
        };
        if !self.shells().iter().any(|s| s.id == id) {
            return Err(arg_err("id", tr("term-unknown-shell", &[("id", &id)])));
        }
        *self.usage.entry(id.clone()).or_default() += 1;
        self.save_usage(rt);
        if reuse && let Some(s) = self.slots.iter().find(|s| s.shell == id) {
            let sid = s.id;
            self.set_active(cfg, sid);
            return Ok(());
        }
        self.start(cfg, rt, &id, cwd, true).map(|_| ())
    }

    /// Запустить сеанс shell в фоне; слот появляется сразу (`Starting`).
    fn start(
        &mut self,
        cfg: &TermConfig,
        rt: &Runtime,
        shell: &str,
        cwd: Option<PathBuf>,
        activate: bool,
    ) -> Result<SessionId, CmdError> {
        let spec = self
            .shells()
            .iter()
            .find(|s| s.id == shell)
            .cloned()
            .ok_or_else(|| arg_err("id", tr("term-unknown-shell", &[("id", shell)])))?;
        let id = self.next_id;
        self.next_id += 1;
        self.slots.push(Slot {
            id,
            shell: spec.id.clone(),
            state: SlotState::Starting,
            title: None,
            session: None,
            shared: None,
            queued: Vec::new(),
            integrated: false,
            running: false,
            cwd: None,
            hooks: true,
            mount_root: None,
            prompt: None,
            path_style: spec.path_style.clone(),
            started: Instant::now(),
            expects_marks: false,
            last_exit: None,
            pending_sync: None,
            cwd_seen: false,
            submitted_line: None,
            blocks: std::collections::VecDeque::new(),
            raw: false,
        });
        if activate {
            self.set_active(cfg, id);
        }
        let nonce = nonce();
        let dirs = self
            .term_dir
            .as_deref()
            .map(|d| crate::integ::Dirs::new(d, id));
        let launch = if cfg.integration {
            crate::integ::launch(&spec, dirs.as_ref(), &nonce)
        } else {
            None
        };
        if let Some(slot) = self.slot_mut(id) {
            slot.expects_marks = launch.is_some();
        }
        let files = launch
            .as_ref()
            .filter(|l| l.needs_files)
            .and(dirs.as_ref())
            .map(|d| d.shell.clone());
        let cwd_dir = dirs
            .as_ref()
            .filter(|_| launch.is_some())
            .and_then(|d| d.cwd_file.parent().map(Path::to_path_buf));
        let spawn = spawn_spec(&spec, cwd, id, &nonce, launch);
        let opts = SessionOpts {
            size: self.area.unwrap_or(START_SIZE),
            scrollback: if activate {
                cfg.scrollback
            } else {
                cfg.background_scrollback
            },
            osc52: cfg.osc52.to_string(),
            nonce: Some(nonce),
        };
        crate::sys::conpty::set_mode(crate::sys::conpty::Mode::parse(&cfg.conpty));
        rt.spawn(Lane::Interactive, "term.start", rt.token(), move |t| {
            // Скрипты интеграции и каталог файла тихого cd — до запуска shell.
            if let Some(dir) = &files
                && let Err(e) = crate::integ::ensure_files(dir)
            {
                tracing::warn!(error = %e, dir = %dir.display(), "term: integration scripts");
            }
            if let Some(dir) = &cwd_dir {
                let _ = std::fs::create_dir_all(dir);
            }
            let sink = Arc::new(CoreSink {
                id,
                sink: t.sink.clone(),
                cancel: CancelToken::new(),
            });
            let result = Session::start(id, &spawn, &opts, sink).map_err(|e| e.to_string());
            // Shutdown: сеанс закроется в Drop.
            let _ = t.sink.send(Box::new(StartedMsg { id, result }), &t.cancel);
        });
        Ok(id)
    }

    /// Сеанс стал активным: прежний — в фон (скроллбэк урезается).
    pub(crate) fn set_active(&mut self, cfg: &TermConfig, id: SessionId) {
        if self.active == Some(id) {
            return;
        }
        if let Some(old) = self.active
            && let Some(sh) = self.slot_mut(old).and_then(|s| s.shared.clone())
        {
            sh.set_scrollback(cfg.background_scrollback, &cfg.osc52);
        }
        self.active = Some(id);
        if let Some(sh) = self.slot_mut(id).and_then(|s| s.shared.clone()) {
            sh.set_scrollback(cfg.scrollback, &cfg.osc52);
            sh.dirty.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// `term.next` / `term.prev`.
    pub(crate) fn cycle(&mut self, cfg: &TermConfig, step: isize) -> Result<(), CmdError> {
        let n = self.slots.len();
        if n == 0 {
            return Err(no_session());
        }
        let cur = self
            .active
            .and_then(|a| self.slots.iter().position(|s| s.id == a))
            .unwrap_or(0);
        let next = (cur.cast_signed() + step)
            .rem_euclid(n.cast_signed())
            .cast_unsigned();
        let id = self.slots[next].id;
        self.set_active(cfg, id);
        Ok(())
    }

    /// `term.close`: сеанс из пула; активным становится соседний.
    pub(crate) fn close(
        &mut self,
        cfg: &TermConfig,
        id: Option<SessionId>,
    ) -> Result<(), CmdError> {
        let id = id.or(self.active).ok_or_else(no_session)?;
        let pos = self
            .slots
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| arg_err("id", "no such session".into()))?;
        // Drop сеанса — закрытие через writer, без ожидания.
        self.slots.remove(pos);
        if self.active == Some(id) {
            self.active = None;
            if let Some(s) = self.slots.get(pos.min(self.slots.len().saturating_sub(1))) {
                let next = s.id;
                self.set_active(cfg, next);
            }
        }
        Ok(())
    }

    /// `term.restart`: тот же shell заново на месте активного сеанса.
    pub(crate) fn restart(&mut self, cfg: &TermConfig, rt: &Runtime) -> Result<(), CmdError> {
        let old = self.active.ok_or_else(no_session)?;
        let pos = self
            .slots
            .iter()
            .position(|s| s.id == old)
            .ok_or_else(no_session)?;
        let shell = self.slots[pos].shell.clone();
        let id = self.start(cfg, rt, &shell, None, false)?;
        let new = self.slots.pop().expect("slot just pushed");
        self.slots[pos] = new;
        self.active = None;
        self.set_active(cfg, id);
        Ok(())
    }

    /// `term.kill`: убить процесс активного сеанса и его дерево.
    pub(crate) fn kill(&mut self) -> Result<(), CmdError> {
        let s = self.active().ok_or_else(no_session)?;
        match &s.session {
            Some(sess) => sess
                .kill()
                .map_err(|e| CmdError::Rejected(e.to_string().into())),
            None => Ok(()),
        }
    }

    /// Байты в активный сеанс. Вышедший процесс перезапускается: Enter после
    /// «процесс завершён» — новый сеанс (сам Enter не уходит).
    pub(crate) fn write(
        &mut self,
        cfg: &TermConfig,
        rt: &Runtime,
        bytes: Vec<u8>,
        restart_only: bool,
    ) -> Result<(), CmdError> {
        let id = self.active.ok_or_else(no_session)?;
        let state = self.slot_mut(id).map(|s| s.state).ok_or_else(no_session)?;
        if matches!(state, SlotState::Exited(_) | SlotState::Failed) {
            self.restart(cfg, rt)?;
            if restart_only {
                return Ok(());
            }
        }
        let id = self.active.ok_or_else(no_session)?;
        let slot = self.slot_mut(id).ok_or_else(no_session)?;
        match &slot.session {
            Some(s) => s.write(bytes),
            None => slot.queued.extend_from_slice(&bytes),
        }
        Ok(())
    }

    /// Область терминала окна (S6-08): размер всех сеансов. Видимость панелей и
    /// высота cmdline на него не влияют — TUI не перерисовываются от Ctrl+O.
    pub(crate) fn resize(&mut self, size: PtySize) {
        self.area = Some(size);
        for s in &self.slots {
            if let Some(sess) = &s.session {
                sess.resize(size);
            } else if let Some(sh) = &s.shared {
                sh.resize(size);
            }
        }
    }

    pub fn area(&self) -> Option<PtySize> {
        self.area
    }

    /// Команда отправлена в активный сеанс (submit): до `D` — «выполняется».
    /// `line` — её текст: ответный `633;E` в историю второй раз не пишется.
    /// Строка отдана shell (родной комплит): до следующего промпта — ввод в PTY.
    pub(crate) fn set_raw(&mut self) {
        if let Some(id) = self.active
            && let Some(s) = self.slot_mut(id)
        {
            s.raw = true;
            s.prompt = None;
        }
    }

    pub(crate) fn mark_running(&mut self, line: Option<String>) {
        if let Some(id) = self.active
            && let Some(s) = self.slot_mut(id)
        {
            s.running = true;
            s.prompt = None;
            s.submitted_line = line;
        }
    }

    /// Спецификация shell активного сеанса.
    pub(crate) fn active_spec(&self) -> Option<&ShellSpec> {
        let s = self.active()?;
        self.shells().iter().find(|k| k.id == s.shell)
    }

    /// Сеансы с инъекцией, от которых за [`INTEG_TIMEOUT`] не пришло ни одного
    /// маркера (cmd с Clink, перетёртый `PROMPT`): дальше — без интеграции, shell
    /// для тоста. Каждый — один раз.
    pub(crate) fn silent_integrations(&mut self) -> Vec<CompactString> {
        let mut out = Vec::new();
        for s in &mut self.slots {
            if s.expects_marks
                && !s.integrated
                && s.state == SlotState::Running
                && s.started.elapsed() >= INTEG_TIMEOUT
            {
                s.expects_marks = false;
                out.push(s.shell.clone());
            }
        }
        out
    }

    /// Submit (S6 §6): сначала ленивая синхронизация cwd (S6 §5.1) — каталог панели
    /// `want` отличается от каталога shell: тихий cd (путь — в файл сеанса, затем
    /// аккорд Ctrl+Alt+Shift+F12 для хука shell: без эха и следа в истории) или, без
    /// хуков (cmd, CLM), видимый cd; затем `bytes`. Всё — одним порядком через writer.
    pub(crate) fn submit(
        &mut self,
        cfg: &TermConfig,
        rt: &Runtime,
        bytes: Vec<u8>,
        want: Option<PathBuf>,
    ) -> Result<(), CmdError> {
        self.sync_then(cfg, rt, bytes, want, true)
    }

    /// Ленивая синхронизация без команды (S6 §5.1: при показе терминала, Ctrl+O) —
    /// только тихая: видимый cd в выводе при каждом Ctrl+O был бы мусором.
    pub(crate) fn sync_quiet(&mut self, cfg: &TermConfig, rt: &Runtime, want: Option<PathBuf>) {
        let _ = self.sync_then(cfg, rt, Vec::new(), want, false);
    }

    fn sync_then(
        &mut self,
        cfg: &TermConfig,
        rt: &Runtime,
        bytes: Vec<u8>,
        want: Option<PathBuf>,
        visible: bool,
    ) -> Result<(), CmdError> {
        self.cd_prompts = 0;
        let sync = matches!(cfg.sync_cwd.as_str(), "both" | "panel_to_shell");
        let kind = self
            .active()
            .and_then(|s| self.shells().iter().find(|k| k.id == s.shell))
            .map(|k| k.kind.clone());
        let term_dir = self.term_dir.clone();
        let cd = self.active().and_then(|slot| {
            let want = want.filter(|_| sync && slot.integrated && !slot.running)?;
            let have = slot.cwd.as_ref()?;
            if same_path(have, &want) || slot.pending_sync.as_ref() == Some(&want) {
                return None;
            }
            Some((
                slot.id,
                slot.hooks,
                want,
                slot.path_style.clone(),
                slot.mount_root.clone(),
            ))
        });
        let write = |m: &mut Manager, bytes: Vec<u8>| {
            if bytes.is_empty() {
                Ok(())
            } else {
                m.write(cfg, rt, bytes, false)
            }
        };
        let Some((id, hooks, want, style, root)) = cd else {
            return write(self, bytes);
        };
        let text = crate::integ::to_shell(&want, &style, root.as_deref());
        let quiet =
            hooks && !matches!(kind, Some(shells::ShellKind::Cmd) | None) && term_dir.is_some();
        if !quiet && !visible {
            return write(self, bytes);
        }
        // Ответный `FnCwd` этого cd — эхо, не переход shell (панель не трогать).
        if let Some(s) = self.slot_mut(id) {
            s.pending_sync = Some(want);
        }
        self.syncs += 1;
        if !quiet {
            let line = visible_cd(kind.as_ref(), &text);
            self.write(cfg, rt, line.into_bytes(), false)?;
            self.cd_prompts = 1;
            return write(self, bytes);
        }
        let Some(tx) = self
            .slots
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.session.as_ref())
            .map(Session::writer)
        else {
            return self.write(cfg, rt, bytes, false);
        };
        self.cd_prompts = u32::from(matches!(
            kind,
            Some(shells::ShellKind::Pwsh | shells::ShellKind::WinPowerShell)
        ));
        let file = crate::integ::Dirs::new(&term_dir.unwrap_or_default(), id).cwd_file;
        rt.spawn(Lane::Interactive, "term.cd", rt.token(), move |_| {
            let tmp = file.with_extension("tmp~");
            if std::fs::write(&tmp, text.as_bytes())
                .and_then(|()| std::fs::rename(&tmp, &file))
                .is_ok()
            {
                let _ = tx.send(crate::session::WriterCmd::Bytes(b"\x1b[24;8~".to_vec()));
            }
            if !bytes.is_empty() {
                let _ = tx.send(crate::session::WriterCmd::Bytes(bytes));
            }
        });
        Ok(())
    }

    /// Режимы ввода активного сеанса.
    pub(crate) fn input_modes(&self) -> input::Modes {
        self.active()
            .and_then(Slot::shared)
            .map(|s| s.input_modes())
            .unwrap_or_default()
    }

    fn on_started(
        &mut self,
        cfg: &TermConfig,
        id: SessionId,
        result: Result<Session, String>,
    ) -> Option<String> {
        let active = self.active == Some(id);
        let Some(slot) = self.slot_mut(id) else {
            return None; // закрыт до старта: Drop закроет сеанс
        };
        match result {
            Ok(s) => {
                if !slot.queued.is_empty() {
                    s.write(std::mem::take(&mut slot.queued));
                }
                if active {
                    s.shared().set_scrollback(cfg.scrollback, &cfg.osc52);
                }
                slot.shared = Some(s.shared().clone());
                slot.session = Some(s);
                slot.state = SlotState::Running;
                slot.started = Instant::now();
                None
            }
            Err(e) => {
                slot.state = SlotState::Failed;
                let shell = slot.shell.clone();
                Some(tr("term-start-failed", &[("shell", &shell), ("error", &e)]))
            }
        }
    }

    /// Событие сеанса: что из него следует для окна и панели.
    #[allow(
        clippy::too_many_lines,
        reason = "разбор событий сеанса — одна таблица"
    )]
    fn on_event(&mut self, id: SessionId, ev: SessionEvent) -> Effect {
        let active = self.active == Some(id);
        let Some(slot) = self.slot_mut(id) else {
            return Effect::default();
        };
        match ev {
            SessionEvent::Exited(code) => {
                slot.state = SlotState::Exited(code);
                slot.session = None;
                let line = match code {
                    Some(c) => tr("term-exited", &[("code", &c.to_string())]),
                    None => tr("term-exited-unknown", &[]),
                };
                if let Some(sh) = &slot.shared {
                    sh.inject(format!("\r\n\x1b[0;2m{line}\x1b[0m\r\n").as_bytes());
                }
            }
            SessionEvent::Title(t) => slot.title = t,
            SessionEvent::Osc { event, at } => match event {
                OscEvent::PromptStart => {
                    slot.integrated = true;
                    slot.running = false;
                    slot.raw = false;
                    slot.prompt = Some((at, None));
                    // Блоки команд (S6-21): метка промпта; старые — за пределом.
                    slot.blocks.push_back(Block {
                        a: at,
                        b: None,
                        d: None,
                    });
                    while slot.blocks.len() > BLOCKS {
                        slot.blocks.pop_front();
                    }
                }
                OscEvent::CommandStart => {
                    if let Some((a, _)) = slot.prompt {
                        slot.prompt = Some((a, Some(at)));
                    }
                    if let Some(b) = slot.blocks.back_mut() {
                        b.b = Some(at);
                    }
                }
                OscEvent::Executed => {
                    slot.running = true;
                    slot.prompt = None;
                }
                OscEvent::Finished(code) => {
                    slot.last_exit = Some(code);
                    if let Some(b) = slot.blocks.back_mut().filter(|b| b.b.is_some()) {
                        b.d = Some(at);
                    }
                    let was = slot.running;
                    slot.running = false;
                    return Effect {
                        finished: active && was,
                        ..Effect::default()
                    };
                }
                // С интеграцией верим только своему маркеру (nonce проверен
                // сканером); без неё — OSC 7 / 9;9 / iTerm (S6 §5.1).
                OscEvent::Cwd { path, source } => {
                    if source != crate::session::CwdSource::FnCwd && slot.expects_marks {
                        return Effect::default();
                    }
                    let Some(new) = crate::integ::to_windows(
                        &path,
                        &slot.path_style,
                        slot.mount_root.as_deref(),
                    ) else {
                        return Effect::default();
                    };
                    let moved = slot.cwd.as_ref().is_none_or(|c| !same_path(c, &new));
                    slot.cwd = Some(new.clone());
                    // Эхо нашего же cd (S6 §5.1 `pending_sync`) — панель уже там.
                    if slot
                        .pending_sync
                        .take()
                        .is_some_and(|p| same_path(&p, &new))
                    {
                        return Effect::default();
                    }
                    // Первый промпт — не переход: shell стартовал там, где сказали.
                    let first = !slot.cwd_seen;
                    slot.cwd_seen = true;
                    return Effect {
                        cwd: (active && moved && !first).then_some(new),
                        ..Effect::default()
                    };
                }
                OscEvent::Prop(k, v) => match k.as_str() {
                    "FnHook" => slot.hooks = v != "0",
                    "FnMountRoot" if v.starts_with('/') => slot.mount_root = Some(v),
                    _ => {}
                },
                // Строка из raw-ввода (shell сам её выполнил) — в историю, если её
                // набрали не в нашей строке (та уже там, S6 §6.1).
                OscEvent::CommandLine(line) => {
                    if slot.submitted_line.take().is_none_or(|l| l != line) {
                        return Effect {
                            command: Some((slot.shell.clone(), line)),
                            ..Effect::default()
                        };
                    }
                }
            },
            // OSC 52 (S6 §3.4): чтение буфера эмулятор не пропускает (`OnlyCopy`),
            // запись — только от активного сеанса.
            SessionEvent::Clipboard(text) => {
                return Effect {
                    clip: active.then_some(text),
                    ..Effect::default()
                };
            }
            SessionEvent::Bell => {}
        }
        Effect::default()
    }

    /// Готов ли активный сеанс принять команду `term.exec`: у промпта (с интеграцией)
    /// или запущен без неё. Вышедший — перезапуск и ждать.
    pub(crate) fn exec_ready(&mut self, cfg: &TermConfig, rt: &Runtime) -> ExecReady {
        let Some(s) = self.active() else {
            return ExecReady::Wait; // старт отложен до списка shells
        };
        match s.state {
            SlotState::Failed => ExecReady::Gone,
            SlotState::Exited(_) => {
                if self.restart(cfg, rt).is_err() {
                    return ExecReady::Gone;
                }
                ExecReady::Wait
            }
            SlotState::Starting => ExecReady::Wait,
            SlotState::Running if s.running || s.raw || s.busy() => ExecReady::Wait,
            SlotState::Running if s.integrated => {
                if s.prompt.is_some_and(|(_, b)| b.is_some()) {
                    ExecReady::Go
                } else {
                    ExecReady::Wait
                }
            }
            // Без интеграции: маркеров не ждём (выключена или shell молчит).
            SlotState::Running if s.expects_marks => ExecReady::Wait,
            SlotState::Running => ExecReady::Go,
        }
    }

    /// Вето выхода: есть сеансы с работающими процессами.
    pub(crate) fn quit_decision(&mut self) -> QuitDecision {
        let n = self.slots.iter().filter(|s| s.busy()).count();
        if n == 0 {
            // Закрыть все: потоки сеансов кончатся сами.
            self.slots.clear();
            self.active = None;
            return QuitDecision::Allow;
        }
        let i18n = urfin_i18n::current();
        let choice = |id: &str, key: &str, k: char| Choice {
            id: id.into(),
            label: i18n.get(key).into(),
            key: k,
        };
        QuitDecision::Veto(PromptSpec {
            anchor: Anchor::Cmdline,
            title: tr("term-quit-title", &[("n", &n.to_string())]),
            body: i18n.get("term-quit-body"),
            choices: [
                choice("cancel", "term-quit-cancel", 'c'),
                choice("quit", "term-quit-terminate", 'q'),
            ]
            .into_iter()
            .collect(),
            default: 0,
            input: None,
            allow_all: false,
            detail: None,
            secret: false,
        })
    }

    /// Статистика выбора — на диск (полоса Bulk).
    fn save_usage(&self, rt: &Runtime) {
        let Some(dir) = self.term_dir.clone() else {
            return;
        };
        let usage = self.usage.clone();
        rt.spawn(Lane::Bulk, "term.usage", rt.token(), move |_| {
            if let Err(e) = save_usage(&dir.join(USAGE_FILE), &usage) {
                tracing::warn!(error = %e, "term: usage not saved");
            }
        });
    }
}

const USAGE_FILE: &str = "usage.toml";

fn save_usage(path: &Path, usage: &BTreeMap<CompactString, u32>) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut t = toml::Table::new();
    for (k, v) in usage {
        t.insert(k.to_string(), toml::Value::Integer(i64::from(*v)));
    }
    std::fs::write(path, toml::to_string(&t).unwrap_or_default())
}

fn load_usage(path: &Path) -> BTreeMap<CompactString, u32> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(t) = text.parse::<toml::Table>() else {
        return BTreeMap::new();
    };
    t.iter()
        .filter_map(|(k, v)| Some((k.as_str().into(), u32::try_from(v.as_integer()?).ok()?)))
        .collect()
}

fn spawn_discovery(rt: &Runtime, cfg: &TermConfig, term_dir: Option<PathBuf>) {
    let user = cfg.shells.clone();
    rt.spawn(Lane::Interactive, "term.shells", rt.token(), move |t| {
        let shells = shells::discover(&SystemProbe, &user);
        let mut usage = BTreeMap::new();
        if let Some(dir) = &term_dir {
            let cache = dir.join("shells.toml");
            if shells::load_cache(&cache).as_ref() != Some(&shells)
                && let Err(e) = shells::save_cache(&cache, &shells)
            {
                tracing::warn!(error = %e, "term: shells cache");
            }
            usage = load_usage(&dir.join(USAGE_FILE));
        }
        let _ = t
            .sink
            .send(Box::new(ShellsMsg { shells, usage }), &t.cancel);
    });
}

/// Что запустить: shell, cwd и окружение ребёнка (S6 §4.2).
fn spawn_spec(
    spec: &ShellSpec,
    cwd: Option<PathBuf>,
    id: SessionId,
    nonce: &str,
    launch: Option<crate::integ::Launch>,
) -> SpawnSpec {
    let mut env: Vec<(OsString, OsString)> = vec![
        ("TERM_PROGRAM".into(), "Urfin".into()),
        ("TERM_PROGRAM_VERSION".into(), crate::version().into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("URFIN_SESSION".into(), id.to_string().into()),
        ("URFIN_NONCE".into(), nonce.into()),
    ];
    let args = match launch {
        Some(l) => {
            env.extend(l.env);
            l.args
        }
        None => spec.args.iter().map(OsString::from).collect(),
    };
    env.extend(spec.env.iter().map(|(k, v)| (k.into(), v.into())));
    SpawnSpec {
        program: spec.program.clone(),
        args,
        cwd,
        env,
    }
}

/// Секрет интеграции, 128 бит (S6 §1.3). Ключи `RandomState` — из генератора ОС.
fn nonce() -> String {
    let s = RandomState::new();
    let t = Instant::now();
    let a = s.hash_one((t, 0u8));
    let b = s.hash_one((t, 1u8));
    format!("{a:016x}{b:016x}")
}

/// Один и тот же каталог (Windows: без учёта регистра и `\` в конце).
fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.display()
            .to_string()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Видимый cd перед командой (S6 §5: cmd и PowerShell без хуков): cmd — `cd /d`,
/// UNC — `pushd`; PowerShell — `Set-Location -LiteralPath`; posix — `cd --`.
fn visible_cd(kind: Option<&shells::ShellKind>, path: &str) -> String {
    use shells::ShellKind as K;
    match kind {
        Some(K::Cmd) if path.starts_with(r"\\") => format!("pushd \"{path}\"\r"),
        Some(K::Cmd) => format!("cd /d \"{path}\"\r"),
        Some(K::Pwsh | K::WinPowerShell) => {
            format!("Set-Location -LiteralPath '{}'\r", path.replace('\'', "''"))
        }
        _ => format!("cd -- '{}'\r", path.replace('\'', r"'\''")),
    }
}

/// Каталог сеанса: локальный путь вкладки или ближайший локальный предок
/// (архив — каталог, где он лежит).
pub fn session_cwd(loc: &Location) -> Option<PathBuf> {
    if loc.is_local() {
        return Some(loc.path.clone());
    }
    let outer = loc.outer.as_deref()?;
    if outer.is_local() {
        return outer.path.parent().map(Path::to_path_buf);
    }
    session_cwd(outer)
}

/// Клавиша по имени и модификаторам `ctrl+alt+shift` (`term.key`).
pub fn key_bytes(key: &str, mods: Option<&str>, term: input::Modes) -> Option<Vec<u8>> {
    let mut m = Mods::NONE;
    for part in mods.unwrap_or("").split('+').filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => m.ctrl = true,
            "alt" => m.alt = true,
            "shift" => m.shift = true,
            _ => return None,
        }
    }
    let key = key.to_ascii_lowercase();
    let text = (key.chars().count() == 1).then_some(key.as_str());
    let mut out = Vec::new();
    // Win32-input-mode (S6-20): полная запись клавиши; неизвестная — как VT.
    if term.win32_input && crate::vt::win32::key(&key, m, None, &mut out) {
        return Some(out);
    }
    input::key(&key, m, text, term, &mut out).then_some(out)
}

fn arg_err(name: &str, msg: String) -> CmdError {
    CmdError::Arg {
        name: name.into(),
        msg: msg.into(),
    }
}

fn no_session() -> CmdError {
    CmdError::Rejected(tr("term-no-session", &[]).into())
}

pub(crate) fn tr(key: &str, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

/// Готовый (или не запустившийся) сеанс из фоновой задачи.
struct StartedMsg {
    id: SessionId,
    result: Result<Session, String>,
}

impl Msg for StartedMsg {
    fn name(&self) -> &'static str {
        "term.started"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let Some(st) = cx.state.ext.get_mut::<TermState>() else {
            return;
        };
        let TermState {
            config, sessions, ..
        } = st;
        if let Some(err) = sessions.on_started(config, self.id, self.result) {
            cx.toast(Level::Error, err);
        }
        // Проверить, ответила ли интеграция.
        cx.rt.waker().wake_after(INTEG_TIMEOUT);
    }
}

/// Найденные shells и статистика выбора.
struct ShellsMsg {
    shells: Vec<ShellSpec>,
    usage: BTreeMap<CompactString, u32>,
}

impl Msg for ShellsMsg {
    fn name(&self) -> &'static str {
        "term.shells"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(st) = cx.state.ext.get_mut::<TermState>() else {
            return;
        };
        let TermState {
            config, sessions, ..
        } = st;
        let errors = sessions.on_shells(config, rt, self.shells, self.usage);
        for e in errors {
            cx.toast(Level::Error, e);
        }
    }
}

/// Событие сеанса (`SessionMsg`).
pub(crate) fn on_session_event(cx: &mut Cx<'_>, id: SessionId, ev: SessionEvent) {
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    // Конец команды `term.exec`: `D` (кроме промптов cd до неё) или выход процесса.
    let done = match &ev {
        SessionEvent::Exited(_) => Some(true),
        SessionEvent::Osc {
            event: OscEvent::Finished(_),
            ..
        } => Some(false),
        _ => None,
    };
    let effect = st.sessions.on_event(id, ev);
    let follow = matches!(st.config.sync_cwd.as_str(), "both" | "shell_to_panel");
    if let Some((shell, line)) = &effect.command {
        crate::history::record(st, cx.rt, shell, line);
    }
    if let Some(text) = effect.clip.clone() {
        cx.ui(urfin_core::UiEvent::Ext {
            id: urfin_core::CommandId::from_static("clip.write"),
            args: urfin_core::Args::default().with("text", text),
        });
    }
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    if effect.finished {
        // Команда кончилась: короткая — автопоказ не нужен; спрятанные им панели —
        // назад (S6 §3.3).
        st.submitted = None;
        if std::mem::take(&mut st.auto_hidden) && st.config.return_to_panels {
            crate::panels(cx, true);
        }
    }
    if let Some(exited) = done {
        crate::exec::finished(cx, id, exited);
    }
    // Shell сам сменил каталог (`cd ..`): активная панель — туда же (S6 §5.1). Таб на
    // архиве или sftp тоже уходит. Обратного cd не будет: панель совпадёт с shell.
    if let Some(p) = effect.cwd.filter(|_| follow) {
        let tab = cx.state.active_tab();
        if !(tab.loc.is_local() && same_path(&tab.loc.path, &p)) {
            cx.dispatch(
                urfin_core::Command::new("panel.go")
                    .arg("path", Location::local(p))
                    .from(urfin_core::Origin::Internal),
            );
        }
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
