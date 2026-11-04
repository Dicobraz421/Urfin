//! Backend `sftp` сцен `--screens` (F03-06): папка `sftp://srv1/var/www/app` из памяти —
//! как на `screen-sftppanel.png`, с правами и владельцами; `app.conf` для правки
//! `screen-sftpedit` (F03-08) — чтение из памяти, запись в никуда. Сети нет: снимки
//! одинаковы на любой машине. В обычном запуске не регистрируется.

use std::path::PathBuf;

use urfin_vfs::{
    Attrs, CancelToken, Caps, ColumnSpec, Entry, EntryKind, ExtCell, ListOpts, ListStream,
    Metadata, Space, Vfs, VfsError, VfsPath, VfsRead, VfsWrite, WatchGuard, WatchTx,
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

/// `sftp://srv1/var/www/app/releases` (`screen-sftpcopy`, F03-07).
#[rustfmt::skip]
const RELEASES: &[Row] = &[
    ("39", EntryKind::Dir, 0, 1_790_954_400, "drwxr-xr-x", "deploy"),
    ("40", EntryKind::Dir, 0, 1_791_565_500, "drwxr-xr-x", "deploy"),
    ("41", EntryKind::Dir, 0, 1_792_174_320, "drwxr-xr-x", "deploy"),
    ("42", EntryKind::Dir, 0, 1_792_694_340, "drwxr-xr-x", "deploy"),
    ("43", EntryKind::Dir, 0, 1_792_753_440, "drwxr-xr-x", "admin"),
];

/// `sftp://srv1/var/www/app/current` (`screen-sftpsync`, F03-07): против локальной `dist`
/// сцен — новее `index.html` и `app.4f2a91c.*` слева, «только здесь» — `app.9b1e07d.*`.
#[rustfmt::skip]
const CURRENT: &[Row] = &[
    ("assets", EntryKind::Dir, 0, 1_792_694_340, "drwxr-xr-x", "deploy"),
    ("fonts", EntryKind::Dir, 0, 1_788_358_800, "drwxr-xr-x", "deploy"),
    ("img", EntryKind::Dir, 0, 1_791_972_300, "drwxr-xr-x", "deploy"),
    ("404.html", EntryKind::File, 1638, 1_788_358_800, "-rw-r--r--", "deploy"),
    ("app.9b1e07d.css", EntryKind::File, 86_016, 1_792_694_340, "-rw-r--r--", "deploy"),
    ("app.9b1e07d.js", EntryKind::File, 1_250_000, 1_792_694_340, "-rw-r--r--", "deploy"),
    ("favicon.ico", EntryKind::File, 15_086, 1_788_358_800, "-rw-r--r--", "deploy"),
    ("index.html", EntryKind::File, 2048, 1_792_694_340, "-rw-r--r--", "deploy"),
    ("manifest.webmanifest", EntryKind::File, 612, 1_788_358_800, "-rw-r--r--", "deploy"),
    ("robots.txt", EntryKind::File, 68, 1_788_358_800, "-rw-r--r--", "deploy"),
];

/// `sftp://srv1/etc/nginx/sites-available` (`screen-sftpedit`, F03-08).
#[rustfmt::skip]
const SITES: &[Row] = &[
    ("api.conf", EntryKind::File, 1_412, 1_791_736_200, "-rw-r--r--", "root"),
    ("app.conf", EntryKind::File, APP_CONF.len() as u64, 1_792_694_400, "-rw-r-----", "root"),
    ("default", EntryKind::File, 2_088, 1_786_709_400, "-rw-r--r--", "root"),
];

/// `app.conf` сцены `screen-sftpedit` — как на макете; правка сцены — `10m` → `25m`.
const APP_CONF: &str = r"# app.corp.local — фронтенд и API
upstream app_backend {
    server 127.0.0.1:8080 max_fails=3 fail_timeout=10s;
    keepalive 32;
}

server {
    listen 80;
    server_name app.corp.local;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    server_name app.corp.local;

    ssl_certificate     /etc/ssl/corp/app.crt;
    ssl_certificate_key /etc/ssl/corp/app.key;
    ssl_protocols       TLSv1.2 TLSv1.3;

    root /var/www/app/current;
    index index.html;
    client_max_body_size 10m;

    location / {
        try_files $uri $uri/ /index.html;
    }

    location /api/ {
        proxy_pass http://app_backend;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 60s;
    }

    location ~* \.(js|css|woff2)$ {
        expires 30d;
        access_log off;
    }
}
";

/// Папка сцен по адресу; прочие — пустые.
fn rows(dir: &VfsPath) -> &'static [Row] {
    match SftpUri::from_location(dir) {
        Some(u) if u.profile.eq_ignore_ascii_case("srv1") => match u.path.as_str() {
            "/var/www/app" => APP,
            "/var/www/app/releases" => RELEASES,
            "/var/www/app/current" => CURRENT,
            "/etc/nginx/sites-available" => SITES,
            _ => &[],
        },
        _ => &[],
    }
}

/// Строка файла по адресу (родитель — папка сцен).
fn row_of(p: &VfsPath) -> Option<&'static Row> {
    let parent = p.parent()?;
    let name = p.file_name()?.to_str()?;
    rows(&parent).iter().find(|r| r.0 == name)
}

/// Содержимое файла сцен: `app.conf`, у прочих — пусто.
fn data_of(r: &Row) -> &'static [u8] {
    if r.0 == "app.conf" {
        APP_CONF.as_bytes()
    } else {
        &[]
    }
}

/// Права из строки `-rw-r-----`.
fn mode_of(perms: &str) -> u32 {
    perms
        .bytes()
        .skip(1)
        .take(9)
        .fold(0, |m, b| (m << 1) | u32::from(b != b'-'))
}

/// Чтение файла сцен из памяти.
struct DemoRead(std::io::Cursor<&'static [u8]>);

impl std::io::Read for DemoRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl std::io::Seek for DemoRead {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

impl VfsRead for DemoRead {
    fn len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

/// Запись сцен (закачка `screen-sftpedit`): байты уходят в никуда; `finish` — как
/// `fsync` на сервере, 0,2 с (итог строки «готово · … · 0,2 с»).
struct DemoWrite;

impl std::io::Write for DemoWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl VfsWrite for DemoWrite {
    fn finish(self: Box<Self>) -> Result<(), VfsError> {
        std::thread::sleep(std::time::Duration::from_millis(200));
        Ok(())
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
        let r = row_of(p).ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        let e = entry(r);
        Ok(Metadata {
            kind: e.kind,
            attrs: e.attrs,
            size: e.size,
            alloc_size: None,
            mtime: e.mtime,
            atime: i64::MIN,
            ctime: None,
            btime: None,
            mode: Some(mode_of(r.4)),
            uid: None,
            gid: None,
            nlink: None,
            file_id: None,
            reparse_tag: None,
            link_target: None,
        })
    }

    fn stat_entry(&self, dir: &VfsPath, name: &str, _hint: Attrs) -> Result<Entry, VfsError> {
        rows(dir)
            .iter()
            .find(|r| r.0 == name)
            .map(entry)
            .ok_or_else(|| VfsError::NotFound(PathBuf::from(name)))
    }

    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        let r = row_of(p).ok_or(VfsError::Unsupported("screens"))?;
        Ok(Box::new(DemoRead(std::io::Cursor::new(data_of(r)))))
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

    fn open_write(&self, _p: &VfsPath, _from: Option<u64>) -> Result<Box<dyn VfsWrite>, VfsError> {
        Ok(Box::new(DemoWrite))
    }

    fn rename(&self, _from: &VfsPath, _to: &VfsPath, _replace: bool) -> Result<(), VfsError> {
        Ok(())
    }

    fn remove(&self, _p: &VfsPath) -> Result<(), VfsError> {
        Ok(())
    }

    fn set_meta(
        &self,
        _p: &VfsPath,
        _mode: Option<u32>,
        _mtime: Option<i64>,
    ) -> Result<(), VfsError> {
        Ok(())
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
