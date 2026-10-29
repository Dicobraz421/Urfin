//! Urfin — SSH и SFTP (F-03, `docs/specs/F03.md`), крейт фазы 3 (S8 §5).
//! Каркас (F03-01): handlers `ssh` и `sftp` с командами-заглушками, [`NetState`],
//! секция `[ssh]`, профили `connections.toml` и их импорт из `~/.ssh/config` и `PuTTY`,
//! история хостов, рантайм tokio. Сеть — только по явному действию пользователя.

mod auth;
mod bridge;
pub mod config;
mod conn;
pub mod history;
mod hostkey;
pub mod import;
pub mod profile;
pub mod rt;
mod sftp;
mod uri;

use std::path::PathBuf;

use urfin_core::{
    ArgSpec, CmdError, Command, CommandMeta, Core, Cx, Handler, Paths, QuitDecision, ValueKind,
};

pub use config::{ConnectionsFile, NetError, ProfileOverride, SshCfg};
pub use history::{History, HistoryEntry};
pub use profile::{Auth, Conflict, Note, Profile, ProfileSource, Profiles, Sources};
pub use rt::NetRt;

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

/// Namespace `ssh` (F03 §6 №6); клавиши — в keymap.
static SSH_COMMANDS: [CommandMeta; 10] = [
    // Ins в меню дисков.
    meta("ssh.new", "New connection", "ssh", &[]),
    meta("ssh.edit", "Edit connection", "ssh", &[]),
    meta("ssh.disconnect", "Disconnect", "ssh", &[]),
    meta("ssh.reconnect", "Reconnect", "ssh", &[]),
    // Shift+Enter в «Подключениях».
    meta("ssh.terminal", "SSH terminal", "ssh", &[]),
    // Ctrl+Tab, контекст `in_sshpair`.
    meta("ssh.pair_focus", "Switch panel and terminal", "ssh", &[]),
    meta("ssh.cd_here", "Go here in terminal", "ssh", &[]),
    // Ctrl+Alt+G.
    meta("ssh.group.edit", "Input group", "ssh", &[]),
    // Пробел в списке сеансов.
    meta("ssh.group.toggle", "Toggle input group", "ssh", &[]),
    // Esc, контекст `ssh_group`.
    meta("ssh.group.leave", "Leave input group", "ssh", &[]),
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
}

impl NetFiles {
    pub fn new(paths: &Paths) -> NetFiles {
        NetFiles {
            connections: paths.config_dir.join("connections.toml"),
            history: paths.state_dir.join("ssh").join("history.toml"),
        }
    }
}

/// Состояние SSH/SFTP в `AppState.ext`. Профили и история читаются задачами Runtime
/// по явному действию (меню дисков, «Подключения»), не при старте.
#[derive(Debug)]
pub struct NetState {
    pub cfg: SshCfg,
    /// `None` — без диска (`--smoke`, `--screens`).
    pub files: Option<NetFiles>,
    /// Последнее слияние источников; `None` — ещё не читались.
    pub profiles: Option<Profiles>,
    pub history: Option<History>,
    rt: NetRt,
}

impl NetState {
    pub fn new(cfg: SshCfg, files: Option<NetFiles>) -> NetState {
        NetState {
            cfg,
            files,
            profiles: None,
            history: None,
            rt: NetRt::new(),
        }
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
            cx.state.ext.insert(st);
        }
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let id = cmd.id.as_str();
        if !self.commands.iter().any(|m| m.id == id) {
            return Err(CmdError::Unknown(id.into()));
        }
        tracing::debug!(target: "net", cmd = id, "not implemented");
        Err(CmdError::Rejected("not yet".into()))
    }
}

/// Регистрирует handlers `ssh` и `sftp` без диска.
pub fn register(core: &mut Core, cfg: SshCfg) {
    register_with(core, cfg, None);
}

/// Как [`register`]; с `paths` — `connections.toml` и история хостов на диске.
pub fn register_with(core: &mut Core, cfg: SshCfg, paths: Option<&Paths>) {
    core.register(Box::new(NetHandler {
        namespace: "ssh",
        commands: &SSH_COMMANDS,
        pending: Some(NetState::new(cfg, paths.map(NetFiles::new))),
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
