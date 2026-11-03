//! Схема `sftp` (F03 §2.6, F03-04): `Vfs`, листинг батчами, чтение и запись, `longname`.
//!
//! Синхронный фасад для потоков Runtime: каждый вызов — задача в рантайме крейта.
//! Каналы приходят извне ([`SftpFs::attach`] после подключения): сам backend не
//! подключается, поэтому таб `sftp://` из сессии без подключения получает «нет связи».
//! Наблюдения нет (без `WATCH`): обновление — по Ctrl+R и после своих операций.

pub mod demo;
pub mod ext;
mod io;
pub mod longname;
mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use russh_sftp::protocol::{File, FileAttributes, OpenFlags};
use urfin_vfs::{
    Attrs, CancelToken, Caps, ColumnSpec, Entry, EntryKind, ExtCell, ListOpts, ListStream,
    Metadata, Space, Vfs, VfsError, VfsPath, VfsRead, WatchGuard, WatchTx,
};

pub use io::{SftpReader, SftpWriter};
pub use longname::{Owner, mode_string};
pub use session::{Session, not_connected};

use crate::uri::{SCHEME, SftpUri};
use ext::{Detail, Details};
use session::{is_eof, map_err};

/// Что поменять у файла (`SETSTAT`). `None` — не трогать.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetStat {
    /// Права (младшие 12 бит `mode`).
    pub mode: Option<u32>,
    /// Секунды Unix; SFTP v3 задаёт `atime` и `mtime` только парой.
    pub times: Option<(u32, u32)>,
}

/// Backend `sftp`: каналы подключённых профилей.
#[derive(Debug, Default)]
pub struct SftpFs {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Права и владельцы прочитанных каталогов — колонки схемы (`Entry.ext`).
    details: Arc<Details>,
}

impl SftpFs {
    pub fn new() -> SftpFs {
        SftpFs::default()
    }

    fn key(profile: &str) -> String {
        // Authority URI сравнивается без учёта регистра (как имена профилей).
        profile.to_ascii_lowercase()
    }

    /// Канал профиля после подключения.
    pub fn attach(&self, s: Arc<Session>) {
        self.map().insert(Self::key(s.profile()), s);
    }

    /// Отключение: дальше профиль — «нет связи».
    pub fn detach(&self, profile: &str) -> Option<Arc<Session>> {
        self.map().remove(&Self::key(profile))
    }

    /// Живой канал профиля.
    pub fn session(&self, profile: &str) -> Option<Arc<Session>> {
        self.map()
            .get(&Self::key(profile))
            .filter(|s| !s.is_lost())
            .cloned()
    }

    /// Пинг заголовка панели (F03 §2.3).
    pub fn rtt(&self, profile: &str) -> Option<Duration> {
        self.session(profile).and_then(|s| s.rtt())
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn at(&self, p: &VfsPath) -> Result<(Arc<Session>, SftpUri), VfsError> {
        let u = SftpUri::from_location(p).ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        let s = self
            .session(&u.profile)
            .ok_or_else(|| not_connected(&u.profile))?;
        Ok((s, u))
    }

    /// Создать каталог.
    pub fn mkdir(&self, p: &VfsPath) -> Result<(), VfsError> {
        let (s, u) = self.at(p)?;
        s.block(move |s| async move {
            s.raw
                .mkdir(u.path.clone(), FileAttributes::empty())
                .await
                .map(drop)
                .map_err(|e| map_err(&shown(&u), e))
        })
    }

    /// Удалить файл, ссылку или пустой каталог (без рекурсии).
    pub fn remove(&self, p: &VfsPath) -> Result<(), VfsError> {
        let (s, u) = self.at(p)?;
        s.block(move |s| async move {
            let err = |e| map_err(&shown(&u), e);
            let a = s.raw.lstat(u.path.clone()).await.map_err(err)?.attrs;
            if a.is_dir() {
                s.raw.rmdir(u.path.clone()).await.map(drop).map_err(err)
            } else {
                s.raw.remove(u.path.clone()).await.map(drop).map_err(err)
            }
        })
    }

    /// Переименовать или переместить в пределах сервера.
    pub fn rename(&self, from: &VfsPath, to: &VfsPath) -> Result<(), VfsError> {
        let (s, a) = self.at(from)?;
        let b = SftpUri::from_location(to).ok_or_else(|| VfsError::NotFound(to.path.clone()))?;
        if !b.profile.eq_ignore_ascii_case(&a.profile) {
            return Err(VfsError::Unsupported("rename across servers"));
        }
        s.block(move |s| async move {
            s.raw
                .rename(a.path.clone(), b.path)
                .await
                .map(drop)
                .map_err(|e| map_err(&shown(&a), e))
        })
    }

    /// Права и времена (`SETSTAT`).
    pub fn setstat(&self, p: &VfsPath, st: SetStat) -> Result<(), VfsError> {
        let (s, u) = self.at(p)?;
        let mut a = FileAttributes::empty();
        a.permissions = st.mode.map(|m| m & 0o7777);
        if let Some((atime, mtime)) = st.times {
            a.atime = Some(atime);
            a.mtime = Some(mtime);
        }
        s.block(move |s| async move {
            s.raw
                .setstat(u.path.clone(), a)
                .await
                .map(drop)
                .map_err(|e| map_err(&shown(&u), e))
        })
    }

    /// Запись файла: создаётся или обрезается; `exclusive` — ошибка, если уже есть.
    pub fn open_write(&self, p: &VfsPath, exclusive: bool) -> Result<SftpWriter, VfsError> {
        let (s, u) = self.at(p)?;
        let mut flags = OpenFlags::WRITE | OpenFlags::CREATE;
        flags |= if exclusive {
            OpenFlags::EXCLUDE
        } else {
            OpenFlags::TRUNCATE
        };
        let path = shown(&u);
        let p2 = path.clone();
        let handle = s.block(move |s| async move {
            s.raw
                .open(u.path, flags, FileAttributes::empty())
                .await
                .map(|h| h.handle)
                .map_err(|e| map_err(&p2, e))
        })?;
        Ok(SftpWriter::new(s, handle, path))
    }

    /// Владельцы и права каталога для колонок «Права»/«Владелец»: по `longname`
    /// (SFTP v3), при мусоре — `uid/gid`. Отдельно от листинга: `Entry` их не несёт.
    pub fn details(&self, dir: &VfsPath) -> Result<Vec<(String, String, Owner)>, VfsError> {
        let (sess, u) = self.at(dir)?;
        let shown_dir = shown(&u);
        sess.block(move |s| async move {
            let err = |e| map_err(&shown_dir, e);
            let dh = s.raw.opendir(u.path.clone()).await.map_err(err)?.handle;
            let mut out = Vec::new();
            let res = loop {
                match s.raw.readdir(dh.clone()).await {
                    Ok(name) => {
                        for f in name.files {
                            if f.filename == "." || f.filename == ".." {
                                continue;
                            }
                            let perms = f.attrs.permissions.map(mode_string).unwrap_or_default();
                            let owner =
                                longname::owner(&f.longname, &f.filename, f.attrs.uid, f.attrs.gid);
                            out.push((f.filename, perms, owner));
                        }
                    }
                    Err(e) if is_eof(&e) => break Ok(()),
                    Err(e) => break Err(err(e)),
                }
            };
            let _ = s.raw.close(dh).await;
            res.map(|()| out)
        })
    }
}

/// Пинг заголовка панели для модели подключений в core (F03-05a).
impl urfin_core::net::Ping for SftpFs {
    fn rtt(&self, profile: &str) -> Option<Duration> {
        SftpFs::rtt(self, profile)
    }
}

/// Путь для ошибок и логов: `sftp://srv1/x`.
fn shown(u: &SftpUri) -> PathBuf {
    PathBuf::from(u.to_string())
}

/// Наносекунды от Unix epoch; `i64::MIN` — нет.
fn ns(secs: Option<u32>) -> i64 {
    secs.map_or(i64::MIN, |s| i64::from(s) * 1_000_000_000)
}

fn kind_of(a: &FileAttributes) -> EntryKind {
    if a.is_dir() {
        EntryKind::Dir
    } else if a.is_symlink() {
        EntryKind::LinkBroken
    } else if a.is_regular() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// Запись по атрибутам `lstat`; у ссылки вид уточняется `stat` цели.
fn entry(name: &str, a: &FileAttributes) -> Entry {
    let kind = kind_of(a);
    let mut attrs = 0;
    if name.starts_with('.') {
        attrs |= Attrs::DOTFILE;
    }
    if kind == EntryKind::File && a.permissions.is_some_and(|m| m & 0o111 != 0) {
        attrs |= Attrs::EXEC;
    }
    Entry {
        name: name.into(),
        size: if kind == EntryKind::Dir {
            0
        } else {
            a.size.unwrap_or(0)
        },
        mtime: ns(a.mtime),
        ctime: i64::MIN,
        atime: ns(a.atime),
        attrs: Attrs(attrs),
        kind,
    }
}

/// Вид ссылки по атрибутам цели (`None` — цели нет).
fn link_kind(target: Option<&FileAttributes>) -> EntryKind {
    match target {
        Some(t) if t.is_dir() => EntryKind::LinkDir,
        Some(_) => EntryKind::LinkFile,
        None => EntryKind::LinkBroken,
    }
}

fn metadata(a: &FileAttributes) -> Metadata {
    let e = entry("", a);
    Metadata {
        kind: e.kind,
        attrs: e.attrs,
        size: a.size.unwrap_or(0),
        alloc_size: None,
        mtime: e.mtime,
        atime: e.atime,
        ctime: None,
        btime: None,
        mode: a.permissions,
        uid: a.uid,
        gid: a.gid,
        nlink: None,
        file_id: None,
        reparse_tag: None,
        link_target: None,
    }
}

impl Vfs for SftpFs {
    fn scheme(&self) -> &'static str {
        SCHEME
    }

    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps(Caps::SEEK | Caps::CASE_SENSITIVE)
    }

    fn list(&self, dir: &VfsPath, opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        let (s, u) = self.at(dir)?;
        let path = shown(&u);
        let p2 = path.clone();
        let remote = u.path.clone();
        let handle = s.block(move |s| async move {
            s.raw
                .opendir(remote)
                .await
                .map(|h| h.handle)
                .map_err(|e| map_err(&p2, e))
        })?;
        let key = Details::key(&u.profile, &u.path);
        self.details.reset(&key);
        Ok(Box::new(SftpList {
            s,
            dir: u,
            path,
            handle: Some(handle),
            resolve_links: opts.resolve_links,
            details: Arc::clone(&self.details),
            key,
        }))
    }

    fn stat(&self, at: &VfsPath, follow: bool) -> Result<Metadata, VfsError> {
        let (sess, u) = self.at(at)?;
        sess.block(move |s| async move {
            let path = shown(&u);
            let err = |e| map_err(&path, e);
            if follow {
                return Ok(metadata(
                    &s.raw.stat(u.path.clone()).await.map_err(err)?.attrs,
                ));
            }
            let attrs = s.raw.lstat(u.path.clone()).await.map_err(err)?.attrs;
            let mut md = metadata(&attrs);
            if attrs.is_symlink() {
                md.kind = link_kind(
                    s.raw
                        .stat(u.path.clone())
                        .await
                        .ok()
                        .map(|t| t.attrs)
                        .as_ref(),
                );
                // Цель — в подсказке.
                md.link_target = s
                    .raw
                    .readlink(u.path.clone())
                    .await
                    .ok()
                    .and_then(|n| n.files.into_iter().next())
                    .map(|f| PathBuf::from(f.filename));
            }
            Ok(md)
        })
    }

    fn stat_entry(&self, dir: &VfsPath, name: &str, _hint: Attrs) -> Result<Entry, VfsError> {
        let (s, u) = self.at(dir)?;
        let child = u.join(name);
        let name = name.to_owned();
        s.block(move |s| async move {
            let err = |e| map_err(&shown(&child), e);
            let a = s.raw.lstat(child.path.clone()).await.map_err(err)?.attrs;
            let mut e = entry(&name, &a);
            if a.is_symlink() {
                e.kind = link_kind(
                    s.raw
                        .stat(child.path.clone())
                        .await
                        .ok()
                        .map(|t| t.attrs)
                        .as_ref(),
                );
            }
            Ok(e)
        })
    }

    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        let (s, u) = self.at(p)?;
        let path = shown(&u);
        let p2 = path.clone();
        let (handle, len) = s.block(move |s| async move {
            let err = |e| map_err(&p2, e);
            let h = s
                .raw
                .open(u.path, OpenFlags::READ, FileAttributes::empty())
                .await
                .map_err(err)?
                .handle;
            let len = s.raw.fstat(h.clone()).await.ok().and_then(|a| a.attrs.size);
            Ok((h, len))
        })?;
        Ok(Box::new(SftpReader::new(s, handle, len, path)))
    }

    fn space(&self, at: &VfsPath) -> Result<Space, VfsError> {
        let (s, u) = self.at(at)?;
        s.block(move |s| async move {
            // `statvfs@openssh.com`; у прочих серверов места не знаем.
            let v = s
                .raw
                .statvfs(u.path)
                .await
                .map_err(|_| VfsError::Unsupported("statvfs"))?;
            let f = v.fragment_size.max(1);
            Ok(Space {
                total: v.blocks.saturating_mul(f),
                free: v.blocks_free.saturating_mul(f),
                avail: v.blocks_avail.saturating_mul(f),
            })
        })
    }

    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }

    /// Без событий и без опроса: `Unsupported` включил бы polling сервера.
    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Ok(WatchGuard::new(()))
    }

    /// Имя · Размер · Изменён · Права · Владелец (`screen-sftppanel`).
    fn columns(&self, _dir: &VfsPath) -> &'static [ColumnSpec] {
        &ext::COLUMNS
    }

    /// Права и владелец — из памяти листинга, без запросов.
    fn ext(&self, dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        let u = SftpUri::from_location(dir)?;
        self.details
            .cell(&Details::key(&u.profile, &u.path), &e.name, col)
    }
}

/// Листинг: один вызов — один `READDIR` (батч сервера, у OpenSSH ~100 записей).
struct SftpList {
    s: Arc<Session>,
    dir: SftpUri,
    path: PathBuf,
    /// `None` — каталог дочитан и закрыт.
    handle: Option<String>,
    resolve_links: bool,
    details: Arc<Details>,
    /// Каталог в [`Details`].
    key: String,
}

impl ListStream for SftpList {
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let Some(h) = self.handle.clone() else {
            return Ok(false);
        };
        let (path, dir, links) = (self.path.clone(), self.dir.clone(), self.resolve_links);
        let started = Instant::now();
        let batch = self.s.block(move |s| async move {
            let files = match s.raw.readdir(h).await {
                Ok(n) => n.files,
                Err(e) if is_eof(&e) => return Ok(None),
                Err(e) => return Err(map_err(&path, e)),
            };
            s.note_rtt(started.elapsed());
            Ok(Some(resolve(&s, &dir, files, links).await))
        })?;
        let Some((batch, details)) = batch else {
            self.close();
            return Ok(false);
        };
        self.details.add(&self.key, details);
        out.extend(batch);
        Ok(true)
    }
}

/// Записи батча с правами и владельцами; цели ссылок — `stat` разом, не по одной.
async fn resolve(
    s: &Session,
    dir: &SftpUri,
    files: Vec<File>,
    links: bool,
) -> (Vec<Entry>, Vec<(String, Detail)>) {
    let files: Vec<File> = files
        .into_iter()
        .filter(|f| f.filename != "." && f.filename != "..")
        .collect();
    let details = files
        .iter()
        .map(|f| {
            let perms = f.attrs.permissions.map(mode_string).unwrap_or_default();
            let owner = longname::owner(&f.longname, &f.filename, f.attrs.uid, f.attrs.gid);
            (f.filename.clone(), Detail::new(perms, &owner))
        })
        .collect();
    let mut out: Vec<Entry> = files.iter().map(|f| entry(&f.filename, &f.attrs)).collect();
    if links {
        let idx: Vec<usize> = (0..files.len())
            .filter(|&i| files[i].attrs.is_symlink())
            .collect();
        let stats = futures::future::join_all(
            idx.iter()
                .map(|&i| s.raw.stat(dir.join(&files[i].filename).path)),
        )
        .await;
        for (i, st) in idx.into_iter().zip(stats) {
            out[i].kind = link_kind(st.ok().map(|a| a.attrs).as_ref());
        }
    }
    (out, details)
}

impl SftpList {
    fn close(&mut self) {
        if let Some(h) = self.handle.take() {
            let s = Arc::clone(&self.s);
            self.s.detach(async move {
                let _ = s.raw.close(h).await;
            });
        }
    }
}

impl Drop for SftpList {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(mode: u32, size: u64) -> FileAttributes {
        let mut a = FileAttributes::empty();
        a.permissions = Some(mode);
        a.size = Some(size);
        a.mtime = Some(1_700_000_000);
        a
    }

    #[test]
    fn entries_from_attrs() {
        let e = entry("run.sh", &attrs(0o100_755, 12));
        assert_eq!((e.kind, e.size), (EntryKind::File, 12));
        assert!(e.attrs.has(Attrs::EXEC));
        assert_eq!(e.mtime, 1_700_000_000 * 1_000_000_000);
        assert_eq!(e.ctime, i64::MIN);
        let e = entry(".ssh", &attrs(0o040_700, 4096));
        assert_eq!((e.kind, e.size), (EntryKind::Dir, 0));
        assert!(e.attrs.has(Attrs::DOTFILE));
        assert_eq!(entry("l", &attrs(0o120_777, 7)).kind, EntryKind::LinkBroken);
        assert_eq!(entry("p", &attrs(0o010_644, 0)).kind, EntryKind::Other);
        assert_eq!(link_kind(Some(&attrs(0o040_755, 0))), EntryKind::LinkDir);
        assert_eq!(link_kind(Some(&attrs(0o100_644, 0))), EntryKind::LinkFile);
        assert_eq!(link_kind(None), EntryKind::LinkBroken);
    }

    #[test]
    fn not_connected_profile() {
        let fs = SftpFs::new();
        let loc = SftpUri::new("srv1", "/etc").to_location();
        let Err(VfsError::Io { source, .. }) = fs.list(&loc, &ListOpts::default()) else {
            panic!("list without connection");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::NotConnected);
        assert!(fs.rtt("srv1").is_none());
        assert!(matches!(
            fs.stat(&Location::local(std::env::temp_dir()), true),
            Err(VfsError::NotFound(_))
        ));
    }

    use urfin_core::Location;
}
