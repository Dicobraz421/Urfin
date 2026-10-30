//! Листинг для анализа места (F01 §3.1): `NtQueryDirectoryFile` с
//! `FileIdFullDirectoryInformation` (имя, логический размер, размер на диске,
//! атрибуты, `FileId`, тег reparse в `EaSize`), подкаталог открывается относительно
//! хэндла родителя (`NtCreateFile` с `RootDirectory`) — путь не собирается.
//! Запасной путь — `FindFirstFileExW`; уточнение размера сжатых —
//! `GetCompressedFileSizeW`.

use std::cell::RefCell;
use std::path::Path;
use std::ptr;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_ID_FULL_DIR_INFORMATION, FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT,
    FILE_SYNCHRONOUS_IO_NONALERT, FileIdFullDirectoryInformation, NtCreateFile,
    NtQueryDirectoryFile,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, NO_ERROR, RtlNtStatusToDosError, STATUS_INVALID_INFO_CLASS,
    STATUS_INVALID_PARAMETER, STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE, STATUS_NOT_IMPLEMENTED,
    UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
    FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetCompressedFileSizeW,
    INVALID_FILE_SIZE, OPEN_EXISTING, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::windows::{is_dot, os_err, verbatim};
use crate::api::{EntryKind, VfsError};

const SYNCHRONIZE: u32 = 0x0010_0000;
/// Буфер листинга (F01 §3.1): 64 KiB на каждый вызов.
const BUF: usize = 64 * 1024;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Запись каталога в терминах ОС.
pub struct DuRec<'a> {
    pub name: &'a [u16],
    pub attrs: u32,
    /// Тег reparse (0 — не точка повторной обработки).
    pub tag: u32,
    pub kind: EntryKind,
    pub eof: u64,
    /// `AllocationSize`; у `FindFirstFileExW` — 0 (неизвестен).
    pub alloc: u64,
    /// нс от Unix epoch.
    pub mtime: i64,
    /// `FileId` (NTFS: 48 бит записи MFT + 16 бит последовательности); 0 — неизвестен.
    pub file_id: u64,
}

/// Открытый каталог (только листинг).
pub struct DirHandle(HANDLE);

// SAFETY: хэндл ядра — значение, не привязанное к потоку; `NtQueryDirectoryFile`
// по нему зовёт только поток, открывший каталог, а дети используют его лишь как
// `RootDirectory` (ядро допускает параллельные открытия относительно хэндла).
unsafe impl Send for DirHandle {}
// SAFETY: см. выше — общий доступ только на чтение значения хэндла.
unsafe impl Sync for DirHandle {}

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: хэндл открыт `CreateFileW`/`NtCreateFile` и закрывается один раз.
        unsafe { CloseHandle(self.0) };
    }
}

/// Открыть каталог по полному пути.
pub fn open_abs(dir: &Path) -> Result<DirHandle, VfsError> {
    let path = verbatim(dir);
    // SAFETY: путь — UTF-16 с NUL; хэндл синхронный (без OVERLAPPED, с SYNCHRONIZE).
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: сразу после неудачного вызова.
        return Err(os_err(dir, unsafe { GetLastError() }));
    }
    Ok(DirHandle(h))
}

/// Открыть подкаталог `name` относительно открытого родителя; `dir` — полный путь
/// (только для ошибок).
pub fn open_rel(parent: &DirHandle, name: &[u16], dir: &Path) -> Result<DirHandle, VfsError> {
    let bytes = u16::try_from(name.len() * 2).map_err(|_| VfsError::NotFound(dir.into()))?;
    let us = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_ptr().cast_mut(),
    };
    let oa = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).expect("small struct"),
        RootDirectory: parent.0,
        ObjectName: &raw const us,
        // Имя — точно из листинга: сравнение с учётом регистра верно и в
        // каталогах с чувствительностью к регистру.
        Attributes: 0,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut h: HANDLE = ptr::null_mut();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `us`/`oa` живут до конца вызова, имя — `bytes` байт; хэндл
    // синхронный (FILE_SYNCHRONOUS_IO_NONALERT + SYNCHRONIZE); без EA.
    let st = unsafe {
        NtCreateFile(
            &raw mut h,
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT,
            ptr::null(),
            0,
        )
    };
    if st < 0 {
        // SAFETY: чистая функция над кодом.
        return Err(os_err(dir, unsafe { RtlNtStatusToDosError(st) }));
    }
    Ok(DirHandle(h))
}

/// Ошибка листинга: класс информации не поддержан (нужен запасной путь) или ошибка ОС.
pub enum ListErr {
    Unsupported,
    Vfs(VfsError),
}

thread_local! {
    /// Буфер листинга потока обхода: без аллокации на каждый каталог.
    static LIST_BUF: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// Перебрать записи каталога (без `.`/`..`). `f` вернул `false` — стоп.
pub fn nt_for_each(
    h: &DirHandle,
    dir: &Path,
    f: &mut dyn FnMut(&DuRec<'_>) -> bool,
) -> Result<(), ListErr> {
    LIST_BUF.with(|b| {
        let mut buf = b.borrow_mut();
        if buf.len() < BUF / 8 {
            buf.resize(BUF / 8, 0);
        }
        nt_loop(h, dir, &mut buf, f)
    })
}

#[allow(
    clippy::cast_ptr_alignment,
    reason = "буфер — Vec<u64> (выравнивание 8), смещения записей ядро выравнивает по 8"
)]
fn nt_loop(
    h: &DirHandle,
    dir: &Path,
    buf: &mut [u64],
    f: &mut dyn FnMut(&DuRec<'_>) -> bool,
) -> Result<(), ListErr> {
    let len = u32::try_from(buf.len() * 8).expect("buffer < 4 GiB");
    let mut first = true;
    loop {
        let mut iosb = IO_STATUS_BLOCK::default();
        // SAFETY: буфер выровнен по 8 и `len` байт; без event/APC; синхронный хэндл.
        let st = unsafe {
            NtQueryDirectoryFile(
                h.0,
                ptr::null_mut(),
                None,
                ptr::null(),
                &raw mut iosb,
                buf.as_mut_ptr().cast(),
                len,
                FileIdFullDirectoryInformation,
                false,
                ptr::null(),
                first,
            )
        };
        if st == STATUS_NO_MORE_FILES || st == STATUS_NO_SUCH_FILE {
            return Ok(());
        }
        if first
            && (st == STATUS_INVALID_INFO_CLASS
                || st == STATUS_NOT_IMPLEMENTED
                || st == STATUS_INVALID_PARAMETER)
        {
            return Err(ListErr::Unsupported);
        }
        if st < 0 {
            // SAFETY: чистая функция над кодом.
            let code = unsafe { RtlNtStatusToDosError(st) };
            return Err(ListErr::Vfs(os_err(dir, code)));
        }
        first = false;
        let base = buf.as_ptr().cast::<u8>();
        let mut off = 0usize;
        loop {
            // SAFETY: `off` получен из `NextEntryOffset` ядра внутри буфера; запись
            // выровнена по 8; имя — `FileNameLength` байт после заголовка.
            let (info, name) = unsafe {
                let info = &*base.add(off).cast::<FILE_ID_FULL_DIR_INFORMATION>();
                let name_ptr = base
                    .add(off + std::mem::offset_of!(FILE_ID_FULL_DIR_INFORMATION, FileName))
                    .cast::<u16>();
                let n = (info.FileNameLength / 2) as usize;
                (info, std::slice::from_raw_parts(name_ptr, n))
            };
            if !is_dot(name) {
                let tag = if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    info.EaSize
                } else {
                    0
                };
                let kind = super::windows::kind(info.FileAttributes, (tag != 0).then_some(tag));
                let rec = DuRec {
                    name,
                    attrs: info.FileAttributes,
                    tag,
                    kind,
                    eof: info.EndOfFile.cast_unsigned(),
                    alloc: info.AllocationSize.cast_unsigned(),
                    mtime: super::windows::filetime_nanos(info.LastWriteTime),
                    file_id: info.FileId.cast_unsigned(),
                };
                if !f(&rec) {
                    return Ok(());
                }
            }
            if info.NextEntryOffset == 0 {
                break;
            }
            off += info.NextEntryOffset as usize;
        }
    }
}

/// Запасной путь: `FindFirstFileExW(FindExInfoBasic, LARGE_FETCH)`; без
/// `AllocationSize` и `FileId`.
pub fn find_for_each(dir: &Path, f: &mut dyn FnMut(&DuRec<'_>) -> bool) -> Result<(), VfsError> {
    let mut pattern = verbatim(dir);
    pattern.pop(); // NUL
    if pattern.last() != Some(&u16::from(b'\\')) {
        pattern.push(u16::from(b'\\'));
    }
    pattern.extend([u16::from(b'*'), 0]);
    let mut d = WIN32_FIND_DATAW::default();
    // SAFETY: шаблон — UTF-16 с NUL; `d` — выход на стеке.
    let h = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            (&raw mut d).cast(),
            FindExSearchNameMatch,
            ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: сразу после неудачного вызова.
        let e = unsafe { GetLastError() };
        return if e == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(os_err(dir, e))
        };
    }
    let r = find_loop(h, dir, &mut d, f);
    // SAFETY: хэндл от FindFirstFileExW, закрывается один раз.
    unsafe { FindClose(h) };
    r
}

fn find_loop(
    handle: HANDLE,
    dir: &Path,
    d: &mut WIN32_FIND_DATAW,
    f: &mut dyn FnMut(&DuRec<'_>) -> bool,
) -> Result<(), VfsError> {
    loop {
        let len = d
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(d.cFileName.len());
        let name = &d.cFileName[..len];
        if !is_dot(name) {
            let tag = if d.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                d.dwReserved0
            } else {
                0
            };
            let ft = (u64::from(d.ftLastWriteTime.dwHighDateTime) << 32)
                | u64::from(d.ftLastWriteTime.dwLowDateTime);
            let rec = DuRec {
                name,
                attrs: d.dwFileAttributes,
                tag,
                kind: super::windows::kind(d.dwFileAttributes, (tag != 0).then_some(tag)),
                eof: (u64::from(d.nFileSizeHigh) << 32) | u64::from(d.nFileSizeLow),
                alloc: 0,
                mtime: super::windows::filetime_nanos(ft.cast_signed()),
                file_id: 0,
            };
            if !f(&rec) {
                return Ok(());
            }
        }
        // SAFETY: хэндл открыт; `d` — выход.
        if unsafe { FindNextFileW(handle, d) } == 0 {
            // SAFETY: сразу после неудачного вызова.
            let code = unsafe { GetLastError() };
            return if code == ERROR_NO_MORE_FILES {
                Ok(())
            } else {
                Err(os_err(dir, code))
            };
        }
    }
}

/// Размер на диске сжатого/разреженного/WOF-файла (`GetCompressedFileSizeW`).
/// Файл не открывается на чтение данных — облачные заглушки сюда не попадают.
pub fn compressed_size(p: &Path) -> Option<u64> {
    let path = verbatim(p);
    let mut high = 0u32;
    // SAFETY: путь — UTF-16 с NUL; `high` — выход.
    let low = unsafe { GetCompressedFileSizeW(path.as_ptr(), &raw mut high) };
    // SAFETY: сразу после вызова (ошибка различима только по GetLastError).
    if low == INVALID_FILE_SIZE && unsafe { GetLastError() } != NO_ERROR {
        return None;
    }
    Some((u64::from(high) << 32) | u64::from(low))
}
