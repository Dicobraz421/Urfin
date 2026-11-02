//! Досчёт с правами администратора (F01-12, ADR-0021): тот же `urfin.exe` с аргументами
//! повышенного режима через UAC — `ShellExecuteExW`, verb `runas`, окно скрыто,
//! `SEE_MASK_NOCLOSEPROCESS` (хэндл процесса — ожидание выхода, код, снятие при
//! отмене). Отказ в окне UAC — `ERROR_CANCELLED`. Вызов блокирует поток задачи до
//! решения пользователя.

#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use urfin_vfs::du::elevated::{Elevated, Elevator, LaunchError};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0};
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

/// Запуск через `runas`.
#[derive(Debug, Default)]
pub struct RunAs;

impl Elevator for RunAs {
    fn launch(&self, args: &[OsString]) -> Result<Box<dyn Elevated>, LaunchError> {
        let exe = std::env::current_exe().map_err(|e| LaunchError::Failed(e.to_string()))?;
        super::shell_open::co_init();
        let file = wide(exe.as_os_str());
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
        Ok(Box::new(Process(process)))
    }
}

/// Повышенный процесс: хэндл от `ShellExecuteExW`.
struct Process(HANDLE);

// SAFETY: хэндл процесса — значение ядра, не привязанное к потоку.
unsafe impl Send for Process {}

impl Elevated for Process {
    fn wait(&mut self, t: Duration) -> Option<u32> {
        let ms = u32::try_from(t.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: хэндл открыт и не закрыт до `Drop`.
        if unsafe { WaitForSingleObject(self.0, ms) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: хэндл открыт; код — на стеке.
        if unsafe { GetExitCodeProcess(self.0, &raw mut code) } == 0 {
            return Some(u32::MAX);
        }
        Some(code)
    }

    fn kill(&mut self) {
        // SAFETY: хэндл открыт. Прав на снятие может не быть — тогда процесс выйдет по
        // знаку отмены (`*.cancel`).
        if unsafe { TerminateProcess(self.0, 1) } == 0 {
            tracing::debug!(error = %io::Error::last_os_error(), "du.elevate: terminate");
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: хэндл открыт `ShellExecuteExW` и закрывается один раз.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{command_line, quote};

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
