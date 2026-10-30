//! Windows fast path (S4 §3.1): `CreateFileW` + `NtQueryDirectoryFile`
//! (`FileFullDirectoryInformation`), буфер 32 КБ на первый вызов и 512 КБ дальше;
//! запасной путь — `FindFirstFileExW(FindExInfoBasic, LARGE_FETCH)`.
//!
//! Без копирования в `WIN32_FIND_DATAW` и с тегом reparse прямо в записи
//! (`EaSize`): тип ссылки — без дополнительных вызовов, облачные файлы не
//! открываются. Пути — verbatim (`\\?\`): без `MAX_PATH`, с хвостовыми точками.

use std::io;
use std::path::Path;
use std::ptr;

use compact_str::CompactString;
use smallvec::SmallVec;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_FULL_DIR_INFORMATION, FileFullDirectoryInformation, NtQueryDirectoryFile,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, RtlNtStatusToDosError, STATUS_INVALID_INFO_CLASS,
    STATUS_INVALID_PARAMETER, STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE, STATUS_NOT_IMPLEMENTED,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
    FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, OPEN_EXISTING, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::api::{Attrs, CancelToken, Entry, EntryKind, ListStream, VfsError};

const SYNCHRONIZE: u32 = 0x0010_0000;
const FIRST_BUF: usize = 32 * 1024;
const NEXT_BUF: usize = 512 * 1024;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;
/// Бит «name surrogate» тега: symlink, junction, WSL-symlink — ссылки (как в std).
const NAME_SURROGATE: u32 = 0x2000_0000;

/// Каталог как verbatim UTF-16 с NUL.
pub(super) fn verbatim(dir: &Path) -> SmallVec<[u16; 264]> {
    let mut b = SmallVec::new();
    crate::path::to_verbatim(dir, &mut b);
    b
}

pub(super) fn os_err(dir: &Path, code: u32) -> VfsError {
    VfsError::from_io(dir, io::Error::from_raw_os_error(code.cast_signed()))
}

/// Открытый каталог: `NtQueryDirectoryFile` по нему.
pub struct NtList {
    dir: std::path::PathBuf,
    h: HANDLE,
    buf: Vec<u64>,
    first: bool,
    done: bool,
}

// SAFETY: HANDLE каталога используется только владеющим потоком (ListStream не
// Sync); передача между потоками безопасна.
unsafe impl Send for NtList {}

impl Drop for NtList {
    fn drop(&mut self) {
        // SAFETY: хэндл открыт `CreateFileW` и закрывается один раз.
        unsafe { CloseHandle(self.h) };
    }
}

/// `Ok(None)` — ФС не поддерживает `FileFullDirectoryInformation`: нужен запасной путь.
pub fn nt_list(dir: &Path) -> Result<Option<NtList>, VfsError> {
    let path = verbatim(dir);
    // SAFETY: путь — UTF-16 с NUL; хэндл синхронный (без OVERLAPPED, с
    // SYNCHRONIZE) — `NtQueryDirectoryFile` не вернёт STATUS_PENDING.
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
    let mut l = NtList {
        dir: dir.to_path_buf(),
        h,
        buf: vec![0u64; FIRST_BUF / 8],
        first: true,
        done: false,
    };
    // Пробный первый вызов: отказ класса информации — запасной путь.
    match l.query() {
        Ok(()) => Ok(Some(l)),
        Err(QueryErr::Unsupported) => Ok(None),
        Err(QueryErr::Vfs(e)) => Err(e),
    }
}

enum QueryErr {
    Unsupported,
    Vfs(VfsError),
}

impl NtList {
    /// Наполнить буфер; `done` — каталог кончился.
    fn query(&mut self) -> Result<(), QueryErr> {
        let mut iosb = IO_STATUS_BLOCK::default();
        let len = u32::try_from(self.buf.len() * 8).expect("buffer < 4 GiB");
        // SAFETY: буфер выровнен по 8 (Vec<u64>) и `len` байт; без event/APC;
        // синхронный хэндл.
        let st = unsafe {
            NtQueryDirectoryFile(
                self.h,
                ptr::null_mut(),
                None,
                ptr::null(),
                &raw mut iosb,
                self.buf.as_mut_ptr().cast(),
                len,
                FileFullDirectoryInformation,
                false,
                ptr::null(),
                self.first,
            )
        };
        let first = std::mem::replace(&mut self.first, false);
        if st == STATUS_NO_MORE_FILES || st == STATUS_NO_SUCH_FILE {
            self.done = true;
            // Пустой буфер: ничего не разбирать.
            self.buf.clear();
            return Ok(());
        }
        if first
            && (st == STATUS_INVALID_INFO_CLASS
                || st == STATUS_NOT_IMPLEMENTED
                || st == STATUS_INVALID_PARAMETER)
        {
            return Err(QueryErr::Unsupported);
        }
        if st < 0 {
            // SAFETY: чистая функция над кодом.
            let code = unsafe { RtlNtStatusToDosError(st) };
            return Err(QueryErr::Vfs(os_err(&self.dir, code)));
        }
        Ok(())
    }

    /// Разобрать цепочку `FILE_FULL_DIR_INFORMATION` в буфере.
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "буфер — Vec<u64> (выравнивание 8), смещения записей ядро выравнивает по 8"
    )]
    fn parse(&self, out: &mut Vec<Entry>) {
        if self.buf.is_empty() {
            return;
        }
        let base = self.buf.as_ptr().cast::<u8>();
        let cap = self.buf.len() * 8;
        let mut off = 0usize;
        loop {
            // SAFETY: `off` получен из `NextEntryOffset` ядра внутри буфера;
            // запись выровнена по 8; имя — `FileNameLength` байт после заголовка.
            let (info, name) = unsafe {
                let info = &*base.add(off).cast::<FILE_FULL_DIR_INFORMATION>();
                let name_ptr = base
                    .add(off + std::mem::offset_of!(FILE_FULL_DIR_INFORMATION, FileName))
                    .cast::<u16>();
                let n = (info.FileNameLength / 2) as usize;
                debug_assert!(off + n * 2 <= cap);
                (info, std::slice::from_raw_parts(name_ptr, n))
            };
            if !is_dot(name) {
                let tag = (info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                    .then_some(info.EaSize);
                out.push(entry(
                    name,
                    info.FileAttributes,
                    tag,
                    info.EndOfFile.cast_unsigned(),
                    [info.LastWriteTime, info.CreationTime, info.LastAccessTime],
                ));
            }
            if info.NextEntryOffset == 0 {
                break;
            }
            off += info.NextEntryOffset as usize;
            debug_assert!(off < cap);
        }
    }
}

impl ListStream for NtList {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if self.done && self.buf.is_empty() {
            return Ok(false);
        }
        // Буфер уже наполнен (пробный вызов или прошлый шаг): разобрать, затем
        // запросить следующий.
        self.parse(out);
        if self.done {
            self.buf.clear();
            return Ok(false);
        }
        if self.buf.len() * 8 < NEXT_BUF {
            self.buf = vec![0u64; NEXT_BUF / 8];
        }
        match self.query() {
            Ok(()) => Ok(!(self.done && self.buf.is_empty())),
            Err(QueryErr::Vfs(e)) => Err(e),
            Err(QueryErr::Unsupported) => Err(VfsError::Unsupported("NtQueryDirectoryFile")),
        }
    }
}

/// Запасной путь: `FindFirstFileExW` / `FindNextFileW`.
pub struct FindList {
    dir: std::path::PathBuf,
    h: HANDLE,
    data: WIN32_FIND_DATAW,
    have: bool,
}

// SAFETY: find-хэндл используется только владеющим потоком.
unsafe impl Send for FindList {}

impl Drop for FindList {
    fn drop(&mut self) {
        if self.h != INVALID_HANDLE_VALUE {
            // SAFETY: хэндл от FindFirstFileExW, закрывается один раз.
            unsafe { FindClose(self.h) };
        }
    }
}

pub fn find_list(dir: &Path) -> Result<FindList, VfsError> {
    let mut pattern = verbatim(dir);
    pattern.pop(); // NUL
    if pattern.last() != Some(&u16::from(b'\\')) {
        pattern.push(u16::from(b'\\'));
    }
    pattern.extend([u16::from(b'*'), 0]);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: шаблон — UTF-16 с NUL; `data` — выход на стеке.
    let h = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            (&raw mut data).cast(),
            FindExSearchNameMatch,
            ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: сразу после неудачного вызова.
        let e = unsafe { GetLastError() };
        // Пустой корень тома: записей нет совсем.
        if e == ERROR_FILE_NOT_FOUND {
            return Ok(FindList {
                dir: dir.to_path_buf(),
                h,
                data,
                have: false,
            });
        }
        return Err(os_err(dir, e));
    }
    Ok(FindList {
        dir: dir.to_path_buf(),
        h,
        data,
        have: true,
    })
}

impl ListStream for FindList {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        for _ in 0..1024 {
            if !self.have {
                return Ok(false);
            }
            let d = &self.data;
            let n = d
                .cFileName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(d.cFileName.len());
            let name = &d.cFileName[..n];
            if !is_dot(name) {
                let tag = (d.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                    .then_some(d.dwReserved0);
                let size = (u64::from(d.nFileSizeHigh) << 32) | u64::from(d.nFileSizeLow);
                let ft = |f: windows_sys::Win32::Foundation::FILETIME| {
                    ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)).cast_signed()
                };
                let times = [
                    ft(d.ftLastWriteTime),
                    ft(d.ftCreationTime),
                    ft(d.ftLastAccessTime),
                ];
                out.push(entry(name, d.dwFileAttributes, tag, size, times));
            }
            // SAFETY: хэндл открыт; `data` — выход.
            self.have = unsafe { FindNextFileW(self.h, &raw mut self.data) } != 0;
            if !self.have {
                // SAFETY: сразу после неудачного вызова.
                let e = unsafe { GetLastError() };
                if e != ERROR_NO_MORE_FILES {
                    return Err(os_err(&self.dir, e));
                }
            }
        }
        Ok(self.have)
    }
}

pub(super) fn is_dot(name: &[u16]) -> bool {
    matches!(name, [0x2E] | [0x2E, 0x2E])
}

/// Запись из полей каталога ОС: те же правила, что `std_fs` (эталон).
/// `times` — FILETIME изменения, создания и доступа.
pub(super) fn entry(name: &[u16], fa: u32, tag: Option<u32>, size: u64, times: [i64; 3]) -> Entry {
    let (name, escaped) = decode_name(name);
    let kind = kind(fa, tag);
    let mut attrs = crate::local::std_fs::windows_attrs(&name, fa).0;
    if escaped {
        attrs |= Attrs::ESCAPED_NAME;
    }
    Entry {
        size: if matches!(kind, EntryKind::File | EntryKind::LinkFile) {
            size
        } else {
            0
        },
        mtime: filetime_nanos(times[0]),
        ctime: filetime_nanos(times[1]),
        atime: filetime_nanos(times[2]),
        name,
        attrs: Attrs(attrs),
        kind,
    }
}

/// Имя: ASCII fast path, иначе UTF-16 с экранированием (S4-02).
pub(super) fn decode_name(units: &[u16]) -> (CompactString, bool) {
    if units.iter().all(|&u| u < 0x80) {
        let bytes: SmallVec<[u8; 64]> = units
            .iter()
            .map(|&u| u8::try_from(u).expect("ASCII"))
            .collect();
        let s = std::str::from_utf8(&bytes).expect("ASCII");
        return (CompactString::from(s), false);
    }
    crate::name::escape_wide(units)
}

/// Вид записи: ссылки — по тегу (name surrogate, как std; `AppExecLink` — файл-ссылка),
/// каталог/файл — по атрибуту; облачные и прочие теги — обычные записи.
pub(super) fn kind(fa: u32, tag: Option<u32>) -> EntryKind {
    let dir = fa & FILE_ATTRIBUTE_DIRECTORY != 0;
    match tag {
        Some(t) if t & NAME_SURROGATE != 0 => {
            if dir {
                EntryKind::LinkDir
            } else {
                EntryKind::LinkFile
            }
        }
        Some(IO_REPARSE_TAG_APPEXECLINK) => EntryKind::LinkFile,
        _ if dir => EntryKind::Dir,
        _ => EntryKind::File,
    }
}

/// FILETIME (100 нс от 1601) → нс от Unix epoch; вне i64 — `i64::MIN`.
pub(super) fn filetime_nanos(t: i64) -> i64 {
    const UNIX_EPOCH: i128 = 116_444_736_000_000_000;
    i64::try_from((i128::from(t) - UNIX_EPOCH) * 100).unwrap_or(i64::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_conversion() {
        assert_eq!(filetime_nanos(116_444_736_000_000_000), 0);
        assert_eq!(filetime_nanos(116_444_736_000_000_001), 100);
        // 1601 год в нс от 1970 не помещается в i64 — как и у std.
        assert_eq!(filetime_nanos(0), i64::MIN);
        // 1900-01-01: до 1970, но в диапазоне.
        assert_eq!(
            filetime_nanos(94_354_848_000_000_000),
            -2_208_988_800_000_000_000
        );
    }

    #[test]
    fn kinds_by_tag() {
        assert_eq!(kind(0x10, None), EntryKind::Dir);
        assert_eq!(kind(0x20, None), EntryKind::File);
        assert_eq!(kind(0x410, Some(0xA000_0003)), EntryKind::LinkDir); // junction
        assert_eq!(kind(0x420, Some(0xA000_000C)), EntryKind::LinkFile); // symlink
        assert_eq!(
            kind(0x420, Some(IO_REPARSE_TAG_APPEXECLINK)),
            EntryKind::LinkFile
        );
        assert_eq!(
            kind(0x420, Some(0x9000_001A)),
            EntryKind::File,
            "cloud is not a link"
        );
    }
}
