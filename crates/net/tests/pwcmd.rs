//! Команда менеджера паролей с фейковыми командами (F03-03): пароль, PEM, ошибка с
//! хвостом stderr, таймаут, «команда не найдена»; PEM принимается агентом Урфина.

use std::io;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::LineEnding;
use urfin_net::bridge::agent::{self, Io};
use urfin_net::pwcmd::{self, PwError, PwOutput, STDERR_TAIL};

fn words(s: &[&str]) -> Vec<String> {
    s.iter().map(|&w| w.to_owned()).collect()
}

#[cfg(windows)]
fn print_password() -> Vec<String> {
    words(&["cmd", "/C", "echo", "pw1"])
}

#[cfg(not(windows))]
fn print_password() -> Vec<String> {
    words(&["sh", "-c", "printf 'pw1\\nsecond\\n'"])
}

/// stdout — секрет, stderr — длинный, код 3.
#[cfg(windows)]
fn fail_loudly(long: &str) -> Vec<String> {
    words(&[
        "cmd",
        "/C",
        "echo",
        "STDOUT-SECRET&",
        "echo",
        long,
        "1>&2&",
        "exit",
        "/b",
        "3",
    ])
}

#[cfg(not(windows))]
fn fail_loudly(long: &str) -> Vec<String> {
    words(&[
        "sh",
        "-c",
        &format!("echo STDOUT-SECRET; echo {long} >&2; exit 3"),
    ])
}

#[cfg(windows)]
fn sleeper() -> Vec<String> {
    words(&["ping", "-n", "30", "127.0.0.1"])
}

#[cfg(not(windows))]
fn sleeper() -> Vec<String> {
    words(&["sleep", "30"])
}

#[cfg(windows)]
fn print_file(p: &std::path::Path) -> Vec<String> {
    words(&["cmd", "/C", "type", &p.display().to_string()])
}

#[cfg(not(windows))]
fn print_file(p: &std::path::Path) -> Vec<String> {
    words(&["cat", &p.display().to_string()])
}

#[test]
fn first_line_is_password() {
    let out = pwcmd::run(&print_password(), pwcmd::TIMEOUT).unwrap();
    assert!(matches!(out, PwOutput::Password(s) if s.expose() == "pw1"));
}

#[test]
fn failure_has_stderr_tail_without_stdout() {
    let long = format!("{}TAIL", "e".repeat(300));
    let e = pwcmd::run(&fail_loudly(&long), pwcmd::TIMEOUT).unwrap_err();
    let PwError::Failed { code, stderr, .. } = &e else {
        panic!("{e:?}");
    };
    assert_eq!(*code, 3);
    assert!(stderr.chars().count() <= STDERR_TAIL);
    assert!(stderr.ends_with("TAIL"));
    assert!(!e.to_string().contains("STDOUT-SECRET"));
}

#[test]
fn timeout_kills_command() {
    let t = Instant::now();
    let e = pwcmd::run(&sleeper(), Duration::from_millis(300)).unwrap_err();
    assert!(matches!(e, PwError::Timeout { .. }), "{e:?}");
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn missing_command_is_not_found() {
    let e = pwcmd::run(&words(&["urfin-no-such-manager", "get"]), pwcmd::TIMEOUT).unwrap_err();
    assert_eq!(e, PwError::NotFound("urfin-no-such-manager".into()));
}

/// Значение с `;`, `&` и кавычками доходит до программы одним аргументом.
#[cfg(not(windows))]
#[test]
fn substitution_reaches_program_as_one_argument() {
    let host = r#"srv; rm -rf ~ & echo "x" 'y'"#;
    let argv = pwcmd::argv(
        "printf %s {host}",
        pwcmd::Vars {
            host,
            ..pwcmd::Vars::default()
        },
    )
    .unwrap();
    let out = pwcmd::run(&argv, pwcmd::TIMEOUT).unwrap();
    assert!(matches!(out, PwOutput::Password(s) if s.expose() == host));
}

#[tokio::test]
async fn pem_from_command_goes_to_urfin_agent() {
    let dir = tempfile::tempdir().unwrap();
    let key = russh::keys::PrivateKey::from(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[3; 32]),
    );
    let file = dir.path().join("key.pem");
    std::fs::write(&file, key.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    let argv = print_file(&file);
    let out = tokio::task::spawn_blocking(move || pwcmd::run(&argv, pwcmd::TIMEOUT))
        .await
        .unwrap()
        .unwrap();
    let PwOutput::Key(pem) = out else {
        panic!("not a key");
    };
    let k =
        russh::keys::decode_secret_key(std::str::from_utf8(pem.expose()).unwrap(), None).unwrap();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Io>>();
    let listener = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|c| (Ok::<_, io::Error>(c), rx))
    })
    .boxed();
    tokio::spawn(agent::serve(listener, vec![k]));
    let (a, b) = tokio::io::duplex(64 * 1024);
    tx.send(Box::new(b)).unwrap();
    // Как `ssh-add -L`: один ключ.
    let ids = AgentClient::connect(a).request_identities().await.unwrap();
    assert_eq!(ids.len(), 1);
}
