//! `testsrv` (F03-02): SSH-сервер russh с SFTP (russh-sftp) на 127.0.0.1 для
//! интеграционных тестов. Ключи и пароль, 2FA-заглушка (keyboard-interactive после
//! первого фактора), `direct-tcpip` для jump. Сервер живёт на своём потоке и рантайме:
//! [`TestSrv::kill`] рвёт все соединения разом, как падение сервера.

#![allow(dead_code, reason = "общий модуль: не каждый тест берёт всё")]
#![allow(
    clippy::unused_async_trait_impl,
    reason = "методы обработчиков russh — async по трейту"
)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::sync::oneshot;

/// Ключ из seed: тесты детерминированы.
pub fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

/// Что принимает сервер.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    /// Seed ключа хоста.
    pub host_seed: u8,
    pub password: Option<&'static str>,
    pub keys: Vec<PublicKey>,
    /// Код второго фактора (keyboard-interactive) после пароля или ключа.
    pub otp: Option<&'static str>,
}

pub struct TestSrv {
    pub port: u16,
    pub host_key: PublicKey,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl TestSrv {
    pub fn start(opts: Opts) -> TestSrv {
        TestSrv::start_on(0, opts)
    }

    /// На заданном порту (подъём после [`TestSrv::kill`]); 0 — любой.
    pub fn start_on(port: u16, opts: Opts) -> TestSrv {
        let host = key(opts.host_seed.max(1));
        let host_key = host.public_key().clone();
        let mut methods = MethodSet::empty();
        if opts.password.is_some() {
            methods.push(MethodKind::Password);
        }
        if !opts.keys.is_empty() {
            methods.push(MethodKind::PublicKey);
        }
        let config = Arc::new(server::Config {
            keys: vec![host],
            methods,
            auth_rejection_time: std::time::Duration::from_millis(1),
            auth_rejection_time_initial: Some(std::time::Duration::ZERO),
            ..server::Config::default()
        });
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = oneshot::channel::<()>();
        let opts = Arc::new(opts);
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let accept = async {
                    loop {
                        let Ok((tcp, _)) = listener.accept().await else {
                            continue;
                        };
                        let h = Srv {
                            opts: Arc::clone(&opts),
                            first_ok: false,
                            channels: HashMap::new(),
                        };
                        let config = Arc::clone(&config);
                        tokio::spawn(async move {
                            if let Ok(s) = server::run_stream(config, tcp, h).await {
                                let _ = s.await;
                            }
                        });
                    }
                };
                tokio::select! {
                    () = accept => {}
                    _ = stopped => {}
                }
            });
            // Рантайм уходит вместе с задачами сеансов: сокеты закрываются.
            drop(rt);
        });
        TestSrv {
            port,
            host_key,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// «kill сервера»: listener и все соединения закрываются.
    pub fn kill(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

impl Drop for TestSrv {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Srv {
    opts: Arc<Opts>,
    /// Первый фактор пройден, ждём код.
    first_ok: bool,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl Srv {
    fn first_factor(&mut self) -> Auth {
        if self.opts.otp.is_none() {
            return Auth::Accept;
        }
        self.first_ok = true;
        let mut kbd = MethodSet::empty();
        kbd.push(MethodKind::KeyboardInteractive);
        Auth::Reject {
            proceed_with_methods: Some(kbd),
            partial_success: true,
        }
    }
}

impl server::Handler for Srv {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if self.opts.password == Some(password) {
            self.first_factor()
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(
            if self
                .opts
                .keys
                .iter()
                .any(|k| k.key_data() == key.public_key.key_data())
            {
                self.first_factor()
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let (true, Some(otp)) = (self.first_ok, self.opts.otp) else {
            return Ok(Auth::reject());
        };
        Ok(match response {
            None => Auth::Partial {
                name: Cow::Borrowed("2FA"),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Verification code: "), false)]),
            },
            Some(mut r) => {
                if r.next().as_deref() == Some(otp.as_bytes()) {
                    Auth::Accept
                } else {
                    Auth::reject()
                }
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match self.channels.remove(&channel) {
            Some(ch) if name == "sftp" => {
                session.channel_success(channel)?;
                russh_sftp::server::run(ch.into_stream(), Sftp).await;
            }
            _ => session.channel_failure(channel)?,
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(port) = u16::try_from(port) else {
            return Ok(());
        };
        let Ok(mut tcp) = tokio::net::TcpStream::connect((host, port)).await else {
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut ch = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut ch, &mut tcp).await;
        });
        Ok(())
    }
}

/// SFTP-заглушка: версия и `realpath`; содержимое — в F03-04.
struct Sftp;

impl russh_sftp::server::Handler for Sftp {
    type Error = russh_sftp::protocol::StatusCode;

    fn unimplemented(&self) -> Self::Error {
        russh_sftp::protocol::StatusCode::OpUnsupported
    }

    async fn realpath(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Name, Self::Error> {
        let path = if path.is_empty() || path == "." {
            "/".to_owned()
        } else {
            path
        };
        Ok(russh_sftp::protocol::Name {
            id,
            files: vec![russh_sftp::protocol::File::dummy(path)],
        })
    }
}
