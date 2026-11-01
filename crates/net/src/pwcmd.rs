//! Команда менеджера паролей (F03 §2.5, F03-03). Шаблон разбирается `shell-words`, затем
//! в каждом слове подставляются `{host}`, `{user}`, `{id}`: значение остаётся внутри
//! своего слова и не может стать новым аргументом или командой. Запуск — без shell,
//! `term::proc::spawn_piped` без окна, дерево в job, таймаут 30 с.
//!
//! Первая строка stdout — пароль; stdout целиком в PEM — закрытый ключ. Код ≠0 — ошибка
//! с хвостом stderr (≤200 символов). stdout не логируется.

#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::io::Read as _;
use std::time::Duration;

#[cfg(windows)]
use urfin_term::proc::{self, PipedFlags};

use crate::config::PasswordManager;
use crate::secrets::{Secret, SecretBytes};

/// Сколько ждать команду.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Хвост stderr в строке ошибки, символов.
pub const STDERR_TAIL: usize = 200;

/// Шаблон менеджера (`[ssh].password_manager`). `BearPass` и Пассворк — сверить на стенде
/// (`docs/manual-tests/F03-03.md`).
pub fn template(m: PasswordManager) -> Option<&'static str> {
    Some(match m {
        PasswordManager::BearPass => "bearpass get {id} --field=password",
        PasswordManager::Passwork => "passwork-cli get --password-id {id}",
        PasswordManager::OnePassword => r#"op read "op://Servers/{host}/password""#,
        PasswordManager::Bitwarden => r#"bw get password "{host}""#,
        PasswordManager::KeePassXc => "keepassxc-cli show -s -a Password db.kdbx {host}",
        PasswordManager::None | PasswordManager::Custom => return None,
    })
}

/// Значения подстановок.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vars<'a> {
    pub host: &'a str,
    pub user: &'a str,
    pub id: &'a str,
}

/// Ошибка команды. `Display` — для строки ошибки UI; stdout в неё не попадает.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PwError {
    #[error("bad command template: {0}")]
    Template(String),
    #[error("command not found: {0}")]
    NotFound(String),
    #[error("{program}: {message}")]
    Spawn { program: String, message: String },
    #[error("{program}: timed out")]
    Timeout { program: String },
    #[error("{program}: exit code {code}: {stderr}")]
    Failed {
        program: String,
        code: i32,
        stderr: String,
    },
    #[error("{program}: empty output")]
    Empty { program: String },
}

/// Ответ команды.
#[derive(Debug)]
pub enum PwOutput {
    Password(Secret),
    /// PEM закрытого ключа.
    Key(SecretBytes),
}

/// argv из шаблона: сначала слова, потом подстановки внутри слов.
pub fn argv(template: &str, vars: Vars<'_>) -> Result<Vec<String>, PwError> {
    let words = shell_words::split(template).map_err(|e| PwError::Template(e.to_string()))?;
    if words.is_empty() {
        return Err(PwError::Template("empty".into()));
    }
    Ok(words.iter().map(|w| substitute(w, vars)).collect())
}

/// Один проход: подставленное значение повторно не разбирается.
fn substitute(word: &str, v: Vars<'_>) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (value, len) = if tail.starts_with("{host}") {
            (v.host, 6)
        } else if tail.starts_with("{user}") {
            (v.user, 6)
        } else if tail.starts_with("{id}") {
            (v.id, 4)
        } else {
            ("{", 1)
        };
        out.push_str(value);
        rest = &tail[len..];
    }
    out.push_str(rest);
    out
}

/// Запуск и разбор ответа. Блокирует: зовётся из `spawn_blocking`, не из UI.
/// stdin команды — пустой пайп (сразу EOF).
#[cfg(windows)]
pub fn run(argv: &[String], timeout: Duration) -> Result<PwOutput, PwError> {
    let program = argv.first().cloned().unwrap_or_default();
    let os: Vec<OsString> = argv.iter().map(OsString::from).collect();
    let flags = PipedFlags {
        no_window: true,
        merge_stderr: false,
        kill_on_drop: true,
    };
    let mut child = proc::spawn_piped(&os, None, &[], flags).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PwError::NotFound(program.clone())
        } else {
            PwError::Spawn {
                program: program.clone(),
                message: e.to_string(),
            }
        }
    })?;
    let out = child.stdout.take().map(|mut r| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            SecretBytes::new(b)
        })
    });
    let err = child.stderr.take().map(|mut r| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            b
        })
    });
    let Some(code) = child.wait_timeout(Some(timeout)) else {
        let _ = child.kill_tree();
        return Err(PwError::Timeout { program });
    };
    let stdout = out
        .and_then(|t| t.join().ok())
        .unwrap_or_else(|| SecretBytes::new(Vec::new()));
    let stderr = err.and_then(|t| t.join().ok()).unwrap_or_default();
    if code != 0 {
        return Err(PwError::Failed {
            program,
            // Код выхода Windows — `u32`; NTSTATUS вроде 0xC0000005 станет отрицательным.
            code: i32::from_ne_bytes(code.to_ne_bytes()),
            stderr: tail(&String::from_utf8_lossy(&stderr)),
        });
    }
    parse(stdout).ok_or(PwError::Empty { program })
}

/// Последние `STDERR_TAIL` символов stderr в одну строку.
fn tail(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = flat.chars().count();
    flat.chars().skip(n.saturating_sub(STDERR_TAIL)).collect()
}

fn parse(stdout: SecretBytes) -> Option<PwOutput> {
    let text = std::str::from_utf8(stdout.expose()).ok()?;
    let t = text.trim();
    if t.starts_with("-----BEGIN ") && t.contains("PRIVATE KEY-----") {
        return Some(PwOutput::Key(stdout));
    }
    let first = text.lines().next()?.trim_end_matches('\r');
    (!first.is_empty()).then(|| PwOutput::Password(Secret::new(first.to_owned())))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const V: Vars<'static> = Vars {
        host: "srv1",
        user: "admin",
        id: "42",
    };

    #[test]
    fn templates_parse() {
        for m in [
            PasswordManager::BearPass,
            PasswordManager::Passwork,
            PasswordManager::OnePassword,
            PasswordManager::Bitwarden,
            PasswordManager::KeePassXc,
        ] {
            assert!(argv(template(m).unwrap(), V).unwrap().len() >= 3);
        }
        assert_eq!(
            argv(template(PasswordManager::OnePassword).unwrap(), V).unwrap(),
            ["op", "read", "op://Servers/srv1/password"]
        );
        assert_eq!(
            argv(template(PasswordManager::BearPass).unwrap(), V).unwrap(),
            ["bearpass", "get", "42", "--field=password"]
        );
        assert!(template(PasswordManager::Custom).is_none());
    }

    #[test]
    fn injection_stays_one_value() {
        let host = r#"a; rm -rf / & "b" 'c' $(x) `y` | z"#;
        let v = Vars { host, ..V };
        assert_eq!(
            argv(r#"bw get password "{host}""#, v).unwrap(),
            ["bw", "get", "password", host]
        );
        assert_eq!(argv("tool {host}", v).unwrap(), ["tool", host]);
        // Подставленное не подставляется второй раз.
        let v = Vars {
            host: "{user}",
            ..V
        };
        assert_eq!(argv("tool {host}", v).unwrap(), ["tool", "{user}"]);
        assert_eq!(argv("tool {nope} {", V).unwrap(), ["tool", "{nope}", "{"]);
        assert!(matches!(argv("", V), Err(PwError::Template(_))));
        assert!(matches!(argv("tool \"open", V), Err(PwError::Template(_))));
    }

    proptest! {
        #[test]
        fn any_value_is_one_argv_element(host in any::<String>(), user in any::<String>(), id in any::<String>()) {
            let v = Vars { host: &host, user: &user, id: &id };
            let a = argv(r#"tool --host={host} "{user}" '{id}' x"#, v).unwrap();
            prop_assert_eq!(a.len(), 5);
            prop_assert_eq!(&a[1], &format!("--host={host}"));
            prop_assert_eq!(&a[2], &user);
            prop_assert_eq!(&a[3], &id);
        }
    }

    #[test]
    fn tail_is_short_and_flat() {
        let long = format!("start\n{}\nend", "x".repeat(500));
        let t = tail(&long);
        assert_eq!(t.chars().count(), STDERR_TAIL);
        assert!(t.ends_with(" end"));
        assert_eq!(tail("a\r\nb"), "a b");
    }

    #[test]
    fn parse_password_and_pem() {
        let p = parse(SecretBytes::new(b"pw1\r\nsecond\n".to_vec()));
        assert!(matches!(p, Some(PwOutput::Password(s)) if s.expose() == "pw1"));
        let pem = b"-----BEGIN OPENSSH PRIVATE KEY-----\nAAA\n-----END OPENSSH PRIVATE KEY-----\n";
        assert!(matches!(
            parse(SecretBytes::new(pem.to_vec())),
            Some(PwOutput::Key(_))
        ));
        assert!(parse(SecretBytes::new(b"\n".to_vec())).is_none());
    }

    #[test]
    fn timeout_is_30_s() {
        assert_eq!(TIMEOUT, Duration::from_secs(30));
    }
}
