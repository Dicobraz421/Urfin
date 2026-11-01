//! Мост к `ssh.exe` (F03 §2.7, F03-03): `--askpass` и pipe-сервер для него, агент Урфина
//! для ключей из менеджера паролей, поиск и версия `ssh.exe`, аргументы и окружение
//! запуска.

pub mod agent;
pub mod askpass;
pub mod ssh_exe;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Случайная часть имён pipe сеанса.
pub fn nonce() -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{}-{}-{:x}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        nanos & 0xffff_ffff_ffff
    )
}

/// `\\.\pipe\urfin-askpass-<nonce>`.
pub fn askpass_pipe(nonce: &str) -> String {
    format!(r"\\.\pipe\urfin-askpass-{nonce}")
}

/// `\\.\pipe\urfin-agent-<nonce>`.
pub fn agent_pipe(nonce: &str) -> String {
    format!(r"\\.\pipe\urfin-agent-{nonce}")
}

/// Окружение `ssh.exe` для `--askpass`: `urfin.exe` помощником, ответы — из pipe.
pub fn askpass_env(urfin_exe: &Path, pipe: &str) -> Vec<(OsString, OsString)> {
    vec![
        ("SSH_ASKPASS".into(), urfin_exe.as_os_str().to_owned()),
        ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
        (askpass::ENV.into(), pipe.into()),
    ]
}

/// Путь для опций `ssh`: прямые слэши (в кавычках `ssh` разбирает `\\` как экранирование).
fn ssh_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// `-o` для `ssh.exe`: ключи серверов проверил Урфин (§2.4), агент Урфина — если есть.
pub fn ssh_options(known_hosts: &[PathBuf], agent: Option<&str>) -> Vec<String> {
    let files: Vec<String> = known_hosts
        .iter()
        .map(|p| format!("\"{}\"", ssh_path(p)))
        .collect();
    let mut out = vec![
        "-o".into(),
        format!("UserKnownHostsFile={}", files.join(" ")),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
    ];
    if let Some(a) = agent {
        out.push("-o".into());
        out.push(format!("IdentityAgent={a}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_and_env() {
        let kh = [
            PathBuf::from(r"C:\Users\A B\.ssh\known_hosts"),
            PathBuf::from(r"C:\Urfin\state\ssh\run\1\known_hosts"),
        ];
        let o = ssh_options(&kh, Some(r"\\.\pipe\urfin-agent-1"));
        assert_eq!(
            o[1],
            r#"UserKnownHostsFile="C:/Users/A B/.ssh/known_hosts" "C:/Urfin/state/ssh/run/1/known_hosts""#
        );
        assert_eq!(o[3], "StrictHostKeyChecking=yes");
        assert_eq!(o[5], r"IdentityAgent=\\.\pipe\urfin-agent-1");
        assert_eq!(ssh_options(&kh, None).len(), 4);
        let env = askpass_env(Path::new(r"C:\Urfin\urfin.exe"), &askpass_pipe("n1"));
        assert_eq!(env[1], ("SSH_ASKPASS_REQUIRE".into(), "force".into()));
        assert_eq!(env[2].1, OsString::from(r"\\.\pipe\urfin-askpass-n1"));
        assert_ne!(nonce(), nonce());
    }
}
