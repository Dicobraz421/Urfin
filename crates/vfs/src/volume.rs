//! Сведения о томе (S0 v2 №12, S5 §3.2): корень, ФС, носитель, возможности, кластер,
//! метка. API — S4; носитель (NVMe/SSD/HDD/USB/сеть) на Windows — по типу диска и
//! `IOCTL_STORAGE_QUERY_PROPERTY` (S5-28, `media_from`); на unix — пока сеть по ФС.
//! Вызов ходит в ОС (сеть может отвечать секунды) — только из фоновой задачи.

use std::path::{Path, PathBuf};

use crate::api::{Space, VfsError};

/// Файловая система тома. В кэше анализа места — индексом варианта (postcard):
/// новые — только в конец.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FsKind {
    Ntfs,
    Refs,
    Fat32,
    ExFat,
    Apfs,
    Hfs,
    Ext4,
    Btrfs,
    Xfs,
    Zfs,
    Smb,
    Nfs,
    Fuse,
    Other,
}

impl FsKind {
    /// По имени ФС из ОС (`NTFS`, `exFAT`, `ext4`…).
    pub fn from_name(name: &str) -> FsKind {
        match name.to_ascii_lowercase().as_str() {
            "ntfs" => FsKind::Ntfs,
            "refs" => FsKind::Refs,
            "fat32" | "fat" | "vfat" | "msdos" => FsKind::Fat32,
            "exfat" => FsKind::ExFat,
            "apfs" => FsKind::Apfs,
            "hfs" | "hfs+" => FsKind::Hfs,
            "ext4" | "ext3" | "ext2" => FsKind::Ext4,
            "btrfs" => FsKind::Btrfs,
            "xfs" => FsKind::Xfs,
            "zfs" => FsKind::Zfs,
            "cifs" | "smb3" | "smbfs" => FsKind::Smb,
            "nfs" | "nfs4" => FsKind::Nfs,
            n if n.starts_with("fuse") => FsKind::Fuse,
            _ => FsKind::Other,
        }
    }
}

/// Носитель.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Media {
    Nvme,
    Ssd,
    Hdd,
    Net,
    Usb,
    Unknown,
}

/// Возможности тома.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct VolCaps(pub u32);

impl VolCaps {
    pub const CLONE: u32 = 1;
    pub const POSIX_DELETE: u32 = 2;
    pub const SPARSE: u32 = 4;
    pub const ADS: u32 = 8;
    pub const XATTR: u32 = 16;
    pub const CASE_SENSITIVE: u32 = 32;
    pub const TRASH: u32 = 64;
    pub const COMPRESSION: u32 = 128;

    pub fn has(self, f: u32) -> bool {
        self.0 & f == f
    }
}

/// Том, на котором лежит путь.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Windows: серийный номер тома; unix: `st_dev`.
    pub id: u64,
    pub root: PathBuf,
    /// Метка («Локальный диск» — нет: пустая метка остаётся пустой).
    pub label: String,
    pub fs: FsKind,
    /// Имя ФС, как его даёт ОС.
    pub fs_name: String,
    pub media: Media,
    pub caps: VolCaps,
    pub cluster: u32,
}

/// Том пути `p`. Медленно на сети — только из фоновой задачи.
pub fn of(p: &Path) -> Result<VolumeInfo, VfsError> {
    #[cfg(windows)]
    {
        crate::sys::win_meta::volume(p)
    }
    #[cfg(not(windows))]
    {
        unix::volume(p)
    }
}

/// Потоков обхода для размера каталогов (S4 §5.6, решение №6): задано в
/// конфиге (`configured` больше нуля) — как задано; иначе по носителю
/// (`auto_threads`). Ходит в ОС (IOCTL тома) — только из фоновой задачи.
pub fn walk_threads(configured: u8, loc: &urfin_core::Location) -> usize {
    if configured > 0 {
        return configured.into();
    }
    if loc.backend != "local" {
        return 1;
    }
    #[cfg(windows)]
    {
        auto_threads(crate::sys::win_meta::media(&loc.path))
    }
    #[cfg(not(windows))]
    {
        auto_threads(Media::Unknown)
    }
}

/// Потоков обхода по носителю (`[du] threads = 0`): сеть и HDD — 1, `NVMe` — 8
/// (S5-28), SSD, USB и неизвестное — 4.
pub fn auto_threads(m: Media) -> usize {
    match m {
        Media::Net | Media::Hdd => 1,
        Media::Nvme => 8,
        Media::Ssd | Media::Usb | Media::Unknown => 4,
    }
}

/// `STORAGE_BUS_TYPE` (winioctl.h): шины, по которым узнаётся носитель.
pub mod bus {
    pub const USB: u8 = 7;
    pub const SD: u8 = 12;
    pub const MMC: u8 = 13;
    pub const NVME: u8 = 17;
}

/// Носитель по ответам ОС (S5 §3.2, S5-28): `remote` — сетевой диск или UNC,
/// `removable` — `DRIVE_REMOVABLE`, `seek` — `IncursSeekPenalty`, `bus` —
/// `STORAGE_ADAPTER_DESCRIPTOR.BusType`. Порядок — от худшего класса: сеть, HDD
/// (в том числе USB-HDD), съёмные и USB/SD-шина, `NVMe`, SSD; иначе неизвестно.
pub fn media_from(remote: bool, removable: bool, seek: Option<bool>, bus: Option<u8>) -> Media {
    if remote {
        Media::Net
    } else if seek == Some(true) {
        Media::Hdd
    } else if removable || matches!(bus, Some(bus::USB | bus::SD | bus::MMC)) {
        Media::Usb
    } else if bus == Some(bus::NVME) {
        Media::Nvme
    } else if seek == Some(false) {
        Media::Ssd
    } else {
        Media::Unknown
    }
}

/// Физическое устройство пути для очереди устройств (S5-19): два раздела одного
/// диска — один ключ. `None` — не узнать (сеть, RAID); тогда ключ — id тома. Ходит в
/// ОС — только из фоновой задачи.
pub fn device_key(p: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        crate::sys::win_meta::device_key(p)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        None
    }
}

/// Сколько job одновременно на устройстве с таким носителем (`[ops.device_jobs]`).
pub fn jobs_for(jobs: &urfin_core::DeviceJobs, m: Media) -> u8 {
    match m {
        Media::Hdd => jobs.hdd,
        Media::Net => jobs.net,
        Media::Usb => jobs.usb,
        Media::Nvme | Media::Ssd | Media::Unknown => jobs.ssd,
    }
}

/// Место на томе пути `p` (то же, что `Vfs::space` у локальной ФС).
pub fn space(p: &Path) -> Result<Space, VfsError> {
    crate::local::space_of(p)
}

#[cfg(not(windows))]
mod unix {
    use std::path::{Path, PathBuf};

    use super::{FsKind, Media, VolCaps, VolumeInfo};
    use crate::api::VfsError;

    /// Точка монтирования и ФС — по самому длинному префиксу из `/proc/self/mounts`
    /// (Linux); на macOS — `/` или `/Volumes/<имя>`.
    pub(super) fn volume(p: &Path) -> Result<VolumeInfo, VfsError> {
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::metadata(p).map_err(|e| VfsError::from_io(p, e))?;
        let (root, fs_name) = mount_of(p);
        let fs = FsKind::from_name(&fs_name);
        let media = match fs {
            FsKind::Smb | FsKind::Nfs => Media::Net,
            _ => Media::Unknown,
        };
        let mut caps = VolCaps::XATTR | VolCaps::POSIX_DELETE | VolCaps::SPARSE;
        if cfg!(target_os = "linux") {
            caps |= VolCaps::CASE_SENSITIVE;
        }
        if media != Media::Net {
            caps |= VolCaps::TRASH;
        }
        Ok(VolumeInfo {
            id: md.dev(),
            label: root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            root,
            fs,
            fs_name,
            media,
            caps: VolCaps(caps),
            cluster: u32::try_from(md.blksize()).unwrap_or(4096),
        })
    }

    fn mount_of(p: &Path) -> (PathBuf, String) {
        let mut best = (PathBuf::from("/"), String::new());
        if let Ok(text) = std::fs::read_to_string("/proc/self/mounts") {
            for line in text.lines() {
                let mut f = line.split_whitespace();
                let (Some(_), Some(at), Some(fs)) = (f.next(), f.next(), f.next()) else {
                    continue;
                };
                let at = PathBuf::from(at.replace("\\040", " "));
                if p.starts_with(&at) && at.components().count() >= best.0.components().count() {
                    best = (at, fs.to_owned());
                }
            }
        } else if p.starts_with("/Volumes")
            && let Some(name) = p.components().nth(2)
        {
            best.0 = Path::new("/Volumes").join(name);
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::{Media, auto_threads, bus, media_from};

    /// S5-28: ответы системы → вид носителя.
    #[test]
    fn media_from_answers() {
        let cases = [
            ((true, false, None, None), Media::Net),
            ((true, false, Some(false), Some(bus::NVME)), Media::Net),
            ((false, false, Some(true), Some(11)), Media::Hdd),
            ((false, false, Some(true), Some(bus::USB)), Media::Hdd),
            ((false, true, None, None), Media::Usb),
            ((false, false, Some(false), Some(bus::USB)), Media::Usb),
            ((false, false, None, Some(bus::SD)), Media::Usb),
            ((false, false, Some(false), Some(bus::NVME)), Media::Nvme),
            ((false, false, None, Some(bus::NVME)), Media::Nvme),
            ((false, false, Some(false), Some(11)), Media::Ssd),
            ((false, false, None, Some(8)), Media::Unknown),
            ((false, false, None, None), Media::Unknown),
        ];
        for ((remote, removable, seek, b), want) in cases {
            assert_eq!(
                media_from(remote, removable, seek, b),
                want,
                "{remote} {removable} {seek:?} {b:?}"
            );
        }
    }

    /// S5-28: `[du] threads = 0` — на `NVMe` 8 потоков.
    #[test]
    fn walk_threads_by_media() {
        assert_eq!(auto_threads(Media::Nvme), 8);
        assert_eq!(auto_threads(Media::Ssd), 4);
        assert_eq!(auto_threads(Media::Unknown), 4);
        assert_eq!(auto_threads(Media::Usb), 4);
        assert_eq!(auto_threads(Media::Hdd), 1);
        assert_eq!(auto_threads(Media::Net), 1);
        let loc = urfin_core::Location::local(std::env::temp_dir());
        assert_eq!(super::walk_threads(3, &loc), 3);
    }
}
