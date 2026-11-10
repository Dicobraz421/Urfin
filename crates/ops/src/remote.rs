//! Не-локальные ФС в операциях (F03 §4.1, F03-07; решение 2026-11-04 «vfs-write»):
//! запись, каталоги, rename, права и время для `Engine::Vfs` — методами записи
//! `urfin_vfs::Vfs` backend'а схемы из реестра (`Schemes` в `AppState.ext`), тем же, что
//! читает и листингует. Ops видит обе стороны операции одинаково — [`End`]: локальный
//! диск (пишет сам, как раньше) или backend схемы.
//!
//! Вызовы блокирующие: только из потоков job, не из UI-потока.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use compact_str::CompactString;
use urfin_core::{CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{Caps, Link, ListOpts, LocalFs, Schemes, Vfs, VfsError, VfsRead, VfsWrite};

/// Точность времени SFTP — секунды (F03 §4.4).
pub(crate) const REMOTE_TOL_NS: i64 = 1_000_000_000;

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
    /// Backend схемы из реестра (`sftp` и др.): запись — методами `Vfs`.
    Remote(Arc<dyn Vfs>),
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

impl VfsWrite for LocalWrite {
    fn finish(self: Box<Self>) -> Result<(), VfsError> {
        Ok(())
    }
}

fn local_err(p: &Path, e: io::Error) -> VfsError {
    VfsError::from_io(p, e)
}

impl End {
    /// Сторона для адреса: локальный путь или схема из реестра, в том числе внутри
    /// архива (`outer`: источник копирования, S8 §3.5). Нет — схема без backend'а или
    /// виртуальная (`du:`, `pkg:` — вид чужих данных).
    pub(crate) fn of(loc: &Location, schemes: Option<&Schemes>) -> Option<End> {
        if loc.is_local() {
            return Some(End::Local);
        }
        let v = schemes?.0.get(loc).ok()?;
        (!v.caps(loc).has(Caps::VIRTUAL)).then(|| End::Remote(Arc::clone(v)))
    }

    /// Пишет ли сторона в `at`: адрес внутри архива (`outer`) и схема с
    /// `Caps::READ_ONLY` — нет (отказ до задачи; решение 2026-11-06 «s8-answers»).
    pub(crate) fn writable(&self, at: &Location) -> bool {
        match self {
            End::Local => true,
            End::Remote(v) => at.outer.is_none() && !v.caps(at).has(Caps::READ_ONLY),
        }
    }

    /// Адрес внутри архива (`outer`): источник выборки (S8 §3.5).
    pub(crate) fn in_archive(at: &Location) -> bool {
        at.outer.is_some()
    }

    /// Произвольный доступ к файлу `at` (`Caps::SEEK`): докачка `.urfin-part` только с
    /// ним (S8 §3.5). Локальный диск — всегда; у архива — stored-записи zip.
    pub(crate) fn seekable(&self, at: &Location) -> bool {
        match self {
            End::Local => true,
            End::Remote(v) => v.caps(at).has(Caps::SEEK),
        }
    }

    /// Допуск времени изменения стороны без I/O (S8 §4, F03 §4.4): zip — DOS-время, 2 с
    /// (как FAT); tar — 1 с; 7z — 100 нс; сервер — секунды. Локальная сторона — 0 (её
    /// ФС учитывает [`crate::ops::compare::time_tolerance`]).
    pub(crate) fn time_tol(&self, at: &Location) -> i64 {
        const S: i64 = REMOTE_TOL_NS;
        match self {
            End::Local => 0,
            End::Remote(_) if Self::in_archive(at) => match at.backend.as_str() {
                "7z" => 100,
                s if s.starts_with("tar") || matches!(s, "tgz" | "tbz2" | "txz" | "tzst") => S,
                _ => 2 * S,
            },
            End::Remote(_) => REMOTE_TOL_NS,
        }
    }

    /// План выборки файлов `files` источника (`Vfs::extract_plan`); у локального диска
    /// и схем без плана — `None`.
    pub(crate) fn extract_plan(
        &self,
        files: &[Location],
        parallel: u8,
    ) -> Option<Vec<Box<dyn urfin_vfs::ExtractStream>>> {
        match self {
            End::Local => None,
            End::Remote(v) => v.extract_plan(files, parallel),
        }
    }

    /// Цель ссылки (`stat` без перехода); нет — `None`.
    pub(crate) fn link_target(&self, p: &Location) -> Option<PathBuf> {
        match self {
            End::Local => LocalFs::new().stat(p, false),
            End::Remote(r) => r.stat(p, false),
        }
        .ok()
        .and_then(|m| m.link_target)
    }

    /// Каналов передачи внутри задачи; у локальной стороны — без ограничения (`None`).
    pub(crate) fn channels(&self) -> Option<u16> {
        match self {
            End::Local => None,
            End::Remote(v) => Some(v.transfer_channels().max(1)),
        }
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

    /// Ребёнок `name` каталога `dir`: у схем — путь через `/`, `authority` и `outer` те же.
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
                    outer: dir.outer.clone(),
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

    /// Ключ устройства для очереди устройств (S5-19): сервер — по профилю; адрес внутри
    /// архива — диск, на котором лежит файл архива.
    pub(crate) fn device(&self, at: &Location) -> u64 {
        use std::hash::{Hash, Hasher};
        match self {
            End::Local => urfin_vfs::volume::device_key(&at.path)
                .or_else(|| urfin_vfs::volume::of(&at.path).ok().map(|v| v.id))
                .unwrap_or(0),
            End::Remote(_) if let Some(file) = archive_file(at) => End::Local.device(file),
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
    pub(crate) fn list(
        &self,
        dir: &Location,
        cancel: &CancelToken,
    ) -> Result<Vec<Entry>, VfsError> {
        let opts = ListOpts {
            resolve_links: true,
            first_batch_hint: 1024,
        };
        let mut s = match self {
            End::Local => LocalFs::new().list(dir, &opts)?,
            End::Remote(r) => r.list(dir, &opts)?,
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
            End::Remote(r) => r.stat(p, false),
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
            End::Remote(r) => r.open_read(p),
        }
    }

    pub(crate) fn open_write(
        &self,
        p: &Location,
        from: Option<u64>,
    ) -> Result<Box<dyn VfsWrite>, VfsError> {
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

    pub(crate) fn rename(
        &self,
        from: &Location,
        to: &Location,
        replace: bool,
    ) -> Result<(), VfsError> {
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
                let mut o = std::fs::OpenOptions::new();
                // Только атрибуты; каталог открывается с `FILE_FLAG_BACKUP_SEMANTICS`.
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
                    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
                    o.access_mode(FILE_WRITE_ATTRIBUTES)
                        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
                }
                #[cfg(not(windows))]
                o.write(true);
                let f = o
                    .open(&os)
                    .or_else(|_| std::fs::File::open(&os))
                    .map_err(|e| local_err(&p.path, e))?;
                f.set_modified(t).map_err(|e| local_err(&p.path, e))
            }
            End::Remote(r) => r.set_meta(p, mode, mtime),
        }
    }
}

/// Файл архива на диске для адреса внутри архива (самый внешний `outer`, если он
/// локальный).
pub(crate) fn archive_file(at: &Location) -> Option<&Location> {
    let mut cur = at.outer.as_deref()?;
    while let Some(o) = cur.outer.as_deref() {
        cur = o;
    }
    cur.is_local().then_some(cur)
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

/// Текст ошибки VFS для отчёта и тоста: без пути и «(os error N)»; схема без записи —
/// строкой Fluent `ops-no-write`.
pub(crate) fn err_text(e: &VfsError) -> String {
    let s = match e {
        VfsError::NoWrite(scheme) => {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("scheme", (*scheme).to_owned());
            return urfin_i18n::current().fmt("ops-no-write", &a);
        }
        VfsError::Io { source, .. } => source.to_string(),
        e => to_io(e).to_string(),
    };
    match s.rfind(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urfin_vfs::{
        Attrs, ListStream, Metadata, Space, VfsPath, VfsRegistry, WatchGuard, WatchTx,
    };

    /// Схема без записи: только обязательные методы `Vfs`.
    struct Nop(&'static str, Caps);

    impl Vfs for Nop {
        fn scheme(&self) -> &'static str {
            self.0
        }
        fn caps(&self, _: &VfsPath) -> Caps {
            self.1
        }
        fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
            Err(VfsError::Unsupported("nop"))
        }
        fn stat(&self, p: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
            Err(VfsError::NotFound(p.path.clone()))
        }
        fn stat_entry(&self, d: &VfsPath, _: &str, _: Attrs) -> Result<Entry, VfsError> {
            Err(VfsError::NotFound(d.path.clone()))
        }
        fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
            Err(VfsError::NotFound(p.path.clone()))
        }
        fn space(&self, _: &VfsPath) -> Result<Space, VfsError> {
            Err(VfsError::Unsupported("nop"))
        }
        fn local_path(&self, _: &VfsPath) -> Option<PathBuf> {
            None
        }
        fn watch(&self, _: &VfsPath, _: WatchTx) -> Result<WatchGuard, VfsError> {
            Err(VfsError::Unsupported("nop"))
        }
    }

    fn schemes(v: Vec<Nop>) -> Schemes {
        let mut r = VfsRegistry::with_local();
        for n in v {
            r.register(Arc::new(n));
        }
        Schemes(Arc::new(r))
    }

    #[test]
    fn remote_join_uses_slash_and_keeps_authority() {
        let end = End::Remote(Arc::new(Nop("sftp", Caps::default())));
        let dir = Location::parse_uri("sftp://srv1/var/www").unwrap();
        let c = end.join(&dir, "app");
        assert_eq!(c.authority.as_deref(), Some("srv1"));
        assert_eq!(End::shown(&c), "sftp://srv1/var/www/app");
        let root = Location::parse_uri("sftp://srv1/").unwrap();
        assert_eq!(End::shown(&end.join(&root, "x")), "sftp://srv1/x");
        assert_ne!(end.device(&dir), End::Local.device(&dir));
        assert_eq!(end.channels(), Some(1));
        assert_eq!(end.link(&dir), Link::Up);
    }

    #[test]
    fn ends_come_from_registry() {
        let s = schemes(vec![
            Nop("sftp", Caps::default()),
            Nop("du", Caps(Caps::VIRTUAL)),
            Nop("ro", Caps(Caps::READ_ONLY)),
        ]);
        let sftp = Location::parse_uri("sftp://srv1/x").unwrap();
        assert!(matches!(End::of(&sftp, Some(&s)), Some(End::Remote(_))));
        assert!(End::of(&sftp, None).is_none());
        let local = Location::local(std::env::temp_dir());
        assert!(End::of(&local, Some(&s)).is_some_and(|e| e.is_local()));
        let du = Location::parse_uri("du:/1").unwrap();
        assert!(End::of(&du, Some(&s)).is_none(), "virtual scheme");
        let ro = Location::parse_uri("ro://h/x").unwrap();
        let ro_end = End::of(&ro, Some(&s)).unwrap();
        assert!(!ro_end.writable(&ro));
        assert!(End::of(&sftp, Some(&s)).unwrap().writable(&sftp));
    }

    #[test]
    fn archive_is_a_source_but_never_a_target() {
        // Схема архива без флагов: запрет записи держится на `outer`, а не на caps.
        let s = schemes(vec![Nop("zip", Caps::default())]);
        let file = Arc::new(Location::local(std::env::temp_dir().join("a.zip")));
        let inner = Location {
            backend: "zip".into(),
            authority: None,
            path: PathBuf::from("/src"),
            outer: Some(Arc::clone(&file)),
        };
        let end = End::of(&inner, Some(&s)).expect("source inside an archive");
        assert!(!end.is_local());
        assert!(!end.writable(&inner), "no target inside an archive");
        let child = end.join(&inner, "main.rs");
        assert_eq!(child.outer.as_deref(), Some(&*file));
        assert_eq!(child.path, PathBuf::from("/src/main.rs"));
        assert!(!end.writable(&child));
        let unknown = Location {
            backend: "rar".into(),
            ..inner.clone()
        };
        assert!(End::of(&unknown, Some(&s)).is_none(), "no backend");
        assert!(End::of(&inner, None).is_none());
    }

    /// Сторона-архив (S8 §4, §3.5): допуск времени по формату, устройство — диск файла
    /// архива, докачка — только с `Caps::SEEK`.
    #[test]
    fn archive_side_tolerance_device_and_seek() {
        let s = schemes(vec![
            Nop("zip", Caps::default()),
            Nop("7z", Caps(Caps::SEEK)),
            Nop("sftp", Caps(Caps::SEEK)),
        ]);
        let file = Location::local(std::env::temp_dir().join("a.zip"));
        let inner = |scheme: &str| Location {
            backend: scheme.into(),
            authority: None,
            path: PathBuf::from("/x"),
            outer: Some(Arc::new(file.clone())),
        };
        let zip = End::of(&inner("zip"), Some(&s)).unwrap();
        assert_eq!(zip.time_tol(&inner("zip")), 2_000_000_000);
        assert!(!zip.seekable(&inner("zip")));
        assert_eq!(zip.device(&inner("zip")), End::Local.device(&file));
        assert_eq!(archive_file(&inner("zip")), Some(&file));
        let sz = End::of(&inner("7z"), Some(&s)).unwrap();
        assert_eq!(sz.time_tol(&inner("7z")), 100);
        assert!(sz.seekable(&inner("7z")));
        let srv = Location::parse_uri("sftp://srv1/x").unwrap();
        let sftp = End::of(&srv, Some(&s)).unwrap();
        assert_eq!(sftp.time_tol(&srv), REMOTE_TOL_NS);
        assert_eq!(archive_file(&srv), None);
        assert_eq!(End::Local.time_tol(&file), 0);
        assert!(End::Local.seekable(&file));
        assert!(
            End::Local
                .extract_plan(std::slice::from_ref(&file), 4)
                .is_none()
        );
    }

    #[test]
    fn scheme_without_write_says_so() {
        let end = End::Remote(Arc::new(Nop("ro", Caps::default())));
        let at = Location::parse_uri("ro://h/x").unwrap();
        let errs = [
            end.open_write(&at, None).err().unwrap(),
            end.mkdir(&at).unwrap_err(),
            end.remove(&at, false).unwrap_err(),
            end.rename(&at, &at, true).unwrap_err(),
            end.set_meta(&at, Some(0o644), None).unwrap_err(),
        ];
        let want = {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("scheme", "ro");
            urfin_i18n::current().fmt("ops-no-write", &a)
        };
        assert!(!want.contains("ops-no-write"), "fluent key exists: {want}");
        for e in errs {
            assert!(matches!(e, VfsError::NoWrite("ro")), "{e:?}");
            assert_eq!(err_text(&e), want);
        }
    }
}
