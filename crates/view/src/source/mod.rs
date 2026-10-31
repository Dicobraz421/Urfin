//! `ByteSource` и `open_source`: Owned, Mapped, File, Seek, Spool (S7 §0, §2). Карточка S7-02.
//!
//! Источник выбирается вне UI-потока (`Lane::Interactive`) по тому, `Caps` и размеру:
//! [`choose`]. UI-поток источник не читает никогда: только `PageCache` (S7-03); в debug
//! `read_at` из потока, помеченного [`mark_ui_thread`], — паника.

mod file;
mod mapped;
mod owned;
mod seek;
mod spool;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use memmap2::Mmap;
use urfin_core::{CancelToken, Location};
use urfin_vfs::volume::{FsKind, Media, VolumeInfo};
use urfin_vfs::{Caps, VfsError, VfsRegistry};

pub use file::FileSource;
pub use mapped::MappedSource;
pub use owned::OwnedSource;
pub use seek::SeekSource;
pub use spool::{SpoolPump, SpoolSource, spool_dir, sweep_spool};

/// Файл до этого размера читается целиком (`Owned`).
pub const OWNED_MAX: u64 = 4 << 20;
/// mmap — от этого размера (S7 §0).
pub const MAP_MIN: u64 = 64 << 20;
/// mmap — только если файл не менялся столько (активные логи — через `seek_read`).
pub const MAP_AGE: Duration = Duration::from_secs(60);

/// Вид источника.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Owned,
    Mapped,
    File,
    Seek,
    Spool,
}

/// Байты документа вьювера. Чтение — только вне UI-потока.
#[allow(clippy::len_without_is_empty, reason = "API из спеки S7 §2")]
pub trait ByteSource: Send + Sync + fmt::Debug + 'static {
    /// Текущая длина (растёт у Spool и follow).
    fn len(&self) -> u64;
    /// Длина больше не вырастет.
    fn is_final(&self) -> bool;
    /// Чтение с `off`, не дальше `len()`. `Ok(0)` — данных дальше нет.
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize>;
    /// Весь файл в памяти без копии (Owned, Mapped) — для индекса и поиска.
    fn mapped(&self) -> Option<&[u8]> {
        None
    }
    /// Отображение для `Page::Mapped` (страницы без копии).
    fn map(&self) -> Option<&Arc<Mmap>> {
        None
    }
    fn kind(&self) -> SourceKind;
    /// Follow: длина по открытому handle.
    fn refresh_len(&self) -> io::Result<u64>;
    /// Отдельный handle с `FILE_FLAG_SEQUENTIAL_SCAN` для последовательного индекса.
    fn seq_file(&self) -> Option<io::Result<File>> {
        None
    }
    /// Follow (S7 §3.8): включить или выключить рост у открытого источника — `is_final`
    /// становится `!on`. `false` — источник так не умеет (не `File`): его заменяют.
    fn set_follow(&self, _on: bool) -> bool {
        false
    }
    /// Id открытого файла (том, индекс): ротация в follow — по смене id по пути.
    fn file_id(&self) -> Option<(u64, u128)> {
        None
    }
}

/// Читает, пока буфер не заполнится или данные не кончатся; возвращает прочитанное.
pub fn read_full(src: &dyn ByteSource, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match src.read_at(off + done as u64, &mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

/// Политика mmap (`[viewer] mmap`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MmapPolicy {
    #[default]
    Auto,
    Off,
}

/// Параметры открытия.
#[derive(Clone, Debug, Default)]
pub struct SourceOpts {
    pub follow: bool,
    pub mmap: MmapPolicy,
    /// Том файла; `None` — `open_source` узнаёт сам (это I/O, но мы и так вне UI).
    pub volume: Option<VolumeInfo>,
    /// Каталог Spool ([`spool_dir`]); `None` — потоки без seek не открываются.
    pub spool_dir: Option<PathBuf>,
}

/// Что известно о файле при выборе источника.
#[derive(Clone, Debug)]
pub struct Facts {
    /// Есть локальный путь (`Vfs::local_path`).
    pub local: bool,
    /// `Caps::SEEK` у backend'а.
    pub seek: bool,
    /// Размер; `None` — неизвестен (поток).
    pub size: Option<u64>,
    /// Сколько прошло с mtime; `None` — неизвестно или mtime в будущем.
    pub age: Option<Duration>,
    pub follow: bool,
    pub mmap: MmapPolicy,
    /// Носитель и ФС тома; `None` — не узнать.
    pub volume: Option<(Media, FsKind)>,
}

/// Выбор источника (S7 §0): Owned ≤ 4 МБ; Mapped — локальный не сетевой и не съёмный
/// том, ≥ 64 МБ, mtime старше 60 с, не follow; остальное локальное — File; не-локальное —
/// Seek при `Caps::SEEK`, иначе Spool.
pub fn choose(f: &Facts) -> SourceKind {
    if !f.follow && f.size.is_some_and(|s| s <= OWNED_MAX) {
        return SourceKind::Owned;
    }
    if f.local {
        let fixed = f.volume.is_some_and(|(m, fs)| {
            !matches!(m, Media::Net | Media::Usb) && !matches!(fs, FsKind::Smb | FsKind::Nfs)
        });
        let map = f.mmap == MmapPolicy::Auto
            && !f.follow
            && fixed
            && f.size.is_some_and(|s| s >= MAP_MIN)
            && f.age.is_some_and(|a| a >= MAP_AGE);
        return if map {
            SourceKind::Mapped
        } else {
            SourceKind::File
        };
    }
    if f.seek {
        SourceKind::Seek
    } else {
        SourceKind::Spool
    }
}

/// Открытый источник. `pump` есть у Spool: копирование потока во временный файл — долгий
/// воркер, вызывающий запускает его через `Runtime::spawn_dedicated`.
pub struct Opened {
    pub src: Arc<dyn ByteSource>,
    pub pump: Option<SpoolPump>,
}

impl fmt::Debug for Opened {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opened")
            .field("src", &self.src)
            .field("pump", &self.pump.is_some())
            .finish()
    }
}

impl Opened {
    fn plain(src: Arc<dyn ByteSource>) -> Opened {
        Opened { src, pump: None }
    }
}

/// Открывает источник документа. Только вне UI (`Lane::Interactive`): stat, том, чтение
/// Owned целиком.
pub fn open_source(
    reg: &VfsRegistry,
    loc: &Location,
    o: &SourceOpts,
    cancel: &CancelToken,
) -> Result<Opened, VfsError> {
    let _span = tracing::debug_span!("view.open_source").entered();
    let vfs = reg.get(loc)?;
    cancel.check().map_err(|_| VfsError::Cancelled)?;
    if let Some(path) = vfs.local_path(loc) {
        let file = File::open(&path).map_err(|e| VfsError::from_io(&path, e))?;
        let meta = file.metadata().map_err(|e| VfsError::from_io(&path, e))?;
        let volume = match &o.volume {
            Some(v) => Some((v.media, v.fs)),
            None => urfin_vfs::volume::of(&path).ok().map(|v| (v.media, v.fs)),
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok());
        let facts = Facts {
            local: true,
            seek: true,
            size: Some(meta.len()),
            age,
            follow: o.follow,
            mmap: o.mmap,
            volume,
        };
        let kind = choose(&facts);
        tracing::debug!(?kind, size = meta.len(), "view source");
        cancel.check().map_err(|_| VfsError::Cancelled)?;
        let src: Arc<dyn ByteSource> = match kind {
            SourceKind::Owned => Arc::new(
                OwnedSource::read(file, meta.len()).map_err(|e| VfsError::from_io(&path, e))?,
            ),
            SourceKind::Mapped => match MappedSource::new(&file, path.clone()) {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    tracing::warn!(error = %e, "mmap failed, falling back to seek_read");
                    Arc::new(FileSource::new(file, path, o.follow))
                }
            },
            _ => Arc::new(FileSource::new(file, path, o.follow)),
        };
        return Ok(Opened::plain(src));
    }
    let caps = vfs.caps(loc);
    let size = vfs.stat(loc, true).ok().map(|m| m.size);
    let facts = Facts {
        local: false,
        seek: caps.has(Caps::SEEK),
        size,
        age: None,
        follow: o.follow,
        mmap: MmapPolicy::Off,
        volume: None,
    };
    let kind = choose(&facts);
    tracing::debug!(?kind, ?size, "view source");
    let reader = vfs.open_read(loc)?;
    let io_err = |e| VfsError::from_io(&loc.path, e);
    match kind {
        SourceKind::Owned => Ok(Opened::plain(Arc::new(
            OwnedSource::from_reader(reader).map_err(io_err)?,
        ))),
        SourceKind::Seek => Ok(Opened::plain(Arc::new(
            SeekSource::new(reader, size).map_err(io_err)?,
        ))),
        _ => {
            let dir = o
                .spool_dir
                .as_deref()
                .ok_or(VfsError::Unsupported("stream without spool dir"))?;
            let (src, pump) = SpoolSource::start(reader, dir, cancel.child()).map_err(io_err)?;
            Ok(Opened {
                src: Arc::new(src),
                pump: Some(pump),
            })
        }
    }
}

thread_local! {
    static UI_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Помечает текущий поток как UI-поток (handler `viewer` в `on_pump`): `read_at` из него в
/// debug — паника (приёмка S7-03).
pub fn mark_ui_thread() {
    UI_THREAD.with(|c| c.set(true));
}

/// Текущий поток помечен как UI.
pub fn is_ui_thread() -> bool {
    UI_THREAD.with(Cell::get)
}

/// Источник не читают из UI-потока (S7 §0).
#[inline]
pub(crate) fn assert_off_ui() {
    debug_assert!(!is_ui_thread(), "ByteSource::read_at from the UI thread");
}

/// Сколько читать с `off` при длине `len`.
pub(crate) fn clamp(off: u64, want: usize, len: u64) -> usize {
    usize::try_from(len.saturating_sub(off)).map_or(want, |left| want.min(left))
}
