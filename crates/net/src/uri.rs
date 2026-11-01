//! `sftp://<профиль>/путь` ↔ `Location`, разбор `user@host:port` (F03 §2.6, F03-04).
//!
//! `Location { backend: "sftp", authority: Some("<профиль>"), path: "/abs/path" }`
//! (F03 №1): `parent` не выходит за корень сервера.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use urfin_core::Location;

use crate::profile::{DEFAULT_PORT, Profile, ProfileSource};

/// `Location.backend` схемы.
pub const SCHEME: &str = "sftp";

/// Адрес на сервере профиля: `sftp://<profile><path>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SftpUri {
    pub profile: String,
    /// Абсолютный путь сервера: `/`, `/var/www`.
    pub path: String,
}

/// Ошибка разбора ввода.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UriError {
    #[error("not an sftp:// address")]
    NotSftp,
    #[error("no host")]
    NoHost,
    #[error("bad port: {0}")]
    BadPort(String),
}

/// Кому подключаться — из ввода `sftp://[user@]host[:port]/…`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub user: String,
    pub host: String,
    pub port: Option<u16>,
}

impl SftpUri {
    pub fn new(profile: impl Into<String>, path: &str) -> SftpUri {
        SftpUri {
            profile: profile.into(),
            path: normalize(path),
        }
    }

    pub fn to_location(&self) -> Location {
        Location {
            backend: SCHEME.into(),
            authority: Some(self.profile.as_str().into()),
            path: PathBuf::from(&self.path),
            outer: None,
        }
    }

    /// `None` — не `sftp` или без профиля.
    pub fn from_location(loc: &Location) -> Option<SftpUri> {
        if loc.backend != SCHEME || loc.outer.is_some() {
            return None;
        }
        let profile = loc.authority.as_deref().filter(|a| !a.is_empty())?;
        let mut path = String::new();
        for c in normal_parts(&loc.path) {
            path.push('/');
            path.push_str(&c);
        }
        if path.is_empty() {
            path.push('/');
        }
        Some(SftpUri {
            profile: profile.to_owned(),
            path,
        })
    }

    /// Дочерний элемент `name`.
    #[must_use]
    pub fn join(&self, name: &str) -> SftpUri {
        let path = if self.path == "/" {
            format!("/{name}")
        } else {
            format!("{}/{name}", self.path)
        };
        SftpUri {
            profile: self.profile.clone(),
            path,
        }
    }
}

impl fmt::Display for SftpUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sftp://{}{}", self.profile, self.path)
    }
}

/// Компоненты пути как строки: на Windows `Path::join` пишет `\`, сервер ждёт `/`.
fn normal_parts(p: &Path) -> impl Iterator<Item = String> + '_ {
    p.components().filter_map(|c| match c {
        Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
        _ => None,
    })
}

/// `/a//b/` → `/a/b`; `.` и `..` — лексически; пусто → `/`.
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for c in path.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    if out.is_empty() {
        return "/".to_owned();
    }
    let mut s = String::new();
    for c in out {
        s.push('/');
        s.push_str(c);
    }
    s
}

/// Ввод адреса: `sftp://[user@]host[:port][/путь]` (схема без учёта регистра). IPv6 —
/// в скобках: `sftp://[::1]:2222/`.
pub fn parse_input(s: &str) -> Result<(Target, String), UriError> {
    let s = s.trim();
    let rest = s
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("sftp://"))
        .map(|_| &s[7..])
        .ok_or(UriError::NotSftp)?;
    let (auth, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (user, hostport) = match auth.rsplit_once('@') {
        Some((u, h)) => (u, h),
        None => ("", auth),
    };
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (h, tail) = v6.split_once(']').ok_or(UriError::NoHost)?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(port(p)?),
            None if tail.is_empty() => None,
            None => return Err(UriError::BadPort(tail.to_owned())),
        };
        (h, port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(port(p)?)),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err(UriError::NoHost);
    }
    Ok((
        Target {
            user: user.to_owned(),
            host: host.to_owned(),
            port,
        },
        normalize(path),
    ))
}

fn port(p: &str) -> Result<u16, UriError> {
    p.parse::<u16>()
        .ok()
        .filter(|&n| n != 0)
        .ok_or_else(|| UriError::BadPort(p.to_owned()))
}

/// Профиль для ввода: имя профиля (`sftp://srv1/…`), затем совпадение `user/host/port`;
/// иначе — разовый профиль `user@host[:port]` с источником `history`. `true` — разовый.
pub fn resolve(profiles: &[Profile], t: &Target) -> (Profile, bool) {
    if t.user.is_empty()
        && t.port.is_none()
        && let Some(p) = profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&t.host))
    {
        return (p.clone(), false);
    }
    let port = t.port.unwrap_or(DEFAULT_PORT);
    let same = |p: &&Profile| {
        p.host.eq_ignore_ascii_case(&t.host)
            && p.port == port
            && (t.user.is_empty() || p.user == t.user)
    };
    if let Some(p) = profiles.iter().find(same) {
        return (p.clone(), false);
    }
    let mut name = if t.user.is_empty() {
        t.host.clone()
    } else {
        format!("{}@{}", t.user, t.host)
    };
    if port != DEFAULT_PORT {
        name = format!("{name}:{port}");
    }
    let mut p = Profile::new(name, t.host.clone());
    p.port = port;
    p.user.clone_from(&t.user);
    p.source = ProfileSource::History;
    (p, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(user: &str, host: &str, port: Option<u16>) -> Target {
        Target {
            user: user.into(),
            host: host.into(),
            port,
        }
    }

    #[test]
    fn location_roundtrip() {
        for (profile, path) in [
            ("srv1", "/"),
            ("srv1", "/path"),
            ("web-prod-01", "/var/www/portal"),
            ("u@h:2222", "/x/имя с пробелом"),
        ] {
            let u = SftpUri::new(profile, path);
            let loc = u.to_location();
            assert_eq!(loc.backend, SCHEME);
            assert_eq!(SftpUri::from_location(&loc).as_ref(), Some(&u));
            // Строковая форма core — та же, что показ.
            assert_eq!(loc.to_uri(), u.to_string());
            let back = Location::parse_uri(&loc.to_uri()).unwrap();
            assert_eq!(SftpUri::from_location(&back), Some(u));
        }
    }

    #[test]
    fn display_and_input_roundtrip() {
        let u = SftpUri::new("srv1", "/path");
        assert_eq!(u.to_string(), "sftp://srv1/path");
        let (t, path) = parse_input(&u.to_string()).unwrap();
        assert_eq!(t, target("", "srv1", None));
        assert_eq!(path, "/path");
    }

    #[test]
    fn parent_stops_at_server_root() {
        let loc = SftpUri::new("srv1", "/var").to_location();
        let root = loc.parent().unwrap();
        assert_eq!(
            SftpUri::from_location(&root),
            Some(SftpUri::new("srv1", "/"))
        );
        assert!(!root.has_parent());
        assert_eq!(root.parent(), None);
    }

    #[test]
    fn child_from_platform_join() {
        // Панель строит ребёнка через `Path::join` (на Windows — `\`).
        let mut loc = SftpUri::new("srv1", "/var").to_location();
        loc.path = loc.path.join("log");
        assert_eq!(
            SftpUri::from_location(&loc),
            Some(SftpUri::new("srv1", "/var/log"))
        );
        assert_eq!(
            SftpUri::new("srv1", "/").join("etc"),
            SftpUri::new("srv1", "/etc")
        );
        assert_eq!(SftpUri::new("srv1", "/etc").join("ssh").path, "/etc/ssh");
    }

    #[test]
    fn foreign_locations() {
        assert_eq!(
            SftpUri::from_location(&Location::local(std::env::temp_dir())),
            None
        );
        let root = Location {
            backend: SCHEME.into(),
            authority: None,
            path: PathBuf::from("/srv1/x"),
            outer: None,
        };
        assert_eq!(SftpUri::from_location(&root), None);
    }

    #[test]
    fn input_forms() {
        assert_eq!(
            parse_input("sftp://u@h:2222/x").unwrap(),
            (target("u", "h", Some(2222)), "/x".into())
        );
        assert_eq!(
            parse_input("SFTP://h").unwrap(),
            (target("", "h", None), "/".into())
        );
        assert_eq!(
            parse_input("sftp://admin@[::1]:22/etc/").unwrap(),
            (target("admin", "::1", Some(22)), "/etc".into())
        );
        assert_eq!(parse_input(" sftp://h/a/../b/./c ").unwrap().1, "/b/c");
        assert_eq!(parse_input("ssh://h"), Err(UriError::NotSftp));
        assert_eq!(parse_input("sftp:///x"), Err(UriError::NoHost));
        assert_eq!(
            parse_input("sftp://h:0/"),
            Err(UriError::BadPort("0".into()))
        );
        assert_eq!(
            parse_input("sftp://h:ssh/"),
            Err(UriError::BadPort("ssh".into()))
        );
    }

    #[test]
    fn input_finds_profile_or_makes_one() {
        let mut srv = Profile::new("srv1", "10.0.0.5");
        srv.user = "u".into();
        srv.port = 2222;
        let mut web = Profile::new("web", "h");
        web.user = "deploy".into();
        let list = [srv, web];

        // Имя профиля.
        let (p, adhoc) = resolve(&list, &target("", "SRV1", None));
        assert_eq!((p.name.as_str(), adhoc), ("srv1", false));
        // user/host/port.
        let (p, adhoc) = resolve(&list, &target("u", "10.0.0.5", Some(2222)));
        assert_eq!((p.name.as_str(), adhoc), ("srv1", false));
        let (p, adhoc) = resolve(&list, &target("", "H", None));
        assert_eq!((p.name.as_str(), adhoc), ("web", false));

        // Нет такого — разовый `user@host`.
        let (p, adhoc) = resolve(&list, &target("u", "h", Some(2222)));
        assert!(adhoc);
        assert_eq!(p.name, "u@h:2222");
        assert_eq!((p.host.as_str(), p.port, p.user.as_str()), ("h", 2222, "u"));
        assert_eq!(p.source, ProfileSource::History);
        let (p, _) = resolve(&list, &target("root", "h", None));
        assert_eq!(p.name, "root@h");
        assert_eq!(p.port, DEFAULT_PORT);
    }
}
