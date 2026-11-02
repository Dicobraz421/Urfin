//! Повышенный режим досчёта (F01-12, ADR-0021): укрепление процесса и запись файла
//! результата. Зовёт только `urfin.exe --du-elevated` ([`crate::du::elevated`]).
//!
//! Токен: включена `SeBackupPrivilege` (обход ACL на чтение при открытии каталогов с
//! `FILE_OPEN_FOR_BACKUP_INTENT`, как у `NtLister`), `SeChangeNotifyPrivilege` оставлена,
//! остальные привилегии удалены. Дочерние процессы запрещены. Файл результата создаётся
//! только новым (`CREATE_NEW`, без следования за ссылкой в последнем звене) и
//! проверяется по итоговому пути хэндла: каталог подменили ссылкой — файл удаляется.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    SE_PRIVILEGE_ENABLED, SE_PRIVILEGE_REMOVED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
    TOKEN_QUERY, TokenPrivileges,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED, FileDispositionInfo,
    GetFinalPathNameByHandleW, SetFileInformationByHandle, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, ProcessChildProcessPolicy, SetProcessMitigationPolicy,
};

const DELETE: u32 = 0x0001_0000;
const FILE_GENERIC_WRITE: u32 = 0x0012_0116;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Хэндл токена, закрывается при выходе из области.
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: хэндл открыт `OpenProcessToken` и закрывается один раз.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn luid(name: &str) -> io::Result<LUID> {
    let w = wide(OsStr::new(name));
    let mut l = LUID::default();
    // SAFETY: строка с нулём и выходной LUID живут до конца вызова.
    if unsafe { LookupPrivilegeValueW(std::ptr::null(), w.as_ptr(), &raw mut l) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(l)
}

fn same(a: LUID, b: LUID) -> bool {
    a.LowPart == b.LowPart && a.HighPart == b.HighPart
}

/// Привилегии токена процесса: `SeBackupPrivilege` — включить, `SeChangeNotifyPrivilege` —
/// оставить, прочие —
/// удалить. `Ok(false)` — Backup у токена нет (процесс без прав: тесты, отказ UAC
/// подменён); прочие всё равно удалены.
pub(crate) fn restrict_privileges() -> io::Result<bool> {
    let (backup, notify) = (luid("SeBackupPrivilege")?, luid("SeChangeNotifyPrivilege")?);
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: псевдохэндл текущего процесса; выходной хэндл — на стеке.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut raw,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let token = Token(raw);
    let mut len = 0u32;
    // SAFETY: запрос длины: буфера нет, длина — на стеке.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenPrivileges,
            std::ptr::null_mut(),
            0,
            &raw mut len,
        );
    }
    // u64 — выравнивание `TOKEN_PRIVILEGES` (LUID внутри).
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: буфер не короче `len` байт и живёт до конца функции.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenPrivileges,
            buf.as_mut_ptr().cast(),
            len,
            &raw mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let tp = buf.as_mut_ptr().cast::<TOKEN_PRIVILEGES>();
    // SAFETY: ОС заполнила `TOKEN_PRIVILEGES` с `PrivilegeCount` элементами подряд в
    // пределах `len` байт буфера.
    let items: &mut [LUID_AND_ATTRIBUTES] = unsafe {
        let n = (*tp).PrivilegeCount as usize;
        std::slice::from_raw_parts_mut((&raw mut (*tp).Privileges).cast(), n)
    };
    let mut has_backup = false;
    for it in items.iter_mut() {
        it.Attributes = if same(it.Luid, backup) {
            has_backup = true;
            SE_PRIVILEGE_ENABLED
        } else if same(it.Luid, notify) {
            SE_PRIVILEGE_ENABLED
        } else {
            SE_PRIVILEGE_REMOVED
        };
    }
    // SAFETY: `tp` — заполненный выше буфер длиной `len`; прежнее состояние не нужно.
    let ok = unsafe {
        AdjustTokenPrivileges(
            token.0,
            0,
            tp,
            len,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: код ошибки потока сразу после вызова.
    let partial = unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED;
    Ok(has_backup && !partial)
}

/// Запрет дочерних процессов (`ProcessChildProcessPolicy`): необратим до выхода.
pub(crate) fn no_child_processes() -> io::Result<()> {
    // PROCESS_MITIGATION_CHILD_PROCESS_POLICY: бит 0 — NoChildProcessCreation.
    let flags: u32 = 1;
    // SAFETY: структура — одно DWORD на стеке, длина — его размер.
    if unsafe {
        SetProcessMitigationPolicy(
            ProcessChildProcessPolicy,
            (&raw const flags).cast(),
            size_of::<u32>(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Итоговый путь открытого файла без `\\?\` (`C:\…`, длинные имена, ссылки раскрыты).
fn final_path(f: &File) -> io::Result<String> {
    let mut buf = vec![0u16; 1024];
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
    let s = std::ffi::OsString::from_wide(&buf)
        .to_string_lossy()
        .into_owned();
    Ok(s.strip_prefix(r"\\?\")
        .map_or_else(|| s.clone(), str::to_owned))
}

/// Пометить открытый файл на удаление (по хэндлу, не по пути).
fn delete_open(f: &File) {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: хэндл открыт с `DELETE`; структура на стеке.
    unsafe {
        SetFileInformationByHandle(
            f.as_raw_handle(),
            FileDispositionInfo,
            (&raw const info).cast(),
            u32::try_from(size_of::<FILE_DISPOSITION_INFO>()).unwrap_or(0),
        );
    }
}

/// Создать новый файл `path` и записать `bytes`. `path` — итоговый (без ссылок) путь:
/// иначе файл удаляется и ответ — ошибка. Существующий файл не перезаписывается.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(FILE_GENERIC_WRITE | DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let want = path.to_string_lossy();
    let got = final_path(&f);
    if !got.as_deref().is_ok_and(|g| g.eq_ignore_ascii_case(&want)) {
        delete_open(&f);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("result path redirected: {got:?}"),
        ));
    }
    if let Err(e) = f.write_all(bytes).and_then(|()| f.flush()) {
        delete_open(&f);
        return Err(e);
    }
    Ok(())
}
