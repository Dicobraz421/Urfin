//! Файловые вызовы копирования и переноса (S5-06): `CopyFileExW` с прогрессом
//! и отменой, `MoveFileExW` с заменой и без, атрибуты, копия ссылки (symlink,
//! junction) как ссылки. Пути в ОС — verbatim (`\\?\`, `urfin_vfs::path`).

#![allow(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};

/// Отмена `CopyFileExW` из колбэка (`ERROR_REQUEST_ABORTED`).
pub const ERR_ABORTED: i32 = 1235;
/// Разные тома у `MoveFileExW` без копирования (`ERROR_NOT_SAME_DEVICE`).
pub const ERR_NOT_SAME_DEVICE: i32 = 17;

/// Атрибуты (`FILE_ATTRIBUTE_*`), которые переносим на каталоги.
pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_ARCHIVE: u32 = 0x20;

/// Как копировать файл.
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые флаги CopyFileEx"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyFlags {
    /// Назначение не должно существовать (`AlreadyExists`).
    pub excl: bool,
    /// Ссылку — как ссылку, без разыменования.
    pub symlink: bool,
    /// Мимо кэша ОС (`COPY_FILE_NO_BUFFERING`): крупные файлы (S5-16).
    pub no_buffering: bool,
    /// Разреженный источник: копировать только данные (`COPY_FILE_ENABLE_SPARSE_COPY`,
    /// Windows 11 22H2+; старые системы флаг не знают — копия обычная).
    pub sparse: bool,
    /// Докачка (S5-27): своя копия кусками с `fsync` каждые [`RESUME_STEP`];
    /// прерванная оставляет файл, повтор с теми же путями дописывает с последней
    /// сброшенной границы. (`COPY_FILE_RESTARTABLE` на Windows 11 недописанное удаляет.)
    pub restartable: bool,
}

/// Копирует данные, альтернативные потоки, атрибуты и время изменения (как
/// Проводник). `progress(done)` — после каждого чанка основного потока, байт
/// с начала файла; `false` — отмена (ошибка с кодом [`ERR_ABORTED`],
/// недописанное назначение система удаляет сама). Пауза — блокировкой
/// внутри `progress`.
pub fn copy_file(
    src: &Path,
    dst: &Path,
    flags: CopyFlags,
    progress: &mut dyn FnMut(u64) -> bool,
) -> io::Result<()> {
    imp::copy_file(src, dst, flags, progress)
}

/// Свой быстрый путь мелкого файла (S5 §4.2, S5-14): весь файл в `buf`, назначение
/// `CREATE_NEW`, именованные потоки (Zone.Identifier), время изменения и атрибуты —
/// последними. `Ok(false)` — файл не для этого пути (ссылка, облачный, с EA,
/// шифрованный, разреженный, сжатый): взять [`copy_file`].
pub fn copy_small(src: &Path, dst: &Path, buf: &mut Vec<u8>) -> io::Result<bool> {
    imp::copy_small(src, dst, buf)
}

/// Граница сброса докачки (S5-27): данные до неё переживают и отключение питания.
pub const RESUME_STEP: u64 = 256 << 20;

/// Копия с докачкой (S5-27): с начала последнего несброшенного куска, кусками по 4 мегабайта,
/// `fsync` на каждой границе [`RESUME_STEP`]; в конце — время изменения, атрибуты и
/// метка «из интернета» (`Zone.Identifier`). `progress(сделано)` → `false` — прервать
/// (файл остаётся для повтора).
pub fn copy_resumable(
    src: &Path,
    dst: &Path,
    progress: &mut dyn FnMut(u64) -> bool,
) -> io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut from = std::fs::File::open(src)?;
    let meta = from.metadata()?;
    let total = meta.len();
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dst)?;
    let have = to.metadata()?.len();
    let start = if have > total {
        0
    } else {
        have / RESUME_STEP * RESUME_STEP
    };
    to.set_len(start)?;
    from.seek(SeekFrom::Start(start))?;
    to.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; 4 << 20];
    let mut done = start;
    if !progress(done) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n])?;
        let before = done;
        done += n as u64;
        if done / RESUME_STEP != before / RESUME_STEP {
            to.sync_data()?;
        }
        if !progress(done) {
            to.sync_data()?;
            return Err(io::ErrorKind::Interrupted.into());
        }
    }
    to.sync_all()?;
    to.set_modified(meta.modified()?)?;
    drop(to);
    // Метка «из интернета» — не терять (на FAT потоков нет — без неё).
    let zone = |p: &Path| {
        let mut s = p.as_os_str().to_owned();
        s.push(":Zone.Identifier");
        std::path::PathBuf::from(s)
    };
    if let Ok(z) = std::fs::read(zone(src)) {
        let _ = std::fs::write(zone(dst), z);
    }
    if let Ok(a) = attrs(src) {
        let _ = set_attrs(dst, a);
    }
    Ok(())
}

/// `CopyFile2` без колбэка (замер S5-14).
#[cfg(test)]
pub fn copy_file2(src: &Path, dst: &Path, excl: bool) -> io::Result<()> {
    imp::copy_file2(src, dst, excl)
}

/// Переименование или перенос в пределах тома. Без `replace` занятое имя —
/// `AlreadyExists`; другой том — [`ERR_NOT_SAME_DEVICE`].
pub fn move_file(src: &Path, dst: &Path, replace: bool) -> io::Result<()> {
    imp::move_file(src, dst, replace)
}

/// Читает файл кусками в `f`: `uncached` — мимо кэша ОС (`FILE_FLAG_NO_BUFFERING`,
/// буфер выровнен по странице), чтобы проверка копии читала диск, а не память.
pub fn read_chunks(p: &Path, uncached: bool, f: &mut dyn FnMut(&[u8])) -> io::Result<()> {
    imp::read_chunks(p, uncached, f)
}

/// Цель junction (точки монтирования) — `None` для symlink и прочих ссылок.
pub fn junction_target(p: &Path) -> Option<PathBuf> {
    imp::junction_target(p)
}

/// Сделать файл разреженным (тест S5-22).
#[cfg(test)]
pub fn make_sparse(p: &Path) -> io::Result<()> {
    imp::make_sparse(p)
}

/// Занято на диске (`GetCompressedFileSizeW`; тест разреженной копии).
#[cfg(test)]
pub fn disk_size(p: &Path) -> io::Result<u64> {
    imp::disk_size(p)
}

/// Junction (точка монтирования) `link` → каталог `target` (абсолютный путь):
/// прав администратора не нужно, в отличие от symlink.
pub fn make_junction(link: &Path, target: &Path) -> io::Result<()> {
    imp::make_junction(link, target)
}

/// Местные дата и время сейчас: `2026-10-21 23:18:05` (поле `ops.touch`).
pub fn local_now_text() -> String {
    imp::local_now_text()
}

/// Местное время → `SystemTime` (`ops.touch mtime=…`).
pub fn local_time(
    y: u16,
    mo: u16,
    d: u16,
    h: u16,
    mi: u16,
    s: u16,
) -> Option<std::time::SystemTime> {
    imp::local_time(y, mo, d, h, mi, s)
}

/// Местная дата (год, месяц, день) по времени изменения (нс от эпохи Unix):
/// `[Y][M][D]` группового переименования.
pub fn local_ymd(ns: i64) -> Option<(u16, u8, u8)> {
    imp::local_ymd(ns)
}

/// Свободное место тома по пути (доступное вызывающему — с квотами) и его размер.
pub fn free_space(p: &Path) -> io::Result<(u64, u64)> {
    imp::free_space(p)
}

/// Атрибуты (`FILE_ATTRIBUTE_*`) по пути, без разыменования ссылки.
pub fn attrs(p: &Path) -> io::Result<u32> {
    imp::attrs(p)
}

/// Выставить атрибуты; 0 → `NORMAL`.
pub fn set_attrs(p: &Path, attrs: u32) -> io::Result<()> {
    imp::set_attrs(p, attrs)
}

/// Копия ссылки (symlink, junction) `src` в `dst` (не существует): сама
/// ссылка с той же целью, цель не трогается.
pub fn copy_link(src: &Path, dst: &Path, dir: bool) -> io::Result<()> {
    imp::copy_link(src, dst, dir)
}

/// Время изменения каталога (после всех его детей, S5 §3.6).
pub fn set_dir_mtime(p: &Path, t: std::time::SystemTime) -> io::Result<()> {
    imp::open_dir_attrs(p)?.set_modified(t)
}

/// Местное время для имён `RenameScheme::Timestamp`: `2026-10-20 23-18-05`.
pub fn local_stamp() -> String {
    imp::local_stamp()
}

/// Путь без `\\?\` (для имён в точке монтирования и сообщений).
fn shown_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\")
        .map_or_else(|| s.clone().into_owned(), str::to_owned)
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::Path;

    use smallvec::SmallVec;
    use windows_sys::Wdk::Storage::FileSystem::{FileEaInformation, NtQueryInformationFile};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        CALLBACK_CHUNK_FINISHED, COPY_FILE_COPY_SYMLINK, COPY_FILE_FAIL_IF_EXISTS, CopyFileExW,
        FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileAttributesW, INVALID_FILE_ATTRIBUTES, LPPROGRESS_ROUTINE_CALLBACK_REASON,
        MAXIMUM_REPARSE_DATA_BUFFER_SIZE, MOVEFILE_REPLACE_EXISTING, MoveFileExW, PROGRESS_CANCEL,
        PROGRESS_CONTINUE, SetFileAttributesW,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_OFFLINE,
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE, FILE_BASIC_INFO,
        FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_DELETE, FILE_SHARE_READ, FileBasicInfo,
        FileStreamInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;

    use super::CopyFlags;

    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
    const GENERIC_WRITE: u32 = 0x4000_0000;

    fn wide(p: &Path) -> SmallVec<[u16; 264]> {
        let mut b = SmallVec::new();
        urfin_vfs::path::to_verbatim(p, &mut b);
        b
    }

    type Progress<'a> = &'a mut dyn FnMut(u64) -> bool;

    #[allow(clippy::too_many_arguments, reason = "сигнатура LPPROGRESS_ROUTINE")]
    unsafe extern "system" fn on_chunk(
        _total: i64,
        _total_done: i64,
        _stream_size: i64,
        stream_done: i64,
        stream: u32,
        reason: LPPROGRESS_ROUTINE_CALLBACK_REASON,
        _src: HANDLE,
        _dst: HANDLE,
        data: *const c_void,
    ) -> u32 {
        if stream != 1 || reason != CALLBACK_CHUNK_FINISHED {
            return PROGRESS_CONTINUE;
        }
        // SAFETY: `data` — указатель на `Progress` из `copy_file`, живой всё
        // время вызова `CopyFileExW`; колбэк зовётся в том же потоке.
        let f = unsafe { &mut *data.cast::<Progress<'_>>().cast_mut() };
        let done = u64::try_from(stream_done).unwrap_or(0);
        // Паника не должна пересечь границу FFI: отмена копии.
        match catch_unwind(AssertUnwindSafe(|| f(done))) {
            Ok(true) => PROGRESS_CONTINUE,
            _ => PROGRESS_CANCEL,
        }
    }

    pub fn read_chunks(p: &Path, uncached: bool, f: &mut dyn FnMut(&[u8])) -> io::Result<()> {
        use std::io::Read;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING;
        /// Кусок чтения: кратен сектору и странице.
        const CHUNK: usize = 1 << 20;
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .custom_flags(if uncached {
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN
            } else {
                FILE_FLAG_SEQUENTIAL_SCAN
            })
            .open(crate::names::os_path(p))?;
        // Выравнивание буфера по 4 КиБ: требование `FILE_FLAG_NO_BUFFERING`.
        let mut raw = vec![0u8; CHUNK + 4096];
        let off = raw.as_ptr().align_offset(4096);
        let buf = &mut raw[off..off + CHUNK];
        loop {
            let n = file.read(buf)?;
            if n == 0 {
                return Ok(());
            }
            f(&buf[..n]);
        }
    }

    pub fn local_ymd(ns: i64) -> Option<(u16, u8, u8)> {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{
            FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime,
        };
        if ns == i64::MIN {
            return None;
        }
        // FILETIME: 100 нс от 1601-01-01.
        let ticks = u64::try_from(ns / 100 + 116_444_736_000_000_000).ok()?;
        let ft = FILETIME {
            dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        // SAFETY: POD.
        let (mut utc, mut local): (SYSTEMTIME, SYSTEMTIME) =
            unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
        // SAFETY: указатели на локальные структуры; синхронные вызовы.
        let ok = unsafe {
            FileTimeToSystemTime(&raw const ft, &raw mut utc) != 0
                && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &raw const utc, &raw mut local)
                    != 0
        };
        ok.then(|| {
            (
                local.wYear,
                u8::try_from(local.wMonth).unwrap_or(0),
                u8::try_from(local.wDay).unwrap_or(0),
            )
        })
    }

    pub fn free_space(p: &Path) -> io::Result<(u64, u64)> {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let w = wide(p);
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: строка с NUL живёт до конца вызова; выходные — локальные u64.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                w.as_ptr(),
                &raw mut free,
                &raw mut total,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((free, total))
    }

    /// Атрибуты, при которых свой путь не годится.
    const NOT_SMALL: u32 = FILE_ATTRIBUTE_REPARSE_POINT
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
        | FILE_ATTRIBUTE_RECALL_ON_OPEN
        | FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_ENCRYPTED
        | FILE_ATTRIBUTE_SPARSE_FILE
        | FILE_ATTRIBUTE_COMPRESSED;

    fn basic(h: HANDLE) -> io::Result<FILE_BASIC_INFO> {
        let mut b = FILE_BASIC_INFO {
            CreationTime: 0,
            LastAccessTime: 0,
            LastWriteTime: 0,
            ChangeTime: 0,
            FileAttributes: 0,
        };
        // SAFETY: буфер — `FILE_BASIC_INFO` своего размера.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                h,
                FileBasicInfo,
                (&raw mut b).cast(),
                u32::try_from(size_of::<FILE_BASIC_INFO>()).unwrap_or(0),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(b)
    }

    /// Размер расширенных атрибутов (EA): WSL хранит в них метаданные.
    fn ea_size(h: HANDLE) -> io::Result<u32> {
        let mut io = IO_STATUS_BLOCK::default();
        let mut size: u32 = 0;
        // SAFETY: буфер — `FILE_EA_INFORMATION` (один `u32`).
        let st = unsafe {
            NtQueryInformationFile(h, &raw mut io, (&raw mut size).cast(), 4, FileEaInformation)
        };
        if st < 0 {
            return Err(io::Error::other(format!("NtQueryInformationFile 0x{st:x}")));
        }
        Ok(size)
    }

    /// Имена именованных потоков (`:Zone.Identifier:$DATA` → `Zone.Identifier`).
    fn named_streams(h: HANDLE) -> io::Result<SmallVec<[String; 1]>> {
        let mut out = SmallVec::new();
        let mut buf = vec![0u64; 512];
        let len = u32::try_from(buf.len() * 8).unwrap_or(0);
        // SAFETY: буфер своего размера, выровнен по 8.
        let ok = unsafe {
            GetFileInformationByHandleEx(h, FileStreamInfo, buf.as_mut_ptr().cast(), len)
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            // Нет потоков (ФС без них) — ERROR_HANDLE_EOF.
            return if e.raw_os_error() == Some(38) {
                Ok(out)
            } else {
                Err(e)
            };
        }
        let base = buf.as_ptr().cast::<u8>();
        let mut off = 0usize;
        loop {
            // SAFETY: записи `FILE_STREAM_INFO` внутри буфера по `NextEntryOffset`.
            let (next, name) = unsafe {
                let e = base.add(off);
                let next = std::ptr::read_unaligned(e.cast::<u32>());
                let name_len = std::ptr::read_unaligned(e.add(4).cast::<u32>()) as usize / 2;
                #[allow(clippy::cast_ptr_alignment, reason = "читается через read_unaligned")]
                let name_ptr = e.add(24).cast::<u16>();
                let name: Vec<u16> = (0..name_len)
                    .map(|i| std::ptr::read_unaligned(name_ptr.add(i)))
                    .collect();
                (next, String::from_utf16_lossy(&name))
            };
            if name != "::$DATA"
                && let Some(n) = name
                    .strip_prefix(':')
                    .and_then(|n| n.strip_suffix(":$DATA"))
            {
                out.push(n.to_owned());
            }
            if next == 0 {
                break;
            }
            off += next as usize;
        }
        Ok(out)
    }

    pub fn copy_small(src: &Path, dst: &Path, buf: &mut Vec<u8>) -> io::Result<bool> {
        use std::io::{Read, Write};
        let sp = crate::names::os_path(src);
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&sp)?;
        let handle = file.as_raw_handle() as HANDLE;
        let info = basic(handle)?;
        if info.FileAttributes & NOT_SMALL != 0 || ea_size(handle)? != 0 {
            return Ok(false);
        }
        let streams = named_streams(handle)?;
        buf.clear();
        file.read_to_end(buf)?;
        drop(file);
        let dp = crate::names::os_path(dst);
        let mut out = OpenOptions::new().write(true).create_new(true).open(&dp)?;
        let r = (|| -> io::Result<()> {
            out.write_all(buf)?;
            for n in &streams {
                let mut si = OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
                    .open(format!("{}:{n}", sp.display()))?;
                buf.clear();
                si.read_to_end(buf)?;
                let mut so = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(format!("{}:{n}", dp.display()))?;
                so.write_all(buf)?;
            }
            // Время изменения и атрибуты — последними (S5 §6.1); создание — сейчас.
            let set = FILE_BASIC_INFO {
                CreationTime: 0,
                LastAccessTime: 0,
                LastWriteTime: info.LastWriteTime,
                ChangeTime: 0,
                FileAttributes: info.FileAttributes,
            };
            // SAFETY: буфер — `FILE_BASIC_INFO` своего размера, handle открыт на запись.
            let ok = unsafe {
                SetFileInformationByHandle(
                    out.as_raw_handle() as HANDLE,
                    FileBasicInfo,
                    (&raw const set).cast(),
                    u32::try_from(size_of::<FILE_BASIC_INFO>()).unwrap_or(0),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        drop(out);
        if let Err(e) = r {
            // Недописанное своё назначение — убрать.
            let _ = std::fs::remove_file(&dp);
            return Err(e);
        }
        Ok(true)
    }

    #[cfg(test)]
    pub fn copy_file2(src: &Path, dst: &Path, excl: bool) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{COPYFILE2_EXTENDED_PARAMETERS, CopyFile2};
        let (s, d) = (wide(src), wide(dst));
        let p = COPYFILE2_EXTENDED_PARAMETERS {
            dwSize: u32::try_from(size_of::<COPYFILE2_EXTENDED_PARAMETERS>()).unwrap_or(0),
            dwCopyFlags: if excl { COPY_FILE_FAIL_IF_EXISTS } else { 0 },
            pfCancel: std::ptr::null_mut(),
            pProgressRoutine: None,
            pvCallbackContext: std::ptr::null_mut(),
        };
        // SAFETY: строки с NUL и параметры живут до конца вызова.
        let hr = unsafe { CopyFile2(s.as_ptr(), d.as_ptr(), &raw const p) };
        if hr < 0 {
            // HRESULT_FROM_WIN32: младшие 16 бит — код Win32.
            #[allow(clippy::cast_sign_loss, reason = "разбор HRESULT")]
            let code = (hr as u32) & 0xFFFF;
            return Err(io::Error::from_raw_os_error(
                i32::try_from(code).unwrap_or(0),
            ));
        }
        Ok(())
    }

    pub fn copy_file(
        src: &Path,
        dst: &Path,
        flags: CopyFlags,
        mut progress: Progress<'_>,
    ) -> io::Result<()> {
        if flags.restartable {
            return super::copy_resumable(src, dst, &mut progress);
        }
        let (s, d) = (wide(src), wide(dst));
        let mut f = 0;
        if flags.excl {
            f |= COPY_FILE_FAIL_IF_EXISTS;
        }
        if flags.symlink {
            f |= COPY_FILE_COPY_SYMLINK;
        }
        if flags.no_buffering {
            f |= windows_sys::Win32::Storage::FileSystem::COPY_FILE_NO_BUFFERING;
        }

        if flags.sparse {
            // Разреженная копия — только у `CopyFile2`; старая система флаг не
            // примет (`ERROR_INVALID_PARAMETER`) — обычная копия ниже.
            match copy_file2_with(&s, &d, f | COPY_FILE_ENABLE_SPARSE_COPY, &mut progress) {
                Err(e) if e.raw_os_error() == Some(87) => {}
                r => return r,
            }
        }
        let data: *mut Progress<'_> = &raw mut progress;
        // SAFETY: строки с NUL живут до конца вызова; `data` указывает на
        // локальную переменную, переживающую вызов.
        let ok = unsafe {
            CopyFileExW(
                s.as_ptr(),
                d.as_ptr(),
                Some(on_chunk),
                data.cast::<c_void>().cast_const(),
                std::ptr::null_mut(),
                f,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `COPY_FILE_ENABLE_SPARSE_COPY` (Windows 11 22H2+, только `CopyFile2`).
    const COPY_FILE_ENABLE_SPARSE_COPY: u32 = 0x2000_0000;

    /// Колбэк `CopyFile2`: байты основного потока по чанкам, отмена.
    unsafe extern "system" fn on_chunk2(
        m: *const windows_sys::Win32::Storage::FileSystem::COPYFILE2_MESSAGE,
        data: *const c_void,
    ) -> windows_sys::Win32::Storage::FileSystem::COPYFILE2_MESSAGE_ACTION {
        use windows_sys::Win32::Storage::FileSystem::{
            COPYFILE2_CALLBACK_CHUNK_FINISHED, COPYFILE2_PROGRESS_CANCEL,
            COPYFILE2_PROGRESS_CONTINUE,
        };
        // SAFETY: `m` — сообщение `CopyFile2` на время колбэка.
        let m = unsafe { &*m };
        if m.Type != COPYFILE2_CALLBACK_CHUNK_FINISHED {
            return COPYFILE2_PROGRESS_CONTINUE;
        }
        // SAFETY: тип сообщения — ChunkFinished, поле союза — его.
        let info = unsafe { m.Info.ChunkFinished };
        if info.dwStreamNumber != 1 {
            return COPYFILE2_PROGRESS_CONTINUE;
        }
        // SAFETY: `data` — `Progress` из `copy_file2_with`, живой всё время вызова.
        let f = unsafe { &mut *data.cast::<Progress<'_>>().cast_mut() };
        match catch_unwind(AssertUnwindSafe(|| f(info.uliStreamBytesTransferred))) {
            Ok(true) => COPYFILE2_PROGRESS_CONTINUE,
            _ => COPYFILE2_PROGRESS_CANCEL,
        }
    }

    fn copy_file2_with(
        s: &[u16],
        d: &[u16],
        flags: u32,
        progress: &mut Progress<'_>,
    ) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{COPYFILE2_EXTENDED_PARAMETERS, CopyFile2};
        let data: *mut Progress<'_> = progress;
        let p = COPYFILE2_EXTENDED_PARAMETERS {
            dwSize: u32::try_from(size_of::<COPYFILE2_EXTENDED_PARAMETERS>()).unwrap_or(0),
            dwCopyFlags: flags,
            pfCancel: std::ptr::null_mut(),
            pProgressRoutine: Some(on_chunk2),
            pvCallbackContext: data.cast(),
        };
        // SAFETY: строки с NUL и параметры живут до конца вызова; колбэк — в этом потоке.
        let hr = unsafe { CopyFile2(s.as_ptr(), d.as_ptr(), &raw const p) };
        if hr < 0 {
            #[allow(clippy::cast_sign_loss, reason = "разбор HRESULT")]
            let code = (hr as u32) & 0xFFFF;
            return Err(io::Error::from_raw_os_error(
                i32::try_from(code).unwrap_or(0),
            ));
        }
        Ok(())
    }

    pub fn move_file(src: &Path, dst: &Path, replace: bool) -> io::Result<()> {
        let (s, d) = (wide(src), wide(dst));
        let f = if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
        // SAFETY: строки с NUL живут до конца вызова.
        if unsafe { MoveFileExW(s.as_ptr(), d.as_ptr(), f) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn attrs(p: &Path) -> io::Result<u32> {
        let w = wide(p);
        // SAFETY: строка с NUL.
        let a = unsafe { GetFileAttributesW(w.as_ptr()) };
        if a == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        Ok(a)
    }

    pub fn set_attrs(p: &Path, attrs: u32) -> io::Result<()> {
        let w = wide(p);
        let a = if attrs == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            attrs
        };
        // SAFETY: строка с NUL.
        if unsafe { SetFileAttributesW(w.as_ptr(), a) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn open_dir_attrs(p: &Path) -> io::Result<File> {
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
    }

    fn open_reparse(p: &Path, write: bool) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(if write { GENERIC_WRITE } else { 0 })
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
    }

    /// Буфер `REPARSE_DATA_BUFFER` ссылки.
    fn reparse_data(src: &Path) -> io::Result<Vec<u8>> {
        let f = open_reparse(src, false)?;
        let mut buf = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
        let mut got = 0u32;
        // SAFETY: хэндл открыт; буфер `buf.len()` байт; синхронный вызов.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle() as HANDLE,
                FSCTL_GET_REPARSE_POINT,
                std::ptr::null(),
                0,
                buf.as_mut_ptr().cast(),
                MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
                &raw mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(got as usize);
        Ok(buf)
    }

    fn set_reparse(dst: &Path, data: &[u8]) -> io::Result<()> {
        let f = open_reparse(dst, true)?;
        let len = u32::try_from(data.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let mut got = 0u32;
        // SAFETY: хэндл открыт на запись; `data` — буфер от
        // FSCTL_GET_REPARSE_POINT; синхронный вызов.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle() as HANDLE,
                FSCTL_SET_REPARSE_POINT,
                data.as_ptr().cast(),
                len,
                std::ptr::null_mut(),
                0,
                &raw mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn junction_target(p: &Path) -> Option<std::path::PathBuf> {
        /// `IO_REPARSE_TAG_MOUNT_POINT`.
        const TAG: u32 = 0xA000_0003;
        let data = reparse_data(p).ok()?;
        let u16at = |i: usize| data.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let tag = data
            .get(..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))?;
        if tag != TAG {
            return None;
        }
        // Заголовок 8 байт, затем смещения и длины имён, имена — с байта 16.
        let (off, len) = (usize::from(u16at(8)?), usize::from(u16at(10)?));
        let start = 16 + off;
        let units: Vec<u16> = data
            .get(start..start + len)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let s = String::from_utf16(&units).ok()?;
        Some(std::path::PathBuf::from(
            s.strip_prefix(r"\??\").unwrap_or(&s),
        ))
    }

    #[cfg(test)]
    pub fn make_sparse(p: &Path) -> io::Result<()> {
        use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
        let f = OpenOptions::new().write(true).open(p)?;
        let mut got = 0u32;
        // SAFETY: хэндл открыт на запись; без буферов; синхронно.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle() as HANDLE,
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &raw mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn disk_size(p: &Path) -> io::Result<u64> {
        use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
        let w = wide(p);
        let mut high = 0u32;
        // SAFETY: строка с NUL; выходной — локальный u32.
        let low = unsafe { GetCompressedFileSizeW(w.as_ptr(), &raw mut high) };
        if low == u32::MAX {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(0) {
                return Err(e);
            }
        }
        Ok((u64::from(high) << 32) | u64::from(low))
    }

    pub fn make_junction(link: &Path, target: &Path) -> io::Result<()> {
        /// `IO_REPARSE_TAG_MOUNT_POINT`.
        const TAG: u32 = 0xA000_0003;
        let shown = super::shown_path(target);
        let subst: Vec<u16> = format!(r"\??\{shown}").encode_utf16().collect();
        let print: Vec<u16> = shown.encode_utf16().collect();
        let (sl, pl) = (subst.len() * 2, print.len() * 2);
        // Имена с NUL; ReparseDataLength — 8 байт заголовка точки монтирования и имена.
        let data_len = 8 + sl + 2 + pl + 2;
        let mut buf: Vec<u8> = Vec::with_capacity(8 + data_len);
        let u16le = |v: usize| u16::try_from(v).map(u16::to_le_bytes);
        let too_long = |_| io::Error::from(io::ErrorKind::InvalidInput);
        buf.extend_from_slice(&TAG.to_le_bytes());
        buf.extend_from_slice(&u16le(data_len).map_err(too_long)?);
        buf.extend_from_slice(&[0, 0]);
        buf.extend_from_slice(&u16le(0).map_err(too_long)?);
        buf.extend_from_slice(&u16le(sl).map_err(too_long)?);
        buf.extend_from_slice(&u16le(sl + 2).map_err(too_long)?);
        buf.extend_from_slice(&u16le(pl).map_err(too_long)?);
        for c in subst.iter().chain(&[0]).chain(&print).chain(&[0]) {
            buf.extend_from_slice(&c.to_le_bytes());
        }
        std::fs::create_dir(link)?;
        set_reparse(link, &buf).inspect_err(|_| {
            let _ = std::fs::remove_dir(link);
        })
    }

    pub fn local_now_text() -> String {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        // SAFETY: POD; GetLocalTime заполняет структуру.
        let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
        // SAFETY: указатель на локальную структуру.
        unsafe { GetLocalTime(&raw mut t) };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        )
    }

    pub fn local_time(
        y: u16,
        mo: u16,
        d: u16,
        h: u16,
        mi: u16,
        s: u16,
    ) -> Option<std::time::SystemTime> {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{
            SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime,
        };
        let local = SYSTEMTIME {
            wYear: y,
            wMonth: mo,
            wDayOfWeek: 0,
            wDay: d,
            wHour: h,
            wMinute: mi,
            wSecond: s,
            wMilliseconds: 0,
        };
        // SAFETY: POD.
        let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
        let mut ft = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: указатели на локальные структуры; синхронные вызовы.
        let ok = unsafe {
            TzSpecificLocalTimeToSystemTime(std::ptr::null(), &raw const local, &raw mut utc) != 0
                && SystemTimeToFileTime(&raw const utc, &raw mut ft) != 0
        };
        if !ok {
            return None;
        }
        let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
        let unix = ticks.checked_sub(116_444_736_000_000_000)?;
        std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_nanos(unix.checked_mul(100)?))
    }

    pub fn copy_link(src: &Path, dst: &Path, dir: bool) -> io::Result<()> {
        let data = reparse_data(src)?;
        let tag = data
            .get(..4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if tag == IO_REPARSE_TAG_SYMLINK {
            // Symlink — через std (без прав администратора в режиме
            // разработчика); относительная цель остаётся относительной.
            let target = std::fs::read_link(src)?;
            return if dir {
                std::os::windows::fs::symlink_dir(target, dst)
            } else {
                std::os::windows::fs::symlink_file(target, dst)
            };
        }
        // Junction и прочие теги — тем же буфером.
        if dir {
            std::fs::create_dir(dst)?;
        } else {
            File::create_new(dst)?;
        }
        set_reparse(dst, &data).inspect_err(|_| {
            let _ = if dir {
                std::fs::remove_dir(dst)
            } else {
                std::fs::remove_file(dst)
            };
        })
    }

    pub fn local_stamp() -> String {
        // SAFETY: структура на стеке заполняется системой.
        let t = unsafe {
            let mut t = std::mem::zeroed();
            GetLocalTime(&raw mut t);
            t
        };
        format!(
            "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        )
    }
}

/// Не Windows (S0-decisions №57: не собирается и не проверяется) — std.
#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::path::Path;

    use super::CopyFlags;

    pub fn copy_file(
        src: &Path,
        dst: &Path,
        flags: CopyFlags,
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> io::Result<()> {
        if flags.excl && std::fs::symlink_metadata(dst).is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let n = std::fs::copy(src, dst)?;
        progress(n);
        Ok(())
    }

    pub fn move_file(src: &Path, dst: &Path, replace: bool) -> io::Result<()> {
        if !replace && std::fs::symlink_metadata(dst).is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        std::fs::rename(src, dst)
    }

    pub fn open_dir_attrs(p: &Path) -> io::Result<std::fs::File> {
        std::fs::File::open(p)
    }

    pub fn attrs(_p: &Path) -> io::Result<u32> {
        Ok(0)
    }

    pub fn set_attrs(_p: &Path, _attrs: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn copy_link(src: &Path, dst: &Path, _dir: bool) -> io::Result<()> {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    }

    pub fn local_stamp() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            .to_string()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn old() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
    }

    /// Докачка (S5-27): копия с `restartable`, прерванная посреди, оставляет файл;
    /// повтор с теми же путями дописывает его и даёт точную копию.
    #[test]
    fn restartable_copy_resumes() {
        let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
        let (src, dst) = (d.path().join("big"), d.path().join("part"));
        let data: Vec<u8> = (0..(24u32 << 20)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let flags = CopyFlags {
            restartable: true,
            ..CopyFlags::default()
        };
        let e = copy_file(&src, &dst, flags, &mut |done| done < 8 << 20).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
        assert!(dst.exists(), "partial copy kept for resume");
        copy_file(&src, &dst, flags, &mut |_| true).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        assert_eq!(
            std::fs::metadata(&dst).unwrap().modified().unwrap(),
            std::fs::metadata(&src).unwrap().modified().unwrap()
        );
    }

    /// Признак завершённости (S5 §6.1): пока `CopyFileExW` пишет данные, время
    /// изменения назначения ещё не исходное — убитая посреди копия не
    /// выглядит готовой.
    #[test]
    fn mtime_is_set_after_data() {
        let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
        let (src, dst) = (d.path().join("big"), d.path().join("copy"));
        std::fs::write(&src, vec![7u8; 48 << 20]).unwrap();
        let f = std::fs::File::options().write(true).open(&src).unwrap();
        f.set_modified(old()).unwrap();
        drop(f);
        let mut seen = Vec::new();
        copy_file(&src, &dst, CopyFlags::default(), &mut |done| {
            if done < 40 << 20 {
                let m = std::fs::metadata(&dst).unwrap().modified().unwrap();
                seen.push(m == old());
            }
            true
        })
        .unwrap();
        assert!(!seen.is_empty(), "no chunk callbacks");
        assert!(
            seen.iter().all(|s| !s),
            "source mtime before data end: {seen:?}"
        );
        let m = std::fs::metadata(&dst).unwrap().modified().unwrap();
        assert_eq!(m, old());
        assert_eq!(std::fs::read(&dst).unwrap(), std::fs::read(&src).unwrap());
    }

    #[test]
    fn cancel_removes_partial_and_excl_refuses() {
        let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
        let (src, dst) = (d.path().join("big"), d.path().join("copy"));
        std::fs::write(&src, vec![1u8; 32 << 20]).unwrap();
        let e = copy_file(&src, &dst, CopyFlags::default(), &mut |_| false).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERR_ABORTED));
        assert!(!dst.exists(), "partial copy left behind");
        std::fs::write(&dst, b"x").unwrap();
        let flags = CopyFlags {
            excl: true,
            ..CopyFlags::default()
        };
        let e = copy_file(&src, &dst, flags, &mut |_| true).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&dst).unwrap(), b"x");
    }

    #[test]
    fn move_noreplace_and_replace() {
        let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let e = move_file(&a, &b, false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        move_file(&a, &b, true).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"a");
        assert!(!a.exists());
    }

    #[test]
    fn junction_is_copied_as_link() {
        let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
        let target = d.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("f"), b"1").unwrap();
        let link = d.path().join("link");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "mklink /J");
        let copy = d.path().join("copy");
        copy_link(&link, &copy, true).unwrap();
        assert!(
            std::fs::symlink_metadata(&copy)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(copy.join("f")).unwrap(), b"1");
        assert_eq!(attrs(&copy).unwrap() & 0x400, 0x400, "reparse point");
    }
}
