//! Открытие файла под чтение подряд (хэши дубликатов, F01): подсказка кэшу ОС.

use std::fs::File;
use std::io;
use std::path::Path;

/// `FILE_FLAG_SEQUENTIAL_SCAN`: чтение подряд, кэш ОС не держит прочитанное.
#[cfg(windows)]
const SEQUENTIAL_SCAN: u32 = 0x0800_0000;

/// Файл на чтение с подсказкой «читаем подряд».
#[cfg(windows)]
pub(crate) fn open_seq(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(SEQUENTIAL_SCAN)
        .open(path)
}

/// Файл на чтение с подсказкой «читаем подряд» (`posix_fadvise(SEQUENTIAL)`).
/// Подсказка необязательна: её отказ (ФС без поддержки) чтению не мешает.
#[cfg(unix)]
pub(crate) fn open_seq(path: &Path) -> io::Result<File> {
    let f = File::open(path)?;
    let _ = rustix::fs::fadvise(&f, 0, None, rustix::fs::Advice::Sequential);
    Ok(f)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::open_seq;

    #[test]
    fn open_seq_reads_file_and_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        std::fs::write(&p, b"urfin").unwrap();
        let mut s = String::new();
        open_seq(&p).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "urfin");
        let err = open_seq(&dir.path().join("нет")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
