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
    ArgSpec, CmdError, Command, CommandMeta, Core, Cx, Handler, Paths, Runtime, ValueKind,
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
    rt: Option<NetRt>,
}

impl NetState {
    pub fn new(cfg: SshCfg, files: Option<NetFiles>) -> NetState {
        NetState {
            cfg,
            files,
            profiles: None,
            history: None,
            rt: None,
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

    /// Рантайм tokio крейта; поток запускается при первом обращении.
    pub fn net_rt(&mut self, rt: &Runtime) -> &NetRt {
        self.rt.get_or_insert_with(|| NetRt::start(rt))
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
