//! Профиль подключения и слияние источников (F03 §2.2): свои (`connections.toml`),
//! `~/.ssh/config`, сеансы `PuTTY`. При совпадении имён: `urfin` > `config` > `putty`.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use toml::value::Datetime;
use urfin_i18n::{FluentArgs, I18n};

use crate::config::{ConnectionsFile, NetError};
use crate::import::{putty, ssh_config};

/// Порт SSH по умолчанию.
pub const DEFAULT_PORT: u16 = 22;

/// Способ входа.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    /// Агент OpenSSH или Pageant, затем ключи `[ssh].default_keys`.
    #[default]
    Agent,
    Key,
    Password,
    /// Пароль — командой менеджера паролей.
    Command,
}

/// Откуда профиль. Порядок вариантов — приоритет при совпадении имён (первый сильнее).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProfileSource {
    #[default]
    Urfin,
    Config,
    Putty,
}

impl ProfileSource {
    pub fn label(self, i18n: &I18n) -> String {
        i18n.get(match self {
            ProfileSource::Urfin => "net-source-urfin",
            ProfileSource::Config => "net-source-config",
            ProfileSource::Putty => "net-source-putty",
        })
    }
}

/// Подпись в свойствах импортированного профиля.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// Директива `ssh_config` пропущена: «не поддерживается: Match (строка N)».
    Unsupported {
        directive: String,
        file: PathBuf,
        line: u32,
    },
    /// `.ppk` из `PuTTY`: ключ — через Pageant.
    KeyViaPageant,
}

impl Note {
    pub fn text(&self, i18n: &I18n) -> String {
        match self {
            Note::Unsupported {
                directive, line, ..
            } => {
                let mut a = FluentArgs::new();
                a.set("directive", directive.clone());
                a.set("line", line.to_string());
                i18n.fmt("net-note-unsupported", &a)
            }
            Note::KeyViaPageant => i18n.get("net-note-pageant"),
        }
    }
}

/// Профиль: запись `[[profile]]` формата v1 и поля, которые знает только память
/// (источник, keep-alive из `ssh_config`, подписи). Секретов здесь нет.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Authority в `sftp://<имя>/…`.
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub user: String,
    pub auth: Auth,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// `ProxyJump`: имя профиля или `user@host:port`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub via: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub start_dir: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<String>,
    /// Пусто — из `[ssh].password_cmd`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password_cmd: String,
    /// Подстановка `{id}` команды пароля.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Datetime>,
    #[serde(skip)]
    pub source: ProfileSource,
    /// `ServerAliveInterval` из `ssh_config`; иначе — `[ssh].keepalive_s`.
    #[serde(skip)]
    pub keepalive_s: Option<u32>,
    #[serde(skip)]
    pub notes: Vec<Note>,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: String::new(),
            host: String::new(),
            port: DEFAULT_PORT,
            user: String::new(),
            auth: Auth::Agent,
            key: String::new(),
            via: String::new(),
            start_dir: String::new(),
            bookmarks: Vec::new(),
            password_cmd: String::new(),
            password_id: String::new(),
            last_used: None,
            source: ProfileSource::Urfin,
            keepalive_s: None,
            notes: Vec::new(),
        }
    }
}

impl Profile {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Profile {
        Profile {
            name: name.into(),
            host: host.into(),
            ..Profile::default()
        }
    }
}

/// Имя профиля скрыто профилем с тем же именем из более сильного источника.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub name: String,
    pub used: ProfileSource,
    pub shadowed: Vec<ProfileSource>,
}

/// Где искать профили.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    /// `connections.toml`; `None` — без своих профилей.
    pub connections: Option<PathBuf>,
    /// Домашний каталог для `~/.ssh/config`; `None` — не импортировать.
    pub home: Option<PathBuf>,
    pub putty: bool,
}

/// Итог слияния: видимые профили (свои, затем `config`, затем `PuTTY`), конфликты имён,
/// ошибки чтения и файлы, по mtime которых видно, что пора перечитать.
#[derive(Clone, Debug, Default)]
pub struct Profiles {
    pub list: Vec<Profile>,
    /// Свои профили и правки как в файле — для записи.
    pub file: ConnectionsFile,
    pub conflicts: Vec<Conflict>,
    pub errors: Vec<NetError>,
    stamps: Vec<(PathBuf, Option<SystemTime>)>,
}

impl Profiles {
    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.list.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Изменился ли (появился, пропал) какой-то из прочитанных файлов. `PuTTY` (реестр)
    /// так не отслеживается — его дешевле перечитать.
    pub fn is_stale(&self) -> bool {
        self.stamps.iter().any(|(p, t)| mtime(p) != *t)
    }
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Слияние: имя сравнивается без учёта регистра (authority URI). Внутри источника
/// побеждает первый. Правки `[[override]]` — только поверх импортированных.
pub fn merge(
    file: ConnectionsFile,
    config: Vec<Profile>,
    putty: Vec<Profile>,
) -> (Vec<Profile>, Vec<Conflict>) {
    let mut list: Vec<Profile> = Vec::new();
    let mut conflicts: Vec<Conflict> = Vec::new();
    let own = file.profiles.into_iter().map(|p| Profile {
        source: ProfileSource::Urfin,
        ..p
    });
    for p in own.chain(config).chain(putty) {
        match list.iter().find(|q| q.name.eq_ignore_ascii_case(&p.name)) {
            Some(winner) => {
                let used = winner.source;
                match conflicts
                    .iter_mut()
                    .find(|c| c.name.eq_ignore_ascii_case(&p.name))
                {
                    Some(c) => c.shadowed.push(p.source),
                    None => conflicts.push(Conflict {
                        name: winner.name.clone(),
                        used,
                        shadowed: vec![p.source],
                    }),
                }
            }
            None => list.push(p),
        }
    }
    for o in &file.overrides {
        if let Some(p) = list
            .iter_mut()
            .find(|p| p.source != ProfileSource::Urfin && p.name.eq_ignore_ascii_case(&o.name))
        {
            o.apply(p);
        }
    }
    (list, conflicts)
}

/// Чтение всех источников и слияние. I/O (файлы, реестр) — только задачами Runtime.
/// Импорт только читает: файлы пользователя не меняются.
pub fn load(src: &Sources) -> Profiles {
    let mut errors = Vec::new();
    let mut stamps = Vec::new();
    let file = match &src.connections {
        Some(path) => {
            stamps.push((path.clone(), mtime(path)));
            ConnectionsFile::load(path).unwrap_or_else(|e| {
                errors.push(e);
                ConnectionsFile::default()
            })
        }
        None => ConnectionsFile::default(),
    };
    let config = match &src.home {
        Some(home) => {
            let cfg = ssh_config::load(home);
            stamps.extend(cfg.files.iter().map(|p| (p.clone(), mtime(p))));
            errors.extend(cfg.errors);
            cfg.profiles
        }
        None => Vec::new(),
    };
    let putty = if src.putty { putty::load() } else { Vec::new() };
    let (list, conflicts) = merge(file.clone(), config, putty);
    Profiles {
        list,
        file,
        conflicts,
        errors,
        stamps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileOverride;

    fn from(source: ProfileSource, name: &str, host: &str) -> Profile {
        Profile {
            source,
            ..Profile::new(name, host)
        }
    }

    #[test]
    fn urfin_beats_config_beats_putty() {
        let file = ConnectionsFile {
            profiles: vec![Profile::new("web", "own")],
            ..ConnectionsFile::default()
        };
        let config = vec![
            from(ProfileSource::Config, "WEB", "cfg"),
            from(ProfileSource::Config, "db", "cfg"),
        ];
        let putty = vec![
            from(ProfileSource::Putty, "web", "putty"),
            from(ProfileSource::Putty, "db", "putty"),
            from(ProfileSource::Putty, "lab", "putty"),
        ];
        let (list, conflicts) = merge(file, config, putty);
        let hosts: Vec<_> = list
            .iter()
            .map(|p| (p.name.as_str(), p.host.as_str(), p.source))
            .collect();
        assert_eq!(
            hosts,
            [
                ("web", "own", ProfileSource::Urfin),
                ("db", "cfg", ProfileSource::Config),
                ("lab", "putty", ProfileSource::Putty),
            ]
        );
        assert_eq!(
            conflicts,
            [
                Conflict {
                    name: "web".into(),
                    used: ProfileSource::Urfin,
                    shadowed: vec![ProfileSource::Config, ProfileSource::Putty],
                },
                Conflict {
                    name: "db".into(),
                    used: ProfileSource::Config,
                    shadowed: vec![ProfileSource::Putty],
                },
            ]
        );
    }

    #[test]
    fn overrides_touch_only_imported() {
        let file = ConnectionsFile {
            profiles: vec![Profile::new("own", "h")],
            overrides: vec![
                ProfileOverride {
                    name: "siem".into(),
                    auth: Some(Auth::Command),
                    password_id: Some("srv-siem01".into()),
                    ..ProfileOverride::default()
                },
                ProfileOverride {
                    name: "own".into(),
                    port: Some(2222),
                    ..ProfileOverride::default()
                },
            ],
            ..ConnectionsFile::default()
        };
        let config = vec![from(ProfileSource::Config, "siem", "10.0.0.9")];
        let (list, _) = merge(file, config, Vec::new());
        assert_eq!(list[0].port, DEFAULT_PORT);
        assert_eq!(list[1].auth, Auth::Command);
        assert_eq!(list[1].password_id, "srv-siem01");
        assert_eq!(list[1].host, "10.0.0.9");
    }

    #[test]
    fn notes_have_strings() {
        let catalog = urfin_i18n::Catalog::embedded();
        let (ru, _) = catalog.build(&"ru".parse().unwrap());
        let note = Note::Unsupported {
            directive: "Match".into(),
            file: PathBuf::from("config"),
            line: 12,
        };
        assert_eq!(
            note.text(&ru).replace(['\u{2068}', '\u{2069}'], ""),
            "не поддерживается: Match (строка 12)"
        );
        assert_eq!(ProfileSource::Putty.label(&ru), "PuTTY");
        assert_eq!(Note::KeyViaPageant.text(&ru), "ключ через Pageant");
    }

    #[test]
    fn load_reads_own_file_and_tracks_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        let src = Sources {
            connections: Some(path.clone()),
            ..Sources::default()
        };
        let empty = load(&src);
        assert!(empty.list.is_empty() && empty.errors.is_empty());
        std::fs::write(
            &path,
            "version = 1\n[[profile]]\nname = \"a\"\nhost = \"h\"\n",
        )
        .unwrap();
        assert!(empty.is_stale());
        let one = load(&src);
        assert_eq!(one.get("A").map(|p| p.host.as_str()), Some("h"));
        assert!(!one.is_stale());
        std::fs::write(&path, "version = 7\n").unwrap();
        let bad = load(&src);
        assert_eq!(bad.list, []);
        assert!(matches!(
            bad.errors[..],
            [NetError::Version { found: 7, .. }]
        ));
    }
}
