//! Windows: отображение файла, позиционное чтение и запись, флаги открытия; замена
//! файла при сохранении редактора (`ReplaceFileW` → `MoveFileExW`, атрибуты, S7 §5.4).

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use memmap2::Mmap;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE,
    FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW, SetFileAttributesW,
};

use super::FileInfo;

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
    file_info(file).map(|i| i.id)
}

/// Id, число жёстких ссылок и атрибуты файла по открытому handle.
pub(crate) fn file_info(file: &File) -> io::Result<FileInfo> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: handle живёт, пока жив `file` (заимствование на время вызова); `info` —
    // локальная структура нужного размера, функция только пишет в неё.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow);
    Ok(FileInfo {
        id: (u64::from(info.dwVolumeSerialNumber), index),
        nlink: info.nNumberOfLinks,
        attrs: info.dwFileAttributes,
    })
}

/// Путь в UTF-16 с нулём в конце.
fn wide(p: &Path) -> Vec<u16> {
    OsStr::new(p)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// `ReplaceFileW(target ← temp)`: содержимое — из `temp`, а ACL, время создания,
/// короткое имя, id объекта и альтернативные потоки — от `target`. `temp` исчезает.
pub(crate) fn replace_file(target: &Path, temp: &Path) -> io::Result<()> {
    let (t, r) = (wide(target), wide(temp));
    // SAFETY: обе строки — живые буферы UTF-16 с нулём на время вызова; резервной копии
    // нет (NULL), `lpExclude` и `lpReserved` по документации — NULL.
    let ok = unsafe {
        ReplaceFileW(
            t.as_ptr(),
            r.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `MoveFileExW(from → to, REPLACE_EXISTING | WRITE_THROUGH)`: запасной путь, если
/// `ReplaceFileW` не смог (ACL — от временного файла).
pub(crate) fn move_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (wide(from), wide(to));
    // SAFETY: обе строки — живые буферы UTF-16 с нулём на время вызова.
    let ok = unsafe {
        MoveFileExW(
            f.as_ptr(),
            t.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Атрибуты файла по пути (`FILE_ATTRIBUTE_*`).
pub(crate) fn attrs(path: &Path) -> io::Result<u32> {
    use std::os::windows::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.file_attributes())
}

/// `SetFileAttributesW`; 0 — `FILE_ATTRIBUTE_NORMAL`.
pub(crate) fn set_attrs(path: &Path, attrs: u32) -> io::Result<()> {
    const NORMAL: u32 = 0x80;
    let p = wide(path);
    let a = if attrs == 0 { NORMAL } else { attrs };
    // SAFETY: строка — живой буфер UTF-16 с нулём на время вызова.
    let ok = unsafe { SetFileAttributesW(p.as_ptr(), a) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Пакет грамматики (S7-21a): `LoadLibraryExW` по абсолютному пути без поиска по PATH
/// (папка самой DLL, папка exe, System32), проверка версии формата, функции пакета.
/// DLL не выгружается: язык tree-sitter — её статическая таблица.
pub(crate) fn load_grammar(path: &Path, abi: u32) -> io::Result<super::GrammarPack> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    type AbiFn = unsafe extern "C" fn() -> u32;
    type LangFn = unsafe extern "C" fn() -> *const ();
    type QueryFn = unsafe extern "C" fn(usize, *mut usize) -> *const u8;

    if !path.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "relative path"));
    }
    let wide = wide(path);
    // SAFETY: `wide` — строка с нулём на конце; флаги ограничивают поиск зависимостей
    // папкой пакета, папкой exe и System32.
    let module = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
                | LOAD_LIBRARY_SEARCH_APPLICATION_DIR
                | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(io::Error::last_os_error());
    }
    let sym = |name: &[u8]| {
        // SAFETY: `module` — загруженный модуль, `name` — ASCII с нулём на конце.
        unsafe { GetProcAddress(module, name.as_ptr()) }.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "no export {}",
                    String::from_utf8_lossy(&name[..name.len() - 1])
                ),
            )
        })
    };
    let (abi_fn, lang_fn, query_fn) = (
        sym(b"urfin_grammar_abi\0")?,
        sym(b"urfin_grammar\0")?,
        sym(b"urfin_highlights\0")?,
    );
    // SAFETY: экспорт пакета `urfin_grammar_abi` — `extern "C" fn() -> u32`
    // (assets/syntax/pack); указатели функций одного размера.
    let abi_fn =
        unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, AbiFn>(abi_fn) };
    // SAFETY: вызов функции пакета без аргументов.
    let got = unsafe { abi_fn() };
    if got != abi {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pack abi {got}, expected {abi}"),
        ));
    }
    // SAFETY: формат пакета проверен по ABI: `urfin_grammar` — `extern "C" fn() ->
    // *const ()` (`TSLanguage*`), `urfin_highlights` — `extern "C" fn(usize, *mut usize)
    // -> *const u8`.
    let (lang_fn, query_fn) = unsafe {
        (
            std::mem::transmute::<unsafe extern "system" fn() -> isize, LangFn>(lang_fn),
            std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryFn>(query_fn),
        )
    };
    let mut query = String::new();
    for i in 0..64 {
        let mut len = 0usize;
        // SAFETY: `len` — валидный `usize`; ответ — null или указатель на `len` байт
        // статической строки пакета (DLL не выгружается).
        let ptr = unsafe { query_fn(i, &raw mut len) };
        if ptr.is_null() {
            break;
        }
        // SAFETY: см. выше — `len` байт по `ptr` живут, пока загружена DLL.
        let part = unsafe { std::slice::from_raw_parts(ptr, len) };
        query.push_str(&String::from_utf8_lossy(part));
        query.push('\n');
    }
    // SAFETY: `lang_fn` — функция грамматики tree-sitter из пакета (ABI проверен).
    let lang = unsafe { tree_sitter_language::LanguageFn::from_raw(lang_fn) };
    Ok(super::GrammarPack { lang, query })
}
