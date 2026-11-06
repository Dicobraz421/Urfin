//! `Vfs::resolve` по умолчанию (S8-01): тот же адрес, что давал `join` панели у `local` и
//! ops у схем.

use std::path::PathBuf;
use std::sync::Arc;

use urfin_vfs::{
    Attrs, Caps, Entry, EntryKind, ListOpts, ListStream, LocalFs, Metadata, Space, Vfs, VfsError,
    VfsPath, VfsRead, WatchGuard, WatchTx,
};

/// Схема без своей реализации `resolve`.
struct Scheme(&'static str);

const NO: VfsError = VfsError::Unsupported("test");

impl Vfs for Scheme {
    fn scheme(&self) -> &'static str {
        self.0
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        Caps::default()
    }
    fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Err(NO)
    }
    fn stat(&self, _: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        Err(NO)
    }
    fn stat_entry(&self, _: &VfsPath, _: &str, _: Attrs) -> Result<Entry, VfsError> {
        Err(NO)
    }
    fn open_read(&self, _: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        Err(NO)
    }
    fn space(&self, _: &VfsPath) -> Result<Space, VfsError> {
        Err(NO)
    }
    fn local_path(&self, _: &VfsPath) -> Option<PathBuf> {
        None
    }
    fn watch(&self, _: &VfsPath, _: WatchTx) -> Result<WatchGuard, VfsError> {
        Err(NO)
    }
}

fn entry(name: &str) -> Entry {
    Entry {
        name: name.into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind: EntryKind::File,
    }
}

fn sftp(path: &str) -> VfsPath {
    VfsPath {
        backend: "sftp".into(),
        authority: Some("srv1".into()),
        path: path.into(),
        outer: None,
    }
}

#[test]
fn resolve_local_is_the_panel_join() {
    let fs = LocalFs::new();
    let base = VfsPath::local(r"C:\Dev\urfin");
    assert_eq!(
        fs.resolve(&base, &entry("Cargo.toml")).unwrap(),
        VfsPath::local(r"C:\Dev\urfin\Cargo.toml")
    );
    let root = VfsPath::local(r"C:\");
    assert_eq!(
        fs.resolve(&root, &entry("a")).unwrap(),
        VfsPath::local(r"C:\a")
    );
}

#[cfg(windows)]
#[test]
fn resolve_local_decodes_escaped_names() {
    use std::os::windows::ffi::OsStringExt;

    let raw = std::ffi::OsString::from_wide(&[0x61, 0xD800]);
    let (name, escaped) = urfin_vfs::name::from_os(&raw);
    assert!(escaped);
    let mut e = entry(&name);
    e.attrs = Attrs(Attrs::ESCAPED_NAME);
    let got = LocalFs::new()
        .resolve(&VfsPath::local(r"C:\Dev"), &e)
        .unwrap();
    assert_eq!(got.path.file_name(), Some(raw.as_os_str()));
}

#[test]
fn resolve_sftp_is_the_ops_join() {
    let fs = Scheme("sftp");
    assert_eq!(
        fs.resolve(&sftp("/var/log"), &entry("syslog")).unwrap(),
        sftp("/var/log/syslog")
    );
    assert_eq!(fs.resolve(&sftp("/"), &entry("etc")).unwrap(), sftp("/etc"));
}

#[test]
fn resolve_keeps_the_container() {
    let inner = VfsPath {
        backend: "zip".into(),
        authority: None,
        path: "/dir".into(),
        outer: Some(Arc::new(VfsPath::local(r"C:\a.zip"))),
    };
    let got = Scheme("zip").resolve(&inner, &entry("f.txt")).unwrap();
    assert_eq!(got.path, PathBuf::from("/dir/f.txt"));
    assert_eq!(got.outer, inner.outer);
}
