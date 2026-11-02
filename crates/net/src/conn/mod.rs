//! Подключение (F03 §2.3, F03-02): цепочка `via` (jump до 3, `direct-tcpip`), пул
//! соединений, keep-alive, переподключение.
//!
//! Всё здесь бежит на потоке [`crate::NetRt`]; наружу — события [`Event`] в канал. Вопросы
//! пользователю (ключ сервера, пароль, код) — события с `oneshot`-ответом: закрытый ответ
//! или канал — «Отмена». Ожидание ответа в таймауты не входит.

mod client;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use russh::client::Handle;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

pub use client::Client;

use crate::auth;
use crate::config::{AgentKind, PasswordManager, SshCfg};
use crate::hostkey;
use crate::profile::Profile;
use crate::secrets::Secrets;

/// TCP и открытие `direct-tcpip` (F03 §2.3).
pub const TCP_TIMEOUT: Duration = Duration::from_secs(10);
/// Обмен ключами и аутентификация, без ожидания пользователя.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(60);
/// Jump-узлов в цепочке не больше.
pub const MAX_JUMPS: usize = 3;
/// Пропущенных keep-alive до обрыва.
const KEEPALIVE_MISSES: u32 = 2;

/// Настройки подключения из `[ssh]` и путей.
#[derive(Clone, Debug)]
pub struct ConnCfg {
    pub agent: AgentKind,
    pub default_keys: Vec<PathBuf>,
    /// Файлы для проверки: `~/.ssh/known_hosts`, `~/.ssh/known_hosts2`.
    pub known_hosts: Vec<PathBuf>,
    /// Куда пишет «Доверять»: `~/.ssh/known_hosts`.
    pub trust_file: Option<PathBuf>,
    /// `state/ssh/run`: файлы сеансов «Только в этот раз».
    pub run_dir: Option<PathBuf>,
    /// `None` — keep-alive выключен.
    pub keepalive: Option<Duration>,
    pub tries: u32,
    pub interval: Duration,
    /// `[ssh].password_cmd`; пусто — шаблон `password_manager`.
    pub password_cmd: String,
    pub password_manager: PasswordManager,
    pub secret_ttl_min: u32,
    /// Кэш паролей (Диспетчер учётных данных) с политикой; `None` — без кэша.
    pub vault: Option<Arc<crate::vault::Vault>>,
}

impl ConnCfg {
    /// `home` — для `~` в путях ключей и `~/.ssh`.
    pub fn new(cfg: &SshCfg, home: Option<&Path>, state_dir: Option<&Path>) -> ConnCfg {
        let ssh = home.map(|h| h.join(".ssh"));
        ConnCfg {
            agent: cfg.agent,
            default_keys: cfg
                .default_keys
                .iter()
                .filter_map(|k| expand_home(k, home))
                .collect(),
            known_hosts: ssh
                .iter()
                .flat_map(|d| [d.join("known_hosts"), d.join("known_hosts2")])
                .collect(),
            trust_file: ssh.map(|d| d.join("known_hosts")),
            run_dir: state_dir.map(|d| d.join("ssh").join("run")),
            keepalive: (cfg.keepalive_s > 0).then(|| Duration::from_secs(cfg.keepalive_s.into())),
            tries: cfg.reconnect.tries,
            interval: Duration::from_secs(cfg.reconnect.interval_s.into()),
            password_cmd: cfg.password_cmd.clone(),
            password_manager: cfg.password_manager,
            secret_ttl_min: cfg.secret_ttl_min,
            vault: None,
        }
    }

    /// Keep-alive профиля: `ServerAliveInterval` из `ssh_config` сильнее `[ssh]`.
    fn keepalive_for(&self, p: &Profile) -> Option<Duration> {
        match p.keepalive_s {
            Some(0) => None,
            Some(s) => Some(Duration::from_secs(s.into())),
            None => self.keepalive,
        }
    }
}

/// `~/x` → `<home>/x`; без домашней папки такие пути отбрасываются.
pub(crate) fn expand_home(p: &str, home: Option<&Path>) -> Option<PathBuf> {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => home.map(|h| h.join(rest)),
        None => Some(PathBuf::from(p)),
    }
}

// Фаза и вид вопроса — в core (модель подключений для UI, F03-05a).
pub use urfin_core::net::{Phase, PromptKind};

/// Ответ на неизвестный ключ сервера.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostKeyChoice {
    /// Строка в конец `~/.ssh/known_hosts`.
    Trust,
    /// Только файл сеанса; удаляется при отключении.
    Once,
    Cancel,
}

/// Ответ строки ввода: текст и флажок «Запомнить» (пароль, F03 §2.5).
#[derive(Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// Положить пароль в Диспетчер учётных данных после успешного входа.
    pub remember: bool,
}

impl fmt::Debug for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Answer")
            .field("remember", &self.remember)
            .finish_non_exhaustive()
    }
}

impl From<String> for Answer {
    fn from(text: String) -> Answer {
        Answer {
            text,
            remember: false,
        }
    }
}

impl From<&str> for Answer {
    fn from(text: &str) -> Answer {
        Answer::from(text.to_owned())
    }
}

/// Событие подключения для UI.
#[derive(Debug)]
pub enum Event {
    Phase {
        host: String,
        phase: Phase,
    },
    HostKeyUnknown {
        /// `host` или `[host]:port`.
        host: String,
        algo: String,
        fingerprint: String,
        reply: oneshot::Sender<HostKeyChoice>,
    },
    /// Ключ сменился: без ответа `true` подключения нет.
    HostKeyChanged {
        host: String,
        algo: String,
        file: PathBuf,
        line: usize,
        old_fingerprint: String,
        fingerprint: String,
        /// `true` — «Заменить ключ и подключиться» (имя хоста введено).
        reply: oneshot::Sender<bool>,
    },
    Prompt {
        host: String,
        kind: PromptKind,
        text: String,
        echo: bool,
        /// `None` — «Отмена».
        reply: oneshot::Sender<Option<Answer>>,
    },
    Connected(Arc<Conn>),
    /// Связь потеряна: попытка `attempt` из `tries` через `delay`.
    Lost {
        attempt: u32,
        tries: u32,
        delay: Duration,
    },
    /// Не подключились; переподключений не будет.
    Failed(ConnError),
    /// Попытки исчерпаны или отключились сами.
    Disconnected,
}

/// Канал событий; закрыт — все вопросы получают «Отмена».
pub type Events = mpsc::UnboundedSender<Event>;

/// Управление сеансом из UI.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Control {
    ReconnectNow,
    Disconnect,
}

/// Ошибка подключения. Текст — для лога; строки UI — по варианту.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConnError {
    #[error("{host}: {message}")]
    Tcp { host: String, message: String },
    #[error("timeout")]
    Timeout,
    #[error("host key rejected")]
    HostKeyRejected,
    #[error("host key changed: {file}:{line}")]
    HostKeyChanged { file: PathBuf, line: usize },
    #[error("host key revoked")]
    HostKeyRevoked,
    #[error("authentication failed")]
    Auth,
    #[error("cancelled")]
    Cancelled,
    #[error("jump chain longer than {MAX_JUMPS}")]
    TooManyJumps,
    #[error("jump host not found: {0}")]
    UnknownJump(String),
    /// Команда менеджера паролей: текст с хвостом stderr, без stdout.
    #[error("{0}")]
    PasswordCommand(String),
    #[error("{0}")]
    Ssh(String),
}

impl ConnError {
    /// После таких ошибок переподключение бессмысленно: нужен пользователь.
    fn is_final(&self) -> bool {
        matches!(
            self,
            ConnError::HostKeyRejected
                | ConnError::HostKeyChanged { .. }
                | ConnError::HostKeyRevoked
                | ConnError::Auth
                | ConnError::Cancelled
                | ConnError::PasswordCommand(_)
                | ConnError::TooManyJumps
                | ConnError::UnknownJump(_)
        )
    }
}

impl From<russh::Error> for ConnError {
    fn from(e: russh::Error) -> Self {
        ConnError::Ssh(e.to_string())
    }
}

/// Цепочка узлов до `target`: сначала первый jump, последним — сам `target`.
/// `via` — имя профиля (через `lookup`) или `user@host:port`.
pub fn chain(
    target: &Profile,
    lookup: impl Fn(&str) -> Option<Profile>,
) -> Result<Vec<Profile>, ConnError> {
    let mut out = vec![target.clone()];
    while !out[out.len() - 1].via.is_empty() {
        if out.len() > MAX_JUMPS {
            return Err(ConnError::TooManyJumps);
        }
        let via = &out[out.len() - 1].via;
        let next = lookup(via)
            .or_else(|| adhoc(via))
            .ok_or_else(|| ConnError::UnknownJump(via.clone()))?;
        out.push(next);
    }
    out.reverse();
    Ok(out)
}

/// Разовый jump-узел из `[user@]host[:port]`.
fn adhoc(s: &str) -> Option<Profile> {
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) => (u, r),
        None => ("", s),
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, p.parse().ok()?),
        _ => (rest, 22),
    };
    if host.is_empty() || !s.contains(['@', ':', '.']) {
        return None;
    }
    let mut p = Profile::new(s, host);
    user.clone_into(&mut p.user);
    p.port = port;
    Some(p)
}

/// Время ожидания пользователя: в таймауты оно не входит.
#[derive(Debug, Default)]
pub(crate) struct Waited {
    /// Миллисекунды законченных ожиданий.
    total: AtomicU64,
    /// Ожиданий идёт сейчас.
    now: AtomicU32,
}

impl Waited {
    fn total(&self) -> Duration {
        Duration::from_millis(self.total.load(Ordering::SeqCst))
    }

    /// Ждёт ответ пользователя, учитывая время.
    pub(crate) async fn user<T>(&self, rx: oneshot::Receiver<T>) -> Option<T> {
        let start = Instant::now();
        self.now.fetch_add(1, Ordering::SeqCst);
        let r = rx.await.ok();
        let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.total.fetch_add(ms, Ordering::SeqCst);
        self.now.fetch_sub(1, Ordering::SeqCst);
        r
    }
}

/// `fut` с пределом `limit` без ожидания пользователя.
async fn within<F: Future>(
    limit: Duration,
    waited: &Waited,
    fut: F,
) -> Result<F::Output, ConnError> {
    let start = Instant::now();
    let mut fut = std::pin::pin!(fut);
    loop {
        let until = start + limit + waited.total();
        tokio::select! {
            r = &mut fut => return Ok(r),
            () = tokio::time::sleep_until(until) => {
                if waited.now.load(Ordering::SeqCst) == 0 && Instant::now() >= start + limit + waited.total() {
                    return Err(ConnError::Timeout);
                }
                if waited.now.load(Ordering::SeqCst) > 0 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
}

/// Поднятое соединение одного узла. Jump-узел живёт, пока жив кто-то из его потомков.
pub struct Conn {
    pub profile: String,
    handle: Handle<Client>,
    _via: Option<Arc<Conn>>,
    lost: watch::Receiver<bool>,
    /// RTT последнего keep-alive, мс; `u32::MAX` — ещё не было.
    rtt_ms: Arc<AtomicU32>,
}

impl fmt::Debug for Conn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conn")
            .field("profile", &self.profile)
            .field("lost", &self.is_lost())
            .finish_non_exhaustive()
    }
}

impl Conn {
    pub fn handle(&self) -> &Handle<Client> {
        &self.handle
    }

    /// Пинг заголовка панели.
    pub fn rtt(&self) -> Option<Duration> {
        match self.rtt_ms.load(Ordering::Relaxed) {
            u32::MAX => None,
            ms => Some(Duration::from_millis(ms.into())),
        }
    }

    pub fn is_lost(&self) -> bool {
        *self.lost.borrow() || self.handle.is_closed()
    }

    /// Ждёт обрыва: EOF, ошибка или пропущенные keep-alive.
    pub async fn lost(&self) {
        let mut rx = self.lost.clone();
        let _ = rx.wait_for(|l| *l).await;
    }

    pub async fn close(&self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    }
}

/// Контекст подключения узлов одного сеанса.
pub(crate) struct Cx<'a> {
    pub cfg: &'a ConnCfg,
    pub events: &'a Events,
    pub mem: &'a mut Secrets,
    pub session_hosts: Option<&'a Path>,
}

impl Cx<'_> {
    pub(crate) fn phase(&self, host: &str, phase: Phase) {
        let _ = self.events.send(Event::Phase {
            host: host.to_owned(),
            phase,
        });
    }
}

/// Пул: соединение на профиль. Jump-узлы переиспользуются, пока живы.
#[derive(Debug, Default)]
pub struct Pool {
    conns: Mutex<HashMap<String, Weak<Conn>>>,
}

impl Pool {
    pub fn new() -> Pool {
        Pool::default()
    }

    /// Живое соединение профиля.
    pub fn get(&self, profile: &str) -> Option<Arc<Conn>> {
        let mut map = self.conns.lock().unwrap_or_else(PoisonError::into_inner);
        let c = map.get(profile).and_then(Weak::upgrade);
        match c {
            Some(c) if !c.is_lost() => Some(c),
            _ => {
                map.remove(profile);
                None
            }
        }
    }

    /// Поднимает цепочку [`chain`], переиспользуя живые узлы.
    pub(crate) async fn connect(
        &self,
        chain: &[Profile],
        cx: &mut Cx<'_>,
    ) -> Result<Arc<Conn>, ConnError> {
        let mut via: Option<Arc<Conn>> = None;
        for p in chain {
            let c = if let Some(c) = self.get(&p.name) {
                c
            } else {
                let c = open(p, via.take(), cx).await?;
                self.conns
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(p.name.clone(), Arc::downgrade(&c));
                c
            };
            via = Some(c);
        }
        via.ok_or(ConnError::Cancelled)
    }
}

/// Один узел: TCP или `direct-tcpip` через `via`, ключ сервера, аутентификация.
async fn open(
    p: &Profile,
    via: Option<Arc<Conn>>,
    cx: &mut Cx<'_>,
) -> Result<Arc<Conn>, ConnError> {
    let name = hostkey::host_name(&p.host, p.port);
    let waited = Arc::new(Waited::default());
    let (lost_tx, lost) = watch::channel(false);
    let lost_tx = Arc::new(lost_tx);
    let outcome = Arc::new(Mutex::new(None));
    let handler = Client::new(
        name.clone(),
        cx,
        Arc::clone(&waited),
        Arc::clone(&outcome),
        Arc::clone(&lost_tx),
    );
    let config = Arc::new(russh::client::Config {
        nodelay: true,
        ..russh::client::Config::default()
    });
    cx.phase(&name, Phase::Tcp);
    let handshake = match &via {
        None => {
            let tcp = tokio::time::timeout(
                TCP_TIMEOUT,
                tokio::net::TcpStream::connect((p.host.as_str(), p.port)),
            )
            .await
            .map_err(|_| ConnError::Timeout)?
            .map_err(|e| ConnError::Tcp {
                host: name.clone(),
                message: e.to_string(),
            })?;
            let _ = tcp.set_nodelay(true);
            within(
                AUTH_TIMEOUT,
                &waited,
                russh::client::connect_stream(config, tcp, handler),
            )
            .await
        }
        Some(j) => {
            let ch = tokio::time::timeout(
                TCP_TIMEOUT,
                j.handle
                    .channel_open_direct_tcpip(p.host.as_str(), p.port.into(), "127.0.0.1", 0),
            )
            .await
            .map_err(|_| ConnError::Timeout)?
            .map_err(|e| ConnError::Tcp {
                host: name.clone(),
                message: e.to_string(),
            })?;
            within(
                AUTH_TIMEOUT,
                &waited,
                russh::client::connect_stream(config, ch.into_stream(), handler),
            )
            .await
        }
    };
    let mut handle = match handshake? {
        Ok(h) => h,
        Err(e) => {
            let why = outcome
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            return Err(why.unwrap_or_else(|| ConnError::from(e)));
        }
    };
    cx.phase(&name, Phase::Auth);
    let auth = auth::authenticate(&mut handle, p, &name, &waited, cx);
    match within(AUTH_TIMEOUT, &waited, auth).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) | Err(e) => {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
            return Err(e);
        }
    }
    let conn = Arc::new(Conn {
        profile: p.name.clone(),
        handle,
        _via: via,
        lost,
        rtt_ms: Arc::new(AtomicU32::new(u32::MAX)),
    });
    if let Some(every) = cx.cfg.keepalive_for(p) {
        tokio::spawn(keepalive(Arc::downgrade(&conn), lost_tx, every));
    }
    tracing::info!(target: "net", host = %name, "connected");
    Ok(conn)
}

/// `keepalive@openssh.com` каждые `every`; `KEEPALIVE_MISSES` без ответа — обрыв.
async fn keepalive(conn: Weak<Conn>, lost: Arc<watch::Sender<bool>>, every: Duration) {
    let mut misses = 0;
    loop {
        tokio::time::sleep(every).await;
        let Some(c) = conn.upgrade() else { return };
        if c.is_lost() {
            lost.send_replace(true);
            return;
        }
        let start = Instant::now();
        match tokio::time::timeout(every, c.handle.send_ping()).await {
            Ok(Ok(())) => {
                misses = 0;
                let ms = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX - 1);
                c.rtt_ms.store(ms, Ordering::Relaxed);
            }
            Ok(Err(_)) => misses = KEEPALIVE_MISSES,
            Err(_) => misses += 1,
        }
        if misses >= KEEPALIVE_MISSES {
            tracing::info!(target: "net", profile = %c.profile, "keep-alive lost");
            lost.send_replace(true);
            c.close().await;
            return;
        }
    }
}

/// Папка сеанса `state/ssh/run/<nonce>` с `known_hosts` «Только в этот раз»;
/// удаляется при отключении.
#[derive(Debug)]
struct RunDir(Option<PathBuf>);

impl RunDir {
    fn new(base: Option<&Path>) -> RunDir {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        RunDir(base.map(|b| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            b.join(format!(
                "{}-{}-{nanos:08x}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ))
        }))
    }

    fn known_hosts(&self) -> Option<PathBuf> {
        self.0.as_ref().map(|d| d.join("known_hosts"))
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        if let Some(d) = &self.0 {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// Сеанс профиля: подключение, затем переподключения после обрыва (F03 §2.3).
/// Секреты из памяти сеанса используются повторно; задача кончается отключением.
pub async fn session(
    chain: Vec<Profile>,
    cfg: ConnCfg,
    pool: Arc<Pool>,
    events: Events,
    mut ctl: mpsc::UnboundedReceiver<Control>,
) {
    let mut mem = Secrets::new(cfg.secret_ttl_min);
    let run = RunDir::new(cfg.run_dir.as_deref());
    let hosts = run.known_hosts();
    let mut cx = Cx {
        cfg: &cfg,
        events: &events,
        mem: &mut mem,
        session_hosts: hosts.as_deref(),
    };
    let mut conn = match pool.connect(&chain, &mut cx).await {
        Ok(c) => c,
        Err(e) => {
            let _ = events.send(Event::Failed(e));
            return;
        }
    };
    let _ = events.send(Event::Connected(Arc::clone(&conn)));
    'live: loop {
        tokio::select! {
            () = conn.lost() => {}
            c = ctl.recv() => match c {
                Some(Control::ReconnectNow) => continue,
                Some(Control::Disconnect) | None => {
                    conn.close().await;
                    let _ = events.send(Event::Disconnected);
                    return;
                }
            },
        }
        tracing::info!(target: "net", profile = %conn.profile, "connection lost");
        drop(conn);
        let mut attempt = 0;
        loop {
            attempt += 1;
            if attempt > cfg.tries {
                let _ = events.send(Event::Disconnected);
                // «Переподключить» — новый круг попыток.
                match ctl.recv().await {
                    Some(Control::ReconnectNow) => {
                        attempt = 0;
                        continue;
                    }
                    Some(Control::Disconnect) | None => return,
                }
            }
            let _ = events.send(Event::Lost {
                attempt,
                tries: cfg.tries,
                delay: cfg.interval,
            });
            tokio::select! {
                () = tokio::time::sleep(cfg.interval) => {}
                c = ctl.recv() => match c {
                    Some(Control::ReconnectNow) => {}
                    Some(Control::Disconnect) | None => {
                        let _ = events.send(Event::Disconnected);
                        return;
                    }
                },
            }
            match pool.connect(&chain, &mut cx).await {
                Ok(c) => {
                    conn = c;
                    let _ = events.send(Event::Connected(Arc::clone(&conn)));
                    continue 'live;
                }
                Err(e) if e.is_final() => {
                    let _ = events.send(Event::Failed(e));
                    return;
                }
                Err(e) => tracing::debug!(target: "net", error = %e, attempt, "reconnect failed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, via: &str) -> Profile {
        let mut p = Profile::new(name, format!("{name}.corp"));
        p.via = via.to_owned();
        p
    }

    #[test]
    fn chain_resolves_profiles_and_adhoc() {
        let lookup = |n: &str| {
            ["j1", "j2"]
                .contains(&n)
                .then(|| profile(n, if n == "j2" { "j1" } else { "" }))
        };
        let c = chain(&profile("web", "j2"), lookup).unwrap();
        let names: Vec<_> = c.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["j1", "j2", "web"]);
        let c = chain(&profile("web", "admin@bastion:2222"), |_| None).unwrap();
        assert_eq!(
            (c[0].user.as_str(), c[0].host.as_str(), c[0].port),
            ("admin", "bastion", 2222)
        );
        assert_eq!(
            chain(&profile("web", "nope"), |_| None),
            Err(ConnError::UnknownJump("nope".into()))
        );
    }

    #[test]
    fn chain_is_limited_to_three_jumps() {
        let lookup = |n: &str| {
            let i: u32 = n[1..].parse().unwrap();
            Some(profile(n, &format!("j{}", i + 1)))
        };
        assert_eq!(
            chain(&profile("web", "j1"), lookup),
            Err(ConnError::TooManyJumps)
        );
        // Ровно три jump — можно.
        let lookup = |n: &str| {
            let i: u32 = n[1..].parse().unwrap();
            Some(profile(
                n,
                &if i < 3 {
                    format!("j{}", i + 1)
                } else {
                    String::new()
                },
            ))
        };
        assert_eq!(chain(&profile("web", "j1"), lookup).unwrap().len(), 4);
    }

    #[test]
    fn cfg_from_defaults() {
        let home = Path::new("/home/u");
        let c = ConnCfg::new(&SshCfg::default(), Some(home), Some(Path::new("/state")));
        assert_eq!((c.tries, c.interval), (5, Duration::from_secs(3)));
        assert_eq!(c.keepalive, Some(Duration::from_secs(30)));
        assert_eq!(c.default_keys[0], home.join(".ssh").join("id_ed25519"));
        assert_eq!(
            c.known_hosts,
            [
                home.join(".ssh/known_hosts"),
                home.join(".ssh/known_hosts2")
            ]
        );
        assert_eq!(c.run_dir, Some(PathBuf::from("/state/ssh/run")));
        let mut p = Profile::new("a", "b");
        p.keepalive_s = Some(0);
        assert_eq!(c.keepalive_for(&p), None);
    }

    #[test]
    fn run_dir_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunDir::new(Some(dir.path()));
        let f = run.known_hosts().unwrap();
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "x").unwrap();
        drop(run);
        assert!(!f.exists());
    }
}
