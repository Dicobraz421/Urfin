//! Подключение к `testsrv` (F03-02): ключ, пароль, 2FA, jump, ключи серверов,
//! переподключение.

mod testsrv;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::keys::ssh_key::LineEnding;
use tokio::sync::mpsc;
use urfin_net::conn::{
    self, ConnCfg, ConnError, Control, Event, HostKeyChoice, Phase, Pool, PromptKind,
};
use urfin_net::hostkey;
use urfin_net::{Auth, Profile, SshCfg};

use testsrv::{Opts, TestSrv, key};

/// Домашняя папка теста: `.ssh/known_hosts`, ключ клиента, `state`.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
        Home { dir }
    }

    fn known_hosts(&self) -> PathBuf {
        self.dir.path().join(".ssh").join("known_hosts")
    }

    fn trust(&self, srv: &TestSrv) {
        hostkey::append(&self.known_hosts(), &name(srv), &srv.host_key).unwrap();
    }

    fn client_key(&self, seed: u8) -> PathBuf {
        let p = self.dir.path().join(".ssh").join(format!("id_{seed}"));
        let text = key(seed).to_openssh(LineEnding::LF).unwrap();
        std::fs::write(&p, text.as_bytes()).unwrap();
        p
    }

    fn cfg(&self) -> ConnCfg {
        let mut c = ConnCfg::new(
            &SshCfg::default(),
            Some(self.dir.path()),
            Some(&self.dir.path().join("state")),
        );
        c.default_keys.clear();
        c.interval = Duration::from_millis(200);
        c
    }

    fn state(&self) -> &Path {
        self.dir.path()
    }
}

fn name(srv: &TestSrv) -> String {
    hostkey::host_name("127.0.0.1", srv.port)
}

fn profile(name: &str, srv: &TestSrv, auth: Auth) -> Profile {
    let mut p = Profile::new(name, "127.0.0.1");
    p.port = srv.port;
    p.user = "tester".into();
    p.auth = auth;
    p
}

/// Ответы на вопросы и итог сеанса.
#[derive(Default)]
struct Answers {
    password: Option<&'static str>,
    otp: Option<&'static str>,
    host_key: Option<HostKeyChoice>,
    replace: bool,
}

/// Что видел UI.
#[derive(Debug, Default)]
struct Seen {
    phases: Vec<(String, Phase)>,
    prompts: Vec<String>,
    unknown: Vec<(String, String)>,
    changed: Vec<(PathBuf, usize)>,
    lost: Vec<(u32, u32, Instant)>,
    connected: Vec<String>,
    failed: Option<ConnError>,
    disconnected: bool,
}

/// Сеанс до первого `Connected` (или конца, если `until_end`).
async fn run(
    chain: Vec<Profile>,
    cfg: ConnCfg,
    a: &Answers,
    until_end: bool,
    mut on_connected: impl FnMut(&mpsc::UnboundedSender<Control>),
) -> Seen {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ctl_tx, ctl) = mpsc::unbounded_channel();
    let task = tokio::spawn(conn::session(chain, cfg, Arc::new(Pool::new()), tx, ctl));
    let mut seen = Seen::default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        match ev {
            Event::Phase { host, phase } => seen.phases.push((host, phase)),
            Event::HostKeyUnknown {
                host,
                fingerprint,
                reply,
                ..
            } => {
                seen.unknown.push((host, fingerprint));
                if let Some(c) = a.host_key {
                    let _ = reply.send(c);
                }
            }
            Event::HostKeyChanged {
                file, line, reply, ..
            } => {
                seen.changed.push((file, line));
                if a.replace {
                    let _ = reply.send(true);
                }
            }
            Event::Prompt {
                kind, text, reply, ..
            } => {
                seen.prompts.push(text);
                let answer = match kind {
                    PromptKind::Password => a.password,
                    PromptKind::Interactive { .. } => a.otp,
                    PromptKind::Passphrase(_) => None,
                };
                let _ = reply.send(answer.map(Into::into));
            }
            Event::Connected(c) => {
                seen.connected.push(c.profile.clone());
                on_connected(&ctl_tx);
                if !until_end {
                    let _ = ctl_tx.send(Control::Disconnect);
                }
            }
            Event::Lost { attempt, tries, .. } => seen.lost.push((attempt, tries, Instant::now())),
            Event::Failed(e) => seen.failed = Some(e),
            Event::Disconnected => {
                seen.disconnected = true;
                if until_end {
                    let _ = ctl_tx.send(Control::Disconnect);
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    seen
}

#[tokio::test]
async fn connects_by_key() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        keys: vec![key(11).public_key().clone()],
        ..Opts::default()
    });
    home.trust(&srv);
    let mut p = profile("srv1", &srv, Auth::Key);
    p.key = home.client_key(11).display().to_string();
    let seen = run(vec![p], home.cfg(), &Answers::default(), false, |_| {}).await;
    assert_eq!(seen.connected, ["srv1"]);
    assert_eq!(seen.prompts.len(), 0);
    let phases: Vec<_> = seen.phases.iter().map(|(_, p)| *p).collect();
    assert_eq!(phases, [Phase::Tcp, Phase::HostKey, Phase::Auth]);
    assert!(seen.disconnected);
}

#[tokio::test]
async fn wrong_key_fails() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        keys: vec![key(11).public_key().clone()],
        ..Opts::default()
    });
    home.trust(&srv);
    let mut p = profile("srv1", &srv, Auth::Key);
    p.key = home.client_key(12).display().to_string();
    let seen = run(vec![p], home.cfg(), &Answers::default(), false, |_| {}).await;
    assert_eq!(seen.failed, Some(ConnError::Auth));
}

#[tokio::test]
async fn connects_by_password() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        password: Some("secret"),
        ..Opts::default()
    });
    home.trust(&srv);
    let a = Answers {
        password: Some("secret"),
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.connected, ["srv1"]);
    assert_eq!(seen.prompts.len(), 1);
    assert!(seen.prompts[0].contains("password"));
}

#[tokio::test]
async fn password_cancel_is_cancelled() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        password: Some("secret"),
        ..Opts::default()
    });
    home.trust(&srv);
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &Answers::default(),
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.failed, Some(ConnError::Cancelled));
}

#[tokio::test]
async fn connects_with_2fa() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        password: Some("secret"),
        otp: Some("123456"),
        ..Opts::default()
    });
    home.trust(&srv);
    let a = Answers {
        password: Some("secret"),
        otp: Some("123456"),
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.connected, ["srv1"]);
    assert_eq!(seen.prompts.len(), 2);
    assert_eq!(seen.prompts[1], "Verification code: ");
    // Неверный код — отказ.
    let a = Answers {
        password: Some("secret"),
        otp: Some("000000"),
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.failed, Some(ConnError::Auth));
}

/// Пароль командой менеджера: без строки ввода.
#[tokio::test]
async fn connects_by_password_command() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        password: Some("secret"),
        ..Opts::default()
    });
    home.trust(&srv);
    let mut p = profile("srv1", &srv, Auth::Command);
    p.password_cmd = if cfg!(windows) {
        "cmd /C echo secret".into()
    } else {
        "sh -c 'echo secret'".into()
    };
    let seen = run(
        vec![p.clone()],
        home.cfg(),
        &Answers::default(),
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.connected, ["srv1"]);
    assert_eq!(seen.prompts.len(), 0);
    // Нет команды — ошибка, не строка ввода.
    p.password_cmd = "urfin-no-such-manager get {host}".into();
    let seen = run(vec![p], home.cfg(), &Answers::default(), false, |_| {}).await;
    assert!(
        matches!(seen.failed, Some(ConnError::PasswordCommand(_))),
        "{:?}",
        seen.failed
    );
}

#[tokio::test]
async fn jump_through_second_server() {
    let home = Home::new();
    let jump = TestSrv::start(Opts {
        host_seed: 2,
        keys: vec![key(11).public_key().clone()],
        ..Opts::default()
    });
    let target = TestSrv::start(Opts {
        host_seed: 3,
        password: Some("secret"),
        ..Opts::default()
    });
    home.trust(&jump);
    home.trust(&target);
    let mut j = profile("bastion", &jump, Auth::Key);
    j.key = home.client_key(11).display().to_string();
    let mut t = profile("web", &target, Auth::Password);
    t.via = "bastion".into();
    let chain = conn::chain(&t, |n| (n == "bastion").then(|| j.clone())).unwrap();
    let a = Answers {
        password: Some("secret"),
        ..Answers::default()
    };
    let seen = run(chain, home.cfg(), &a, false, |_| {}).await;
    assert_eq!(seen.connected, ["web"]);
    let hosts: Vec<_> = seen
        .phases
        .iter()
        .filter(|(_, p)| *p == Phase::Auth)
        .map(|(h, _)| h.clone())
        .collect();
    assert_eq!(hosts, [name(&jump), name(&target)]);
}

#[tokio::test]
async fn unknown_key_asks_and_trust_appends_one_line() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        password: Some("secret"),
        ..Opts::default()
    });
    let before = "# my hosts\r\nother.host ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOxEXu3e7bUf7pNEHcRvL0Gz3kQ5n2n4Hn1bkC9m0xYZ\r\n";
    std::fs::write(home.known_hosts(), before).unwrap();
    // «Отмена» — без подключения, файл не тронут.
    let a = Answers {
        password: Some("secret"),
        host_key: Some(HostKeyChoice::Cancel),
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.failed, Some(ConnError::HostKeyRejected));
    assert_eq!(
        seen.unknown,
        [(name(&srv), hostkey::fingerprint(&srv.host_key))]
    );
    assert!(seen.unknown[0].1.starts_with("SHA256:"));
    assert_eq!(std::fs::read_to_string(home.known_hosts()).unwrap(), before);

    let a = Answers {
        password: Some("secret"),
        host_key: Some(HostKeyChoice::Trust),
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.connected, ["srv1"]);
    let after = std::fs::read_to_string(home.known_hosts()).unwrap();
    assert!(after.starts_with(before));
    assert_eq!(after[before.len()..].lines().count(), 1);
    assert!(after[before.len()..].starts_with(&format!("[127.0.0.1]:{} ssh-ed25519 ", srv.port)));
    // Второй раз — без вопроса.
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.unknown.len(), 0);
    assert_eq!(seen.connected, ["srv1"]);
}

#[tokio::test]
async fn trust_once_uses_session_file() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        password: Some("secret"),
        ..Opts::default()
    });
    let a = Answers {
        password: Some("secret"),
        host_key: Some(HostKeyChoice::Once),
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.connected, ["srv1"]);
    assert!(!home.known_hosts().exists());
    // Файл сеанса удалён при отключении.
    let run_dir = home.state().join("state").join("ssh").join("run");
    let left = std::fs::read_dir(&run_dir).map_or(0, Iterator::count);
    assert_eq!(left, 0);
}

#[tokio::test]
async fn changed_key_does_not_connect() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        host_seed: 5,
        password: Some("secret"),
        ..Opts::default()
    });
    hostkey::append(&home.known_hosts(), "other", &key(9).public_key().clone()).unwrap();
    hostkey::append(
        &home.known_hosts(),
        &name(&srv),
        &key(6).public_key().clone(),
    )
    .unwrap();
    let a = Answers {
        password: Some("secret"),
        host_key: Some(HostKeyChoice::Trust),
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.connected.len(), 0);
    assert_eq!(seen.prompts.len(), 0);
    assert_eq!(seen.changed, [(home.known_hosts(), 2)]);
    assert_eq!(
        seen.failed,
        Some(ConnError::HostKeyChanged {
            file: home.known_hosts(),
            line: 2
        })
    );
    // «Заменить» — меняется строка 2, копия рядом.
    let a = Answers {
        password: Some("secret"),
        replace: true,
        ..Answers::default()
    };
    let seen = run(
        vec![profile("srv1", &srv, Auth::Password)],
        home.cfg(),
        &a,
        false,
        |_| {},
    )
    .await;
    assert_eq!(seen.connected, ["srv1"]);
    let kh = hostkey::KnownHosts::load(&[home.known_hosts()]);
    assert_eq!(
        kh.check(&name(&srv), &srv.host_key),
        hostkey::Verdict::Known
    );
    assert!(
        home.dir
            .path()
            .join(".ssh")
            .join("known_hosts.urfin-bak")
            .exists()
    );
}

#[tokio::test]
async fn reconnects_five_times_then_disconnected() {
    let home = Home::new();
    let mut srv = TestSrv::start(Opts {
        password: Some("secret"),
        ..Opts::default()
    });
    home.trust(&srv);
    let a = Answers {
        password: Some("secret"),
        ..Answers::default()
    };
    let cfg = home.cfg();
    let interval = cfg.interval;
    let p = profile("srv1", &srv, Auth::Password);
    let mut killed = false;
    let seen = run(vec![p], cfg, &a, true, |_| {
        if !killed {
            killed = true;
            srv.kill();
        }
    })
    .await;
    assert_eq!(seen.connected, ["srv1"]);
    let attempts: Vec<_> = seen.lost.iter().map(|(k, m, _)| (*k, *m)).collect();
    assert_eq!(attempts, [(1, 5), (2, 5), (3, 5), (4, 5), (5, 5)]);
    for w in seen.lost.windows(2) {
        assert!(w[1].2 - w[0].2 >= interval);
    }
    assert!(seen.disconnected);
    // Пароль не спрашивался заново: секрет из памяти сеанса.
    assert_eq!(seen.prompts.len(), 1);
}

#[tokio::test]
async fn reconnects_after_server_restart() {
    let home = Home::new();
    let srv = TestSrv::start(Opts {
        password: Some("secret"),
        ..Opts::default()
    });
    home.trust(&srv);
    let port = srv.port;
    let a = Answers {
        password: Some("secret"),
        ..Answers::default()
    };
    let p = profile("srv1", &srv, Auth::Password);
    let mut srv = Some(srv);
    let mut n = 0;
    let seen = run(vec![p], home.cfg(), &a, true, |ctl| {
        n += 1;
        if n == 1 {
            // Падение и подъём на том же порту.
            srv.take().unwrap().kill();
            srv = Some(TestSrv::start_on(
                port,
                Opts {
                    password: Some("secret"),
                    ..Opts::default()
                },
            ));
        } else {
            let _ = ctl.send(Control::Disconnect);
        }
    })
    .await;
    assert_eq!(seen.connected, ["srv1", "srv1"]);
    assert_ne!(seen.lost.len(), 0);
    assert_eq!(seen.prompts.len(), 1);
}

/// UI-поток не ждёт сеть: задача уходит на поток `net` сразу, даже если сервер молчит.
#[test]
fn ui_thread_does_not_block() {
    struct NoWake;
    impl urfin_core::Waker for NoWake {
        fn wake(&self) {}
        fn wake_after(&self, _d: Duration) {}
    }
    let (rt, inbox) =
        urfin_core::Runtime::new(&urfin_core::RuntimeConfig::default(), Arc::new(NoWake));
    // Принимает TCP и молчит.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut p = Profile::new("mute", "127.0.0.1");
    p.port = silent.local_addr().unwrap().port();
    let home = Home::new();
    let net = urfin_net::NetRt::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_ctl_tx, ctl) = mpsc::unbounded_channel();
    let start = Instant::now();
    assert!(net.spawn(
        &rt,
        conn::session(vec![p], home.cfg(), Arc::new(Pool::new()), tx, ctl)
    ));
    assert!(start.elapsed() < Duration::from_millis(100));
    // Сеанс идёт: фаза TCP пришла, подключения нет.
    let deadline = Instant::now() + Duration::from_secs(5);
    let ev = loop {
        if let Ok(ev) = rx.try_recv() {
            break ev;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(matches!(
        ev,
        Event::Phase {
            phase: Phase::Tcp,
            ..
        }
    ));
    net.stop();
    assert_eq!(rt.shutdown(inbox, Duration::from_secs(5)), 0);
}
