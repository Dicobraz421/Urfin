//! `--bench-sftp` (F03-06): сценарий `--bench` на панели `sftp://` без сети — папка из
//! 10k строк с правами и владельцами из памяти. Меряется отрисовка панели; сети, пинга и
//! переподключения в замере нет. В обычном запуске не регистрируется.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use urfin_core::Location;
use urfin_net::SftpUri;
use urfin_vfs::{
    Attrs, CancelToken, Caps, ColumnSpec, Entry, EntryKind, ExtCell, ListOpts, ListStream,
    Metadata, Space, Vfs, VfsError, VfsPath, VfsRead, WatchGuard, WatchTx,
};

const ROWS: u32 = 10_000;
/// Каталог на каждую 20-ю строку, остальное — файлы.
const DIR_EVERY: u32 = 20;
/// 2026-10-01 12:00 UTC.
const MTIME: i64 = 1_790_856_000;

static ON: AtomicBool = AtomicBool::new(false);

/// Замер на панели `sftp://`: backend регистрируется вместо настоящего.
pub(crate) fn enable() {
    ON.store(true, Ordering::Relaxed);
}

pub(crate) fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Папка замера.
pub(crate) fn location() -> Location {
    SftpUri::new("srv1", "/var/www/app").to_location()
}

fn entry(n: u32) -> Entry {
    let dir = n.is_multiple_of(DIR_EVERY);
    Entry {
        name: if dir {
            format!("release-{n:05}").into()
        } else {
            format!("access-{n:05}.log").into()
        },
        size: if dir { 0 } else { u64::from(n) * 1021 },
        mtime: (MTIME + i64::from(n) * 61) * 1_000_000_000,
        ctime: i64::MIN,
        atime: i64::MIN,
        attrs: Attrs(0),
        kind: if dir { EntryKind::Dir } else { EntryKind::File },
    }
}

struct Once(Option<Vec<Entry>>);

impl ListStream for Once {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        match self.0.take() {
            Some(b) => {
                out.extend(b);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// Backend `sftp` замера.
#[derive(Debug)]
pub(crate) struct BenchFs;

impl Vfs for BenchFs {
    fn scheme(&self) -> &'static str {
        urfin_net::uri::SCHEME
    }

    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps(Caps::SEEK | Caps::CASE_SENSITIVE)
    }

    fn list(&self, _dir: &VfsPath, _opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Ok(Box::new(Once(Some((0..ROWS).map(entry).collect()))))
    }

    fn stat(&self, p: &VfsPath, _follow: bool) -> Result<Metadata, VfsError> {
        Err(VfsError::NotFound(p.path.clone()))
    }

    fn stat_entry(&self, _dir: &VfsPath, name: &str, _hint: Attrs) -> Result<Entry, VfsError> {
        Err(VfsError::NotFound(PathBuf::from(name)))
    }

    fn open_read(&self, _p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        Err(VfsError::Unsupported("bench"))
    }

    fn space(&self, _at: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("bench"))
    }

    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }

    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Ok(WatchGuard::new(()))
    }

    fn columns(&self, _dir: &VfsPath) -> &'static [ColumnSpec] {
        &urfin_net::sftp::ext::COLUMNS
    }

    fn ext(&self, _dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        let text = match (col, e.kind) {
            ("perms", EntryKind::Dir) => "drwxr-xr-x",
            ("perms", _) => "-rw-r-----",
            ("owner", _) => "www-data",
            _ => return None,
        };
        Some(ExtCell::Text(text.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_unique() {
        let names: std::collections::HashSet<_> = (0..ROWS).map(|n| entry(n).name).collect();
        assert_eq!(names.len(), ROWS as usize);
    }
}
