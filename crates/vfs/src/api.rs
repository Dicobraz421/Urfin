//! Типы и trait `Vfs` (S4 §2.1).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use compact_str::CompactString;
use rustc_hash::FxHashMap;

pub use urfin_core::{Attrs, CancelToken, Entry, EntryKind};

/// Адрес в VFS — тот же `Location`, что у всего приложения (S4 §0).
pub type VfsPath = urfin_core::Location;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VfsError {
    #[error("not found: {0}")]
    NotFound(PathBuf),
    #[error("access denied: {0}")]
    Denied(PathBuf),
    #[error("not a directory: {0}")]
    NotDir(PathBuf),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("cancelled")]
    Cancelled,
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl VfsError {
    /// Код ошибки ОС, если есть.
    pub fn os_code(&self) -> Option<i32> {
        match self {
            VfsError::Io { source, .. } => source.raw_os_error(),
            _ => None,
        }
    }

    /// `io::Error` для `path` → `NotFound` / `Denied` / `NotDir` / `Io`.
    pub fn from_io(path: impl Into<PathBuf>, e: io::Error) -> VfsError {
        let path = path.into();
        match e.kind() {
            io::ErrorKind::NotFound => VfsError::NotFound(path),
            io::ErrorKind::PermissionDenied => VfsError::Denied(path),
            io::ErrorKind::NotADirectory => VfsError::NotDir(path),
            _ => VfsError::Io { path, source: e },
        }
    }
}

#[derive(Clone, Debug)]
pub struct ListOpts {
    /// Определять `LinkDir`/`LinkFile`/`LinkBroken` (unix: stat цели ссылки).
    pub resolve_links: bool,
    /// Сколько записей желательно в первом батче (первые строки — сразу).
    pub first_batch_hint: u32,
}

impl Default for ListOpts {
    fn default() -> Self {
        ListOpts {
            resolve_links: true,
            first_batch_hint: 256,
        }
    }
}

/// Pull-стрим каталога. Один вызов — один системный буфер. Живёт в одном потоке.
pub trait ListStream: Send {
    /// Дописывает записи в `out`. `Ok(false)` — конец каталога.
    fn next_batch(&mut self, out: &mut Vec<Entry>, cancel: &CancelToken) -> Result<bool, VfsError>;
    /// Оценка числа записей, если ФС её знает.
    fn size_hint(&self) -> Option<u32> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct Metadata {
    pub kind: EntryKind,
    pub attrs: Attrs,
    pub size: u64,
    pub alloc_size: Option<u64>,
    /// Наносекунды от Unix epoch; `i64::MIN` — неизвестно.
    pub mtime: i64,
    pub atime: i64,
    /// Change time (unix) / `ChangeTime` (NT).
    pub ctime: Option<i64>,
    /// Время создания.
    pub btime: Option<i64>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub nlink: Option<u32>,
    /// (устройство / серийный номер тома, inode / `FileId`).
    pub file_id: Option<(u64, u128)>,
    /// Windows.
    pub reparse_tag: Option<u32>,
    /// Только при `stat(follow = false)` на ссылке.
    pub link_target: Option<PathBuf>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    pub free: u64,
    pub avail: u64,
}

/// Возможности backend'а для каталога.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps(pub u32);

impl Caps {
    pub const WATCH: u32 = 1;
    pub const SEEK: u32 = 2;
    pub const LOCAL_PATH: u32 = 4;
    pub const CASE_SENSITIVE: u32 = 8;
    pub const DIR_SIZE_FAST: u32 = 16;
    /// Только чтение: записи и операций над схемой нет (v5 №14).
    pub const READ_ONLY: u32 = 32;
    /// Чтение только подряд, без `Seek` по файлу (v5 №14).
    pub const SEQUENTIAL: u32 = 64;
    /// Схема без своей ФС (`du:`, `pkg:`): строки — вид чужих данных (v5 №14, v6 №1).
    pub const VIRTUAL: u32 = 128;

    pub fn has(self, f: u32) -> bool {
        self.0 & f == f
    }
}

/// Вид колонки схемы (v6 №1): как рисовать ячейку.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ColumnView {
    /// Строка (обрезается «…»).
    Text,
    /// Байты — как колонка «Размер».
    Size,
    /// Наносекунды Unix — как колонка «Изменён».
    Date,
    /// Доля строки в сумме каталога: полоса и процент (`size / parent_total`).
    Bar,
}

/// Колонка, которую объявляет схема (v6 №1, №21): `Vfs::columns(dir)` — колонки после
/// имени слева направо. Данные встроенных `id` — поля `Entry`: `size`, `mtime`,
/// `ctime`, `atime` и `share` (вид `Bar`, доля размера); прочих — данные схемы
/// (`Entry.ext`, [`Vfs::ext`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ColumnSpec {
    /// Имя колонки (`perms`, `owner`).
    pub id: &'static str,
    /// Ключ Fluent заголовка.
    pub title: &'static str,
    /// Ширина по умолчанию, px при 100 %.
    pub width: f32,
    /// Прижата вправо (размер, даты); иначе — влево с отступом.
    pub right: bool,
    pub view: ColumnView,
    /// Моноширинный шрифт (права `drwxr-xr-x`).
    pub mono: bool,
}

impl ColumnSpec {
    /// «Размер» (`Entry.size`).
    pub const SIZE: ColumnSpec = ColumnSpec {
        id: "size",
        title: "ui-col-size",
        width: 92.0,
        right: true,
        view: ColumnView::Size,
        mono: false,
    };
    /// «Изменён» (`Entry.mtime`).
    pub const MTIME: ColumnSpec = ColumnSpec {
        id: "mtime",
        title: "ui-col-mtime",
        width: 120.0,
        right: true,
        view: ColumnView::Date,
        mono: false,
    };

    /// Данные — поле `Entry`, а не схемы.
    pub fn builtin(&self) -> bool {
        matches!(self.id, "size" | "mtime" | "ctime" | "atime" | "share")
    }
}

/// Значение ячейки колонки схемы — `Entry.ext` (v6 №1): провайдер ячеек схемы, данные
/// уже в памяти backend'а (публичный `Entry` не меняется, v6 №21).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtCell {
    Text(CompactString),
    Size(u64),
    Date(i64),
}

/// Чтение файла (вьювер S7, хэши S5). `len` без `is_empty` — так в S4 §2.1.
#[allow(clippy::len_without_is_empty, reason = "API из спеки S4 §2.1")]
pub trait VfsRead: io::Read + io::Seek + Send {
    fn len(&self) -> Option<u64>;
}

/// Событие наблюдения за каталогом (S4 §5; реализация — S4-12/13).
#[derive(Clone, Debug)]
pub enum WatchEvent {
    /// `Entry` есть у Windows Ex-событий.
    Added(CompactString, Option<Entry>),
    Removed(CompactString),
    Modified(CompactString, Option<Entry>),
    Renamed {
        from: CompactString,
        to: CompactString,
        entry: Option<Entry>,
    },
    /// Буфер переполнен → полное перечитывание.
    Overflow,
    /// Сам каталог удалён или недоступен.
    Gone,
}

/// Номер наблюдения у `WatchHub`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WatchId(pub u64);

/// Куда backend шлёт события наблюдения.
#[derive(Clone, Debug)]
pub struct WatchTx {
    pub id: WatchId,
    pub tx: crossbeam_channel::Sender<(WatchId, WatchEvent)>,
}

/// Пока жив — каталог наблюдается; `Drop` снимает наблюдение.
pub struct WatchGuard {
    _inner: Box<dyn Send>,
}

impl WatchGuard {
    pub fn new(inner: impl Send + 'static) -> WatchGuard {
        WatchGuard {
            _inner: Box::new(inner),
        }
    }
}

impl std::fmt::Debug for WatchGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchGuard").finish_non_exhaustive()
    }
}

pub trait Vfs: Send + Sync + 'static {
    /// `"local"`, `"zip"`, …
    fn scheme(&self) -> &'static str;
    fn caps(&self, at: &VfsPath) -> Caps;
    fn list(&self, dir: &VfsPath, opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError>;
    fn stat(&self, p: &VfsPath, follow: bool) -> Result<Metadata, VfsError>;
    /// Метаданные одного имени в каталоге в форме `Entry` (патчи watcher'а).
    fn stat_entry(&self, dir: &VfsPath, name: &str, attrs_hint: Attrs) -> Result<Entry, VfsError>;
    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError>;
    fn space(&self, at: &VfsPath) -> Result<Space, VfsError>;
    /// Для быстрых путей S5/S7 (mmap, `CopyFileEx`). `None` у не-локальных.
    fn local_path(&self, p: &VfsPath) -> Option<PathBuf>;
    /// `Unsupported` → hub включит polling.
    fn watch(&self, dir: &VfsPath, tx: WatchTx) -> Result<WatchGuard, VfsError>;

    /// Колонки после имени, которые задаёт схема для каталога `dir` (v6 №1, №21;
    /// решение 2026-11-02 «du-filters»): набор может
    /// зависеть от папки и вида (`du:/3?f=types` — «Файлов · Доля · Размер»). Зовётся из
    /// UI-потока: только разбор адреса и память backend'а, без I/O. Пусто — колонки вида
    /// панели (Full, Wide, наборы).
    fn columns(&self, _dir: &VfsPath) -> &'static [ColumnSpec] {
        &[]
    }

    /// Ячейка колонки схемы `col` строки `e` каталога `dir` (`Entry.ext`, v6 №1). Зовётся
    /// из UI-потока на видимых строках: только память backend'а, без I/O.
    fn ext(&self, _dir: &VfsPath, _e: &Entry, _col: &str) -> Option<ExtCell> {
        None
    }
}

/// Реестр backend'ов в `AppState.ext` (кладёт handler `vfs`): UI берёт из него колонки
/// схемы таба ([`Vfs::columns`]) и ячейки `Entry.ext` ([`Vfs::ext`]).
#[derive(Clone, Debug)]
pub struct Schemes(pub Arc<VfsRegistry>);

impl Schemes {
    /// Backend `loc`, если он объявил свои колонки для этого каталога.
    pub fn with_columns(&self, loc: &VfsPath) -> Option<&Arc<dyn Vfs>> {
        self.0.get(loc).ok().filter(|v| !v.columns(loc).is_empty())
    }
}

/// Backend'ы по `Location::backend` (S8 регистрирует `zip`, `sftp`, …).
#[derive(Default)]
pub struct VfsRegistry {
    backends: FxHashMap<&'static str, Arc<dyn Vfs>>,
}

impl VfsRegistry {
    /// Реестр с локальной ФС.
    pub fn with_local() -> Self {
        let mut r = VfsRegistry::default();
        r.register(Arc::new(crate::LocalFs::new()));
        r
    }

    pub fn register(&mut self, v: Arc<dyn Vfs>) {
        self.backends.insert(v.scheme(), v);
    }

    pub fn get(&self, loc: &VfsPath) -> Result<&Arc<dyn Vfs>, VfsError> {
        self.backends
            .get(loc.backend.as_str())
            .ok_or(VfsError::Unsupported("no backend for this location"))
    }
}

impl std::fmt::Debug for VfsRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VfsRegistry")
            .field("backends", &self.backends.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Тип корня (диска, тома) — для `LocalFs::roots` (S4-10).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RootKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Ram,
    Other,
}

/// Корень локальной ФС (S4-10).
#[derive(Clone, Debug)]
pub struct RootInfo {
    pub loc: VfsPath,
    pub label: CompactString,
    pub kind: RootKind,
    pub space: Option<Space>,
}
