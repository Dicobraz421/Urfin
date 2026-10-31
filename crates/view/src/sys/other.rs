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
    let m = file.metadata()?;
    Ok((m.dev(), u128::from(m.ino())))
}
