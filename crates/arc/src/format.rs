//! Формат архива для [`crate::ArcVfs`]: разбор оглавления и чтение записей. Форматы —
//! `formats/{zip,tar,sevenz,rar}` (S8-25, S8-30, S8-32, S8-34); здесь — общий контракт и
//! источник байт архива.

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use urfin_core::{CancelToken, Location};
use urfin_vfs::{ExtractStream, LocalFs, Vfs, VfsError, VfsRead};

use crate::index::{ArchiveIndex, IndexBuilder, NodeId, OuterKey};
use crate::spill::Spill;

/// Откуда байты архива (S8 §3.4).
#[derive(Clone)]
pub(crate) enum Origin {
    /// Обычный файл.
    Local,
    /// Запись другого архива с `Caps::SEEK` (stored): поддиапазон без копирования.
    Range(Arc<dyn Vfs>),
    /// Копия на диске: сжатая запись, rar в архиве, файл сервера.
    Spill(Arc<Spill>),
    /// Не локальный, а реестра VFS нет ([`crate::ArcCache::set_vfs`]).
    Missing,
}

impl Origin {
    fn name(&self) -> &'static str {
        match self {
            Origin::Local => "local",
            Origin::Range(_) => "range",
            Origin::Spill(_) => "spill",
            Origin::Missing => "missing",
        }
    }
}

/// Байты файла архива. Открывается по требованию: повторный вход в архив из кэша его не
/// открывает.
#[derive(Clone)]
pub struct ArcSource {
    key: OuterKey,
    /// Пароль разбора оглавления ([`crate::ArcCache::set_password`]).
    password: Option<Arc<Zeroizing<String>>>,
    origin: Origin,
}

impl std::fmt::Debug for ArcSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArcSource")
            .field("key", &self.key)
            .field("password", &self.password.is_some())
            .field("origin", &self.origin.name())
            .finish()
    }
}

fn origin_of(key: &OuterKey) -> Origin {
    if key.loc.is_local() {
        Origin::Local
    } else {
        Origin::Missing
    }
}

impl ArcSource {
    pub(crate) fn new(key: OuterKey) -> ArcSource {
        let origin = origin_of(&key);
        ArcSource {
            key,
            password: None,
            origin,
        }
    }

    /// С паролем для разбора оглавления (зашифрованные имена 7z).
    pub(crate) fn with_password(key: OuterKey, pw: Option<Zeroizing<String>>) -> ArcSource {
        let origin = origin_of(&key);
        ArcSource {
            key,
            password: pw.map(Arc::new),
            origin,
        }
    }

    /// Байты не из самого файла: поддиапазон или спилл.
    pub(crate) fn with_origin(mut self, origin: Origin) -> ArcSource {
        self.origin = origin;
        self
    }

    /// Копия на диске, если архив спиллен.
    pub(crate) fn spill(&self) -> Option<&Arc<Spill>> {
        match &self.origin {
            Origin::Spill(s) => Some(s),
            _ => None,
        }
    }

    /// Пароль для разбора оглавления: заданный до индекса. Пароль записей — в
    /// [`ArchiveIndex::ctx`].
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref().map(|p| p.as_str())
    }

    /// Адрес файла архива.
    pub fn outer(&self) -> &Location {
        &self.key.loc
    }

    /// Ключ файла архива: адрес, размер, время — по нему формат кэширует своё.
    pub fn key(&self) -> &OuterKey {
        &self.key
    }

    /// Размер файла архива.
    pub fn len(&self) -> u64 {
        self.key.size
    }

    pub fn is_empty(&self) -> bool {
        self.key.size == 0
    }

    /// Путь на диске: обычный файл или его спилл (`UnRAR` открывает по пути, S8 §3.3).
    pub fn local_path(&self) -> Option<&Path> {
        match &self.origin {
            Origin::Local => Some(self.outer().path.as_path()),
            Origin::Spill(s) => Some(s.path()),
            Origin::Range(_) | Origin::Missing => None,
        }
    }

    /// Новый читатель с начала файла; на поток — свой.
    pub fn open(&self) -> Result<Box<dyn VfsRead>, VfsError> {
        match &self.origin {
            Origin::Local => LocalFs::new().open_read(self.outer()),
            Origin::Range(vfs) => vfs.open_read(self.outer()),
            Origin::Spill(s) => LocalFs::new().open_read(&Location::local(s.path())),
            Origin::Missing => Err(VfsError::Unsupported(
                "archive inside a container: no VFS registry",
            )),
        }
    }
}

/// Порция разбора оглавления.
pub trait Scan: Send {
    /// Дописывает следующие записи в `b`. `Ok(false)` — оглавление дочитано.
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError>;
}

/// Формат архива: одна схема адреса (`zip`, `tar`, `7z`, `rar`).
pub trait Format: Send + Sync + 'static {
    /// Схема адреса внутри архива.
    fn scheme(&self) -> &'static str;

    /// Метка формата у пути: у `tar` зависит от файла (`tar.gz`, `tar.zst`).
    fn label(&self, outer: &Location) -> &'static str {
        let _ = outer;
        self.scheme()
    }

    /// Оглавление в начале или в конце файла (zip, 7z): индекс готов после разбора
    /// оглавления. `false` (tar.*, rar) — записи узнаются по ходу чтения: корень
    /// листается потоком, сводка `complete = false`, пока архив не дочитан.
    fn catalog_first(&self) -> bool {
        true
    }

    /// Формат читает архив только по пути на диске (rar — UnRAR): вложенный архив
    /// спиллится и тогда, когда запись читается поддиапазоном.
    fn needs_path(&self) -> bool {
        false
    }

    /// Начать разбор оглавления.
    fn scan(&self, src: &ArcSource) -> Result<Box<dyn Scan>, VfsError>;

    /// Данные файла `node`.
    fn open_entry(
        &self,
        src: &ArcSource,
        idx: &ArchiveIndex,
        node: NodeId,
    ) -> Result<Box<dyn VfsRead>, VfsError>;

    /// Флаги `Caps` чтения файла `node`: `SEEK` (stored zip, plain tar) или
    /// `SEQUENTIAL` (tar.*, solid-7z, rar). `READ_ONLY` ставит `ArcVfs`.
    fn entry_caps(&self, idx: &ArchiveIndex, node: NodeId) -> u32 {
        let _ = (idx, node);
        0
    }

    /// План выборки ([`Vfs::extract_plan`]): `files` — (индекс во входном списке, узел).
    /// `None` — ops читает [`Format::open_entry`] по файлу.
    fn plan(
        &self,
        src: &ArcSource,
        idx: &Arc<ArchiveIndex>,
        files: &[(u32, NodeId)],
        parallel: u8,
    ) -> Option<Vec<Box<dyn ExtractStream>>> {
        let _ = (src, idx, files, parallel);
        None
    }

    /// Независимых читателей одного архива ([`Vfs::transfer_channels`]).
    fn transfer_channels(&self) -> u16 {
        1
    }
}

/// Ошибка «не тот формат» (`arc-error-format`) для архива `outer`.
pub fn format_error(outer: &Location) -> VfsError {
    data_error(outer, urfin_i18n::current().get("arc-error-format"))
}

/// Ошибка «архив повреждён» (`arc-error-corrupt`) с подробностью разборщика.
pub fn corrupt(outer: &Location, detail: impl std::fmt::Display) -> VfsError {
    let msg = urfin_i18n::current().get("arc-error-corrupt");
    data_error(outer, format!("{msg}: {detail}"))
}

/// Запись зашифрована, а пароля нет или он не подошёл (`arc-error-no-password`): вопрос
/// пароля — S8-35.
pub fn no_password(outer: &Location) -> VfsError {
    VfsError::Io {
        path: outer.path.clone(),
        source: io::Error::new(
            io::ErrorKind::PermissionDenied,
            urfin_i18n::current().get("arc-error-no-password"),
        ),
    }
}

/// Многотомный архив (`arc-error-multivolume`).
pub fn multivolume(outer: &Location) -> VfsError {
    data_error(outer, urfin_i18n::current().get("arc-error-multivolume"))
}

/// Пароль и прочее, что нужно для чтения записей, кроме байт архива (S8 §3.7). Живёт в
/// индексе ([`ArchiveIndex::ctx`]): уходит из кэша вместе с ним, пароль затирается
/// (`zeroize`). В `Debug`, логи и сессию не попадает.
#[derive(Default)]
pub struct OpenCtx {
    password: Mutex<Option<Zeroizing<String>>>,
    /// Спилл архива (S8-31): файл живёт, пока жив индекс.
    spill: Mutex<Option<Arc<Spill>>>,
}

impl std::fmt::Debug for OpenCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCtx")
            .field("password", &self.has_password())
            .finish()
    }
}

impl OpenCtx {
    /// Пароль архива (запрос — S8-35); `None` — забыть.
    pub fn set_password(&self, pw: Option<&str>) {
        *self.lock() = pw.map(|p| Zeroizing::new(p.to_owned()));
    }

    pub fn has_password(&self) -> bool {
        self.lock().is_some()
    }

    /// Копия пароля байтами — для потока чтения; затирается при сбросе.
    pub fn password(&self) -> Option<Zeroizing<Vec<u8>>> {
        self.lock()
            .as_ref()
            .map(|p| Zeroizing::new(p.as_bytes().to_vec()))
    }

    /// Копия архива на диске, из которой читаются записи.
    pub(crate) fn spill(&self) -> Option<Arc<Spill>> {
        self.spill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set_spill(&self, s: Option<Arc<Spill>>) {
        *self
            .spill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = s;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Zeroizing<String>>> {
        self.password
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn data_error(outer: &Location, msg: String) -> VfsError {
    VfsError::Io {
        path: outer.path.clone(),
        source: io::Error::new(io::ErrorKind::InvalidData, msg),
    }
}
