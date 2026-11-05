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
    Anchor, CancelToken, Choice, CmdError, Cx, Lane, Level, Location, Msg, MsgSink, PromptSpec,
    QuitDecision, Runtime,
};

use crate::TermState;
use crate::config::TermConfig;
use crate::feed::FeedSink;
use crate::rec::{self, Recording, SshRule, UserMode};
use crate::session::{
    CoreSink, Mark, OscEvent, Session, SessionEvent, SessionId, SessionOpts, Tap, TapRx,
};
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

/// Что запущено в сеансе (S0 F03 №3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SessionKind {
    /// Shell из списка shells: интеграция, cmdline Урфина.
    #[default]
    Shell,
    /// Внешняя программа (`ssh.exe`, F03 §3.1): без интеграции, ввод сырой, OSC 7 доверен.
    External(Arc<External>),
    /// Ленточный сеанс (S0 v6 №15): эмулятор без процесса, только для чтения.
    Feed,
}

/// Запуск внешней программы: `argv[0]` — программа, окружение — поверх окружения Урфина.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct External {
    pub argv: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    /// Заголовок в шапке и меню сеансов: «ssh srv1».
    pub title: String,
}

/// Папка внешнего сеанса по OSC 7 (`file://host/path`): путь POSIX как прислан.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCwd {
    pub host: String,
    pub path: String,
    /// Растёт с каждым OSC 7: тот же путь второй раз — тоже событие.
    pub seq: u64,
}

/// Сеанс в пуле.
#[allow(clippy::struct_excessive_bools, reason = "независимые признаки сеанса")]
#[derive(Debug)]
pub struct Slot {
    pub id: SessionId,
    /// Первый сеанс цепочки перезапусков (`term.restart`, Enter после выхода): по нему
    /// пара узнаёт свой терминал после перезапуска.
    pub origin: SessionId,
    pub shell: CompactString,
    pub kind: SessionKind,
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
    /// Папка внешнего сеанса по OSC 7 (F03 §3.2).
    pub remote: Option<RemoteCwd>,
    /// Свой размер PTY (терминал пары, F03 §3.2): размер окна его не меняет.
    pin: Option<PtySize>,
    /// Отвод сырого вывода (S0 F03 №3): ставится в сетку до старта reader.
    tap: Arc<Tap>,
    /// Идущая запись сеанса (F03 §3.5, Ctrl+Alt+R).
    pub rec: Option<Recording>,
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

    /// Отвод сырого вывода сеанса (`TermApi::tap`, S0 F03 №3): подписка видит вывод с
    /// этого момента; у нового слота — с первого байта.
    pub fn tap(&self) -> TapRx {
        self.tap.subscribe()
    }

    /// Ввод в сеанс заблокирован: обязательная запись не пишется (F03 §3.5).
    pub fn input_blocked(&self) -> bool {
        self.rec.as_ref().is_some_and(Recording::blocks_input)
    }

    /// Имя записи и заголовок: профиль SSH («ssh srv1» → «srv1») или shell.
    fn rec_plan(&self, size: PtySize, required: Option<urfin_core::PolicySource>) -> rec::Plan {
        let (name, shell) = match &self.kind {
            SessionKind::External(e) => (
                e.title.strip_prefix("ssh ").unwrap_or(&e.title).to_owned(),
                e.argv
                    .first()
                    .and_then(|a| Path::new(a).file_name())
                    .map_or_else(
                        || "ssh.exe".to_owned(),
                        |f| f.to_string_lossy().into_owned(),
                    ),
            ),
            SessionKind::Shell | SessionKind::Feed => {
                (self.shell.to_string(), self.shell.to_string())
            }
        };
        rec::Plan {
            name,
            shell,
            size: self.shared.as_ref().map_or(size, |s| s.size()),
            required,
        }
    }

    /// Внешняя программа (`ssh.exe`), не shell.
    pub fn external(&self) -> Option<&External> {
        match &self.kind {
            SessionKind::External(e) => Some(e),
            SessionKind::Shell | SessionKind::Feed => None,
        }
    }

    /// Ленточный сеанс: процесса нет, ввод некуда.
    pub fn is_feed(&self) -> bool {
        self.kind == SessionKind::Feed
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
    /// Настройки записи сеансов: политика `ssh.recording` и `[ssh]` (F03 §3.5).
    pub(crate) rec: rec::Settings,
    /// Ленточные сеансы по заголовку: закрытый слот возвращается со своим выводом.
    feeds: Vec<(CompactString, FeedSink)>,
    /// Сеанс, вместо которого показан ленточный.
    feed_back: Option<SessionId>,
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

    /// Сеанс без интеграции закончил команду (по процессам job, S6 §9): маркера `D`
    /// не будет, «выполняется» снять самим.
    pub(crate) fn mark_idle(&mut self, id: SessionId) {
        if let Some(s) = self.slot_mut(id) {
            s.running = false;
        }
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
        if self.slots.iter().all(Slot::is_feed)
            && let Some(id) = self.default_shell(cfg).map(|s| s.id.clone())
        {
            // Лента уже на экране — shell готовится за ней.
            let activate = self.active.is_none();
            let _ = self.start(cfg, rt, &id, self.home.clone(), activate);
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
        let id = self.push_slot(spec.id.clone(), SessionKind::Shell, spec.path_style.clone());
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
        self.launch(cfg, rt, id, spawn, Some(nonce), activate, (files, cwd_dir));
        Ok(id)
    }

    /// Внешняя программа в сеансе (F03 §3.1): слот появляется сразу (`Starting`), фаза —
    /// без интеграции (вид «экран», ввод сырой).
    pub(crate) fn start_external(
        &mut self,
        cfg: &TermConfig,
        rt: &Runtime,
        ext: &Arc<External>,
        activate: bool,
        pin: Option<PtySize>,
    ) -> Result<SessionId, CmdError> {
        let program = ext
            .argv
            .first()
            .map(PathBuf::from)
            .ok_or_else(|| arg_err("argv", "empty".into()))?;
        let id = self.push_slot(
            "ssh".into(),
            SessionKind::External(Arc::clone(ext)),
            crate::shells::PathStyle::Windows,
        );
        if let Some(s) = self.slot_mut(id) {
            s.pin = pin;
        }
        if activate {
            self.set_active(cfg, id);
        }
        // Обязательная запись и `[ssh] recording = always` — с первого байта: подписка
        // на отвод до запуска процесса.
        match self.rec.ssh() {
            SshRule::Required(src) => self.record_start(rt, id, Some(src)),
            SshRule::User(UserMode::Always) => self.record_start(rt, id, None),
            SshRule::Forbidden(_) | SshRule::User(_) => {}
        }
        let mut env: Vec<(OsString, OsString)> = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("TERM_PROGRAM".into(), "Urfin".into()),
            ("TERM_PROGRAM_VERSION".into(), crate::version().into()),
            ("COLORTERM".into(), "truecolor".into()),
        ];
        env.extend(ext.env.iter().cloned());
        let spawn = SpawnSpec {
            program,
            args: ext.argv[1..].to_vec(),
            cwd: None,
            env,
        };
        self.launch(cfg, rt, id, spawn, None, activate, (None, None));
        Ok(id)
    }

    fn push_slot(
        &mut self,
        shell: CompactString,
        kind: SessionKind,
        path_style: crate::shells::PathStyle,
    ) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;
        self.slots.push(Slot {
            id,
            origin: id,
            shell,
            kind,
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
            path_style,
            started: Instant::now(),
            expects_marks: false,
            last_exit: None,
            pending_sync: None,
            cwd_seen: false,
            submitted_line: None,
            blocks: std::collections::VecDeque::new(),
            raw: false,
            remote: None,
            pin: None,
            tap: Arc::default(),
            rec: None,
        });
        id
    }

    /// Процесс сеанса `id` — задачей Interactive; готовый [`Session`] приходит `Msg`.
    /// `dirs` — каталог скриптов интеграции и каталог файла тихого cd.
    #[allow(clippy::too_many_arguments, reason = "запуск сеанса — одним местом")]
    fn launch(
        &self,
        cfg: &TermConfig,
        rt: &Runtime,
        id: SessionId,
        spawn: SpawnSpec,
        nonce: Option<String>,
        activate: bool,
        dirs: (Option<PathBuf>, Option<PathBuf>),
    ) {
        let (files, cwd_dir) = dirs;
        let size = self.launch_size(id);
        let tap = self
            .slots
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.tap.clone())
            .unwrap_or_default();
        let opts = SessionOpts {
            size,
            scrollback: if activate {
                cfg.scrollback
            } else {
                cfg.background_scrollback
            },
            osc52: cfg.osc52.to_string(),
            nonce,
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
            let result =
                Session::start_tapped(id, &spawn, &opts, sink, tap).map_err(|e| e.to_string());
            // Shutdown: сеанс закроется в Drop.
            let _ = t.sink.send(Box::new(StartedMsg { id, result }), &t.cancel);
        });
    }

    /// Размер PTY при запуске сеанса `id`: свой, иначе окна.
    fn launch_size(&self, id: SessionId) -> PtySize {
        self.slots
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.pin)
            .or(self.area)
            .unwrap_or(START_SIZE)
    }

    /// Сеанс стал активным: прежний — в фон (скроллбэк урезается).
    pub(crate) fn set_active(&mut self, cfg: &TermConfig, id: SessionId) {
        if self.active == Some(id) {
            return;
        }
        // Внешний сеанс (терминал пары) виден и в фоне: скроллбэк не урезается; лента
        // тоже, и показ ленты не уводит shell в фон.
        let to_feed = self.slots.iter().any(|s| s.id == id && s.is_feed());
        if let Some(old) = self.active.filter(|_| !to_feed)
            && let Some(sh) = self
                .slot_mut(old)
                .filter(|s| s.kind == SessionKind::Shell)
                .and_then(|s| s.shared.clone())
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
        // Лента — не сеанс для ввода: её показывает своя команда.
        let ids: Vec<SessionId> = self
            .slots
            .iter()
            .filter(|s| !s.is_feed())
            .map(|s| s.id)
            .collect();
        let n = ids.len();
        if n == 0 {
            return Err(no_session());
        }
        let cur = self
            .active
            .and_then(|a| ids.iter().position(|s| *s == a))
            .unwrap_or(0);
        let next = (cur.cast_signed() + step)
            .rem_euclid(n.cast_signed())
            .cast_unsigned();
        self.set_active(cfg, ids[next]);
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
        let feed = self.slots.remove(pos).is_feed();
        if self.active == Some(id) {
            self.active = None;
            // Лента уступает тому, вместо кого показана, иначе — ближайшему справа.
            let back = self.feed_back.take().filter(|_| feed);
            let (left, right) = self.slots.split_at(pos);
            let near = right
                .iter()
                .chain(left.iter().rev())
                .find(|s| !s.is_feed())
                .map(|s| s.id);
            let next = back.filter(|b| self.slots.iter().any(|s| s.id == *b));
            if let Some(next) = next.or(near) {
                self.set_active(cfg, next);
            }
        }
        Ok(())
    }

    /// Ленточный сеанс `title`: есть — его вход, закрытый — слот возвращается со своим
    /// выводом, иначе новый. Активным не становится.
    pub(crate) fn feed(&mut self, cfg: &TermConfig, title: &str, wake: MsgSink) -> FeedSink {
        let known = self.feeds.iter().find(|(t, _)| t == title);
        let sink = if let Some((_, s)) = known {
            s.clone()
        } else {
            let size = self.area.unwrap_or(START_SIZE);
            let s = FeedSink::new(size, cfg.scrollback, &cfg.osc52, wake);
            self.feeds.push((title.into(), s.clone()));
            s
        };
        if !self.slots.iter().any(|s| s.is_feed() && s.shell == title) {
            let id = self.push_slot(
                title.into(),
                SessionKind::Feed,
                crate::shells::PathStyle::Windows,
            );
            if let Some(size) = self.area {
                sink.shared().resize(size);
            }
            if let Some(slot) = self.slot_mut(id) {
                slot.title = Some(title.to_owned());
                slot.shared = Some(sink.shared().clone());
                slot.state = SlotState::Running;
            }
        }
        sink
    }

    /// Показать ленточный сеанс `title` вместо активного.
    pub(crate) fn feed_enter(&mut self, cfg: &TermConfig, title: &str) {
        let Some(id) = self
            .slots
            .iter()
            .find(|s| s.is_feed() && s.shell == title)
            .map(|s| s.id)
        else {
            return;
        };
        if self.active != Some(id) {
            if !self.active().is_some_and(Slot::is_feed) {
                self.feed_back = self.active;
            }
            self.set_active(cfg, id);
        }
    }

    /// Панели вернулись: лента уступает сеансу, вместо которого показана.
    pub(crate) fn feed_leave(&mut self, cfg: &TermConfig) {
        if !self.active().is_some_and(Slot::is_feed) {
            return;
        }
        let next = self
            .feed_back
            .take()
            .filter(|b| self.slots.iter().any(|s| s.id == *b))
            .or_else(|| self.slots.iter().find(|s| !s.is_feed()).map(|s| s.id));
        match next {
            Some(id) => self.set_active(cfg, id),
            None => self.active = None,
        }
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
        let pin = self.slots[pos].pin;
        let id = match self.slots[pos].kind.clone() {
            SessionKind::External(ext) => self.start_external(cfg, rt, &ext, false, pin)?,
            SessionKind::Shell => self.start(cfg, rt, &shell, None, false)?,
            SessionKind::Feed => return Ok(()),
        };
        let mut new = self.slots.pop().expect("slot just pushed");
        new.origin = self.slots[pos].origin;
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
        let (state, feed) = self
            .slot_mut(id)
            .map(|s| (s.state, s.is_feed()))
            .ok_or_else(no_session)?;
        if feed {
            return Ok(());
        }
        if matches!(state, SlotState::Exited(_) | SlotState::Failed) {
            self.restart(cfg, rt)?;
            if restart_only {
                return Ok(());
            }
        }
        let id = self.active.ok_or_else(no_session)?;
        let slot = self.slot_mut(id).ok_or_else(no_session)?;
        if slot.input_blocked() {
            return Err(rec_blocked());
        }
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
        for s in self.slots.iter().filter(|s| s.pin.is_none()) {
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

    /// Свой размер сеанса `id` (терминал пары): `None` — снова размер окна.
    pub(crate) fn pin(&mut self, id: SessionId, size: Option<PtySize>) -> Result<(), CmdError> {
        let area = self.area;
        let slot = self
            .slot_mut(id)
            .ok_or_else(|| arg_err("id", "no such session".into()))?;
        if slot.pin == size {
            return Ok(());
        }
        slot.pin = size;
        if let Some(size) = size.or(area) {
            if let Some(sess) = &slot.session {
                sess.resize(size);
            } else if let Some(sh) = &slot.shared {
                sh.resize(size);
            }
        }
        Ok(())
    }

    /// Сделать активным сеанс `id` (пара, F03 §3.2).
    pub(crate) fn activate(&mut self, cfg: &TermConfig, id: SessionId) -> Result<(), CmdError> {
        if !self.slots.iter().any(|s| s.id == id) {
            return Err(arg_err("id", "no such session".into()));
        }
        self.set_active(cfg, id);
        Ok(())
    }

    /// Начать запись сеанса `id` (уже идущая остаётся).
    pub(crate) fn record_start(
        &mut self,
        rt: &Runtime,
        id: SessionId,
        required: Option<urfin_core::PolicySource>,
    ) {
        let size = self.launch_size(id);
        let settings = self.rec.clone();
        let Some(slot) = self.slot_mut(id) else {
            return;
        };
        if slot.rec.as_ref().is_some_and(|r| !r.shared.failed()) {
            return;
        }
        let plan = slot.rec_plan(size, required);
        slot.rec = Some(rec::start(rt, &slot.tap, &settings, &plan));
    }

    /// `term.record.toggle` (Ctrl+Alt+R) для активного сеанса: политика `ssh.recording`
    /// и `[ssh] recording` — только для SSH (F03 §3.5).
    pub(crate) fn record_toggle(&mut self, rt: &Runtime) -> Result<(), CmdError> {
        let id = self.active.ok_or_else(no_session)?;
        let rule = self.rec.ssh();
        let slot = self.slot_mut(id).ok_or_else(no_session)?;
        if slot.is_feed() {
            return Ok(());
        }
        let recording = slot.rec.as_ref().is_some_and(|r| !r.shared.failed());
        match toggle(slot.external().is_some(), rule, recording) {
            Toggle::Start(required) => {
                self.record_start(rt, id, required);
                Ok(())
            }
            Toggle::Stop => {
                slot.rec = None;
                Ok(())
            }
            Toggle::Deny(key) => Err(CmdError::Rejected(tr(key, &[]).into())),
        }
    }

    /// Новые настройки записи (каждый pump): политика применяется на лету — при
    /// `required` SSH-сеансы начинают запись, при `forbidden` останавливают.
    pub(crate) fn set_rec(&mut self, rt: &Runtime, settings: rec::Settings) {
        self.rec = settings;
        let rule = self.rec.ssh();
        let ids: Vec<SessionId> = self
            .slots
            .iter()
            .filter(|s| {
                s.external().is_some()
                    && !matches!(s.state, SlotState::Exited(_) | SlotState::Failed)
            })
            .map(|s| s.id)
            .collect();
        for id in ids {
            match rule {
                SshRule::Required(src) => {
                    self.record_start(rt, id, Some(src));
                    if let Some(r) = self.slot_mut(id).and_then(|s| s.rec.as_mut()) {
                        r.required = Some(src);
                    }
                }
                SshRule::Forbidden(_) => {
                    if let Some(s) = self.slot_mut(id) {
                        s.rec = None;
                    }
                }
                SshRule::User(_) => {
                    if let Some(r) = self.slot_mut(id).and_then(|s| s.rec.as_mut()) {
                        r.required = None;
                    }
                }
            }
        }
    }

    /// Новые ошибки записи: `(обязательна, текст)`. Необязательная запись после
    /// ошибки снимается; обязательная остаётся и держит ввод заблокированным.
    pub(crate) fn rec_failures(&mut self) -> Vec<(bool, String)> {
        let mut out = Vec::new();
        for s in &mut self.slots {
            let Some(r) = s.rec.as_mut().filter(|r| !r.reported && r.shared.failed()) else {
                continue;
            };
            r.reported = true;
            let required = r.required.is_some();
            out.push((required, r.shared.error().unwrap_or_default()));
            if !required {
                s.rec = None;
            }
        }
        out
    }

    /// Идёт запись хотя бы одного сеанса.
    pub fn recording(&self) -> bool {
        self.slots.iter().any(|s| s.rec.is_some())
    }

    /// Байты в сеанс `id` (не обязательно активный): `ssh.cd_here` пары. Вышедший —
    /// не перезапускается.
    pub(crate) fn write_to(&mut self, id: SessionId, bytes: Vec<u8>) -> Result<(), CmdError> {
        let slot = self.slot_mut(id).ok_or_else(no_session)?;
        if matches!(slot.state, SlotState::Exited(_) | SlotState::Failed) {
            return Err(no_session());
        }
        if slot.input_blocked() {
            return Err(rec_blocked());
        }
        match &slot.session {
            Some(s) => s.write(bytes),
            None => slot.queued.extend_from_slice(&bytes),
        }
        Ok(())
    }

    /// Ввод во все сеансы цепочек `ids` одним проходом (`TermApi::broadcast`, F03 §3.4):
    /// байты для режимов каждого сеанса. Вышедшие и с заблокированным вводом (обязательная
    /// запись) пропускаются, остальные получают один и тот же порядок: у каждого сеанса
    /// свой writer, очередь — FIFO. Ни одного живого — ошибка.
    pub(crate) fn broadcast_with(
        &mut self,
        ids: &[SessionId],
        mut bytes: impl FnMut(input::Modes) -> Vec<u8>,
    ) -> Result<(), CmdError> {
        let mut sent = 0;
        for id in ids {
            let Some(slot) = self.slots.iter_mut().find(|s| s.origin == *id) else {
                continue;
            };
            if matches!(slot.state, SlotState::Exited(_) | SlotState::Failed)
                || slot.input_blocked()
            {
                continue;
            }
            let b = bytes(
                slot.shared
                    .as_ref()
                    .map(|s| s.input_modes())
                    .unwrap_or_default(),
            );
            match &slot.session {
                Some(s) => s.write(b),
                None => slot.queued.extend_from_slice(&b),
            }
            sent += 1;
        }
        if sent == 0 {
            return Err(no_session());
        }
        Ok(())
    }

    /// Сеанс над фейковым PTY.
    #[cfg(test)]
    pub(crate) fn push_fake(&mut self, kind: SessionKind, s: Session) -> SessionId {
        let id = self.push_slot("ssh".into(), kind, crate::shells::PathStyle::Windows);
        if let Some(slot) = self.slot_mut(id) {
            slot.shared = Some(s.shared().clone());
            slot.session = Some(s);
            slot.state = SlotState::Running;
        }
        id
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
                // Размер пары пришёл, пока процесс запускался.
                if let Some(pin) = slot.pin.filter(|p| *p != s.shared().size()) {
                    s.resize(pin);
                }
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
                // `ssh.exe` с ошибкой (F03 §3.1): «соединение закрыто», Enter — заново.
                let line = match code {
                    Some(c) if c != 0 && slot.external().is_some() => format!(
                        "{}\r\n{}",
                        tr("term-ssh-closed", &[("code", &c.to_string())]),
                        tr("term-ssh-reconnect", &[])
                    ),
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
                OscEvent::Cwd { path, source, host } => {
                    // Внешний сеанс (`ssh.exe`): папка — удалённая, панель ведёт пара.
                    if slot.external().is_some() {
                        if source != crate::session::CwdSource::FnCwd {
                            let seq = slot.remote.as_ref().map_or(1, |r| r.seq + 1);
                            slot.remote = Some(RemoteCwd { host, path, seq });
                        }
                        return Effect::default();
                    }
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
    pub(crate) fn quit_decision(&self) -> QuitDecision {
        let n = self.slots.iter().filter(|s| s.busy()).count();
        if n == 0 {
            // Сеансы не закрывать: вето может наложить handler после нас (редактор
            // «Не сохранено» → «Отмена»). Выход состоялся — Core::shutdown роняет
            // состояние, Drop сеанса закрывает ConPTY, потоки кончатся сами.
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

/// Что делает Ctrl+Alt+R.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Toggle {
    /// Начать; `Some` — обязательна по политике.
    Start(Option<urfin_core::PolicySource>),
    Stop,
    /// Отказ: ключ сообщения.
    Deny(&'static str),
}

/// Решение Ctrl+Alt+R: локальный терминал — всегда по выбору; SSH — по политике
/// `ssh.recording`, при `user` — по `[ssh] recording` (`off` — команда выключена).
pub(crate) fn toggle(ssh: bool, rule: SshRule, recording: bool) -> Toggle {
    if !ssh {
        return if recording {
            Toggle::Stop
        } else {
            Toggle::Start(None)
        };
    }
    match rule {
        SshRule::Required(_) if recording => Toggle::Deny("term-rec-required"),
        SshRule::Required(src) => Toggle::Start(Some(src)),
        SshRule::Forbidden(_) => Toggle::Deny("term-rec-forbidden"),
        SshRule::User(UserMode::Off) => Toggle::Deny("term-rec-off"),
        SshRule::User(_) if recording => Toggle::Stop,
        SshRule::User(_) => Toggle::Start(None),
    }
}

fn rec_blocked() -> CmdError {
    CmdError::Rejected(tr("term-rec-blocked", &[]).into())
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
    // Команда `term.exec` (и промпты cd перед ней): панели вернёт её конец.
    let exec = st.exec_back.as_ref().is_some_and(|b| b.session == id);
    if effect.finished && !exec {
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
