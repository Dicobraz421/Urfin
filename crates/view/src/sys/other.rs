//! Не Windows: только чтобы крейт собирался в проверках; продукт — только Windows (№57).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use memmap2::Mmap;

pub(crate) fn map(file: &File) -> io::Result<Mmap> {
    // SAFETY: как в `windows.rs`: срез читается как `&[u8]`, без инвариантов над
    // содержимым; политика §0 не отображает сетевые и съёмные тома.
    unsafe { Mmap::map(file) }
}

pub(crate) fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    file.read_at(buf, off)
}

pub(crate) fn write_all_at(file: &File, buf: &[u8], off: u64) -> io::Result<()> {
    file.write_all_at(buf, off)
}

pub(crate) fn open_spool(path: &Path) -> io::Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    std::fs::remove_file(path)?;
    Ok(f)
}

pub(crate) fn open_seq(path: &Path) -> io::Result<File> {
    File::open(path)
}

pub(crate) fn file_id(file: &File) -> io::Result<(u64, u128)> {
    file_info(file).map(|i| i.id)
}

pub(crate) fn file_info(file: &File) -> io::Result<super::FileInfo> {
    let m = file.metadata()?;
    Ok(super::FileInfo {
        id: (m.dev(), u128::from(m.ino())),
        nlink: u32::try_from(m.nlink()).unwrap_or(u32::MAX),
        attrs: u32::from(m.permissions().readonly()),
    })
}

pub(crate) fn replace_file(target: &Path, temp: &Path) -> io::Result<()> {
    std::fs::rename(temp, target)
}

pub(crate) fn move_replace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

pub(crate) fn attrs(path: &Path) -> io::Result<u32> {
    Ok(u32::from(std::fs::metadata(path)?.permissions().readonly()))
}

pub(crate) fn set_attrs(path: &Path, attrs: u32) -> io::Result<()> {
    let mut p = std::fs::metadata(path)?.permissions();
    p.set_readonly(attrs & 1 != 0);
    std::fs::set_permissions(path, p)
}
