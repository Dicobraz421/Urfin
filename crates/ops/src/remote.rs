//! Не-локальные ФС в операциях (F03 §4.1, F03-07): запись, каталоги, rename, права и
//! время для `Engine::Vfs`. Схема с записью (`sftp`) реализует [`RemoteFs`] у себя и
//! кладёт его в [`Remotes`] (`AppState.ext`); ops видит обе стороны операции одинаково —
//! [`End`]: локальный диск или backend схемы.
//!
//! Чтение и листинг — `urfin_vfs::Vfs` того же backend'а ([`RemoteFs::vfs`]). Вызовы
//! блокирующие: только из потоков job, не из UI-потока.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use compact_str::CompactString;
use urfin_core::{CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{ListOpts, LocalFs, Vfs, VfsError, VfsRead};

/// Связь с сервером схемы для ожидания в job («ждёт связи», F03-07).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Канал есть: можно работать.
    Up,
    /// Связь потеряна, идёт переподключение: job ждёт и продолжит.
    Waiting,
    /// Не подключено (отключились, попытки исчерпаны): ждать нечего.
    Down,
}

/// Открытый на запись файл схемы.
pub trait RemoteWrite: io::Write + Send {
    /// Последний байт записан: `fsync@openssh.com` (если сервер его объявил) и закрытие с
    /// ответом сервера. Без вызова файл закрывается в `Drop` без ожидания.
    fn finish(self: Box<Self>) -> Result<(), VfsError>;
}

/// Запись в схему с операциями (F03 §4.1): реализует backend (`urfin-net` для `sftp`).
pub trait RemoteFs: Send + Sync + 'static {
    /// Схема адресов (`Location::backend`).
    fn scheme(&self) -> &'static str;
    /// Листинг, `stat` и чтение — тот же backend VFS.
    fn vfs(&self) -> &dyn Vfs;
    /// Запись файла: `None` — создать или обрезать; `Some(n)` — файл есть, писать с
    /// байта `n` (докачка), хвост после `n` отбрасывается сервером при перезаписи.
    fn open_write(&self, p: &Location, from: Option<u64>) -> Result<Box<dyn RemoteWrite>, VfsError>;
    fn mkdir(&self, p: &Location) -> Result<(), VfsError>;
    /// Файл, ссылка или пустой каталог.
    fn remove(&self, p: &Location) -> Result<(), VfsError>;
    /// Переименовать в пределах сервера. `replace` — поверх существующего: атомарно, если
    /// сервер умеет (`posix-rename@openssh.com`), иначе удаление и rename (окно риска — в
    /// лог).
    fn rename(&self, from: &Location, to: &Location, replace: bool) -> Result<(), VfsError>;
    /// Права (младшие 12 бит) и время изменения (нс Unix); `None` — не трогать.
    fn set_meta(&self, p: &Location, mode: Option<u32>, mtime: Option<i64>)
    -> Result<(), VfsError>;
    /// Связь с сервером адреса `at`.
    fn link(&self, at: &Location) -> Link;
    /// Каналов передачи внутри одной задачи (`[ssh] transfer_channels`).
    fn channels(&self) -> u16 {
        1
    }
}

/// Backend'ы с записью по схеме (`AppState.ext`): кладёт крейт схемы.
#[derive(Clone, Default)]
pub struct Remotes(HashMap<&'static str, Arc<dyn RemoteFs>>);

impl Remotes {
    pub fn add(&mut self, fs: Arc<dyn RemoteFs>) {
        self.0.insert(fs.scheme(), fs);
    }

    pub fn get(&self, loc: &Location) -> Option<&Arc<dyn RemoteFs>> {
        if loc.outer.is_some() {
            return None;
        }
        self.0.get(loc.backend.as_str())
    }

    /// Положить `fs` в `AppState.ext` (реестр создаётся при первом).
    pub fn install(ext: &mut urfin_core::Extensions, fs: Arc<dyn RemoteFs>) {
        if ext.get::<Remotes>().is_none() {
            ext.insert(Remotes::default());
        }
        if let Some(r) = ext.get_mut::<Remotes>() {
            r.add(fs);
        }
    }
}

impl std::fmt::Debug for Remotes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Remotes")
            .field(&self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Что известно об элементе.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    pub kind: EntryKind,
    pub size: u64,
    /// нс Unix; `i64::MIN` — неизвестно.
    pub mtime: i64,
    /// Права схемы (`sftp`); у локальных — `None`.
    pub mode: Option<u32>,
}

/// Сторона операции: локальный диск или схема с записью.
#[derive(Clone)]
pub(crate) enum End {
    Local,
    Remote(Arc<dyn RemoteFs>),
}

impl std::fmt::Debug for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            End::Local => f.write_str("Local"),
            End::Remote(r) => write!(f, "Remote({})", r.scheme()),
        }
    }
}

/// Локальная запись: файл ОС.
struct LocalWrite(std::fs::File);

impl io::Write for LocalWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl RemoteWrite for LocalWrite {
    fn finish(self: Box<Self>) -> Result<(), VfsError> {
        Ok(())
    }
}

fn local_err(p: &Path, e: io::Error) -> VfsError {
    VfsError::from_io(p, e)
}

impl End {
    /// Сторона для адреса: локальный путь или схема из реестра.
    pub(crate) fn of(loc: &Location, remotes: Option<&Remotes>) -> Option<End> {
        if loc.is_local() {
            return Some(End::Local);
        }
        remotes?.get(loc).cloned().map(End::Remote)
    }

    pub(crate) fn is_local(&self) -> bool {
        matches!(self, End::Local)
    }

    /// Та же ФС (один backend): перенос — rename.
    pub(crate) fn same_fs(&self, a: &Location, other: &End, b: &Location) -> bool {
        match (self, other) {
            (End::Local, End::Local) => true,
            (End::Remote(x), End::Remote(y)) => {
                Arc::ptr_eq(x, y)
                    && a.authority
                        .as_deref()
                        .zip(b.authority.as_deref())
                        .is_some_and(|(p, q)| p.eq_ignore_ascii_case(q))
            }
            _ => false,
        }
    }

    /// Ребёнок `name` каталога `dir`: у схем — путь через `/`.
    pub(crate) fn join(&self, dir: &Location, name: &str) -> Location {
        match self {
            End::Local => Location::local(dir.path.join(name)),
            End::Remote(_) => {
                let base = dir.path.to_string_lossy();
                let base = base.trim_end_matches(['/', '\\']);
                Location {
                    backend: dir.backend.clone(),
                    authority: dir.authority.clone(),
                    path: PathBuf::from(format!("{base}/{name}")),
                    outer: None,
                }
            }
        }
    }

    /// Имена без учёта регистра (NTFS) или с учётом (сервер).
    pub(crate) fn key(&self, name: &str) -> CompactString {
        match self {
            End::Local => crate::names::name_key(name),
            End::Remote(_) => name.into(),
        }
    }

    /// Адрес для показа и отчёта: путь ОС или `sftp://srv1/…` (№87).
    pub(crate) fn shown(loc: &Location) -> String {
        if loc.is_local() {
            loc.path.display().to_string()
        } else {
            loc.to_uri().replace('\\', "/")
        }
    }

    pub(crate) fn link(&self, at: &Location) -> Link {
        match self {
            End::Local => Link::Up,
            End::Remote(r) => r.link(at),
        }
    }

    /// Ключ устройства для очереди устройств (S5-19): сервер — по профилю.
    pub(crate) fn device(&self, at: &Location) -> u64 {
        use std::hash::{Hash, Hasher};
        match self {
            End::Local => urfin_vfs::volume::device_key(&at.path)
                .or_else(|| urfin_vfs::volume::of(&at.path).ok().map(|v| v.id))
                .unwrap_or(0),
            End::Remote(r) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                r.scheme().hash(&mut h);
                at.authority
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .hash(&mut h);
                h.finish() | 1 << 63
            }
        }
    }

    /// Каталог целиком (без `.` и `..`).
    pub(crate) fn list(&self, dir: &Location, cancel: &CancelToken) -> Result<Vec<Entry>, VfsError> {
        let opts = ListOpts {
            resolve_links: true,
            first_batch_hint: 1024,
        };
        let mut s = match self {
            End::Local => LocalFs::new().list(dir, &opts)?,
            End::Remote(r) => r.vfs().list(dir, &opts)?,
        };
        let mut out = Vec::new();
        while s.next_batch(&mut out, cancel)? {}
        out.retain(|e| e.name != "." && e.name != "..");
        Ok(out)
    }

    /// `None` — нет такого.
    pub(crate) fn stat(&self, p: &Location) -> Result<Option<Meta>, VfsError> {
        let md = match self {
            End::Local => LocalFs::new().stat(p, false),
            End::Remote(r) => r.vfs().stat(p, false),
        };
        match md {
            Ok(m) => Ok(Some(Meta {
                kind: m.kind,
                size: m.size,
                mtime: m.mtime,
                mode: if self.is_local() { None } else { m.mode },
            })),
            Err(VfsError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn open_read(&self, p: &Location) -> Result<Box<dyn VfsRead>, VfsError> {
        match self {
            End::Local => LocalFs::new().open_read(p),
            End::Remote(r) => r.vfs().open_read(p),
        }
    }

    pub(crate) fn open_write(
        &self,
        p: &Location,
        from: Option<u64>,
    ) -> Result<Box<dyn RemoteWrite>, VfsError> {
        match self {
            End::Local => {
                use std::io::Seek;
                let os = crate::names::os_path(&p.path);
                let mut o = std::fs::OpenOptions::new();
                o.write(true).create(true);
                if from.is_none() {
                    o.truncate(true);
                }
                let mut f = o.open(&os).map_err(|e| local_err(&p.path, e))?;
                if let Some(n) = from {
                    f.set_len(n).map_err(|e| local_err(&p.path, e))?;
                    f.seek(io::SeekFrom::Start(n))
                        .map_err(|e| local_err(&p.path, e))?;
                }
                Ok(Box::new(LocalWrite(f)))
            }
            End::Remote(r) => r.open_write(p, from),
        }
    }

    pub(crate) fn mkdir(&self, p: &Location) -> Result<(), VfsError> {
        match self {
            End::Local => std::fs::create_dir(crate::names::os_path(&p.path))
                .map_err(|e| local_err(&p.path, e)),
            End::Remote(r) => r.mkdir(p),
        }
    }

    pub(crate) fn remove(&self, p: &Location, dir: bool) -> Result<(), VfsError> {
        match self {
            End::Local => {
                let os = crate::names::os_path(&p.path);
                if dir {
                    std::fs::remove_dir(os)
                } else {
                    std::fs::remove_file(os)
                }
                .map_err(|e| local_err(&p.path, e))
            }
            End::Remote(r) => r.remove(p),
        }
    }

    pub(crate) fn rename(&self, from: &Location, to: &Location, replace: bool) -> Result<(), VfsError> {
        match self {
            End::Local => {
                let (a, b) = (
                    crate::names::os_path(&from.path),
                    crate::names::os_path(&to.path),
                );
                if !replace && std::fs::symlink_metadata(&b).is_ok() {
                    return Err(local_err(&to.path, io::ErrorKind::AlreadyExists.into()));
                }
                std::fs::rename(a, b).map_err(|e| local_err(&to.path, e))
            }
            End::Remote(r) => r.rename(from, to, replace),
        }
    }

    /// Права — только у схем (локально их нет); время — везде.
    pub(crate) fn set_meta(
        &self,
        p: &Location,
        mode: Option<u32>,
        mtime: Option<i64>,
    ) -> Result<(), VfsError> {
        match self {
            End::Local => {
                let Some(t) = mtime.and_then(time_of) else {
                    return Ok(());
                };
                let os = crate::names::os_path(&p.path);
                let f = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&os)
                    .or_else(|_| std::fs::File::open(&os))
                    .map_err(|e| local_err(&p.path, e))?;
                f.set_modified(t).map_err(|e| local_err(&p.path, e))
            }
            End::Remote(r) => r.set_meta(p, mode, mtime),
        }
    }
}

/// нс Unix → время ОС.
fn time_of(ns: i64) -> Option<std::time::SystemTime> {
    if ns == i64::MIN {
        return None;
    }
    let d = std::time::Duration::from_nanos(ns.unsigned_abs());
    if ns >= 0 {
        std::time::UNIX_EPOCH.checked_add(d)
    } else {
        std::time::UNIX_EPOCH.checked_sub(d)
    }
}

/// Ошибка VFS → `io::Error` (классы ошибок S5-11).
pub(crate) fn to_io(e: &VfsError) -> io::Error {
    match e {
        VfsError::NotFound(_) => io::ErrorKind::NotFound.into(),
        VfsError::Denied(_) => io::ErrorKind::PermissionDenied.into(),
        VfsError::NotDir(_) => io::ErrorKind::NotADirectory.into(),
        VfsError::Cancelled => io::ErrorKind::Interrupted.into(),
        VfsError::Io { source, .. } => io::Error::new(source.kind(), source.to_string()),
        e => io::Error::other(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_join_uses_slash_and_keeps_authority() {
        struct Nop;
        impl RemoteFs for Nop {
            fn scheme(&self) -> &'static str {
                "sftp"
            }
            fn vfs(&self) -> &dyn Vfs {
                unimplemented!()
            }
            fn open_write(&self, _: &Location, _: Option<u64>) -> Result<Box<dyn RemoteWrite>, VfsError> {
                Err(VfsError::Unsupported("nop"))
            }
            fn mkdir(&self, _: &Location) -> Result<(), VfsError> {
                Ok(())
            }
            fn remove(&self, _: &Location) -> Result<(), VfsError> {
                Ok(())
            }
            fn rename(&self, _: &Location, _: &Location, _: bool) -> Result<(), VfsError> {
                Ok(())
            }
            fn set_meta(&self, _: &Location, _: Option<u32>, _: Option<i64>) -> Result<(), VfsError> {
                Ok(())
            }
            fn link(&self, _: &Location) -> Link {
                Link::Up
            }
        }
        let end = End::Remote(Arc::new(Nop));
        let dir = Location::parse_uri("sftp://srv1/var/www").unwrap();
        let c = end.join(&dir, "app");
        assert_eq!(c.authority.as_deref(), Some("srv1"));
        assert_eq!(End::shown(&c), "sftp://srv1/var/www/app");
        let root = Location::parse_uri("sftp://srv1/").unwrap();
        assert_eq!(End::shown(&end.join(&root, "x")), "sftp://srv1/x");
        assert_ne!(end.device(&dir), End::Local.device(&dir));
        let mut r = Remotes::default();
        r.add(Arc::new(Nop));
        assert!(r.get(&dir).is_some());
        assert!(r.get(&Location::local(std::env::temp_dir())).is_none());
    }
}
