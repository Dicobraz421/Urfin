//! Стенд F03-09 (`#[ignore]`, нужен OpenSSH-клиент Windows ≥ 8.9): настоящий `ssh.exe` к
//! `testsrv` через мост Урфина — пароль из памяти сеанса через `SSH_ASKPASS` (вопроса
//! нет), ключ менеджера паролей через агент Урфина, ключ сервера проверен заранее
//! (`StrictHostKeyChecking=yes`, вопроса нет).
//!
//! `cargo nextest run -p urfin-net --run-ignored only -E 'binary(term_ssh)'`

mod testsrv;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use urfin_net::bridge::{self, agent, askpass, ssh_exe};
use urfin_net::hostkey;
use urfin_net::secrets::{Secret, Shared};

use testsrv::{Opts, TestSrv, key};

fn ssh() -> PathBuf {
    let exe = ssh_exe::find(
        std::env::var_os("SystemRoot").as_deref().map(Path::new),
        std::env::var_os("PATH").as_deref(),
    )
    .expect("ssh.exe");
    let tmp = tempfile::tempdir().unwrap();
    let v = ssh_exe::detect(&tmp.path().join("ssh_exe.toml")).expect("ssh -V");
    assert!(v.bridge_ok(), "OpenSSH ≥ 8.9: {}", v.version);
    exe
}

/// `askpass_helper` рядом с тестом (`target/<profile>/examples`).
fn helper() -> PathBuf {
    let me = std::env::current_exe().unwrap();
    let dir = me.parent().unwrap().parent().unwrap();
    let p = dir.join("examples").join("askpass_helper.exe");
    assert!(p.is_file(), "build examples: {}", p.display());
    p
}

/// `ssh.exe -N` (без оболочки): вход и ожидание. Жив через 3 с — вошёл.
fn run_ssh(
    srv: &TestSrv,
    known: &Path,
    agent: Option<&str>,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    method: &str,
) -> Child {
    let mut opts = bridge::ssh_options(&[known.to_path_buf()], agent);
    opts.extend(
        [
            "-F",
            "NUL",
            "-N",
            "-o",
            "BatchMode=no",
            "-o",
            &format!("PreferredAuthentications={method}"),
            "-o",
            "NumberOfPasswordPrompts=1",
            "-p",
            &srv.port.to_string(),
            "admin@127.0.0.1",
        ]
        .map(String::from),
    );
    if agent.is_none() {
        opts.splice(0..0, ["-o".to_owned(), "IdentityAgent=none".to_owned()]);
    }
    let mut c = Command::new(ssh());
    c.args(&opts)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    c.spawn().unwrap()
}

fn logged_in(mut child: Child) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        if let Some(st) = child.try_wait().unwrap() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut e, &mut err);
            }
            eprintln!("ssh exited {st}: {err}");
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    true
}

fn trusted(srv: &TestSrv) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let name = hostkey::host_name("127.0.0.1", srv.port);
    hostkey::append(&dir.path().join("known_hosts"), &name, &srv.host_key).unwrap();
    dir
}

/// Пароль введён один раз (SFTP положил его в память сеанса): `ssh.exe` получает его
/// через `--askpass`, строки ввода нет; ключ сервера проверен — вопроса нет.
#[test]
#[ignore = "стенд: OpenSSH-клиент Windows"]
fn password_from_session_memory_via_askpass() {
    let srv = TestSrv::start(Opts {
        password: Some("s3cret"),
        ..Opts::default()
    });
    let home = trusted(&srv);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let secrets = Shared::new(0);
    let host = hostkey::host_name("127.0.0.1", srv.port);
    // Сначала неверный: `ssh.exe` не входит — проверка, что вход в самом деле по паролю.
    secrets.put(format!("pw:{host}"), Secret::new("wrong".into()));
    let nonce = bridge::nonce();
    let pipe = bridge::askpass_pipe(&nonce);
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let _g = rt.enter();
    let (tx, rx) = mpsc::unbounded_channel();
    let serve = askpass::bind(pipe.clone(), tx).unwrap();
    rt.spawn(serve);
    rt.spawn(askpass::respond(rx, secrets.clone(), ev_tx, host.clone()));
    let env = bridge::askpass_env(&helper(), &pipe);
    let known = home.path().join("known_hosts");
    let child = run_ssh(&srv, &known, None, &env, "password");
    assert!(!logged_in(child), "wrong password is refused");
    secrets.put(format!("pw:{host}"), Secret::new("s3cret".into()));
    let child = run_ssh(&srv, &known, None, &env, "password");
    assert!(
        logged_in(child),
        "ssh.exe logged in with the session password"
    );
    assert!(
        ev_rx.try_recv().is_err(),
        "no prompt: the password came from memory"
    );
}

/// Ключ из менеджера паролей — через агент Урфина (`IdentityAgent=<pipe>`), не с диска.
#[test]
#[ignore = "стенд: OpenSSH-клиент Windows"]
fn manager_key_via_urfin_agent() {
    let srv = TestSrv::start(Opts {
        keys: vec![key(5).public_key().clone()],
        ..Opts::default()
    });
    let home = trusted(&srv);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let name = bridge::agent_pipe(&bridge::nonce());
    let _g = rt.enter();
    let listener = agent::pipe_listener(name.clone()).unwrap();
    rt.spawn(agent::serve(listener, vec![key(5)]));
    let child = run_ssh(
        &srv,
        &home.path().join("known_hosts"),
        Some(&name),
        &[],
        "publickey",
    );
    assert!(logged_in(child), "ssh.exe logged in with the agent key");
}
