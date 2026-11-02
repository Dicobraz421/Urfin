//! USN-журнал NTFS для анализа места (F01 §3.3, F01-11): позиция журнала тома и
//! чтение записей с сохранённой позиции — только номера записей каталогов (сам файл
//! и его родитель), без имён. Открытие тома `\\.\X:` на чтение требует прав
//! администратора: без них — ошибка, анализ идёт по `mtime`.

use std::path::{Component, Path, Prefix};
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_HANDLE_EOF, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
    USN_RECORD_V2,
};

use super::windows::os_err;
use crate::api::VfsError;

/// Буфер чтения журнала.
const BUF: usize = 64 * 1024;

/// Позиция журнала тома.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    pub id: u64,
    /// Самая старая запись в журнале.
    pub first: i64,
    /// Следующая запись.
    pub next: i64,
}

/// Открытый том (только чтение журнала).
pub struct Volume(HANDLE);

impl Drop for Volume {
    fn drop(&mut self) {
        // SAFETY: хэндл открыт `CreateFileW` и закрывается один раз.
        unsafe { CloseHandle(self.0) };
    }
}

/// Буква диска пути (`C:\x` → `C`); UNC и прочее — `None`.
fn letter(p: &Path) -> Option<char> {
    match p.components().next()? {
        Component::Prefix(pre) => match pre.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some(char::from(d)),
            _ => None,
        },
        _ => None,
    }
}

/// Открыть том пути `root` (`\\.\C:`). Без прав администратора — `Denied`.
pub fn open(root: &Path) -> Result<Volume, VfsError> {
    let d = letter(root).ok_or(VfsError::Unsupported("usn: not a drive path"))?;
    let name: Vec<u16> = format!(r"\\.\{d}:")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: имя — UTF-16 с NUL; хэндл закрывается в `Drop`.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: сразу после неудачного вызова.
        return Err(os_err(root, unsafe { GetLastError() }));
    }
    Ok(Volume(h))
}

/// Позиция журнала тома (журнала нет — ошибка).
pub fn query(v: &Volume, root: &Path) -> Result<Journal, VfsError> {
    let mut out = USN_JOURNAL_DATA_V0::default();
    let mut got = 0u32;
    // SAFETY: выходной буфер — структура V0 своего размера; хэндл открыт.
    let ok = unsafe {
        DeviceIoControl(
            v.0,
            FSCTL_QUERY_USN_JOURNAL,
            ptr::null(),
            0,
            (&raw mut out).cast(),
            u32::try_from(size_of::<USN_JOURNAL_DATA_V0>()).unwrap_or(0),
            &raw mut got,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: сразу после неудачного вызова.
        return Err(os_err(root, unsafe { GetLastError() }));
    }
    Ok(Journal {
        id: out.UsnJournalID,
        first: out.FirstUsn,
        next: out.NextUsn,
    })
}

/// Записи журнала `j` с `from` до `j.next`: `f(файл, родитель, это каталог)`.
/// Возвращает позицию после прочитанного. Журнал пересоздан или усечён между
/// `query` и чтением — ошибка (вызывающий откатывается на `mtime`).
pub fn read(
    v: &Volume,
    root: &Path,
    j: Journal,
    from: i64,
    f: &mut dyn FnMut(u64, u64, bool),
) -> Result<i64, VfsError> {
    const DIRECTORY: u32 = 0x10;
    let mut buf = vec![0u64; BUF / 8];
    let mut at = from;
    while at < j.next {
        let req = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: at,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: j.id,
        };
        let mut got = 0u32;
        // SAFETY: вход — структура V0, выход — буфер `BUF` байт; хэндл открыт.
        let ok = unsafe {
            DeviceIoControl(
                v.0,
                FSCTL_READ_USN_JOURNAL,
                (&raw const req).cast(),
                u32::try_from(size_of::<READ_USN_JOURNAL_DATA_V0>()).unwrap_or(0),
                buf.as_mut_ptr().cast(),
                u32::try_from(BUF).unwrap_or(0),
                &raw mut got,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: сразу после неудачного вызова.
            let code = unsafe { GetLastError() };
            if code == ERROR_HANDLE_EOF {
                break;
            }
            return Err(os_err(root, code));
        }
        let got = (got as usize).min(BUF);
        if got < 8 {
            break;
        }
        let bytes = buf.as_ptr().cast::<u8>();
        // SAFETY: первые 8 байт выхода — следующая позиция (USN).
        let next = unsafe { bytes.cast::<i64>().read_unaligned() };
        let mut off = 8usize;
        while off + size_of::<USN_RECORD_V2>() <= got {
            // SAFETY: запись внутри прочитанного; заголовок читаем копией.
            let r = unsafe { bytes.add(off).cast::<USN_RECORD_V2>().read_unaligned() };
            let len = r.RecordLength as usize;
            if len == 0 || off + len > got {
                break;
            }
            if r.MajorVersion == 2 {
                f(
                    r.FileReferenceNumber,
                    r.ParentFileReferenceNumber,
                    r.FileAttributes & DIRECTORY != 0,
                );
            }
            off += len;
        }
        if next <= at {
            break;
        }
        at = next;
    }
    Ok(at.max(from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letter() {
        assert_eq!(letter(Path::new(r"C:\Dev")), Some('C'));
        assert_eq!(letter(Path::new(r"\\?\D:\x")), Some('D'));
        assert_eq!(letter(Path::new(r"\\srv\share\x")), None);
    }

    /// Стенд, права администратора: позиция журнала C: и чтение последних записей
    /// (только чтение номеров).
    #[test]
    #[ignore = "нужны права администратора и NTFS C:"]
    fn read_tail_of_c() {
        let root = Path::new(r"C:\");
        let v = open(root).expect("admin rights");
        let j = query(&v, root).expect("journal");
        assert!(j.next >= j.first);
        let mut n = 0;
        let end = read(
            &v,
            root,
            j,
            (j.next - 64 * 1024).max(j.first),
            &mut |_, _, _| n += 1,
        )
        .expect("read");
        assert!(end >= j.first);
        eprintln!("journal {:x}: {n} records", j.id);
    }
}
