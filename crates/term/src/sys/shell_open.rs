//! `ShellExecuteExW` для Enter по файлу (S6-19, S6 §7): ассоциация ОС, «Открыть с
//! помощью», от администратора, новое окно консоли.
//!
//! Вызов — на своём коротком потоке с `CoInitializeEx(APARTMENTTHREADED |
//! DISABLE_OLE1DDE)`: поток живёт ровно один вызов, и зависание сетевого пути или
//! расширения оболочки не трогает пулы Runtime и UI.
//!
//! `shell32`/`ole32` берутся через `LoadLibraryExW` из System32 и `GetProcAddress`:
//! у крейта нет фич `windows-sys` для `UI_Shell` и `System_Com` (Cargo.toml — полоса
//! deps), а структура `SHELLEXECUTEINFOW` описана здесь же (x64/arm64, без упаковки).

#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

/// `SEE_MASK_NOASYNC`: вызов не возвращается, пока DDE/COM не отработали (поток всё
/// равно наш и короткий).
const SEE_MASK_NOASYNC: u32 = 0x0000_0100;
/// Без окон ошибок оболочки (тесты).
const SEE_MASK_FLAG_NO_UI: u32 = 0x0000_0400;
const SW_SHOWNORMAL: i32 = 1;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const COINIT_DISABLE_OLE1DDE: u32 = 0x4;

/// `SHELLEXECUTEINFOW` (shellapi.h) для 64-битных Windows.
#[repr(C)]
struct ShellExecuteInfoW {
    cb_size: u32,
    f_mask: u32,
    hwnd: *mut c_void,
    lp_verb: *const u16,
    lp_file: *const u16,
    lp_parameters: *const u16,
    lp_directory: *const u16,
    n_show: i32,
    h_inst_app: *mut c_void,
    lp_id_list: *mut c_void,
    lp_class: *const u16,
    hkey_class: *mut c_void,
    dw_hot_key: u32,
    h_icon_or_monitor: *mut c_void,
    h_process: *mut c_void,
}

type ShellExecuteExW = unsafe extern "system" fn(*mut ShellExecuteInfoW) -> i32;
type CoInitializeEx = unsafe extern "system" fn(*const c_void, u32) -> i32;
type CoUninitialize = unsafe extern "system" fn();

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Функция `name` (C-строка) из системной DLL `dll`.
fn proc(dll: &str, name: &std::ffi::CStr) -> Option<unsafe extern "system" fn() -> isize> {
    let d = wide(OsStr::new(dll));
    // SAFETY: строки с нулём живут до конца вызова; модуль из System32 не
    // выгружается (ссылка остаётся до конца процесса — функция нужна повторно).
    unsafe {
        let m = LoadLibraryExW(
            d.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if m.is_null() {
            return None;
        }
        GetProcAddress(m, name.as_ptr().cast())
    }
}

/// Вызвать `ShellExecuteExW` в текущем потоке (он должен быть своим и коротким).
pub fn shell_execute(spec: &crate::open::Spec, no_ui: bool) -> io::Result<()> {
    let missing = || io::Error::new(io::ErrorKind::Unsupported, "shell32!ShellExecuteExW");
    let exec = proc("shell32.dll", c"ShellExecuteExW").ok_or_else(missing)?;
    // SAFETY: сигнатура `BOOL ShellExecuteExW(SHELLEXECUTEINFOW*)`.
    let exec: ShellExecuteExW = unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, ShellExecuteExW>(exec)
    };
    let init = proc("ole32.dll", c"CoInitializeEx").map(|f| {
        // SAFETY: сигнатура `HRESULT CoInitializeEx(LPVOID, DWORD)`.
        unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, CoInitializeEx>(f) }
    });
    let uninit = proc("ole32.dll", c"CoUninitialize").map(|f| {
        // SAFETY: сигнатура `void CoUninitialize(void)`.
        unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, CoUninitialize>(f) }
    });
    // SAFETY: инициализация COM текущего потока без указателей.
    let com = init.is_some_and(|f| unsafe {
        f(
            std::ptr::null(),
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        )
    } >= 0);
    let verb = spec.verb.map(|v| wide(OsStr::new(v)));
    let file = wide(spec.file.as_os_str());
    let params = spec.params.as_deref().map(wide);
    let dir = spec.dir.as_deref().map(|d| wide(d.as_os_str()));
    let mut info = ShellExecuteInfoW {
        cb_size: u32::try_from(size_of::<ShellExecuteInfoW>()).unwrap_or(0),
        f_mask: SEE_MASK_NOASYNC | if no_ui { SEE_MASK_FLAG_NO_UI } else { 0 },
        hwnd: std::ptr::null_mut(),
        lp_verb: verb.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
        lp_file: file.as_ptr(),
        lp_parameters: params.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
        lp_directory: dir.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
        n_show: SW_SHOWNORMAL,
        h_inst_app: std::ptr::null_mut(),
        lp_id_list: std::ptr::null_mut(),
        lp_class: std::ptr::null(),
        hkey_class: std::ptr::null_mut(),
        dw_hot_key: 0,
        h_icon_or_monitor: std::ptr::null_mut(),
        h_process: std::ptr::null_mut(),
    };
    // SAFETY: структура на стеке, строки живут до конца вызова.
    let ok = unsafe { exec(&raw mut info) } != 0;
    let err = (!ok).then(io::Error::last_os_error);
    if com && let Some(f) = uninit {
        // SAFETY: парный к успешному `CoInitializeEx` на этом же потоке.
        unsafe { f() };
    }
    err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn struct_layout_matches_shellapi() {
        assert_eq!(size_of::<ShellExecuteInfoW>(), 112);
        assert_eq!(std::mem::offset_of!(ShellExecuteInfoW, n_show), 48);
        assert_eq!(std::mem::offset_of!(ShellExecuteInfoW, h_process), 104);
    }

    #[test]
    fn functions_resolve() {
        assert!(proc("shell32.dll", c"ShellExecuteExW").is_some());
        assert!(proc("ole32.dll", c"CoInitializeEx").is_some());
        assert!(proc("shell32.dll", c"NoSuchExport").is_none());
    }

    /// Нет файла — ошибка ОС (без окна: `no_ui`).
    #[test]
    fn missing_file_is_error() {
        let spec = crate::open::Spec {
            verb: Some("open"),
            file: std::env::temp_dir()
                .join("urfin-no-such-dir-s6-19")
                .join("x.urfin-none"),
            params: None,
            dir: None,
        };
        let r = std::thread::spawn(move || shell_execute(&spec, true))
            .join()
            .unwrap();
        assert!(r.is_err());
    }
}
