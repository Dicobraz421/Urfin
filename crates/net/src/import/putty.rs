//! Сеансы `PuTTY` (F03 §2.2): `HKCU\Software\SimonTatham\PuTTY\Sessions\*` — `HostName`,
//! `PortNumber`, `UserName`, `PublicKeyFile`. `.ppk` не читаем: ключ — через Pageant.
//! Тот же разбор — для экспорта `.reg` (фикстуры, перенос с другой машины).

use crate::profile::{Auth, DEFAULT_PORT, Note, Profile, ProfileSource};

/// Ветка сеансов под `HKEY_CURRENT_USER`.
pub const SESSIONS_KEY: &str = r"Software\SimonTatham\PuTTY\Sessions";

/// Шаблон `PuTTY` для новых сеансов — не сеанс.
const DEFAULT_SESSION: &str = "Default Settings";

/// Значения одного сеанса как в реестре.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PuttySession {
    /// Имя без `%XX`-экранирования `PuTTY`.
    pub name: String,
    pub host: String,
    pub port: Option<u32>,
    pub user: String,
    pub key_file: String,
    /// `Protocol`; пусто — не задан (ssh).
    pub protocol: String,
}

impl PuttySession {
    /// Профиль SSH; `None` — шаблон, другой протокол или нет хоста.
    pub fn to_profile(&self) -> Option<Profile> {
        if self.name == DEFAULT_SESSION
            || self.host.is_empty()
            || !(self.protocol.is_empty() || self.protocol.eq_ignore_ascii_case("ssh"))
        {
            return None;
        }
        let mut p = Profile {
            port: self
                .port
                .and_then(|p| u16::try_from(p).ok())
                .filter(|&p| p != 0)
                .unwrap_or(DEFAULT_PORT),
            user: self.user.clone(),
            source: ProfileSource::Putty,
            ..Profile::new(self.name.clone(), self.host.clone())
        };
        // `user@host` в HostName PuTTY допускает.
        if p.user.is_empty()
            && let Some((user, host)) = self.host.rsplit_once('@')
        {
            user.clone_into(&mut p.user);
            host.clone_into(&mut p.host);
        }
        if !self.key_file.is_empty() {
            if self
                .key_file
                .rsplit('.')
                .next()
                .is_some_and(|e| e.eq_ignore_ascii_case("ppk"))
            {
                p.notes.push(Note::KeyViaPageant);
            } else {
                p.auth = Auth::Key;
                p.key.clone_from(&self.key_file);
            }
        }
        Some(p)
    }
}

/// Профили из реестра; вне Windows и без `PuTTY` — пусто.
pub fn load() -> Vec<Profile> {
    read_registry()
        .iter()
        .filter_map(PuttySession::to_profile)
        .collect()
}

#[cfg(windows)]
fn read_registry() -> Vec<PuttySession> {
    let Ok(root) = windows_registry::CURRENT_USER.open(SESSIONS_KEY) else {
        return Vec::new();
    };
    let Ok(keys) = root.keys() else {
        return Vec::new();
    };
    keys.filter_map(|raw| {
        let key = root.open(&raw).ok()?;
        let s = |name: &str| key.get_string(name).unwrap_or_default();
        Some(PuttySession {
            name: unescape(&raw),
            host: s("HostName"),
            port: key.get_u32("PortNumber").ok(),
            user: s("UserName"),
            key_file: s("PublicKeyFile"),
            protocol: s("Protocol"),
        })
    })
    .collect()
}

#[cfg(not(windows))]
fn read_registry() -> Vec<PuttySession> {
    Vec::new()
}

/// Текст `.reg`: regedit пишет UTF-16LE с BOM, руками — UTF-8.
pub fn decode_reg(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Сеансы из экспорта `.reg` (`[HKEY_CURRENT_USER\…\Sessions\<имя>]`, `"Имя"="…"`,
/// `dword:…`). Прочие ключи и значения пропускаются.
pub fn parse_reg(text: &str) -> Vec<PuttySession> {
    let prefix = format!(r"HKEY_CURRENT_USER\{SESSIONS_KEY}\");
    let mut out: Vec<PuttySession> = Vec::new();
    let mut cur: Option<PuttySession> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(key) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.extend(cur.take());
            cur = key
                .get(..prefix.len())
                .filter(|p| p.eq_ignore_ascii_case(&prefix))
                .map(|_| &key[prefix.len()..])
                .filter(|name| !name.is_empty() && !name.contains('\\'))
                .map(|name| PuttySession {
                    name: unescape(name),
                    ..PuttySession::default()
                });
            continue;
        }
        let Some(s) = cur.as_mut() else { continue };
        let Some((name, value)) = reg_value(line) else {
            continue;
        };
        match (name.as_str(), value) {
            ("HostName", RegValue::Str(v)) => s.host = v,
            ("PortNumber", RegValue::Dword(v)) => s.port = Some(v),
            ("UserName", RegValue::Str(v)) => s.user = v,
            ("PublicKeyFile", RegValue::Str(v)) => s.key_file = v,
            ("Protocol", RegValue::Str(v)) => s.protocol = v,
            _ => {}
        }
    }
    out.extend(cur);
    out
}

enum RegValue {
    Str(String),
    Dword(u32),
}

/// `"Имя"="строка"` или `"Имя"=dword:00000016`.
fn reg_value(line: &str) -> Option<(String, RegValue)> {
    let (name, rest) = reg_string(line.strip_prefix('"')?)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let value = if let Some(s) = rest.strip_prefix('"') {
        RegValue::Str(reg_string(s)?.0)
    } else {
        let hex = rest.strip_prefix("dword:")?;
        RegValue::Dword(u32::from_str_radix(hex.trim(), 16).ok()?)
    };
    Some((name, value))
}

/// Строка после открывающей кавычки до закрывающей; `\\` и `\"` — экранирование.
fn reg_string(s: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 1..])),
            '\\' => out.push(chars.next()?.1),
            c => out.push(c),
        }
    }
    None
}

/// Имя сеанса: `PuTTY` экранирует `%XX` пробелы, `\`, `%` и прочее.
fn unescape(name: &str) -> String {
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = name
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unescaped() {
        assert_eq!(unescape("web%20prod"), "web prod");
        assert_eq!(unescape("a%5Cb%25"), r"a\b%");
        assert_eq!(unescape("bad%zz"), "bad%zz");
    }

    #[test]
    fn values() {
        let Some((n, RegValue::Str(v))) = reg_value(r#""PublicKeyFile"="C:\\keys\\a \"b\".ppk""#)
        else {
            panic!()
        };
        assert_eq!(
            (n.as_str(), v.as_str()),
            ("PublicKeyFile", r#"C:\keys\a "b".ppk"#)
        );
        let Some((_, RegValue::Dword(p))) = reg_value(r#""PortNumber"=dword:00000016"#) else {
            panic!()
        };
        assert_eq!(p, 22);
        assert!(reg_value(r#""TermHeight"=hex:00,01"#).is_none());
    }

    #[test]
    fn non_ssh_and_template_are_skipped() {
        let s = |name: &str, protocol: &str| PuttySession {
            name: name.into(),
            host: "h".into(),
            protocol: protocol.into(),
            ..PuttySession::default()
        };
        assert!(s(DEFAULT_SESSION, "ssh").to_profile().is_none());
        assert!(s("tel", "telnet").to_profile().is_none());
        assert!(s("ok", "").to_profile().is_some());
        assert!(
            PuttySession {
                host: String::new(),
                ..s("empty", "ssh")
            }
            .to_profile()
            .is_none()
        );
    }
}
