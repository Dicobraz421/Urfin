//! Канал SFTP поверх поднятого соединения (F03 §2.3: листинг — 1 канал) и синхронный
//! вызов из потоков Runtime: запрос уходит задачей в рантайм крейта, поток ждёт ответ.
//! UI-поток сюда не ходит.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::StatusCode;
use urfin_vfs::VfsError;

use crate::conn::Conn;

/// Rename поверх существующего (OpenSSH): атомарная замена при закачке (F03 §4.2).
pub const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// Ответ сервера дольше этого — ошибка запроса (соединение решает обрыв само).
pub const REQUEST_TIMEOUT_S: u64 = 30;

/// Открытый SFTP-канал профиля.
pub struct Session {
    pub(super) raw: RawSftpSession,
    rt: tokio::runtime::Handle,
    conn: Arc<Conn>,
    /// RTT последнего запроса листинга, мс; `u32::MAX` — ещё не было.
    list_rtt_ms: AtomicU32,
    /// Сервер объявил `posix-rename@openssh.com` (rename поверх, атомарно).
    pub(super) posix_rename: bool,
    /// Сервер объявил `fsync@openssh.com`.
    pub(super) fsync: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("profile", &self.conn.profile)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Открывает подсистему `sftp` на `conn`. Зовётся внутри рантайма крейта: его ручка
    /// запоминается для синхронных вызовов.
    pub async fn open(conn: Arc<Conn>) -> Result<Arc<Session>, VfsError> {
        let at = || PathBuf::from(format!("sftp://{}", conn.profile));
        let ch = conn
            .handle()
            .channel_open_session()
            .await
            .map_err(|e| io(at(), std::io::ErrorKind::NotConnected, e.to_string()))?;
        ch.request_subsystem(true, "sftp")
            .await
            .map_err(|e| io(at(), std::io::ErrorKind::Unsupported, e.to_string()))?;
        let raw = RawSftpSession::new(ch.into_stream());
        raw.set_timeout(REQUEST_TIMEOUT_S);
        let v = raw.init().await.map_err(|e| map_err(&at(), e))?;
        let has = |ext: &str| v.extensions.contains_key(ext);
        let (posix_rename, fsync) = (has(POSIX_RENAME), has(russh_sftp::extensions::FSYNC));
        tracing::debug!(target: "net", posix_rename, fsync, "sftp extensions");
        Ok(Arc::new(Session {
            raw,
            rt: tokio::runtime::Handle::current(),
            conn,
            list_rtt_ms: AtomicU32::new(u32::MAX),
            posix_rename,
            fsync,
        }))
    }

    pub fn profile(&self) -> &str {
        &self.conn.profile
    }

    pub fn is_lost(&self) -> bool {
        self.conn.is_lost()
    }

    /// Пинг заголовка панели: RTT последнего листинга, иначе — keep-alive.
    pub fn rtt(&self) -> Option<Duration> {
        match self.list_rtt_ms.load(Ordering::Relaxed) {
            u32::MAX => self.conn.rtt(),
            ms => Some(Duration::from_millis(ms.into())),
        }
    }

    pub(super) fn note_rtt(&self, d: Duration) {
        let ms = u32::try_from(d.as_millis()).unwrap_or(u32::MAX - 1);
        self.list_rtt_ms.store(ms, Ordering::Relaxed);
    }

    /// Выполнить `f` в рантайме крейта и дождаться. Только не из потока рантайма.
    pub(super) fn block<T, F>(
        self: &Arc<Self>,
        f: impl FnOnce(Arc<Session>) -> F,
    ) -> Result<T, VfsError>
    where
        F: Future<Output = Result<T, VfsError>> + Send + 'static,
        T: Send + 'static,
    {
        if self.is_lost() {
            return Err(not_connected(self.profile()));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let fut = f(Arc::clone(self));
        drop(self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        }));
        // Рантайм остановлен — задача ушла вместе с отправителем.
        rx.blocking_recv()
            .unwrap_or_else(|_| Err(not_connected(self.profile())))
    }

    /// Запустить `f` в рантайме крейта, не дожидаясь: ответ — в приёмнике (конвейер
    /// записи и чтения).
    pub(super) fn start<T, F>(
        self: &Arc<Self>,
        f: impl FnOnce(Arc<Session>) -> F,
    ) -> tokio::sync::oneshot::Receiver<Result<T, VfsError>>
    where
        F: Future<Output = Result<T, VfsError>> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let fut = f(Arc::clone(self));
        drop(self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        }));
        rx
    }

    /// Дождаться ответа [`Session::start`]. Только не из потока рантайма.
    pub(super) fn wait<T>(
        &self,
        rx: tokio::sync::oneshot::Receiver<Result<T, VfsError>>,
    ) -> Result<T, VfsError> {
        rx.blocking_recv()
            .unwrap_or_else(|_| Err(not_connected(self.profile())))
    }

    /// Не ждать ответа (закрытие хэндлов в `Drop`).
    pub(super) fn detach(self: &Arc<Self>, f: impl Future<Output = ()> + Send + 'static) {
        drop(self.rt.spawn(f));
    }
}

/// Профиль не подключён: таб из сессии сам не подключается (F03 §2.6).
pub fn not_connected(profile: &str) -> VfsError {
    io(
        PathBuf::from(format!("sftp://{profile}")),
        std::io::ErrorKind::NotConnected,
        "not connected".to_owned(),
    )
}

fn io(path: PathBuf, kind: std::io::ErrorKind, msg: String) -> VfsError {
    VfsError::Io {
        path,
        source: std::io::Error::new(kind, msg),
    }
}

/// Ошибка SFTP → `VfsError` для `path`.
pub(super) fn map_err(path: &std::path::Path, e: SftpError) -> VfsError {
    use std::io::ErrorKind as K;
    let path = path.to_path_buf();
    match e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => VfsError::NotFound(path),
            StatusCode::PermissionDenied => VfsError::Denied(path),
            StatusCode::OpUnsupported => VfsError::Unsupported("sftp operation"),
            StatusCode::NoConnection | StatusCode::ConnectionLost => {
                io(path, K::NotConnected, s.error_message)
            }
            _ => io(
                path,
                K::Other,
                format!("{}: {}", s.status_code, s.error_message),
            ),
        },
        SftpError::Timeout => io(path, K::TimedOut, "timeout".to_owned()),
        e => io(path, K::Other, e.to_string()),
    }
}

/// `Eof` у `READ`/`READDIR` — не ошибка, а конец.
pub(super) fn is_eof(e: &SftpError) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == StatusCode::Eof)
}
