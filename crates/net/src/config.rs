//! Секция `[ssh]` (F03 §2.9) и `connections.toml` v1 (F03 §2.2, формат фиксирован —
//! запрос №7). Пароли, фразы-пароли и ключи сюда не пишутся: у записей нет таких
//! полей, а неизвестные поля при чтении отбрасываются.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use toml::value::Datetime;

use crate::profile::{Auth, Profile};

/// Версия формата `connections.toml`.
pub const CONNECTIONS_VERSION: u32 = 1;

/// Ошибка чтения файлов крейта. Текст — для лога и статуса раздела «Подключения».
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{path}: unsupported version {found}")]
    Version { path: PathBuf, found: u32 },
}

impl NetError {
    pub(crate) fn io(path: &Path, e: &io::Error) -> NetError {
        NetError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        }
    }

    pub(crate) fn parse(path: &Path, e: impl std::fmt::Display) -> NetError {
        NetError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        }
    }
}

/// Агент для ключей: `auto` — OpenSSH, затем Pageant.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    #[default]
    Auto,
    Openssh,
    Pageant,
}

/// Шаблон команды менеджера паролей; пусто — не выбран.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasswordManager {
    #[default]
    #[serde(rename = "")]
    None,
    #[serde(rename = "bearpass")]
    BearPass,
    #[serde(rename = "passwork")]
    Passwork,
    #[serde(rename = "1password")]
    OnePassword,
    #[serde(rename = "bitwarden")]
    Bitwarden,
    #[serde(rename = "keepassxc")]
    KeePassXc,
    #[serde(rename = "custom")]
    Custom,
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

/// Секция `[ssh]` конфига (F03 §2.9). `[term.highlight]` — у терминала (S6).
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

/// `[[override]]`: правки поверх импортированного профиля с тем же именем.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileOverride {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmarks: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Datetime>,
}

impl ProfileOverride {
    pub fn apply(&self, p: &mut Profile) {
        fn set<T: Clone>(dst: &mut T, src: Option<&T>) {
            if let Some(v) = src {
                dst.clone_from(v);
            }
        }
        set(&mut p.host, self.host.as_ref());
        set(&mut p.port, self.port.as_ref());
        set(&mut p.user, self.user.as_ref());
        set(&mut p.auth, self.auth.as_ref());
        set(&mut p.key, self.key.as_ref());
        set(&mut p.via, self.via.as_ref());
        set(&mut p.start_dir, self.start_dir.as_ref());
        set(&mut p.bookmarks, self.bookmarks.as_ref());
        set(&mut p.password_cmd, self.password_cmd.as_ref());
        set(&mut p.password_id, self.password_id.as_ref());
        if self.last_used.is_some() {
            p.last_used = self.last_used;
        }
    }
}

/// `%APPDATA%\Urfin\connections.toml` v1: свои профили и правки импортированных.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionsFile {
    pub version: u32,
    #[serde(default, rename = "profile", skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<Profile>,
    #[serde(default, rename = "override", skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<ProfileOverride>,
}

impl Default for ConnectionsFile {
    fn default() -> Self {
        ConnectionsFile {
            version: CONNECTIONS_VERSION,
            profiles: Vec::new(),
            overrides: Vec::new(),
        }
    }
}

impl ConnectionsFile {
    /// Разбор текста; `path` — для ошибок. Версия, кроме 1, — ошибка.
    pub fn parse(text: &str, path: &Path) -> Result<ConnectionsFile, NetError> {
        let file: ConnectionsFile = toml::from_str(text).map_err(|e| NetError::parse(path, e))?;
        if file.version != CONNECTIONS_VERSION {
            return Err(NetError::Version {
                path: path.to_path_buf(),
                found: file.version,
            });
        }
        Ok(file)
    }

    /// Чтение файла; нет файла — пустой v1. I/O: только задачами Runtime.
    pub fn load(path: &Path) -> Result<ConnectionsFile, NetError> {
        match std::fs::read_to_string(path) {
            Ok(text) => ConnectionsFile::parse(&text, path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ConnectionsFile::default()),
            Err(e) => Err(NetError::io(path, &e)),
        }
    }

    pub fn to_toml(&self) -> String {
        let body = toml::to_string(self).expect("connections.toml serializes");
        format!("# Urfin connections. Passwords and keys are never stored here.\n{body}")
    }

    /// Атомарная запись (temp + rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, self.to_toml().as_bytes())
    }
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Атомарная запись: временный файл рядом, fsync, rename поверх.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(
        ".{}.{}-{}.tmp",
        name.as_deref().unwrap_or("file"),
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ssh_section_snapshot() {
        insta::assert_snapshot!(toml::to_string(&SshCfg::default()).unwrap());
    }

    #[test]
    fn ssh_section_reads_spec_values() {
        let cfg: SshCfg = toml::from_str(
            r#"
            agent = "pageant"
            import = { ssh_config = false, putty = true }
            password_manager = "1password"
            recording = "always"
            reconnect = { tries = 2, interval_s = 10 }
            "#,
        )
        .unwrap();
        assert_eq!(cfg.agent, AgentKind::Pageant);
        assert!(!cfg.import.ssh_config && cfg.import.putty);
        assert_eq!(cfg.password_manager, PasswordManager::OnePassword);
        assert_eq!(cfg.recording, Recording::Always);
        assert_eq!(cfg.reconnect.tries, 2);
        assert_eq!(cfg.keepalive_s, 30);
        let empty: SshCfg = toml::from_str(r#"password_manager = """#).unwrap();
        assert_eq!(empty, SshCfg::default());
    }

    #[test]
    fn version_other_than_1_is_rejected() {
        let p = Path::new("connections.toml");
        assert!(matches!(
            ConnectionsFile::parse("version = 2", p),
            Err(NetError::Version { found: 2, .. })
        ));
        assert!(matches!(
            ConnectionsFile::parse("[[profile]]\nname = \"a\"", p),
            Err(NetError::Parse { .. })
        ));
        assert_eq!(
            ConnectionsFile::parse("version = 1", p).unwrap(),
            ConnectionsFile::default()
        );
    }

    #[test]
    fn missing_file_is_empty_and_save_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("connections.toml");
        assert_eq!(
            ConnectionsFile::load(&path).unwrap(),
            ConnectionsFile::default()
        );
        let file = ConnectionsFile {
            profiles: vec![Profile::new("a", "10.0.0.1")],
            ..ConnectionsFile::default()
        };
        file.save(&path).unwrap();
        assert_eq!(ConnectionsFile::load(&path).unwrap(), file);
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["connections.toml"]);
    }
}
