//! Сведения о томе (S0 v2 №12, S5 §3.2): корень, ФС, носитель, возможности, кластер,
//! метка. API — S4; определение носителя через `IOCTL_STORAGE_QUERY_PROPERTY`
//! (NVMe/SSD/HDD) — задача S5-02, пока `Media` различает только сеть и съёмные.
//! Вызов ходит в ОС (сеть может отвечать секунды) — только из фоновой задачи.

use std::path::{Path, PathBuf};

use crate::api::{Space, VfsError};

/// Файловая система тома.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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
/// конфиге (`configured` больше нуля) — как задано; иначе сеть и HDD — 1, SSD и
/// неизвестное — 4. Ходит в ОС (IOCTL тома) — только из фоновой задачи.
pub fn walk_threads(configured: u8, loc: &urfin_core::Location) -> usize {
    if configured > 0 {
        return configured.into();
    }
    if loc.backend != "local" {
        return 1;
    }
    #[cfg(windows)]
    {
        let p = &loc.path;
        if crate::sys::win_meta::is_remote(p) || crate::sys::win_meta::seek_penalty(p) == Some(true)
        {
            return 1;
        }
    }
    4
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
