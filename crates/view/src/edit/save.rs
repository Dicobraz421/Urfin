//! Сохранение документа редактора на диск (S7 §5.4), вне UI-потока.
//!
//! - Кодирование: кодировка и BOM «как были»; непредставимые символы — вопрос
//!   ([`Outcome::Unmappable`]) или `?` по ответу.
//! - Перед записью — сверка [`DiskStamp`] (внешнее изменение → [`Outcome::Changed`]) и
//!   атрибут «только чтение» (→ [`Outcome::ReadOnly`]).
//! - Атомарно: `.<имя>.urfin~` в том же каталоге → `sync_all` → `ReplaceFileW` (ACL,
//!   время создания, потоки — от прежнего файла); не вышло — `MoveFileExW`. Атрибуты
//!   (скрытый, системный, только чтение…) возвращаются файлу.
//! - На месте (truncate + write + flush): у файла несколько жёстких ссылок или нет прав
//!   на каталог. Symlink — пишется в цель.
//! - Документ на движке (S7-31, ADR-0023 §8; [`Body::Snap`]): снимок кусков пишется
//!   **потоком** (`stream.rs`).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use encoding_rs::Encoding;
use urfin_core::Cancelled;

use super::DiskStamp;
use crate::sys::FileInfo;

#[path = "stream.rs"]
mod stream;

pub use stream::{Reopened, Saved, SnapBody};

/// `FILE_ATTRIBUTE_ARCHIVE`: файл изменён.
const ATTR_ARCHIVE: u32 = 0x20;

/// Непредставимые символы.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unmappable {
    /// Спросить ([`Outcome::Unmappable`]), ничего не записав.
    #[default]
    Ask,
    /// Записать `?` вместо них.
    Replace,
}

/// Атрибут «только чтение» у файла.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadOnly {
    /// Спросить ([`Outcome::ReadOnly`]), ничего не записав.
    #[default]
    Ask,
    /// Записать, атрибут остаётся.
    Keep,
    /// Записать и снять атрибут.
    Clear,
}

/// Сбой для тестов: процесс «упал» после записи временного файла.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    AfterTemp,
}

/// Что записать.
#[derive(Clone, Debug)]
pub enum Body {
    /// Документ на движке: снимок кусков.
    Snap(SnapBody),
}

impl Body {
    /// Байт текста (без BOM): прогресс и порог задачи job-бара.
    pub fn len(&self) -> u64 {
        match self {
            Body::Snap(s) => s.snap.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Что и куда записать.
#[derive(Clone, Debug)]
pub struct SaveReq {
    pub text: Body,
    pub enc: &'static Encoding,
    pub bom: bool,
    pub path: PathBuf,
    /// Отметка, с которой сверяется файл; `None` — не сверять (новый файл, «Сохранить
    /// поверх», «Сохранить как» в другой файл).
    pub check: Option<DiskStamp>,
    pub unmappable: Unmappable,
    pub readonly: ReadOnly,
    /// Только тесты: сбой посреди записи.
    pub fault: Option<Fault>,
}

/// Как файл попал на диск.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// `ReplaceFileW`.
    Replaced,
    /// `MoveFileExW` (запасной путь или новый файл).
    Moved,
    /// На месте: жёсткие ссылки или нет прав на каталог.
    InPlace,
}

/// Итог сохранения.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Saved {
        stamp: DiskStamp,
        bytes: u64,
        how: How,
        /// У файла остался атрибут «только чтение».
        readonly: bool,
    },
    /// Столько символов нельзя записать в кодировке; ничего не записано.
    Unmappable(usize),
    /// Файл изменён на диске после загрузки (или удалён); ничего не записано.
    Changed,
    /// Атрибут «только чтение»; ничего не записано.
    ReadOnly,
    /// На томе меньше места, чем нужно (байты); ничего не записано.
    NoSpace { need: u64, free: u64 },
}

/// Ошибка записи.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("cancelled")]
    Cancelled,
}

impl From<Cancelled> for SaveError {
    fn from(_: Cancelled) -> Self {
        SaveError::Cancelled
    }
}

/// Время изменения в наносекундах Unix; неизвестно — `i64::MIN` (как в VFS).
pub(crate) fn mtime_ns(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(i64::MIN)
}

/// Отметка и сведения о файле по пути; `None` — файла нет.
fn probe(path: &Path) -> io::Result<Option<(DiskStamp, FileInfo)>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = f.metadata()?;
    let info = crate::sys::file_info(&f)?;
    Ok(Some((
        DiskStamp {
            size: meta.len(),
            mtime: mtime_ns(&meta),
            file_id: Some(info.id),
        },
        info,
    )))
}

/// Отметка файла сейчас (возврат на экран редактора); `None` — файла нет.
pub fn stamp_of(path: &Path) -> io::Result<Option<DiskStamp>> {
    Ok(probe(&target_of(path)?)?.map(|(s, _)| s))
}

/// Отметки совпадают: размер, время и id файла (если известны оба).
pub fn same_stamp(a: &DiskStamp, b: &DiskStamp) -> bool {
    a.size == b.size
        && a.mtime == b.mtime
        && match (a.file_id, b.file_id) {
            (Some(x), Some(y)) => x == y,
            _ => true,
        }
}

/// Куда писать: symlink — в цель (относительная — от каталога ссылки).
fn target_of(path: &Path) -> io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            let t = std::fs::read_link(path)?;
            Ok(match path.parent() {
                Some(dir) if t.is_relative() => dir.join(t),
                _ => t,
            })
        }
        _ => Ok(path.to_path_buf()),
    }
}

/// Имя временного файла: `.<имя>.urfin~` рядом с целью.
pub fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.urfin~"))
}

/// Нет прав создать файл рядом или имя не годится — писать на месте.
fn temp_refused(e: &io::Error) -> bool {
    // 123 — ERROR_INVALID_NAME, 206 — ERROR_FILENAME_EXCED_RANGE.
    e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(123 | 206))
}

/// Сохраняет документ (вне UI-потока). `check` — точка отмены (пауза задачи), `progress`
/// — записано байт.
pub fn save(
    req: &SaveReq,
    check: &dyn Fn() -> Result<(), Cancelled>,
    progress: &mut dyn FnMut(u64),
) -> Result<Outcome, SaveError> {
    save_full(req, check, progress).map(|s| s.outcome)
}

/// Как [`save`], и записанный файл, открытый заново (снимок движка с `reopen`).
pub fn save_full(
    req: &SaveReq,
    check: &dyn Fn() -> Result<(), Cancelled>,
    progress: &mut dyn FnMut(u64),
) -> Result<Saved, SaveError> {
    match &req.text {
        Body::Snap(b) => stream::save_snap(req, b, check, progress),
    }
}

/// Имя резервной копии запасного пути замены: `.<имя>.urfin-old~`.
fn backup_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.urfin-old~"))
}

/// Временный файл — на место цели: `ReplaceFileW`; отказ — `ReplaceFileW` с резервной
/// копией, которая сразу удаляется (handle документа на прежний файл держит её байты до
/// закрытия, ADR-0023 §8 п. 4); не вышло — `MoveFileExW`.
fn replace(target: &Path, temp: &Path, exists: bool) -> Result<How, SaveError> {
    if exists {
        match crate::sys::replace_file(target, temp) {
            Ok(()) => return Ok(How::Replaced),
            Err(e) => tracing::debug!(error = %e, "edit.save: ReplaceFileW failed, with backup"),
        }
        let backup = backup_path(target);
        match crate::sys::replace_file_backup(target, temp, &backup) {
            Ok(()) => {
                if let Err(e) = std::fs::remove_file(&backup) {
                    tracing::warn!(error = %e, "edit.save: backup not removed");
                }
                return Ok(How::Replaced);
            }
            Err(e) => tracing::debug!(error = %e, "edit.save: ReplaceFileW failed, MoveFileExW"),
        }
    }
    match crate::sys::move_replace(temp, target) {
        Ok(()) => Ok(How::Moved),
        Err(e) => {
            let _ = std::fs::remove_file(temp);
            Err(e.into())
        }
    }
}
