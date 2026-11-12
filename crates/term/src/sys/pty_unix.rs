//! PTY для unix (L1-06) за тем же API, что `conpty`.
//!
//! Пока — только общие типы и замеры: сам запуск (`openpty`, `setsid` + `TIOCSCTTY`,
//! `TIOCSWINSZ`, сигнал группе процессов) требует фич `rustix` (`pty`, `termios`,
//! `process`, `stdio`), которых у крейта нет, — карточка полосы `deps`. До неё
//! [`spawn`] отвечает `Unsupported`, а [`PtyControl`] необитаем: сеансов не бывает.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Размер терминала.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

/// Что запустить.
#[derive(Clone, Debug, Default)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Добавить к окружению Urfin (или заменить одноимённые).
    pub env: Vec<(OsString, OsString)>,
}

/// Запущенный процесс в PTY: вывод, ввод и управление.
#[derive(Debug)]
pub struct Pty {
    /// Ведущий конец: читать в отдельном потоке до EOF.
    pub reader: File,
    /// Ведущий конец: запись.
    pub writer: File,
    pub control: Arc<PtyControl>,
}

/// Управление PTY и процессом. Необитаем, пока нет запуска (см. шапку модуля).
#[derive(Debug)]
pub enum PtyControl {}

/// `[term] conpty` — настройка Windows; на unix значение не используется.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Bundled,
    System,
}

impl Mode {
    /// Значение конфига; неизвестное — `auto`.
    pub fn parse(s: &str) -> Mode {
        match s {
            "bundled" => Mode::Bundled,
            "system" => Mode::System,
            _ => Mode::Auto,
        }
    }
}

/// На unix выбора бэкенда нет.
pub fn set_mode(_mode: Mode) {}

/// Своего `ConPTY` на unix нет.
pub fn bundled_available() -> bool {
    false
}

/// Запустить процесс в PTY. Пока не реализовано: `Unsupported`.
pub fn spawn(spec: &SpawnSpec, size: PtySize) -> io::Result<Pty> {
    tracing::warn!(program = %spec.program.display(), ?size, "unix PTY is not implemented");
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "unix PTY backend is not implemented",
    ))
}

impl PtyControl {
    pub fn pid(&self) -> u32 {
        match *self {}
    }

    /// Новый размер терминала.
    pub fn resize(&self, _size: PtySize) -> io::Result<()> {
        match *self {}
    }

    /// Закрыть PTY: reader дочитает вывод до EOF.
    pub fn close(&self) {
        match *self {}
    }

    /// Процессов в сеансе.
    pub fn active_processes(&self) -> u32 {
        match *self {}
    }

    /// Занят: кроме самого shell есть ещё процессы.
    pub fn busy(&self) -> bool {
        match *self {}
    }

    /// Убить процесс и всё его дерево.
    pub fn kill_tree(&self) -> io::Result<()> {
        match *self {}
    }

    /// Ждать выхода процесса; `None` — ещё работает (таймаут).
    pub fn wait(&self, _timeout: Option<Duration>) -> Option<u32> {
        match *self {}
    }

    /// Колбэк при выходе процесса. Отписка — drop результата.
    pub fn on_exit(
        self: &Arc<Self>,
        _f: impl FnOnce(u32) + Send + 'static,
    ) -> io::Result<ExitWatch> {
        match **self {}
    }
}

/// Подписка на выход процесса; drop — отписка.
#[derive(Debug)]
pub struct ExitWatch(());

/// Тиков `utime`/`stime` в секунде: `USER_HZ` в Linux — 100 на всех архитектурах.
const USER_HZ: u64 = 100;

/// Процессорное время текущего процесса, user + kernel (`/proc/self/stat`); 0 — ошибка.
pub fn cpu_time() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    Duration::from_millis(stat_ticks(&stat) * 1000 / USER_HZ)
}

/// `utime + stime` из строки `/proc/<pid>/stat`: поля 14 и 15; имя процесса (поле 2)
/// в скобках и может содержать пробелы — счёт от последней `)`.
fn stat_ticks(stat: &str) -> u64 {
    let rest = stat.rsplit_once(')').map_or("", |(_, r)| r);
    // После `)` идёт поле 3 (состояние): utime и stime — 12-е и 13-е отсюда.
    rest.split_ascii_whitespace()
        .skip(11)
        .take(2)
        .filter_map(|t| t.parse::<u64>().ok())
        .sum()
}

/// Анонимная память текущего процесса (`RssAnon` из `/proc/self/status`); 0 — ошибка.
pub fn private_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status_kb(&status, "RssAnon:") * 1024
}

fn status_kb(status: &str, key: &str) -> u64 {
    status
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|v| v.split_ascii_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_is_unsupported() {
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            ..SpawnSpec::default()
        };
        let err = spawn(&spec, PtySize { cols: 80, rows: 24 }).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn stat_ticks_skips_comm_with_spaces() {
        let stat = "42 (a b) c) S 1 42 42 0 -1 4194304 100 0 0 0 7 5 0 0 20 0 1 0 1 2 3";
        assert_eq!(stat_ticks(stat), 12);
        assert_eq!(stat_ticks(""), 0);
    }

    #[test]
    fn status_kb_reads_field() {
        assert_eq!(
            status_kb("VmRSS:\t 10 kB\nRssAnon:\t  2048 kB\n", "RssAnon:"),
            2048
        );
        assert_eq!(status_kb("VmRSS: 10 kB\n", "RssAnon:"), 0);
    }

    #[test]
    fn own_counters_are_readable() {
        assert!(private_bytes() > 0);
        let _ = cpu_time();
    }
}
