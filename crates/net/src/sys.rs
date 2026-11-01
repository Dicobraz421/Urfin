//! Платформенная часть `urfin-net`. `unsafe` — только здесь (CLAUDE.md).
//! Pipe моста к `ssh.exe` с DACL «только владелец» (F03 §2.7) и перечисление целей
//! Диспетчера учётных данных (F03 §2.5, `keyring` этого не умеет).

#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt as _;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Credentials::{CREDENTIALW, CredEnumerateW, CredFree};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

/// Только владелец pipe (текущий пользователь): полный доступ; остальным — ничего.
const OWNER_ONLY: &str = "D:P(A;;GA;;;OW)";

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// Экземпляр pipe `name` с DACL «только владелец», без удалённых клиентов. `first` —
/// первый экземпляр: имя не должно быть занято (защита от подмены).
pub(crate) fn pipe_server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sddl = wide(OWNER_ONLY);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: строка с нулём в конце, `sd` — выход.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut sd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first).reject_remote_clients(true);
    // SAFETY: `sa` и дескриптор живы до конца вызова.
    let r = unsafe { opts.create_with_security_attributes_raw(name, (&raw mut sa).cast()) };
    // SAFETY: дескриптор выделен `Convert…` через LocalAlloc.
    unsafe { LocalFree(sd) };
    r
}

/// Цели Диспетчера учётных данных с префиксом `prefix`.
pub(crate) fn cred_targets(prefix: &str) -> io::Result<Vec<String>> {
    let filter = wide(&format!("{prefix}*"));
    let mut count = 0u32;
    let mut list: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: фильтр с нулём в конце; `count` и `list` — выходы.
    if unsafe { CredEnumerateW(filter.as_ptr(), 0, &raw mut count, &raw mut list) } == 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == i32::try_from(ERROR_NOT_FOUND).ok() {
            Ok(Vec::new())
        } else {
            Err(e)
        };
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: `list` — массив из `count` указателей на CREDENTIALW.
        let name = unsafe { (**list.add(i)).TargetName };
        if name.is_null() {
            continue;
        }
        let mut len = 0;
        // SAFETY: TargetName — строка UTF-16 с нулём в конце.
        while unsafe { *name.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` символов прочитаны выше.
        let s = unsafe { std::slice::from_raw_parts(name, len) };
        out.push(String::from_utf16_lossy(s));
    }
    // SAFETY: буфер выделен CredEnumerateW.
    unsafe { CredFree(list.cast()) };
    Ok(out)
}
