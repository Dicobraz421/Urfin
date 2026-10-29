//! Платформенная часть ops. `unsafe` — только здесь (CLAUDE.md).

#[cfg(windows)]
pub(crate) mod recycle_fast;
/// Клон блоков ReFS (S5-15).
#[cfg(windows)]
pub(crate) mod refs_clone;
/// Копия, перенос и удаление через Shell — эталон «как Проводник» для бенча S5-25.
#[cfg(all(test, windows))]
pub(crate) mod shell_ops;
#[cfg(windows)]
pub(crate) mod trash_win;
#[cfg(windows)]
mod windows;

pub(crate) mod fs;
/// Кто держит файл (№69).
pub(crate) mod rm;

/// Как открыть файл.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// Ассоциация ОС.
    Default,
    /// От имени администратора.
    Elevated,
    /// Диалог «Открыть с помощью».
    With,
    /// Показать в Проводнике (выделить файл).
    Reveal,
}

/// Переименовать без замены существующего (`AlreadyExists`). `case_only` — та же
/// запись другим регистром.
pub fn rename(
    src: &std::path::Path,
    dst: &std::path::Path,
    case_only: bool,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let _ = case_only;
        windows::rename_noreplace(src, dst)
    }
    #[cfg(not(windows))]
    {
        if !case_only && std::fs::symlink_metadata(dst).is_ok() {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        }
        std::fs::rename(src, dst)
    }
}

/// Открыть файл средствами ОС. Вызывать вне UI-потока: `ShellExecute` может ждать.
pub fn open(path: &std::path::Path, how: Open) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        windows::open(path, how)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, how);
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}
