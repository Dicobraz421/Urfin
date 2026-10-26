//! Платформенная часть терминала. `unsafe` — только здесь (CLAUDE.md).

#[cfg(windows)]
pub mod conpty;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod shell_open;
