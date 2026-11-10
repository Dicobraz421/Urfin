//! ОС для датасетов: разреженный файл (V3). Без `unsafe` и зависимостей — системный
//! `fsutil` (есть в любой Windows, прав администратора не нужно).

use std::io;
use std::path::Path;

#[cfg(windows)]
fn fsutil(args: &[&std::ffi::OsStr]) -> io::Result<String> {
    let exe = std::env::var_os("SystemRoot").map_or_else(
        || "fsutil".into(),
        |r| Path::new(&r).join(r"System32\fsutil.exe"),
    );
    let out = std::process::Command::new(exe).args(args).output()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "fsutil {:?}: {} {}",
            args,
            text.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(text)
}

/// Пометить пустой файл разреженным; не вышло — ошибка (иначе 4,5 ГБ лягут на диск).
#[cfg(windows)]
pub fn set_sparse(path: &Path) -> io::Result<()> {
    fsutil(&["sparse".as_ref(), "setflag".as_ref(), path.as_os_str()])?;
    if is_sparse(path)? {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{}: файловая система не дала разреженный файл",
            path.display()
        )))
    }
}

/// Unix: `set_len` и так не выделяет блоки.
#[cfg(not(windows))]
pub fn set_sparse(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn is_sparse(path: &Path) -> io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
    Ok(std::fs::metadata(path)?.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE != 0)
}

#[cfg(not(windows))]
pub fn is_sparse(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path)?;
    Ok(m.blocks() * 512 < m.len())
}

/// Выделено на диске: сумма длин из `fsutil sparse queryrange` (последнее
/// шестнадцатеричное число строки — длина; от языка системы не зависит).
#[cfg(windows)]
pub fn allocated(path: &Path) -> io::Result<u64> {
    if !is_sparse(path)? {
        return Ok(std::fs::metadata(path)?.len());
    }
    let text = fsutil(&["sparse".as_ref(), "queryrange".as_ref(), path.as_os_str()])?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let hex = l.rsplit("0x").next().filter(|_| l.contains("0x"))?;
            let digits: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
            u64::from_str_radix(&digits, 16).ok()
        })
        .sum())
}

#[cfg(not(windows))]
pub fn allocated(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.blocks() * 512)
}
