//! Досчёт с правами администратора (F01-12, ADR-0021): тот же `urfin.exe` с аргументами
//! повышенного режима через UAC — `ShellExecuteExW`, verb `runas`, окно скрыто,
//! `SEE_MASK_NOCLOSEPROCESS` (хэндл процесса — ожидание выхода, код, снятие при
//! отмене). Отказ в окне UAC — `ERROR_CANCELLED`. Вызов блокирует поток задачи до
//! решения пользователя.
//!
//! Подмена файлов (ADR-0021 «Подмена файлов»): до `runas` свой exe (путь
//! `GetModuleFileNameW`) и все `*.dll` в его каталоге открываются без
//! `FILE_SHARE_WRITE | FILE_SHARE_DELETE` ([`ImageLock`]) и держатся до выхода
//! повышенного процесса; запускается путь из хэндла (`GetFinalPathNameByHandleW`). Пока
//! файл открыт, NTFS не даёт переименовать и каталоги над ним. Повышенный процесс первым
//! делом ограничивает поиск DLL каталогом System32 ([`dll_search_system32`]).

#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

use urfin_vfs::du::elevated::{Elevated, Elevator, LaunchError};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, ERROR_SHARING_VIOLATION, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_NAME_NORMALIZED, FILE_SHARE_READ, GetFileInformationByHandle,
    GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories, SetDllDirectoryW,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Аргумент командной строки по правилам `CommandLineToArgvW`: без пробелов и кавычек —
/// как есть; иначе в кавычках, обратные косые перед кавычкой удваиваются.
pub fn quote(arg: &OsStr) -> Vec<u16> {
    let w: Vec<u16> = arg.encode_wide().collect();
    let plain = !w.is_empty()
        && !w
            .iter()
            .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
    if plain {
        return w;
    }
    let (bs, q) = (u16::from(b'\\'), u16::from(b'"'));
    let mut out = vec![q];
    let mut slashes = 0usize;
    for c in w {
        if c == bs {
            slashes += 1;
            continue;
        }
        let n = if c == q { slashes * 2 + 1 } else { slashes };
        out.extend(std::iter::repeat_n(bs, n));
        out.push(c);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n(bs, slashes * 2));
    out.push(q);
    out
}

/// Командная строка из аргументов (без имени exe).
pub fn command_line(args: &[OsString]) -> Vec<u16> {
    let mut out = Vec::new();
    for (k, a) in args.iter().enumerate() {
        if k > 0 {
            out.push(u16::from(b' '));
        }
        out.extend(quote(a));
    }
    out
}

/// Повышенный режим (`--du-elevated`), первым делом: DLL грузятся только из System32 —
/// ни из каталога exe, ни из текущего, ни из `PATH`. Ошибка — режим не продолжает.
pub fn dll_search_system32() -> io::Result<()> {
    // SAFETY: флаги — константа; вызов меняет только порядок поиска DLL процесса.
    if unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let empty = [0u16];
    // SAFETY: пустая строка с нулём живёт до конца вызова; убирает текущий каталог из
    // поиска `LoadLibrary` без флагов.
    if unsafe { SetDllDirectoryW(empty.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Файл держит другой на запись или удаление — открыть без общего доступа на запись
/// нельзя (`ERROR_SHARING_VIOLATION`).
pub fn is_locked_by_other(e: &io::Error) -> bool {
    e.raw_os_error()
        .and_then(|c| u32::try_from(c).ok())
        .is_some_and(|c| c == ERROR_SHARING_VIOLATION)
}

/// Открыть на чтение; другим — только чтение: ни записать, ни переименовать, ни удалить.
fn open_locked(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

/// Итоговый путь открытого файла: ссылки раскрыты, регистр и длинные имена — как на
/// диске; `\\?\C:\…` → `C:\…`, `\\?\UNC\…` → `\\…`.
fn final_path(f: &File) -> io::Result<PathBuf> {
    let mut buf = vec![0u16; 512];
    loop {
        let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: хэндл открыт `f`; буфер на `cap` символов живёт до конца вызова.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                f.as_raw_handle(),
                buf.as_mut_ptr(),
                cap,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        if n < buf.len() {
            buf.truncate(n);
            break;
        }
        buf.resize(n + 1, 0);
    }
    let s = OsString::from_wide(&buf).to_string_lossy().into_owned();
    Ok(PathBuf::from(
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            rest.to_owned()
        } else {
            s
        },
    ))
}

/// Том и номер файла — один ли это файл.
fn file_id(f: &File) -> io::Result<(u32, u64)> {
    // SAFETY: структура из чисел, нули допустимы.
    let mut i: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: хэндл открыт `f`; структура на стеке.
    if unsafe { GetFileInformationByHandle(f.as_raw_handle(), &raw mut i) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        i.dwVolumeSerialNumber,
        (u64::from(i.nFileIndexHigh) << 32) | u64::from(i.nFileIndexLow),
    ))
}

/// Exe и `*.dll` его каталога, открытые без права записи и удаления для других: пока
/// жив, их не переписать, не переименовать и не удалить (каталоги над ними — тоже).
#[derive(Debug)]
pub struct ImageLock {
    exe: PathBuf,
    files: Vec<File>,
}

impl ImageLock {
    /// Открыть `exe` и `*.dll` рядом. Кто-то держит файл на запись — ошибка
    /// [`is_locked_by_other`].
    pub fn acquire(exe: &Path) -> io::Result<ImageLock> {
        let f = open_locked(exe)?;
        let path = final_path(&f)?;
        // Путь запуска ведёт к тому же файлу (том и номер файла).
        if file_id(&File::open(&path)?)? != file_id(&f)? {
            return Err(io::Error::other("exe path from handle leads elsewhere"));
        }
        let mut files = vec![f];
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("exe without directory"))?;
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let dll = Path::new(&e.file_name())
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("dll"));
            if dll && !e.file_type()?.is_dir() {
                files.push(open_locked(&e.path())?);
            }
        }
        Ok(ImageLock { exe: path, files })
    }

    /// Путь exe из хэндла — по нему и запускать.
    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Сколько файлов держит: exe и DLL.
    pub fn count(&self) -> usize {
        self.files.len()
    }
}

/// Запуск через `runas`.
#[derive(Debug, Default)]
pub struct RunAs;

impl Elevator for RunAs {
    fn launch(&self, args: &[OsString]) -> Result<Box<dyn Elevated>, LaunchError> {
        // `current_exe` — `GetModuleFileNameW(NULL)`; запуск — по пути из хэндла.
        let exe = std::env::current_exe().map_err(|e| LaunchError::Failed(e.to_string()))?;
        let lock = ImageLock::acquire(&exe).map_err(|e| {
            tracing::error!(error = %e, "du.elevate: lock exe and dlls");
            LaunchError::Failed(if is_locked_by_other(&e) {
                urfin_i18n::current().get("du-elevate-locked")
            } else {
                e.to_string()
            })
        })?;
        super::shell_open::co_init();
        let file = wide(lock.exe().as_os_str());
        let mut params = command_line(args);
        params.push(0);
        let verb = wide(OsStr::new("runas"));
        // SAFETY: структура на стеке, строки с нулём живут до конца вызова.
        let (ok, process) = unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0);
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_HIDE;
            let ok = ShellExecuteExW(&raw mut info);
            (ok, info.hProcess)
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(c) if u32::try_from(c).is_ok_and(|c| c == ERROR_CANCELLED) => {
                    LaunchError::Declined
                }
                _ => LaunchError::Failed(e.to_string()),
            });
        }
        if process.is_null() {
            return Err(LaunchError::Failed("no process handle".into()));
        }
        Ok(Box::new(Process {
            handle: process,
            _lock: lock,
        }))
    }
}

/// Повышенный процесс: хэндл от `ShellExecuteExW`; exe и DLL рядом держатся открытыми
/// до его выхода или снятия.
struct Process {
    handle: HANDLE,
    _lock: ImageLock,
}

// SAFETY: хэндл процесса — значение ядра, не привязанное к потоку.
unsafe impl Send for Process {}

impl Elevated for Process {
    fn wait(&mut self, t: Duration) -> Option<u32> {
        let ms = u32::try_from(t.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: хэндл открыт и не закрыт до `Drop`.
        if unsafe { WaitForSingleObject(self.handle, ms) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: хэндл открыт; код — на стеке.
        if unsafe { GetExitCodeProcess(self.handle, &raw mut code) } == 0 {
            return Some(u32::MAX);
        }
        Some(code)
    }

    fn kill(&mut self) {
        // SAFETY: хэндл открыт. Прав на снятие может не быть — тогда процесс выйдет по
        // знаку отмены (`*.cancel`).
        if unsafe { TerminateProcess(self.handle, 1) } == 0 {
            tracing::debug!(error = %io::Error::last_os_error(), "du.elevate: terminate");
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: хэндл открыт `ShellExecuteExW` и закрывается один раз.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{ImageLock, command_line, dll_search_system32, is_locked_by_other, quote};

    /// Каталог `app\` с `urfin.exe`, двумя DLL (одна — в верхнем регистре), текстом и
    /// подкаталогом `x.dll\`.
    fn portable() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let app = t.path().join("app");
        std::fs::create_dir_all(app.join("x.dll")).unwrap();
        for n in ["urfin.exe", "conpty.dll", "EXTRA.DLL", "notes.txt"] {
            std::fs::write(app.join(n), b"MZ").unwrap();
        }
        (t, app)
    }

    fn write(p: &Path) -> io::Result<File> {
        OpenOptions::new().write(true).open(p)
    }

    fn sharing(r: io::Result<impl Sized>) -> bool {
        r.err().is_some_and(|e| is_locked_by_other(&e))
    }

    #[test]
    fn lock_blocks_rewrite_rename_and_delete() {
        let (t, app) = portable();
        let lock = ImageLock::acquire(&app.join("urfin.exe")).unwrap();
        assert_eq!(lock.count(), 3, "exe + 2 dll");
        for n in ["urfin.exe", "conpty.dll", "EXTRA.DLL"] {
            let p = app.join(n);
            assert!(sharing(write(&p)), "{n}: write");
            assert!(
                std::fs::rename(&p, app.join("moved")).is_err(),
                "{n}: rename"
            );
            assert!(std::fs::remove_file(&p).is_err(), "{n}: delete");
            // Читать можно: так открывает exe и DLL загрузчик повышенного процесса.
            assert!(File::open(&p).is_ok(), "{n}: read");
        }
        // Каталог exe и каталог над ним не переименовать, пока файлы открыты (NTFS).
        assert!(std::fs::rename(&app, t.path().join("app2")).is_err());
        assert!(write(&app.join("notes.txt")).is_ok());
        drop(lock);
        assert!(write(&app.join("urfin.exe")).is_ok());
        assert!(std::fs::rename(&app, t.path().join("app2")).is_ok());
    }

    #[test]
    fn held_for_write_refuses() {
        let (_t, app) = portable();
        let w = write(&app.join("conpty.dll")).unwrap();
        let e = ImageLock::acquire(&app.join("urfin.exe")).unwrap_err();
        assert!(is_locked_by_other(&e), "{e}");
        drop(w);
        let _w = write(&app.join("urfin.exe")).unwrap();
        let e = ImageLock::acquire(&app.join("urfin.exe")).unwrap_err();
        assert!(is_locked_by_other(&e), "{e}");
    }

    #[test]
    fn launch_path_comes_from_handle() {
        let (_t, app) = portable();
        // Путь с `.`, `..` и другим регистром — запуск по итоговому пути файла.
        let odd = app.join(".").join("x.dll").join("..").join("URFIN.EXE");
        let lock = ImageLock::acquire(&odd).unwrap();
        let real = std::fs::canonicalize(app.join("urfin.exe")).unwrap();
        let real = real.to_string_lossy();
        let want = real.strip_prefix(r"\\?\").unwrap_or(&real);
        assert_eq!(lock.exe(), Path::new(want));
        assert!(lock.exe().ends_with("urfin.exe"), "{:?}", lock.exe());
        assert!(!lock.exe().to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn elevated_dll_search_is_system32_only() {
        // Возвращаемые значения `SetDefaultDllDirectories` и `SetDllDirectoryW` (тест —
        // в своём процессе nextest); повтор тоже успешен.
        dll_search_system32().unwrap();
        dll_search_system32().unwrap();
    }

    fn s(w: &[u16]) -> String {
        String::from_utf16(w).unwrap()
    }

    #[test]
    fn quoting_like_command_line_to_argv() {
        assert_eq!(s(&quote("C:\\".as_ref())), "C:\\");
        assert_eq!(s(&quote("C:\\Users\\a b".as_ref())), "\"C:\\Users\\a b\"");
        // Хвостовые `\` в кавычках удваиваются: `"C:\a b\\"` → `C:\a b\`.
        assert_eq!(s(&quote("C:\\a b\\".as_ref())), "\"C:\\a b\\\\\"");
        assert_eq!(s(&quote("".as_ref())), "\"\"");
        assert_eq!(s(&quote("a\"b".as_ref())), "\"a\\\"b\"");
        let args: Vec<OsString> = ["--du-elevated", "--out", "C:\\x y\\f", "D:\\"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(
            s(&command_line(&args)),
            "--du-elevated --out \"C:\\x y\\f\" D:\\"
        );
    }
}
