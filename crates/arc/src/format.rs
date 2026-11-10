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

/// Байты файла архива. Открывается по требованию: повторный вход в архив из кэша его не
/// открывает.
#[derive(Clone, Debug)]
pub struct ArcSource {
    key: OuterKey,
}

impl ArcSource {
    pub(crate) fn new(key: OuterKey) -> ArcSource {
        ArcSource { key }
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

    /// Путь на диске, если архив — обычный файл (`UnRAR` открывает по пути, S8 §3.3).
    pub fn local_path(&self) -> Option<&Path> {
        let outer = self.outer();
        outer.is_local().then_some(outer.path.as_path())
    }

    /// Новый читатель с начала файла; на поток — свой.
    pub fn open(&self) -> Result<Box<dyn VfsRead>, VfsError> {
        if self.outer().is_local() {
            return LocalFs::new().open_read(self.outer());
        }
        // Архив в архиве и архив на сервере — поддиапазон или спилл (S8-31).
        Err(VfsError::Unsupported("archive inside a container: S8-31"))
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
