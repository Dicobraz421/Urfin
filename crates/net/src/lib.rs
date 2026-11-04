//! Urfin — SSH и SFTP (F-03, `docs/specs/F03.md`), крейт фазы 3 (S8 §5).
//! Каркас (F03-01): handlers `ssh` и `sftp` с командами-заглушками, [`NetState`],
//! секция `[ssh]`, профили `connections.toml` и их импорт из `~/.ssh/config` и `PuTTY`,
//! история хостов, рантайм tokio. Сеть — только по явному действию пользователя.

mod auth;
pub mod bridge;
pub mod config;
pub mod conn;
pub mod history;
pub mod hostkey;
pub mod import;
pub mod live;
pub mod profile;
pub mod pwcmd;
pub mod rt;
pub mod secrets;
pub mod sftp;
#[cfg(windows)]
mod sys;
pub mod term;
pub mod uri;
pub mod vault;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use urfin_core::net::NetModel;
use urfin_core::{
    ArgSpec, CmdError, Command, CommandMeta, Core, Cx, Handler, Paths, QuitDecision, Side,
    ValueKind,
};

pub use config::{ConnectionsFile, NetError, ProfileOverride, SshCfg};
pub use history::{History, HistoryEntry};
pub use live::{Ask, AskKind, Form, Live, LiveState};
pub use profile::{Auth, Conflict, Note, Profile, ProfileSource, Profiles, Sources};
pub use rt::NetRt;
pub use sftp::SftpFs;
pub use uri::SftpUri;

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
) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category,
        args,
        repeatable: false,
    }
}

const PROFILE: ArgSpec = arg("profile", ValueKind::Str, false);
const SIDE: ArgSpec = arg("side", ValueKind::Str, false);

/// Namespace `ssh` (F03 §6 №6); клавиши — в keymap.
static SSH_COMMANDS: [CommandMeta; 14] = [
    // Ins в меню дисков.
    meta(
        "ssh.new",
        "New connection",
        "ssh",
        &[arg("host", ValueKind::Str, false), SIDE],
    ),
    meta("ssh.edit", "Edit connection", "ssh", &[PROFILE, SIDE]),
    meta("ssh.disconnect", "Disconnect", "ssh", &[PROFILE]),
    meta("ssh.reconnect", "Reconnect", "ssh", &[PROFILE]),
    // Shift+Enter в «Подключениях».
    meta(
        "ssh.terminal",
        "SSH terminal",
        "ssh",
        &[
            PROFILE,
            SIDE,
            // Другой сеанс профиля в паре / ещё один сеанс (меню шапки «ssh srv1 ▾»).
            arg("session", ValueKind::Int, false),
            arg("new", ValueKind::Bool, false),
        ],
    ),
    // Ctrl+Tab, контекст `in_sshpair`.
    meta("ssh.pair_focus", "Switch panel and terminal", "ssh", &[]),
    meta("ssh.cd_here", "Go here in terminal", "ssh", &[]),
    // Ctrl+Alt+G.
    meta("ssh.group.edit", "Input group", "ssh", &[]),
    // Пробел в списке сеансов.
    meta("ssh.group.toggle", "Toggle input group", "ssh", &[]),
    // Esc, контекст `ssh_group`.
    meta("ssh.group.leave", "Leave input group", "ssh", &[]),
    // Строка «Новое подключение» (F03-05): Enter — подключить, профиль — по флажку.
    meta(
        "ssh.connect",
        "Connect",
        "ssh",
        &[
            arg("host", ValueKind::Str, true),
            arg("user", ValueKind::Str, false),
            arg("port", ValueKind::Str, false),
            arg("via", ValueKind::Str, false),
            arg("auth", ValueKind::Str, false),
            arg("save", ValueKind::Bool, false),
            arg("edit", ValueKind::Str, false),
            SIDE,
        ],
    ),
    // Ответ на вопрос подключения: ключ сервера, пароль, код; Esc — `cancel`.
    meta(
        "ssh.answer",
        "Answer connection question",
        "ssh",
        &[
            arg("choice", ValueKind::Str, true),
            arg("text", ValueKind::Str, false),
            arg("remember", ValueKind::Bool, false),
        ],
    ),
    // Del в «Подключениях»: свой профиль.
    meta("ssh.remove", "Remove connection", "ssh", &[PROFILE]),
    // Перечитать профили, историю хостов и known_hosts («Импорт…», меню дисков).
    meta("ssh.reload", "Reload connections", "ssh", &[]),
];

/// Namespace `sftp` (F03 §6 №6).
static SFTP_COMMANDS: [CommandMeta; 2] = [
    // Enter на профиле.
    meta(
        "sftp.open",
        "Open SFTP connection",
        "sftp",
        &[
            arg("profile", ValueKind::Str, true),
            arg("path", ValueKind::Str, false),
            SIDE,
        ],
    ),
    // Ctrl+R, контекст `in_sftp`.
    meta("sftp.refresh", "Refresh SFTP folder", "sftp", &[]),
];

/// Файлы крейта на диске (F03 §6 №7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetFiles {
    /// `%APPDATA%\Urfin\connections.toml`.
    pub connections: PathBuf,
    /// `state/ssh/history.toml`.
    pub history: PathBuf,
    /// Каталог состояния: `state/ssh/run/<nonce>` — «Только в этот раз».
    pub state_dir: PathBuf,
}

impl NetFiles {
    pub fn new(paths: &Paths) -> NetFiles {
        NetFiles {
            connections: paths.config_dir.join("connections.toml"),
            history: paths.state_dir.join("ssh").join("history.toml"),
            state_dir: paths.state_dir.clone(),
        }
    }
}

/// Состояние SSH/SFTP крейта в `AppState.ext`: рантайм, пул, каналы. Данные для UI —
/// [`NetModel`] из core рядом (F03-05a). Профили и история читаются задачами Runtime по
/// явному действию (меню дисков, «Подключения»), не при старте.
#[derive(Debug)]
pub struct NetState {
    pub cfg: SshCfg,
    /// `None` — без диска (`--smoke`, `--screens`).
    pub files: Option<NetFiles>,
    /// Свои профили и правки как в `connections.toml` — для записи; `None` — ещё не
    /// читались.
    pub file: Option<ConnectionsFile>,
    /// Управление сеансами (ключ — имя профиля в нижнем регистре).
    ctl: HashMap<String, tokio::sync::mpsc::UnboundedSender<conn::Control>>,
    /// Каналы ответов на вопросы модели (по `Ask::id`).
    replies: HashMap<u64, live::Reply>,
    /// Память секретов и папка «Только в этот раз» сеансов профилей — и для их
    /// терминалов (ключ — имя профиля в нижнем регистре).
    shares: HashMap<String, Arc<conn::Share>>,
    /// Профиль, чей терминал ждёт подключения панели (`ssh.terminal`).
    pending_term: Option<String>,
    /// Остановка мостов, ещё без сеанса терминала (по nonce) и по сеансу (`origin`).
    stops: HashMap<String, tokio::sync::oneshot::Sender<()>>,
    bridges: HashMap<u32, tokio::sync::oneshot::Sender<()>>,
    /// Когда запущен терминал — «папка: сервер не сообщает» через `term::OSC7_WAIT`.
    term_started: HashMap<u32, std::time::Instant>,
    /// Последний учтённый OSC 7 терминала (`RemoteCwd::seq`).
    osc_seen: HashMap<u32, u64>,
    /// Backend `sftp`: тот же, что в реестрах VFS приложения.
    pub sftp: Arc<SftpFs>,
    /// Кэш паролей с политикой; `None` — без него (тесты, `--screens`).
    pub vault: Option<Arc<vault::Vault>>,
    pool: Arc<conn::Pool>,
    rt: NetRt,
}

impl NetState {
    pub fn new(cfg: SshCfg, files: Option<NetFiles>) -> NetState {
        NetState {
            cfg,
            files,
            file: None,
            ctl: HashMap::new(),
            replies: HashMap::new(),
            shares: HashMap::new(),
            pending_term: None,
            stops: HashMap::new(),
            bridges: HashMap::new(),
            term_started: HashMap::new(),
            osc_seen: HashMap::new(),
            sftp: Arc::new(SftpFs::new()),
            vault: None,
            pool: Arc::new(conn::Pool::new()),
            rt: NetRt::new(),
        }
    }

    /// Пустая модель для UI: путь `connections.toml` и пинг backend'а `sftp`.
    pub fn model(&self) -> NetModel {
        NetModel {
            connections: self.files.as_ref().map(|f| f.connections.clone()),
            ping: Some(Arc::clone(&self.sftp) as Arc<dyn urfin_core::net::Ping>),
            ..NetModel::default()
        }
    }

    /// «Запомнить» пароль под замком: `password_cache = false` задан политикой.
    pub fn remember_locked(&self) -> bool {
        self.vault.as_ref().is_some_and(|v| v.locked())
    }

    /// Источники профилей по `[ssh].import`.
    pub fn sources(&self) -> Sources {
        Sources {
            connections: self.files.as_ref().map(|f| f.connections.clone()),
            home: if self.cfg.import.ssh_config {
                dirs::home_dir()
            } else {
                None
            },
            putty: self.cfg.import.putty,
        }
    }

    /// Рантайм tokio крейта; поток запускается первой задачей.
    pub fn net_rt(&self) -> &NetRt {
        &self.rt
    }
}

/// Handler одного namespace: `ssh` или `sftp`. Каркас (F03-01): команды
/// зарегистрированы и пока отказывают.
#[derive(Debug)]
pub struct NetHandler {
    namespace: &'static str,
    commands: &'static [CommandMeta],
    /// Уходит в `AppState.ext` на первом pump (у `Core` нет доступа к состоянию до цикла).
    pending: Option<NetState>,
}

impl Handler for NetHandler {
    fn namespace(&self) -> &'static str {
        self.namespace
    }

    fn commands(&self) -> &'static [CommandMeta] {
        self.commands
    }

    /// Выход: поток tokio останавливается здесь — `Runtime::shutdown` ждёт его, а отмену
    /// Runtime он не смотрит (F03-01b).
    fn on_quit(&mut self, cx: &mut Cx<'_>) -> QuitDecision {
        if let Some(st) = cx.state.ext.get::<NetState>() {
            st.rt.stop();
        }
        QuitDecision::Allow
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some(st) = self.pending.take() {
            // `password_cache = false` политикой — записи Урфина удаляются при запуске
            // (решение №94.2): перечисление хранилища — задачей, не в UI-потоке.
            if let Some(v) = st.vault.clone().filter(|v| v.locked()) {
                cx.rt.spawn(
                    urfin_core::Lane::Bulk,
                    "net.vault",
                    cx.rt.token(),
                    move |_| {
                        let n = v.purge_if_forbidden();
                        if n > 0 {
                            tracing::info!(target: "net", n, "password cache purged by policy");
                        }
                    },
                );
            }
            cx.state.ext.insert(st.model());
            // Запись в `sftp://` для операций F5/F6/F7/F8 — методами `Vfs` backend'а из
            // реестра (решение 2026-11-04 «vfs-write»); здесь — только число каналов.
            st.sftp.set_transfer_channels(st.cfg.transfer_channels);
            cx.state.ext.insert(st);
        }
        if cx.state.ext.get::<NetState>().is_some() && self.namespace == "ssh" {
            term::pump(cx);
        }
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = cmd.id.as_str();
        if !self.commands.iter().any(|m| m.id == id) {
            return Err(CmdError::Unknown(id.into()));
        }
        if cx.state.ext.get::<NetState>().is_none() {
            return Err(CmdError::Rejected("ssh is not ready".into()));
        }
        handle(id, &cmd.args, cx)
    }
}

/// Команды `ssh.*` и `sftp.*` (F03-05).
#[allow(clippy::too_many_lines, reason = "таблица команд namespace подряд")]
fn handle(id: &str, a: &urfin_core::Args, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let side = match a.str("side") {
        Some("left") => Side::Left,
        Some("right") => Side::Right,
        _ => cx.state.active,
    };
    let named = a.str("profile").map(str::to_owned);
    let profile = |cx: &Cx<'_>| {
        named
            .clone()
            .or_else(|| active_profile(cx.state))
            .ok_or_else(|| CmdError::Rejected("no profile".into()))
    };
    let rejected = |e: String| CmdError::Rejected(e.into());
    match id {
        "sftp.open" => {
            let p = a.str("profile").unwrap_or_default().to_owned();
            live::open(cx, &p, a.str("path"), side).map_err(rejected)
        }
        "sftp.refresh" => {
            cx.dispatch(Command::new("panel.refresh"));
            Ok(())
        }
        "ssh.reload" => {
            live::reload(cx);
            Ok(())
        }
        "ssh.new" | "ssh.edit" => {
            let edit = if id == "ssh.edit" {
                Some(profile(cx)?)
            } else {
                None
            };
            let st = model(cx)?;
            let base = edit.as_deref().and_then(|n| st.profile(n)).cloned();
            let seq = st.form.as_ref().map_or(0, |f| f.seq) + 1;
            st.form = Some(match base {
                Some(p) => Form {
                    seq,
                    host: p.host.clone(),
                    user: p.user.clone(),
                    port: p.port.to_string(),
                    via: p.via.clone(),
                    auth: p.auth,
                    save: true,
                    edit: Some(p.name),
                    side,
                },
                None => Form {
                    seq,
                    host: a.str("host").unwrap_or_default().to_owned(),
                    user: String::new(),
                    port: profile::DEFAULT_PORT.to_string(),
                    via: String::new(),
                    auth: Auth::Agent,
                    save: true,
                    edit: None,
                    side,
                },
            });
            st.rev += 1;
            Ok(())
        }
        "ssh.connect" => {
            let st = model(cx)?;
            let auth = a.str("auth").and_then(live::parse_auth).unwrap_or_default();
            let p = live::form_profile(
                st,
                a.str("host").unwrap_or_default(),
                a.str("user").unwrap_or_default(),
                a.str("port").unwrap_or_default(),
                a.str("via").unwrap_or_default(),
                auth,
                a.str("edit"),
            )
            .map_err(rejected)?;
            st.form = None;
            st.rev += 1;
            let name = p.name.clone();
            if a.bool("save").unwrap_or(false) {
                live::save_profile(cx, p);
            } else {
                st.adhoc.retain(|q| !q.name.eq_ignore_ascii_case(&name));
                st.adhoc.push(Profile {
                    source: ProfileSource::History,
                    ..p
                });
            }
            live::open(cx, &name, None, side).map_err(rejected)
        }
        "ssh.answer" => {
            let choice = a.str("choice").unwrap_or("cancel").to_owned();
            let st = model(cx)?;
            if choice == "cancel" && st.asks.is_empty() {
                // Esc в строке «Новое подключение».
                st.form = None;
                st.rev += 1;
                return Ok(());
            }
            live::answer(
                cx,
                &choice,
                a.str("text"),
                a.bool("remember").unwrap_or(false),
            )
            .map_err(rejected)
        }
        "ssh.disconnect" => {
            let p = profile(cx)?;
            live::disconnect(cx, &p);
            Ok(())
        }
        "ssh.reconnect" => {
            let p = profile(cx)?;
            live::reconnect(cx, &p).map_err(rejected)
        }
        "ssh.remove" => {
            let p = profile(cx)?;
            live::remove_profile(cx, &p).map_err(rejected)
        }
        "ssh.terminal" => {
            let p = profile(cx)?;
            let side = match a.str("side") {
                Some("left") => Some(Side::Left),
                Some("right") => Some(Side::Right),
                _ => None,
            };
            let session = a.int("session").and_then(|n| u32::try_from(n).ok());
            term::terminal(cx, &p, side, session, a.bool("new").unwrap_or(false)).map_err(rejected)
        }
        "ssh.pair_focus" => term::pair_focus(cx).map_err(rejected),
        "ssh.cd_here" => term::cd_here(cx).map_err(rejected),
        _ => {
            tracing::debug!(target: "net", cmd = id, "not implemented");
            Err(CmdError::Rejected("not yet".into()))
        }
    }
}

/// Модель подключений (кладётся вместе с [`NetState`]).
fn model<'a>(cx: &'a mut Cx<'_>) -> Result<&'a mut NetModel, CmdError> {
    cx.state
        .ext
        .get_mut::<NetModel>()
        .ok_or_else(|| CmdError::Rejected("ssh is not ready".into()))
}

/// Профиль таба `sftp://` активной панели.
fn active_profile(s: &urfin_core::AppState) -> Option<String> {
    SftpUri::from_location(&s.panel(s.active).tab().loc).map(|u| u.profile)
}

/// Регистрирует handlers `ssh` и `sftp` без диска.
pub fn register(core: &mut Core, cfg: SshCfg) {
    register_with(core, cfg, None);
}

/// Как [`register`]; с `paths` — `connections.toml` и история хостов на диске.
pub fn register_with(core: &mut Core, cfg: SshCfg, paths: Option<&Paths>) {
    register_state(core, NetState::new(cfg, paths.map(NetFiles::new)));
}

/// Как [`register_with`], с общим backend `sftp` (он же — в реестрах VFS приложения) и
/// кэшем паролей.
pub fn register_shared(
    core: &mut Core,
    cfg: SshCfg,
    paths: Option<&Paths>,
    sftp: Arc<SftpFs>,
    vault: Option<Arc<vault::Vault>>,
) {
    let mut st = NetState::new(cfg, paths.map(NetFiles::new));
    st.sftp = sftp;
    st.vault = vault;
    register_state(core, st);
}

fn register_state(core: &mut Core, st: NetState) {
    core.register(Box::new(NetHandler {
        namespace: "ssh",
        commands: &SSH_COMMANDS,
        pending: Some(st),
    }));
    core.register(Box::new(NetHandler {
        namespace: "sftp",
        commands: &SFTP_COMMANDS,
        pending: None,
    }));
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_valid_and_in_namespace() {
        let mut reg = urfin_core::CommandRegistry::new();
        reg.register(&SSH_COMMANDS).unwrap();
        reg.register(&SFTP_COMMANDS).unwrap();
        assert!(SSH_COMMANDS.iter().all(|m| m.id.starts_with("ssh.")));
        assert!(SFTP_COMMANDS.iter().all(|m| m.id.starts_with("sftp.")));
        let open = Command::new("sftp.open").arg("profile", "web");
        assert_eq!(reg.validate(&open), Ok(()));
        assert!(reg.validate(&Command::new("sftp.open")).is_err());
    }

    #[test]
    fn command_titles_have_strings() {
        let catalog = urfin_i18n::Catalog::embedded();
        for lang in ["en", "ru"] {
            let lang = lang.parse().unwrap();
            for m in SSH_COMMANDS.iter().chain(&SFTP_COMMANDS) {
                let key = format!("cmd-{}", m.id.replace('.', "-"));
                assert!(catalog.has_own(&lang, &key), "{lang}: {key}");
            }
        }
    }

    #[test]
    fn quit_stops_net_thread() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        struct NoWake;
        impl urfin_core::Waker for NoWake {
            fn wake(&self) {}
            fn wake_after(&self, _d: Duration) {}
        }

        let root = urfin_core::Location::local(std::env::temp_dir());
        let boot = urfin_core::Boot::new(root.clone(), root);
        let mut core = Core::new(boot, urfin_core::Services::null(), Arc::new(NoWake));
        register(&mut core, SshCfg::default());
        core.pump(Duration::from_millis(5));
        // Задача без конца — как открытое подключение.
        let (rt, inbox) =
            urfin_core::Runtime::new(&urfin_core::RuntimeConfig::default(), Arc::new(NoWake));
        let (tx, rx) = mpsc::channel();
        let net = core.state().ext.get::<NetState>().unwrap().net_rt();
        assert!(net.spawn(&rt, async move {
            tx.send(()).unwrap();
            std::future::pending::<()>().await;
        }));
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        core.dispatch(Command::new("app.quit"));
        core.pump(Duration::from_millis(5));
        assert!(core.state().quit);
        let net = core.state().ext.get::<NetState>().unwrap().net_rt();
        assert!(!net.is_running());
        assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
        let _ = core.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn files_live_in_config_and_state() {
        let paths = Paths::in_dir(std::env::temp_dir().join("urfin-net-paths"));
        let f = NetFiles::new(&paths);
        assert_eq!(f.connections, paths.config_dir.join("connections.toml"));
        assert!(f.history.ends_with("ssh/history.toml"));
    }

    #[test]
    fn version_smoke() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }
}
