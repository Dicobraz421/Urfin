//! Один экземпляр (S1-15, решение владельца №3). Второй запуск передаёт первому
//! `{argv, cwd}` и выходит; первый открывает пути новыми табами и выходит на
//! передний план. `--new-instance` — отдельный экземпляр (отладка).
//!
//! Windows: mutex `Local\urfin-<sid>-<key>` + named pipe
//! `\\.\pipe\urfin-<sid>-<key>`. Unix: `flock` на `state_dir/instance.lock` +
//! unix socket. `key` — хэш `state_dir`: portable-копии — разные экземпляры.
//! Lock держит ОС: умерший процесс его отпускает; старый сокет удаляет новый
//! первый экземпляр.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use urfin_core::{Command, Location, Paths};

/// Запрос больше этого не читается (защита от мусора в канале).
const MAX_REQUEST: u64 = 64 * 1024;
/// Сколько второй экземпляр ждёт канал первого (тот может ещё стартовать).
const CONNECT_WAIT: Duration = Duration::from_secs(2);

/// Что второй запуск передаёт первому.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Аргументы без флагов `--…`: пути.
    pub argv: Vec<String>,
    /// Текущий каталог второго запуска: от него — относительные пути.
    pub cwd: PathBuf,
}

impl Request {
    /// Пути из командной строки (`flags::parse`), от текущего каталога.
    pub fn new(argv: Vec<String>) -> Request {
        Request {
            argv,
            cwd: std::env::current_dir().unwrap_or_default(),
        }
    }

    /// Каждый путь — новый таб в активной панели (`tab.new` + `panel.go`).
    pub fn commands(&self) -> Vec<Command> {
        self.argv
            .iter()
            .flat_map(|a| {
                let p = Path::new(a);
                let path = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    self.cwd.join(p)
                };
                [
                    Command::new("tab.new"),
                    Command::new("panel.go").arg("path", Location::local(path)),
                ]
            })
            .collect()
    }
}

/// Итог старта.
pub enum Start {
    /// Мы первые: принимаем запросы.
    Primary(Inbox),
    /// Запрос отдан работающему экземпляру — выходим.
    Forwarded(Duration),
    /// Экземпляр есть, но не отвечает: работаем отдельно, без приёма.
    Standalone,
}

/// Чем поток приёма будит окно.
type WakerSlot = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

/// Запросы от вторых запусков. Поток приёма будит окно через `set_waker`.
pub struct Inbox {
    rx: Receiver<Request>,
    waker: WakerSlot,
}

impl Inbox {
    /// Вызывается после каждого принятого запроса (из потока приёма).
    pub fn set_waker(&self, f: impl Fn() + Send + 'static) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(f));
    }

    pub fn try_iter(&self) -> impl Iterator<Item = Request> + '_ {
        self.rx.try_iter()
    }

    /// Блокирующее ожидание (для `--ipc-test-server`).
    pub fn recv(&self) -> Option<Request> {
        self.rx.recv().ok()
    }
}

/// Стать первым экземпляром или отдать `req` уже работающему.
pub fn start(paths: &Paths, req: &Request) -> io::Result<Start> {
    let key = key(&paths.state_dir);
    if let Some(lock) = imp::try_primary(paths, &key)? {
        return Ok(Start::Primary(serve(lock, paths, key)?));
    }
    let t = Instant::now();
    let deadline = t + CONNECT_WAIT;
    let body = serde_json::to_vec(req).map_err(io::Error::other)?;
    loop {
        match imp::connect(paths, &key) {
            Ok(mut c) => {
                c.write_all(&body)?;
                c.flush()?;
                return Ok(Start::Forwarded(t.elapsed()));
            }
            Err(e) if Instant::now() < deadline => {
                tracing::debug!(error = %e, "instance not ready, retrying");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                tracing::warn!(error = %e, "running instance does not answer, starting standalone");
                return Ok(Start::Standalone);
            }
        }
    }
}

fn serve(lock: imp::Lock, paths: &Paths, key: String) -> io::Result<Inbox> {
    let (tx, rx) = channel();
    let waker: WakerSlot = Arc::default();
    let w = Arc::clone(&waker);
    let paths = paths.clone();
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || accept_loop(&lock, &paths, &key, &tx, &w))?;
    Ok(Inbox { rx, waker })
}

fn accept_loop(
    lock: &imp::Lock,
    paths: &Paths,
    key: &str,
    tx: &Sender<Request>,
    waker: &Mutex<Option<Box<dyn Fn() + Send>>>,
) {
    loop {
        let mut conn = match imp::accept(lock, paths, key) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "single-instance: accept failed, stop listening");
                return;
            }
        };
        let mut body = Vec::new();
        if let Err(e) = Read::by_ref(&mut conn)
            .take(MAX_REQUEST)
            .read_to_end(&mut body)
        {
            tracing::warn!(error = %e, "single-instance: read failed");
            continue;
        }
        match serde_json::from_slice::<Request>(&body) {
            Ok(req) => {
                tracing::info!(argv = ?req.argv, cwd = %req.cwd.display(), "request from another instance");
                if tx.send(req).is_err() {
                    return;
                }
                if let Some(w) = waker
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                {
                    w();
                }
            }
            Err(e) => tracing::warn!(error = %e, "single-instance: bad request"),
        }
    }
}

/// FNV-1a 64 от `state_dir` (на Windows — без учёта регистра): одинаков во всех
/// процессах и версиях.
fn key(state_dir: &Path) -> String {
    let s = state_dir.to_string_lossy();
    let s = if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    };
    let h = s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{h:016x}")
}

#[cfg(windows)]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::io::OwnedHandle;
    use std::sync::OnceLock;

    use urfin_core::Paths;

    use crate::sys::ipc_win as sys;

    pub struct Lock {
        _mutex: OwnedHandle,
    }

    fn base(key: &str) -> String {
        static SID: OnceLock<String> = OnceLock::new();
        let sid = SID.get_or_init(|| sys::user_sid().unwrap_or_else(|| "user".into()));
        format!("urfin-{sid}-{key}")
    }

    fn pipe_name(key: &str) -> String {
        format!(r"\\.\pipe\{}", base(key))
    }

    pub fn try_primary(_paths: &Paths, key: &str) -> io::Result<Option<Lock>> {
        Ok(sys::create_mutex(&format!(r"Local\{}", base(key)))?.map(|m| Lock { _mutex: m }))
    }

    pub fn accept(_lock: &Lock, _paths: &Paths, key: &str) -> io::Result<File> {
        sys::pipe_accept(&pipe_name(key))
    }

    pub fn connect(_paths: &Paths, key: &str) -> io::Result<File> {
        let f = OpenOptions::new().write(true).open(pipe_name(key))?;
        sys::allow_server_foreground(&f);
        Ok(f)
    }
}

#[cfg(unix)]
mod imp {
    use std::fs::{self, File, OpenOptions};
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use urfin_core::Paths;

    pub struct Lock {
        _file: File,
        listener: UnixListener,
    }

    /// Сокет — в `XDG_RUNTIME_DIR` (только для пользователя), иначе в `state_dir`.
    fn socket(paths: &Paths, key: &str) -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .map_or_else(
                || paths.state_dir.join("instance.sock"),
                |d| d.join(format!("urfin-{key}.sock")),
            )
    }

    pub fn try_primary(paths: &Paths, key: &str) -> io::Result<Option<Lock>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.state_dir.join("instance.lock"))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(None),
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
        // Lock наш — сокет от умершего экземпляра можно удалить.
        let sock = socket(paths, key);
        let _ = fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock)?;
        Ok(Some(Lock {
            _file: file,
            listener,
        }))
    }

    pub fn accept(lock: &Lock, _paths: &Paths, _key: &str) -> io::Result<UnixStream> {
        lock.listener.accept().map(|(s, _)| s)
    }

    pub fn connect(paths: &Paths, key: &str) -> io::Result<UnixStream> {
        UnixStream::connect(socket(paths, key))
    }

    #[cfg(test)]
    pub fn socket_for_tests(paths: &Paths, key: &str) -> PathBuf {
        socket(paths, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths::in_dir(dir)
    }

    #[test]
    fn key_is_stable_and_case_insensitive_on_windows() {
        assert_eq!(key(Path::new("/a/b")), key(Path::new("/a/b")));
        assert_ne!(key(Path::new("/a/b")), key(Path::new("/a/c")));
        if cfg!(windows) {
            assert_eq!(key(Path::new(r"C:\X\State")), key(Path::new(r"c:\x\state")));
        }
    }

    #[test]
    fn request_commands_resolve_relative_paths() {
        let cwd = std::env::temp_dir();
        let abs = cwd.join("abs");
        let req = Request {
            argv: vec!["rel".into(), abs.to_string_lossy().into_owned()],
            cwd: cwd.clone(),
        };
        let c = req.commands();
        assert_eq!(c.len(), 4);
        assert_eq!(c[0], Command::new("tab.new"));
        assert_eq!(
            c[1],
            Command::new("panel.go").arg("path", Location::local(cwd.join("rel")))
        );
        assert_eq!(
            c[3],
            Command::new("panel.go").arg("path", Location::local(abs))
        );
    }

    #[test]
    fn second_start_forwards_to_first_fast() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        p.ensure().unwrap();
        let Start::Primary(inbox) = start(&p, &Request::new(Vec::new())).unwrap() else {
            panic!("first start must be primary");
        };
        let req = Request {
            argv: vec!["one".into()],
            cwd: dir.path().to_path_buf(),
        };
        let Start::Forwarded(took) = start(&p, &req).unwrap() else {
            panic!("second start must forward");
        };
        assert_eq!(inbox.recv(), Some(req));
        assert!(took < Duration::from_millis(100), "S.5: {took:?}");
        // Другой state_dir — другой экземпляр.
        let other = tempfile::tempdir().unwrap();
        let po = paths(other.path());
        po.ensure().unwrap();
        assert!(matches!(
            start(&po, &Request::new(Vec::new())).unwrap(),
            Start::Primary(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn stale_socket_is_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        p.ensure().unwrap();
        // Сокет от «умершего» экземпляра: файл есть, lock никто не держит.
        let sock = imp::socket_for_tests(&p, &key(&p.state_dir));
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        assert!(sock.exists());
        assert!(matches!(
            start(&p, &Request::new(Vec::new())).unwrap(),
            Start::Primary(_)
        ));
    }
}
