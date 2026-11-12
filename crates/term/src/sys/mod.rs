//! Платформенная часть терминала. `unsafe` — только здесь (CLAUDE.md).

#[cfg(windows)]
pub mod conpty;
#[cfg(unix)]
pub mod pty_unix;

/// Бэкенд PTY под общим именем: `ConPTY` на Windows, `pty_unix` на unix (L1-06).
#[cfg(windows)]
pub use conpty as pty;
#[cfg(unix)]
pub use pty_unix as pty;

#[cfg(windows)]
pub mod piped;
#[cfg(windows)]
pub mod procs;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod shell_open;
