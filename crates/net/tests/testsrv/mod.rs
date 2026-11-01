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
    /// Корень SFTP (F03-04): `/` сервера — этот каталог; `None` — только `realpath`.
    pub sftp_root: Option<std::path::PathBuf>,
    /// Ссылки сервера `путь → цель` (на Windows симлинки создавать нельзя без прав).
    pub links: Vec<(String, String)>,
    /// Владелец и группа в `longname`; `None` — `longname` без имён (мусор).
    pub owner: Option<(&'static str, &'static str)>,
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
                russh_sftp::server::run(ch.into_stream(), Sftp::new(&self.opts)).await;
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

/// SFTP-сервер над каталогом (F03-04): листинг батчами по 100, `longname` с владельцем,
/// чтение и запись, `mkdir`/`rmdir`/`remove`/`rename`/`setstat`, ссылки из
/// [`Opts::links`]. Без корня — только `realpath`.
struct Sftp {
    root: Option<std::path::PathBuf>,
    links: Vec<(String, String)>,
    owner: Option<(&'static str, &'static str)>,
    next: u64,
    dirs: HashMap<String, std::collections::VecDeque<russh_sftp::protocol::File>>,
    files: HashMap<String, std::fs::File>,
}

/// Записей в одном `READDIR` (как у OpenSSH).
pub const READDIR_BATCH: usize = 100;

use russh_sftp::protocol::{
    Attrs as SAttrs, Data, File as SFile, FileAttributes, Handle as SHandle, Name, OpenFlags,
    Status, StatusCode,
};

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

fn code(e: &std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

/// Нормализованный путь сервера: `/a/b`.
fn clean(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    format!("/{}", out.join("/"))
}

fn attrs_of(md: &std::fs::Metadata) -> FileAttributes {
    let mut a = FileAttributes::empty();
    a.size = Some(md.len());
    a.uid = Some(1000);
    a.gid = Some(1000);
    let ty = if md.is_dir() { 0o040_000 } else { 0o100_000 };
    let rw = if md.permissions().readonly() {
        0o444
    } else {
        0o644
    };
    a.permissions = Some(ty | if md.is_dir() { rw | 0o111 } else { rw });
    let secs = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| u32::try_from(d.as_secs()).ok())
    };
    a.mtime = secs(md.modified());
    a.atime = secs(md.accessed());
    a
}

impl Sftp {
    fn new(opts: &Opts) -> Sftp {
        Sftp {
            root: opts.sftp_root.clone(),
            links: opts.links.clone(),
            owner: opts.owner,
            next: 0,
            dirs: HashMap::new(),
            files: HashMap::new(),
        }
    }

    fn local(&self, p: &str) -> Result<std::path::PathBuf, StatusCode> {
        let root = self.root.as_ref().ok_or(StatusCode::OpUnsupported)?;
        let mut out = root.clone();
        for c in clean(p).split('/').filter(|c| !c.is_empty()) {
            out.push(c);
        }
        Ok(out)
    }

    fn link(&self, p: &str) -> Option<&str> {
        let p = clean(p);
        self.links
            .iter()
            .find(|(l, _)| clean(l) == p)
            .map(|(_, t)| t.as_str())
    }

    fn lstat_attrs(&self, p: &str) -> Result<FileAttributes, StatusCode> {
        if self.link(p).is_some() {
            let mut a = FileAttributes::empty();
            a.size = Some(0);
            a.permissions = Some(0o120_777);
            a.uid = Some(1000);
            a.gid = Some(1000);
            return Ok(a);
        }
        std::fs::metadata(self.local(p)?)
            .map(|m| attrs_of(&m))
            .map_err(|e| code(&e))
    }

    fn stat_attrs(&self, p: &str) -> Result<FileAttributes, StatusCode> {
        match self.link(p) {
            Some(t) => {
                let parent = clean(&format!("{}/..", clean(p)));
                let t = if t.starts_with('/') {
                    t.to_owned()
                } else {
                    format!("{parent}/{t}")
                };
                self.lstat_attrs(&t)
            }
            None => self.lstat_attrs(p),
        }
    }

    fn file(&self, name: String, attrs: FileAttributes) -> SFile {
        let mut f = SFile::new(name, attrs);
        if self.owner.is_none() {
            f.longname = format!("?? {}", f.filename);
        } else if let Some((u, g)) = self.owner {
            f.longname = format!(
                "{} 1 {u} {g} {} Jan  1 12:00 {}",
                mode_str(f.attrs.permissions.unwrap_or(0)),
                f.attrs.size.unwrap_or(0),
                f.filename
            );
        }
        f
    }

    fn handle(&mut self) -> String {
        self.next += 1;
        format!("h{}", self.next)
    }
}

fn mode_str(m: u32) -> String {
    let t = match m & 0o170_000 {
        0o040_000 => 'd',
        0o120_000 => 'l',
        _ => '-',
    };
    let mut s = String::from(t);
    for (bit, c) in [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ] {
        s.push(if m & bit != 0 { c } else { '-' });
    }
    s
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![SFile::dummy(clean(&path))],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<SHandle, Self::Error> {
        let dir = self.local(&path)?;
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| code(&e))?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect();
        let base = clean(&path);
        for (l, _) in &self.links {
            let l = clean(l);
            if clean(&format!("{l}/..")) == base
                && let Some(n) = l.rsplit('/').next()
            {
                names.push(n.to_owned());
            }
        }
        names.sort();
        let mut list = std::collections::VecDeque::new();
        list.push_back(self.file(".".into(), self.lstat_attrs(&base)?));
        for n in names {
            let p = format!("{}/{n}", base.trim_end_matches('/'));
            if let Ok(a) = self.lstat_attrs(&p) {
                list.push_back(self.file(n, a));
            }
        }
        let h = self.handle();
        self.dirs.insert(h.clone(), list);
        Ok(SHandle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let list = self.dirs.get_mut(&handle).ok_or(StatusCode::Failure)?;
        if list.is_empty() {
            return Err(StatusCode::Eof);
        }
        let n = list.len().min(READDIR_BATCH);
        Ok(Name {
            id,
            files: list.drain(..n).collect(),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.dirs.remove(&handle);
        self.files.remove(&handle);
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<SAttrs, Self::Error> {
        Ok(SAttrs {
            id,
            attrs: self.lstat_attrs(&path)?,
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<SAttrs, Self::Error> {
        Ok(SAttrs {
            id,
            attrs: self.stat_attrs(&path)?,
        })
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let t = self.link(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Name {
            id,
            files: vec![SFile::dummy(t)],
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<SAttrs, Self::Error> {
        let f = self.files.get(&handle).ok_or(StatusCode::Failure)?;
        let md = f.metadata().map_err(|e| code(&e))?;
        Ok(SAttrs {
            id,
            attrs: attrs_of(&md),
        })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<SHandle, Self::Error> {
        let p = self.local(&filename)?;
        let f = std::fs::OpenOptions::from(pflags)
            .open(p)
            .map_err(|e| code(&e))?;
        let h = self.handle();
        self.files.insert(h.clone(), f);
        Ok(SHandle { id, handle: h })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        use std::io::{Read as _, Seek as _};
        let f = self.files.get_mut(&handle).ok_or(StatusCode::Failure)?;
        f.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| code(&e))?;
        let mut buf = vec![0; len as usize];
        let n = f.read(&mut buf).map_err(|e| code(&e))?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        use std::io::{Seek as _, Write as _};
        let f = self.files.get_mut(&handle).ok_or(StatusCode::Failure)?;
        f.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| code(&e))?;
        f.write_all(&data).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        std::fs::create_dir(self.local(&path)?).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_dir(self.local(&path)?).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.local(&filename)?).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        std::fs::rename(self.local(&oldpath)?, self.local(&newpath)?).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let p = self.local(&path)?;
        if let Some(m) = attrs.permissions {
            let mut perm = std::fs::metadata(&p).map_err(|e| code(&e))?.permissions();
            perm.set_readonly(m & 0o200 == 0);
            std::fs::set_permissions(&p, perm).map_err(|e| code(&e))?;
        }
        if let Some(t) = attrs.mtime {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&p)
                .map_err(|e| code(&e))?;
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t.into()))
                .map_err(|e| code(&e))?;
        }
        Ok(ok(id))
    }
}
