//! Внешняя ссылка Markdown-документа (S7 §3.10, решение §13, вопрос 2): Ctrl+клик
//! открывает `http(s)` и `mailto` программой Windows — `ShellExecuteExW` с verb по
//! умолчанию на своём коротком потоке, как Enter по файлу (S6 §7). UI не ждёт; сам Урфин
//! в сеть не ходит.
//!
//! `CoInitializeEx` берётся из `ole32` через `GetProcAddress`: у крейта нет фичи
//! `windows-sys` `Win32_System_Com` (Cargo.toml — полоса deps).

#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const COINIT_APARTMENTTHREADED: u32 = 0x2;
const COINIT_DISABLE_OLE1DDE: u32 = 0x4;

type CoInitializeEx = unsafe extern "system" fn(*const c_void, u32) -> i32;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// STA текущего потока: расширения оболочки и DDE браузеров ждут его.
fn co_init() {
    let dll = wide(OsStr::new("ole32.dll"));
    // SAFETY: строка с нулём живёт до конца вызова; модуль из System32 не выгружается.
    let f = unsafe {
        let m = LoadLibraryExW(
            dll.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if m.is_null() {
            return;
        }
        GetProcAddress(m, c"CoInitializeEx".as_ptr().cast())
    };
    let Some(f) = f else {
        return;
    };
    // SAFETY: сигнатура `CoInitializeEx` из combaseapi.h; указатель — из `GetProcAddress`.
    unsafe {
        let init = std::mem::transmute::<unsafe extern "system" fn() -> isize, CoInitializeEx>(f);
        init(
            std::ptr::null(),
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        );
    }
}

/// `ShellExecuteExW` для `url` в текущем потоке (он должен быть своим и коротким).
pub fn shell_execute(url: &str) -> io::Result<()> {
    co_init();
    let file = wide(OsStr::new(url));
    // SAFETY: структура на стеке, строка с нулём живёт до конца вызова.
    let ok = unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0);
        info.fMask = SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        info.lpFile = file.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        ShellExecuteExW(&raw mut info)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
