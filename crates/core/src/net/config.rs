//! Секция `[ssh]` конфига (F03 §2.9): чистые данные — её читают `urfin-net`, настройки UI
//! (F03-14) и терминал. `[term.highlight]` — у терминала (S6).

use serde::{Deserialize, Serialize};

use super::PasswordManager;

/// Агент для ключей: `auto` — OpenSSH, затем Pageant.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    #[default]
    Auto,
    Openssh,
    Pageant,
}

impl AgentKind {
    /// Значение в конфиге.
    pub fn id(self) -> &'static str {
        match self {
            AgentKind::Auto => "auto",
            AgentKind::Openssh => "openssh",
            AgentKind::Pageant => "pageant",
        }
    }
}

/// Какой агент отвечает на машине: для пояснения в настройках «SSH» (F03 §2.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AgentSeen {
    OpenSsh,
    Pageant,
    Missing,
}

/// Запись сеансов; политика `ssh.recording` сильнее.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Recording {
    Off,
    #[default]
    Manual,
    Always,
}

impl Recording {
    /// Значение в конфиге.
    pub fn id(self) -> &'static str {
        match self {
            Recording::Off => "off",
            Recording::Manual => "manual",
            Recording::Always => "always",
        }
    }
}

/// Какие внешние источники профилей читать.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportCfg {
    pub ssh_config: bool,
    pub putty: bool,
}

impl Default for ImportCfg {
    fn default() -> Self {
        ImportCfg {
            ssh_config: true,
            putty: true,
        }
    }
}

/// Переподключение после обрыва.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReconnectCfg {
    pub tries: u32,
    pub interval_s: u32,
}

impl Default for ReconnectCfg {
    fn default() -> Self {
        ReconnectCfg {
            tries: 5,
            interval_s: 3,
        }
    }
}

/// Секция `[ssh]` конфига (F03 §2.9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "флажки секции конфига как в F03 §2.9"
)]
pub struct SshCfg {
    pub agent: AgentKind,
    pub default_keys: Vec<String>,
    pub import: ImportCfg,
    /// Пользовательский флажок; политика `password_cache` сильнее.
    pub password_cache: bool,
    pub password_manager: PasswordManager,
    pub password_cmd: String,
    /// Сколько минут секрет живёт в памяти сеанса; 0 — до отключения.
    pub secret_ttl_min: u32,
    pub keepalive_s: u32,
    pub reconnect: ReconnectCfg,
    pub transfer_channels: u8,
    pub recording: Recording,
    pub recordings_dir: String,
    pub multi_exec_confirm: bool,
}

impl Default for SshCfg {
    fn default() -> Self {
        SshCfg {
            agent: AgentKind::Auto,
            default_keys: ["~/.ssh/id_ed25519", "~/.ssh/id_ecdsa", "~/.ssh/id_rsa"]
                .map(String::from)
                .to_vec(),
            import: ImportCfg::default(),
            password_cache: false,
            password_manager: PasswordManager::None,
            password_cmd: String::new(),
            secret_ttl_min: 0,
            keepalive_s: 30,
            reconnect: ReconnectCfg::default(),
            transfer_channels: 4,
            recording: Recording::Manual,
            recordings_dir: r"%LOCALAPPDATA%\Urfin\recordings".into(),
            multi_exec_confirm: true,
        }
    }
}
