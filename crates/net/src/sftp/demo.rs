//! Backend `sftp` сцен `--screens` (F03-06): папка `sftp://srv1/var/www/app` из памяти —
//! как на `screen-sftppanel.png`, с правами и владельцами. Сети нет: снимки одинаковы на
//! любой машине. В обычном запуске не регистрируется.

use std::path::PathBuf;

use urfin_vfs::{
    Attrs, CancelToken, Caps, ColumnSpec, Entry, EntryKind, ExtCell, ListOpts, ListStream,
    Metadata, Space, Vfs, VfsError, VfsPath, VfsRead, WatchGuard, WatchTx,
};

use super::ext::COLUMNS;
use crate::uri::{SCHEME, SftpUri};

/// Строка папки: имя, вид, размер, изменён (секунды Unix), права, владелец.
type Row = (
    &'static str,
    EntryKind,
    u64,
    i64,
    &'static str,
    &'static str,
);

/// `sftp://srv1/var/www/app` (`screen-sftppanel`, `screen-sftplost`).
const APP: &[Row] = &[
    (
        ".well-known",
        EntryKind::Dir,
        0,
        1_786_709_400,
        "drwxr-xr-x",
        "www-data",
    ),
    (
        "current",
        EntryKind::LinkDir,
        0,
        1_792_694_400,
        "lrwxrwxrwx",
        "deploy",
    ),
    (
        "releases",
        EntryKind::Dir,
        0,
        1_792_694_340,
        "drwxr-xr-x",
        "deploy",
    ),
    (
        "repo",
        EntryKind::Dir,
        0,
        1_792_694_280,
        "drwxr-xr-x",
        "deploy",
    ),
    (
        "shared",
        EntryKind::Dir,
        0,
        1_788_426_900,
        "drwxr-x---",
        "www-data",
    ),
    (
        "tmp",
        EntryKind::Dir,
        0,
        1_792_746_120,
        "drwxrwxr-x",
        "www-data",
    ),
    (
        ".env",
        EntryKind::File,
        1_126,
        1_788_426_960,
        "-rw-------",
        "www-data",
    ),
    (
        "deploy.sh",
        EntryKind::File,
        2_458,
        1_791_736_200,
        "-rwxr-xr-x",
        "deploy",
    ),
    (
        "maintenance.html",
        EntryKind::File,
        3_891,
        1_786_709_520,
        "-rw-r--r--",
        "www-data",
    ),
    (
        "revisions.log",
        EntryKind::File,
        19_046,
        1_792_694_400,
        "-rw-r--r--",
        "deploy",
    ),
    (
        "robots.txt",
        EntryKind::File,
        68,
        1_786_709_520,
        "-rw-r--r--",
        "www-data",
    ),
    (
        "VERSION",
        EntryKind::File,
        7,
        1_792_694_400,
        "-rw-r--r--",
        "deploy",
    ),
];

/// Папка сцен по адресу; прочие — пустые.
fn rows(dir: &VfsPath) -> &'static [Row] {
    match SftpUri::from_location(dir) {
        Some(u) if u.profile.eq_ignore_ascii_case("srv1") && u.path == "/var/www/app" => APP,
        _ => &[],
    }
}

/// Время макета — московское (стенд владельца, UTC+3): секунды строк — UTC.
const MSK: i64 = 3 * 3600;

fn entry(r: &Row) -> Entry {
    let (name, kind, size, mtime, perms, _) = *r;
    let mut attrs = 0;
    if name.starts_with('.') {
        attrs |= Attrs::DOTFILE;
    }
    if kind == EntryKind::File && perms.as_bytes().get(3) == Some(&b'x') {
        attrs |= Attrs::EXEC;
    }
    Entry {
        name: name.into(),
        size,
        mtime: (mtime - MSK) * 1_000_000_000,
        ctime: i64::MIN,
        atime: i64::MIN,
        attrs: Attrs(attrs),
        kind,
    }
}

/// Backend `sftp` сцен: листинг, права и владельцы — из [`APP`].
#[derive(Debug, Default)]
pub struct DemoFs;

impl DemoFs {
    pub fn new() -> DemoFs {
        DemoFs
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

impl Vfs for DemoFs {
    fn scheme(&self) -> &'static str {
        SCHEME
    }

    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps(Caps::SEEK | Caps::CASE_SENSITIVE)
    }

    fn list(&self, dir: &VfsPath, _opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Ok(Box::new(Once(Some(rows(dir).iter().map(entry).collect()))))
    }

    fn stat(&self, p: &VfsPath, _follow: bool) -> Result<Metadata, VfsError> {
        Err(VfsError::NotFound(p.path.clone()))
    }

    fn stat_entry(&self, dir: &VfsPath, name: &str, _hint: Attrs) -> Result<Entry, VfsError> {
        rows(dir)
            .iter()
            .find(|r| r.0 == name)
            .map(entry)
            .ok_or_else(|| VfsError::NotFound(PathBuf::from(name)))
    }

    fn open_read(&self, _p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        Err(VfsError::Unsupported("screens"))
    }

    fn space(&self, _at: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("screens"))
    }

    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }

    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Ok(WatchGuard::new(()))
    }

    fn columns(&self, _dir: &VfsPath) -> &'static [ColumnSpec] {
        &COLUMNS
    }

    fn ext(&self, dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        let r = rows(dir).iter().find(|r| r.0 == e.name.as_str())?;
        let text = match col {
            "perms" => r.4,
            "owner" => r.5,
            _ => return None,
        };
        Some(ExtCell::Text(text.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_app_folder_with_cells() {
        let fs = DemoFs::new();
        let dir = SftpUri::new("srv1", "/var/www/app").to_location();
        let mut out = Vec::new();
        let mut s = fs.list(&dir, &ListOpts::default()).unwrap();
        let t = CancelToken::new();
        while s.next_batch(&mut out, &t).unwrap() {}
        assert_eq!(out.len(), 12);
        let sh = out.iter().find(|e| e.name == "deploy.sh").unwrap();
        assert!(sh.attrs.has(Attrs::EXEC));
        assert_eq!(
            fs.ext(&dir, sh, "owner"),
            Some(ExtCell::Text("deploy".into()))
        );
        let other = SftpUri::new("srv1", "/etc").to_location();
        assert_eq!(fs.ext(&other, sh, "perms"), None);
    }
}
