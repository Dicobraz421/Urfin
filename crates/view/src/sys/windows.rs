//! Windows: отображение файла, позиционное чтение и запись, флаги открытия.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use memmap2::Mmap;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE,
    FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle,
};

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// Отображение файла целиком только для чтения. Вызывает только `source::open_source`
/// по политике S7 §0: локальный том (не сеть, не USB), файл ≥ 64 МБ, mtime старше 60 с,
/// не follow.
pub(crate) fn map(file: &File) -> io::Result<Mmap> {
    // SAFETY: `Mmap::map` небезопасен, потому что другой процесс может изменить или
    // усечь файл под отображением. На Windows усечение отображённого файла ОС
    // отвергает (`ERROR_USER_MAPPED_FILE`), страница исчезнуть не может. Запись в файл
    // другим процессом меняет байты среза — вьювер читает их как `&[u8]`, не строит из
    // них `str` без проверки и не полагается на их неизменность (страница — снимок
    // на момент чтения). Ошибка страницы на сети или USB — `EXCEPTION_IN_PAGE_ERROR`
    // (abort), поэтому такие тома политика §0 к mmap не допускает. Отображение живёт
    // в `Arc<Mmap>` и снимается после последнего `Page::Mapped`.
    unsafe { Mmap::map(file) }
}

/// Позиционное чтение (`ReadFile` с OVERLAPPED-смещением): потоки не делят позицию.
pub(crate) fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    file.seek_read(buf, off)
}

/// Позиционная запись всего буфера.
pub(crate) fn write_all_at(file: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = file.seek_write(buf, off)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// Временный файл Spool: удаляется ОС, когда закрыт последний handle (в том числе при
/// падении процесса).
pub(crate) fn open_spool(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .attributes(FILE_ATTRIBUTE_TEMPORARY)
        .open(path)
}

/// Отдельный handle для последовательного прохода индексатора (`FILE_FLAG_SEQUENTIAL_SCAN`).
pub(crate) fn open_seq(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
}

/// Id файла по открытому handle: серийный номер тома и индекс файла
/// (`GetFileInformationByHandle`). Follow сравнивает его с файлом по пути — ротация
/// (S7 §3.8); редактор — отметка диска (`DiskStamp`).
pub(crate) fn file_id(file: &File) -> io::Result<(u64, u128)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: handle живёт, пока жив `file` (заимствование на время вызова); `info` —
    // локальная структура нужного размера, функция только пишет в неё.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow);
    Ok((u64::from(info.dwVolumeSerialNumber), index))
}
