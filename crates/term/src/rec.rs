//! Запись сеанса терминала (F03 §3.5, Ctrl+Alt+R): asciicast v2 потоком на диск.
//!
//! Источник — отвод сырого вывода сеанса ([`crate::session::Tap`]): байты PTY до
//! эмулятора и смены размера. Ввод (`"i"`) не пишется — пароли: отвод видит только
//! вывод процесса, а пароль ssh.exe (askpass) идёт мостом Урфина, мимо PTY.
//!
//! Файл пишет свой поток (Runtime, `spawn_dedicated`): буфер 64 КБ, сброс раз в 1 с,
//! `FlushFileBuffers` (`File::sync_all`) при остановке. UI-поток только подписывается
//! на отвод и читает счётчики. Ошибка записи при обязательной записи (политика
//! `ssh.recording = required`) блокирует ввод в сеанс (менеджер).
//!
//! Политика `ssh.recording` касается только SSH-сеансов (F03 §7 №94.3); локальные
//! терминалы пишутся только по Ctrl+Alt+R.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use urfin_core::{Config, PolicySet, PolicySource, SshRecording};

use crate::session::tap::{TapEvent, TapRx};
use crate::sys::conpty::PtySize;

/// Буфер файла записи (F03 §3.5).
pub const BUF: usize = 64 * 1024;
/// Как часто сбрасывать буфер на диск.
pub const FLUSH_EVERY: Duration = Duration::from_secs(1);
/// Шаг проверки отмены (shutdown не ждёт дольше).
const TICK: Duration = Duration::from_millis(200);

/// Папка записей по умолчанию (`[ssh] recordings_dir`, F03 §2.9).
pub const DEFAULT_DIR: &str = r"%LOCALAPPDATA%\Urfin\recordings";

// ---- формат ----

/// Заголовок asciicast v2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub width: u16,
    pub height: u16,
    /// Начало записи, секунды Unix.
    pub timestamp: u64,
    pub term: String,
    pub shell: String,
    /// Профиль SSH или shell.
    pub title: String,
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Строка заголовка (с `\n`).
pub fn header_line(h: &Header) -> String {
    format!(
        "{{\"version\":2,\"width\":{},\"height\":{},\"timestamp\":{},\"env\":{{\"TERM\":{},\"SHELL\":{}}},\"title\":{}}}\n",
        h.width,
        h.height,
        h.timestamp,
        json_str(&h.term),
        json_str(&h.shell),
        json_str(&h.title),
    )
}

/// Событие вывода `[t, "o", data]` (с `\n`); неверный UTF-8 — U+FFFD, как у asciinema.
pub fn output_line(t: Duration, data: &[u8]) -> String {
    format!(
        "[{:.6},\"o\",{}]\n",
        t.as_secs_f64(),
        json_str(&String::from_utf8_lossy(data))
    )
}

/// Смена размера `[t, "r", "WxH"]` (с `\n`).
pub fn resize_line(t: Duration, size: PtySize) -> String {
    format!(
        "[{:.6},\"r\",\"{}x{}\"]\n",
        t.as_secs_f64(),
        size.cols,
        size.rows
    )
}

// ---- настройки ----

/// `[ssh] recording` (F03 §2.9): решает пользователь, если политика `user` или не задана.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UserMode {
    Off,
    #[default]
    Manual,
    Always,
}

/// Правило записи SSH-сеанса: политика сильнее настройки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshRule {
    /// С первого байта, стоп недоступен.
    Required(PolicySource),
    /// Команда выключена.
    Forbidden(PolicySource),
    User(UserMode),
}

/// Действующие настройки записи: политика и `[ssh]` конфига (секцию разбирает
/// `urfin-net`; здесь — только два её ключа записи, дефолты те же).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub policy: Option<(SshRecording, PolicySource)>,
    pub user: UserMode,
    /// Папка как в конфиге (`%LOCALAPPDATA%\…` — для статуса).
    pub dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            policy: None,
            user: UserMode::Manual,
            dir: DEFAULT_DIR.into(),
        }
    }
}

fn ssh_keys(config: &Config) -> (UserMode, &str) {
    let ssh = config.ext.get("ssh").and_then(toml::Value::as_table);
    let user = match ssh
        .and_then(|t| t.get("recording"))
        .and_then(toml::Value::as_str)
    {
        Some("off") => UserMode::Off,
        Some("always") => UserMode::Always,
        _ => UserMode::Manual,
    };
    let dir = ssh
        .and_then(|t| t.get("recordings_dir"))
        .and_then(toml::Value::as_str)
        .filter(|d| !d.trim().is_empty())
        .unwrap_or(DEFAULT_DIR);
    (user, dir)
}

fn policy_of(p: Option<&PolicySet>) -> Option<(SshRecording, PolicySource)> {
    let p = p?;
    let src = p.get(urfin_core::PolicyKey::SshRecording)?.source;
    Some((p.ssh_recording()?, src))
}

impl Settings {
    pub fn read(config: &Config, policies: Option<&PolicySet>) -> Settings {
        let (user, dir) = ssh_keys(config);
        Settings {
            policy: policy_of(policies),
            user,
            dir: dir.to_owned(),
        }
    }

    /// Новые настройки, если конфиг или политика их поменяли; без изменений — `None`
    /// и без аллокаций (зовётся каждый pump).
    pub fn changed(&self, config: &Config, policies: Option<&PolicySet>) -> Option<Settings> {
        let (user, dir) = ssh_keys(config);
        let policy = policy_of(policies);
        if self.user == user && self.policy == policy && self.dir == dir {
            return None;
        }
        Some(Settings {
            policy,
            user,
            dir: dir.to_owned(),
        })
    }

    /// Правило для SSH-сеанса.
    pub fn ssh(&self) -> SshRule {
        match self.policy {
            Some((SshRecording::Required, src)) => SshRule::Required(src),
            Some((SshRecording::Forbidden, src)) => SshRule::Forbidden(src),
            Some((SshRecording::User, _)) | None => SshRule::User(self.user),
        }
    }
}

/// `%VAR%` → значение окружения; неизвестная переменная остаётся как есть.
pub fn expand_env(s: &str, env: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        out.push_str(&rest[..a]);
        let tail = &rest[a + 1..];
        match tail.find('%') {
            Some(b) if b > 0 => {
                let name = &tail[..b];
                if let Some(v) = env(name) {
                    out.push_str(&v);
                } else {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
                rest = &tail[b + 1..];
            }
            _ => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Имя файла записи без папки: `<имя>-YYYY-MM-DD-HHMM[-N].cast`; символы, запрещённые
/// в именах Windows, — `_`.
pub fn file_name(name: &str, stamp: &str, n: u32) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim_matches(['.', ' ']);
    let clean = if clean.is_empty() { "session" } else { clean };
    if n <= 1 {
        format!("{clean}-{stamp}.cast")
    } else {
        format!("{clean}-{stamp}-{n}.cast")
    }
}

// ---- поток записи ----

/// Куда пишет запись: файл в бою, память и «диск полон» в тестах.
pub trait CastFile: Write + Send {
    /// Сброс на носитель (`FlushFileBuffers`).
    fn sync(&mut self) -> io::Result<()>;
}

impl CastFile for std::fs::File {
    fn sync(&mut self) -> io::Result<()> {
        self.sync_all()
    }
}

/// Состояние записи для UI: счётчики пишет поток записи.
#[derive(Debug, Default)]
pub struct RecShared {
    /// Байт записано в файл (с заголовком).
    bytes: AtomicU64,
    failed: AtomicBool,
    done: AtomicBool,
    error: Mutex<Option<String>>,
    /// Путь файла и он же для показа (`%LOCALAPPDATA%\…`), когда файл создан.
    path: Mutex<Option<(PathBuf, String)>>,
}

impl RecShared {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }

    /// Запись не удалась (диск, права): файл неполон.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Поток записи закончил (файл закрыт).
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.error).clone()
    }

    pub fn path(&self) -> Option<PathBuf> {
        lock(&self.path).as_ref().map(|p| p.0.clone())
    }

    /// Путь для статуса: папка как в конфиге.
    pub fn shown(&self) -> Option<String> {
        lock(&self.path).as_ref().map(|p| p.1.clone())
    }

    fn fail(&self, e: &io::Error) {
        tracing::warn!(error = %e, "term: recording failed");
        *lock(&self.error) = Some(e.to_string());
        self.failed.store(true, Ordering::Release);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Открыть файл записи: путь и показ.
pub type Opener = Box<dyn FnOnce() -> io::Result<(Box<dyn CastFile>, PathBuf, String)> + Send>;

/// Что нужно потоку записи.
#[allow(
    missing_debug_implementations,
    reason = "замыкания открытия, отмены и пробуждения"
)]
pub struct Job {
    pub rx: TapRx,
    pub stop: Receiver<()>,
    pub open: Opener,
    pub header: Header,
    /// Момент начала: t = 0.
    pub start: Instant,
    pub shared: std::sync::Arc<RecShared>,
    /// Отмена (shutdown): дописать и закрыть.
    pub cancel: Box<dyn Fn() -> bool + Send>,
    /// Разбудить Core (ошибка записи).
    pub poke: Box<dyn Fn() + Send>,
}

struct Out<'a> {
    w: BufWriter<Box<dyn CastFile>>,
    shared: &'a RecShared,
}

impl Out<'_> {
    fn put(&mut self, line: &str) -> io::Result<()> {
        self.w.write_all(line.as_bytes())?;
        self.shared
            .bytes
            .fetch_add(line.len() as u64, Ordering::AcqRel);
        Ok(())
    }

    fn event(&mut self, ev: TapEvent, start: Instant) -> io::Result<()> {
        match ev {
            TapEvent::Output(at, b) => {
                self.put(&output_line(at.saturating_duration_since(start), &b))
            }
            TapEvent::Resize(at, size) => {
                self.put(&resize_line(at.saturating_duration_since(start), size))
            }
        }
    }
}

/// Поток записи: до остановки, конца сеанса или отмены. Ошибка — флаг в `shared`,
/// дальше не пишет.
pub fn run(job: Job) {
    let Job {
        rx,
        stop,
        open,
        header,
        start,
        shared,
        cancel,
        poke,
    } = job;
    let _span = tracing::info_span!("term.record").entered();
    let (file, path, shown) = match open() {
        Ok(f) => f,
        Err(e) => {
            shared.fail(&e);
            shared.done.store(true, Ordering::Release);
            poke();
            return;
        }
    };
    *lock(&shared.path) = Some((path, shown));
    let mut out = Out {
        w: BufWriter::with_capacity(BUF, file),
        shared: &shared,
    };
    let result = (|| -> io::Result<()> {
        out.put(&header_line(&header))?;
        let mut flush_at = Instant::now() + FLUSH_EVERY;
        loop {
            let wait = flush_at.saturating_duration_since(Instant::now()).min(TICK);
            crossbeam_channel::select! {
                recv(rx) -> ev => match ev {
                    Ok(ev) => out.event(ev, start)?,
                    Err(_) => break, // сеанс закрыт
                },
                recv(stop) -> _ => break,
                default(wait) => {}
            }
            if Instant::now() >= flush_at {
                out.w.flush()?;
                flush_at = Instant::now() + FLUSH_EVERY;
            }
            if cancel() {
                break;
            }
        }
        // Уже прочитанное до остановки — в файл.
        while let Ok(ev) = rx.try_recv() {
            out.event(ev, start)?;
        }
        out.w.flush()?;
        out.w.get_mut().sync()
    })();
    drop(rx);
    if let Err(e) = result {
        shared.fail(&e);
        poke();
    }
    shared.done.store(true, Ordering::Release);
}

/// Файл в папке `dir` (создаётся): имя по профилю и времени, занятое — с `-2`, `-3`…
pub fn open_in(
    dir: &Path,
    shown_dir: &str,
    name: &str,
    stamp: &str,
) -> io::Result<(std::fs::File, PathBuf, String)> {
    std::fs::create_dir_all(dir)?;
    let mut n = 1;
    loop {
        let file = file_name(name, stamp, n);
        let path = dir.join(&file);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => {
                let shown = format!("{}\\{file}", shown_dir.trim_end_matches(['\\', '/']));
                return Ok((f, path, shown));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(e),
        }
    }
}

// ---- запись у слота ----

/// Идущая запись сеанса (у слота менеджера). Drop — остановка: поток дописывает
/// прочитанное, сбрасывает буфер и закрывает файл.
#[derive(Debug)]
pub struct Recording {
    pub shared: std::sync::Arc<RecShared>,
    started: Instant,
    /// Обязательна по политике: источник для чипа «обязательна · политика · TOML».
    pub required: Option<PolicySource>,
    /// Ошибка уже показана (тост или баннер).
    pub(crate) reported: bool,
    /// Время чипа для сцен (`--screens`): не идёт.
    frozen: Option<Duration>,
    _stop: Option<Sender<()>>,
}

impl Recording {
    /// Идёт мм:сс.
    pub fn elapsed(&self) -> Duration {
        self.frozen.unwrap_or_else(|| self.started.elapsed())
    }

    /// Ввод в сеанс заблокирован: обязательная запись не пишется.
    pub fn blocks_input(&self) -> bool {
        self.required.is_some() && self.shared.failed()
    }

    /// Запись, которая не удалась (тесты менеджера).
    #[cfg(test)]
    pub(crate) fn failed_for_test(required: Option<PolicySource>) -> Recording {
        let shared = RecShared::default();
        shared.fail(&io::Error::from(io::ErrorKind::StorageFull));
        Recording {
            shared: std::sync::Arc::new(shared),
            started: Instant::now(),
            required,
            reported: false,
            frozen: None,
            _stop: None,
        }
    }

    /// Запись для сцены `screen-sshrec`: путь, размер и время заданы.
    #[doc(hidden)]
    pub fn demo(
        shown: &str,
        bytes: u64,
        elapsed: Duration,
        required: Option<PolicySource>,
    ) -> Recording {
        let shared = RecShared::default();
        shared.bytes.store(bytes, Ordering::Release);
        *lock(&shared.path) = Some((PathBuf::from(shown), shown.to_owned()));
        Recording {
            shared: std::sync::Arc::new(shared),
            started: Instant::now(),
            required,
            reported: false,
            frozen: Some(elapsed),
            _stop: None,
        }
    }
}

/// Что записывать.
#[derive(Clone, Debug)]
pub struct Plan {
    /// Имя файла и заголовок: профиль SSH или shell.
    pub name: String,
    pub shell: String,
    pub size: PtySize,
    pub required: Option<PolicySource>,
}

/// Начать запись: подписка на отвод — сразу (в UI-потоке, без I/O), файл открывает
/// и пишет поток Runtime.
pub fn start(
    rt: &urfin_core::Runtime,
    tap: &crate::session::Tap,
    settings: &Settings,
    plan: &Plan,
) -> Recording {
    let rx = tap.subscribe();
    let (stop_tx, stop_rx) = bounded(1);
    let shared = std::sync::Arc::new(RecShared::default());
    let started = Instant::now();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let shown_dir = settings.dir.clone();
    let name = plan.name.clone();
    let header = Header {
        width: plan.size.cols,
        height: plan.size.rows,
        timestamp,
        term: "xterm-256color".into(),
        shell: plan.shell.clone(),
        title: plan.name.clone(),
    };
    let open: Opener = Box::new(move || {
        let dir = PathBuf::from(expand_env(&shown_dir, |v| std::env::var(v).ok()));
        let stamp = urfin_i18n::fmt::file_stamp(i64::try_from(timestamp).unwrap_or(0));
        let (f, path, shown) = open_in(&dir, &shown_dir, &name, &stamp)?;
        Ok((Box::new(f) as Box<dyn CastFile>, path, shown))
    });
    let s2 = shared.clone();
    rt.spawn_dedicated("term-rec".into(), rt.token(), move |t| {
        let cancel = t.cancel.clone();
        let sink = t.sink.clone();
        run(Job {
            rx,
            stop: stop_rx,
            open,
            header,
            start: started,
            shared: s2,
            cancel: Box::new(move || cancel.is_cancelled()),
            poke: Box::new(move || sink.poke()),
        });
    });
    Recording {
        shared,
        started,
        required: plan.required,
        reported: false,
        frozen: None,
        _stop: Some(stop_tx),
    }
}

#[cfg(test)]
#[path = "rec_tests.rs"]
mod tests;
