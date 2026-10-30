//! Вызовы ОС (S0 v4 №1, v5 №5: `unsafe` в vfs — только здесь).

#[cfg(windows)]
#[allow(unsafe_code, reason = "WinAPI/NT: листинг каталогов")]
pub(crate) mod windows;

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "WinAPI: метаданные по хэндлу, место, тома (S4-10)"
)]
pub(crate) mod win_meta;

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "WinAPI: ReadDirectoryChangesExW и порт завершения (S4-12)"
)]
pub(crate) mod win_watch;

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "NT: листинг с FileId и открытие относительно родителя (F01-02)"
)]
pub(crate) mod win_du;
