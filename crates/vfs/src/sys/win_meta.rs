//! Windows: метаданные по хэндлу, место на томе, корни (S4 §3.1, S4-10).
//!
//! `stat`: `CreateFileW(FILE_READ_ATTRIBUTES, share all, BACKUP_SEMANTICS
//! [| OPEN_REPARSE_POINT])` и `GetFileInformationByHandleEx` (Basic, Standard,
//! `AttributeTag`, `Id`) — время изменения метаданных, размер на диске, число жёстких
//! ссылок, тег reparse и `FileId` без чтения содержимого. Место — `GetDiskFreeSpaceExW`.
//! Корни — `GetLogicalDrives`/`GetDriveTypeW` сразу, метки и место — в потоке на
//! диск (`roots` в `local`).

use std::mem::{MaybeUninit, size_of};
use std::path::Path;
use std::ptr;

use smallvec::SmallVec;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileAttributeTagInfo,
    FileBasicInfo, FileIdInfo, FileStandardInfo, GetDiskFreeSpaceExW, GetDriveTypeW,
    GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
};

use crate::api::{RootKind, Space, VfsError};

/// Сверх std: что даёт хэндл.
#[derive(Clone, Copy, Debug)]
pub struct Extra {
    pub change: i64,
    pub alloc: u64,
    pub nlink: u32,
    pub reparse_tag: Option<u32>,
    pub file_id: (u64, u128),
}

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

fn wide(p: &Path) -> SmallVec<[u16; 264]> {
    let mut b = SmallVec::new();
    crate::path::to_verbatim(p, &mut b);
    b
}

fn info<T>(
    h: windows_sys::Win32::Foundation::HANDLE,
    class: FILE_INFO_BY_HANDLE_CLASS,
) -> Option<T> {
    let mut v = MaybeUninit::<T>::uninit();
    // SAFETY: буфер — `T` нужного размера для класса; хэндл открыт.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx(
            h,
            class,
            v.as_mut_ptr().cast(),
            u32::try_from(size_of::<T>()).ok()?,
        )
    };
    // SAFETY: при успехе ОС заполнила структуру.
    (ok != 0).then(|| unsafe { v.assume_init() })
}

/// Метаданные по хэндлу; `None` — открыть не удалось (нет прав на атрибуты и т. п.):
/// тогда остаётся то, что дал std.
pub fn stat_extra(p: &Path, follow: bool) -> Option<Extra> {
    let path = wide(p);
    let flags = FILE_FLAG_BACKUP_SEMANTICS
        | if follow {
            0
        } else {
            FILE_FLAG_OPEN_REPARSE_POINT
        };
    // SAFETY: путь — NUL-терминированный UTF-16; хэндл закрывается ниже.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            flags,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let basic: Option<FILE_BASIC_INFO> = info(h, FileBasicInfo);
    let std_info: Option<FILE_STANDARD_INFO> = info(h, FileStandardInfo);
    let tag: Option<FILE_ATTRIBUTE_TAG_INFO> = info(h, FileAttributeTagInfo);
    let id: Option<FILE_ID_INFO> = info(h, FileIdInfo);
    // SAFETY: хэндл открыт выше и закрывается один раз.
    unsafe { CloseHandle(h) };
    Some(Extra {
        change: basic.map_or(i64::MIN, |b| filetime_nanos(b.ChangeTime)),
        alloc: std_info.map_or(0, |s| u64::try_from(s.AllocationSize).unwrap_or(0)),
        nlink: std_info.map_or(1, |s| s.NumberOfLinks),
        reparse_tag: tag
            .filter(|t| t.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .map(|t| t.ReparseTag),
        file_id: id.map_or((0, 0), |i| {
            (
                i.VolumeSerialNumber,
                u128::from_le_bytes(i.FileId.Identifier),
            )
        }),
    })
}

/// FILETIME (100 нс от 1601) → нс от Unix epoch.
fn filetime_nanos(t: i64) -> i64 {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    if t <= 0 {
        return i64::MIN;
    }
    (t - EPOCH_DIFF).saturating_mul(100)
}

/// Корень/каталог как обычный путь с `\` в конце (для `GetDiskFreeSpaceExW` и
/// `GetVolumeInformationW` verbatim-форма не нужна).
fn dir_wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
    if v.last() != Some(&u16::from(b'\\')) {
        v.push(u16::from(b'\\'));
    }
    v.push(0);
    v
}

/// Место на томе, где лежит `p`.
pub fn space(p: &Path) -> Result<Space, VfsError> {
    let path = dir_wide(p);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: путь NUL-терминирован, выходы — валидные `u64`.
    let ok = unsafe {
        GetDiskFreeSpaceExW(path.as_ptr(), &raw mut avail, &raw mut total, &raw mut free)
    };
    if ok == 0 {
        // SAFETY: без аргументов.
        let code = unsafe { GetLastError() };
        return Err(VfsError::from_io(
            p,
            std::io::Error::from_raw_os_error(code.cast_signed()),
        ));
    }
    Ok(Space { total, free, avail })
}

/// Буквы дисков (`C:\`) с типом — быстро, без обращения к томам.
pub fn drives() -> Vec<(String, RootKind)> {
    // SAFETY: без аргументов.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let root = format!("{}:\\", char::from(b'A' + i));
            let w: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
            // SAFETY: NUL-терминированная строка корня.
            let kind = match unsafe { GetDriveTypeW(w.as_ptr()) } {
                2 => RootKind::Removable,
                3 => RootKind::Fixed,
                4 => RootKind::Network,
                5 => RootKind::Optical,
                6 => RootKind::Ram,
                _ => RootKind::Other,
            };
            (root, kind)
        })
        .collect()
}

/// Буква диска пути (`C:\\…`, `\\\\?\\C:\\…`); UNC и прочее — `None`.
fn drive_letter(p: &Path) -> Option<u8> {
    use std::path::{Component, Prefix};
    let Some(Component::Prefix(pre)) = p.components().next() else {
        return None;
    };
    match pre.kind() {
        Prefix::Disk(l) | Prefix::VerbatimDisk(l) => Some(l),
        _ => None,
    }
}

/// `GetDriveTypeW` корня диска пути (2 съёмный, 3 несъёмный, 4 сеть, 6 RAM).
/// Вызов не ходит на диск.
fn drive_type(p: &Path) -> Option<u32> {
    let letter = drive_letter(p)?;
    let root: Vec<u16> = format!("{}:\\", char::from(letter))
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: NUL-терминированная строка корня.
    Some(unsafe { GetDriveTypeW(root.as_ptr()) })
}

/// Можно ли наблюдать каталог ради кэша: только локальные несъёмные диски
/// (`DRIVE_FIXED`, `DRIVE_RAMDISK`); UNC, сеть, съёмные — нет (S4 §5.5).
pub fn cache_watchable(p: &Path) -> bool {
    matches!(drive_type(p), Some(3 | 6))
}

/// Сеть: UNC или сетевой диск.
pub fn is_remote(p: &Path) -> bool {
    drive_letter(p).is_none() || drive_type(p) == Some(4)
}

/// Штраф позиционирования тома пути (`true` — HDD), по
/// `IOCTL_STORAGE_QUERY_PROPERTY(StorageDeviceSeekPenaltyProperty)`. Хэндл тома
/// без прав доступа — администратор не нужен. `None` — не узнать (сеть, RAID).
pub fn seek_penalty(p: &Path) -> Option<bool> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        STORAGE_PROPERTY_QUERY, StorageDeviceSeekPenaltyProperty,
    };
    let letter = drive_letter(p)?;
    let dev: Vec<u16> = format!("\\\\.\\{}:", char::from(letter))
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: NUL-терминированное имя устройства; хэндл закрывается ниже.
    let h = unsafe {
        CreateFileW(
            dev.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    // SAFETY: POD.
    let mut desc: DEVICE_SEEK_PENALTY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let mut got = 0u32;
    // SAFETY: буферы на стеке с верными размерами; синхронный вызов.
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&raw const query).cast(),
            u32::try_from(size_of::<STORAGE_PROPERTY_QUERY>()).unwrap_or(0),
            (&raw mut desc).cast(),
            u32::try_from(size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>()).unwrap_or(0),
            &raw mut got,
            ptr::null_mut(),
        )
    };
    // SAFETY: хэндл открыт выше.
    unsafe { CloseHandle(h) };
    (ok != 0 && got as usize >= size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>())
        .then_some(desc.IncursSeekPenalty)
}

/// Физическое устройство тома пути (`IOCTL_STORAGE_GET_DEVICE_NUMBER`): два раздела
/// одного диска — один ключ (S5-19). `None` — не узнать (сеть, RAID).
pub fn device_key(p: &Path) -> Option<u64> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER,
    };
    let letter = drive_letter(p)?;
    let dev: Vec<u16> = format!(r"\\.\{}:", char::from(letter))
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: NUL-терминированное имя устройства; хэндл закрывается ниже.
    let h = unsafe {
        CreateFileW(
            dev.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut out = STORAGE_DEVICE_NUMBER {
        DeviceType: 0,
        DeviceNumber: 0,
        PartitionNumber: 0,
    };
    let mut got = 0u32;
    // SAFETY: выходной буфер — `STORAGE_DEVICE_NUMBER` своего размера; синхронно.
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            ptr::null(),
            0,
            (&raw mut out).cast(),
            u32::try_from(size_of::<STORAGE_DEVICE_NUMBER>()).unwrap_or(0),
            &raw mut got,
            ptr::null_mut(),
        )
    };
    // SAFETY: хэндл открыт выше.
    unsafe { CloseHandle(h) };
    (ok != 0).then(|| (u64::from(out.DeviceType) << 32) | u64::from(out.DeviceNumber))
}

/// Метка тома (может ждать: сеть, оптический привод) — вызывается в своём потоке.
pub fn label(root: &Path) -> Option<String> {
    let path = dir_wide(root);
    let mut buf = [0u16; 261];
    // SAFETY: путь NUL-терминирован, буфер метки — 261 символ (MAX_PATH + 1).
    let ok = unsafe {
        GetVolumeInformationW(
            path.as_ptr(),
            buf.as_mut_ptr(),
            u32::try_from(buf.len()).ok()?,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// Том пути `p` (S5 §3.2 без IOCTL носителя — S5-02): `GetVolumePathNameW`,
/// `GetVolumeInformationW` (метка, ФС, флаги), `GetDriveTypeW`, `GetDiskFreeSpaceW`.
pub fn volume(p: &Path) -> Result<crate::volume::VolumeInfo, VfsError> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceW, GetVolumePathNameW};

    use crate::volume::{FsKind, Media, VolCaps, VolumeInfo};

    const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x1;
    const FILE_FILE_COMPRESSION: u32 = 0x10;
    const FILE_SUPPORTS_SPARSE_FILES: u32 = 0x40;
    const FILE_SUPPORTS_POSIX_UNLINK_RENAME: u32 = 0x400;
    const FILE_NAMED_STREAMS: u32 = 0x0004_0000;
    const FILE_SUPPORTS_BLOCK_REFCOUNTING: u32 = 0x0800_0000;

    let err = |p: &Path| {
        // SAFETY: без аргументов.
        let code = unsafe { GetLastError() };
        VfsError::from_io(p, std::io::Error::from_raw_os_error(code.cast_signed()))
    };
    let path: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = [0u16; 261];
    // SAFETY: путь NUL-терминирован, буфер — 261 символ.
    let ok = unsafe {
        GetVolumePathNameW(
            path.as_ptr(),
            root.as_mut_ptr(),
            u32::try_from(root.len()).unwrap_or(0),
        )
    };
    if ok == 0 {
        return Err(err(p));
    }
    let n = root.iter().position(|&c| c == 0).unwrap_or(root.len());
    let root_path = std::path::PathBuf::from(String::from_utf16_lossy(&root[..n]));
    let (mut label, mut fs_name) = ([0u16; 261], [0u16; 261]);
    let (mut serial, mut max_len, mut flags) = (0u32, 0u32, 0u32);
    // SAFETY: корень NUL-терминирован (из `GetVolumePathNameW`), буферы по 261.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            u32::try_from(label.len()).unwrap_or(0),
            &raw mut serial,
            &raw mut max_len,
            &raw mut flags,
            fs_name.as_mut_ptr(),
            u32::try_from(fs_name.len()).unwrap_or(0),
        )
    };
    if ok == 0 {
        return Err(err(&root_path));
    }
    let text = |b: &[u16]| {
        let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf16_lossy(&b[..n])
    };
    // SAFETY: NUL-терминированный корень.
    let media = match unsafe { GetDriveTypeW(root.as_ptr()) } {
        2 => Media::Usb,
        4 => Media::Net,
        _ => Media::Unknown,
    };
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: корень NUL-терминирован, выходы — валидные `u32`.
    let cluster = if unsafe {
        GetDiskFreeSpaceW(
            root.as_ptr(),
            &raw mut spc,
            &raw mut bps,
            &raw mut free,
            &raw mut total,
        )
    } != 0
    {
        spc.saturating_mul(bps)
    } else {
        0
    };
    let mut caps = 0;
    for (flag, cap) in [
        (FILE_SUPPORTS_BLOCK_REFCOUNTING, VolCaps::CLONE),
        (FILE_SUPPORTS_POSIX_UNLINK_RENAME, VolCaps::POSIX_DELETE),
        (FILE_SUPPORTS_SPARSE_FILES, VolCaps::SPARSE),
        (FILE_NAMED_STREAMS, VolCaps::ADS),
        (FILE_CASE_SENSITIVE_SEARCH, VolCaps::CASE_SENSITIVE),
        (FILE_FILE_COMPRESSION, VolCaps::COMPRESSION),
    ] {
        if flags & flag != 0 {
            caps |= cap;
        }
    }
    // Корзина — у локальных томов (на сетевых Проводник удаляет насовсем).
    if media != Media::Net {
        caps |= VolCaps::TRASH;
    }
    let fs_name = text(&fs_name);
    Ok(VolumeInfo {
        id: u64::from(serial),
        root: root_path,
        label: text(&label),
        fs: FsKind::from_name(&fs_name),
        fs_name,
        media,
        caps: VolCaps(caps),
        cluster,
    })
}

/// Пик памяти процесса: рабочий набор и private (байт) — для бенчей (S4-11).
pub fn peak_memory() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c = MaybeUninit::<PROCESS_MEMORY_COUNTERS_EX>::zeroed();
    let cb = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?;
    // SAFETY: псевдохэндл текущего процесса, буфер нужного размера.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), c.as_mut_ptr().cast(), cb) };
    if ok == 0 {
        return None;
    }
    // SAFETY: при успехе структура заполнена.
    let c = unsafe { c.assume_init() };
    Some((
        u64::try_from(c.PeakWorkingSetSize).unwrap_or(0),
        u64::try_from(c.PeakPagefileUsage).unwrap_or(0),
    ))
}
