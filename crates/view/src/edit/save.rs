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

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use encoding_rs::{EncoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE};
use ropey::Rope;
use urfin_core::Cancelled;

use super::DiskStamp;
use crate::sys::{ATTR_KEEP, ATTR_READONLY, FileInfo};

/// Кусок записи (прогресс и отмена между кусками).
const WRITE_CHUNK: usize = 1 << 20;
/// `FILE_ATTRIBUTE_ARCHIVE`: файл изменён.
const ATTR_ARCHIVE: u32 = 0x20;

/// Байты файла и сколько символов не представимы в кодировке (заменены на `?`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub unmappable: usize,
}

/// Текст в кодировке `enc`; `bom` — только у UTF-8 и UTF-16. Концы строк не трогаются:
/// документ хранит их как в файле (смешанные — тоже).
pub fn encode(text: &Rope, enc: &'static Encoding, bom: bool) -> Encoded {
    let mut out = Encoded {
        bytes: Vec::with_capacity(text.len() + 4),
        unmappable: 0,
    };
    if enc == UTF_16LE || enc == UTF_16BE {
        let be = enc == UTF_16BE;
        if bom {
            out.bytes
                .extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
        }
        for c in text.chunks() {
            for u in c.encode_utf16() {
                out.bytes
                    .extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
        }
        return out;
    }
    if enc == UTF_8 {
        if bom {
            out.bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        for c in text.chunks() {
            out.bytes.extend_from_slice(c.as_bytes());
        }
        return out;
    }
    let mut e = enc.new_encoder();
    for c in text.chunks() {
        feed(&mut e, c, false, &mut out);
    }
    feed(&mut e, "", true, &mut out);
    out
}

fn feed(e: &mut encoding_rs::Encoder, mut s: &str, last: bool, out: &mut Encoded) {
    loop {
        let need = e
            .max_buffer_length_from_utf8_without_replacement(s.len())
            .unwrap_or(s.len() * 4 + 16);
        out.bytes.reserve(need);
        let (r, read) = e.encode_from_utf8_to_vec_without_replacement(s, &mut out.bytes, last);
        s = &s[read..];
        match r {
            EncoderResult::InputEmpty => return,
            EncoderResult::OutputFull => {}
            EncoderResult::Unmappable(_) => {
                out.unmappable += 1;
                out.bytes.push(b'?');
            }
        }
    }
}

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

/// Что и куда записать.
#[derive(Clone, Debug)]
pub struct SaveReq {
    pub text: Rope,
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

/// Пишет `bytes` кусками: отмена и прогресс между ними; затем `sync_all`.
fn write_all(
    f: &mut File,
    bytes: &[u8],
    check: &dyn Fn() -> Result<(), Cancelled>,
    progress: &mut dyn FnMut(u64),
) -> Result<(), SaveError> {
    let mut done = 0u64;
    for c in bytes.chunks(WRITE_CHUNK) {
        check()?;
        f.write_all(c)?;
        done += c.len() as u64;
        progress(done);
    }
    f.flush()?;
    f.sync_all()?;
    Ok(())
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
    let _span = tracing::debug_span!("edit.save", path = %req.path.display()).entered();
    let target = target_of(&req.path)?;
    let now = probe(&target)?;
    if let Some(want) = &req.check {
        match &now {
            Some((s, _)) if same_stamp(want, s) => {}
            _ => return Ok(Outcome::Changed),
        }
    }
    let attrs = now.as_ref().map_or(0, |(_, i)| i.attrs);
    let ro = attrs & ATTR_READONLY != 0;
    if ro && req.readonly == ReadOnly::Ask {
        return Ok(Outcome::ReadOnly);
    }
    check()?;
    let enc = encode(&req.text, req.enc, req.bom);
    if enc.unmappable > 0 && req.unmappable == Unmappable::Ask {
        return Ok(Outcome::Unmappable(enc.unmappable));
    }
    let bytes = enc.bytes;
    let keep_ro = ro && req.readonly == ReadOnly::Keep;
    // Атрибуты после записи: прежние (без «только чтение», если его сняли) и «архивный».
    let after = now.is_some().then(|| {
        let ro_bit = if keep_ro { ATTR_READONLY } else { 0 };
        (attrs & ATTR_KEEP & !ATTR_READONLY) | ATTR_ARCHIVE | ro_bit
    });
    if ro {
        crate::sys::set_attrs(&target, attrs & ATTR_KEEP & !ATTR_READONLY)?;
    }
    let how = match write_file(req, &target, &bytes, now.as_ref(), check, progress) {
        Ok(h) => h,
        Err(e) => {
            if ro {
                let _ = crate::sys::set_attrs(&target, attrs & ATTR_KEEP);
            }
            return Err(e);
        }
    };
    if let Some(a) = after
        && crate::sys::attrs(&target).ok().map(|now| now & ATTR_KEEP) != Some(a)
        && let Err(e) = crate::sys::set_attrs(&target, a)
    {
        tracing::warn!(error = %e, "edit.save: attributes not restored");
    }
    let stamp = probe(&target)?
        .map(|(s, _)| s)
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    Ok(Outcome::Saved {
        stamp,
        bytes: bytes.len() as u64,
        how,
        readonly: keep_ro,
    })
}

/// Запись: временный файл и замена, или на месте.
fn write_file(
    req: &SaveReq,
    target: &Path,
    bytes: &[u8],
    now: Option<&(DiskStamp, FileInfo)>,
    check: &dyn Fn() -> Result<(), Cancelled>,
    progress: &mut dyn FnMut(u64),
) -> Result<How, SaveError> {
    let exists = now.is_some();
    if now.is_some_and(|(_, i)| i.nlink > 1) {
        return in_place(target, bytes, check, progress);
    }
    let temp = temp_path(target);
    let mut f = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)
    {
        Ok(f) => f,
        Err(e) if exists && temp_refused(&e) => {
            tracing::debug!(error = %e, "edit.save: no temp file, writing in place");
            return in_place(target, bytes, check, progress);
        }
        Err(e) => return Err(e.into()),
    };
    if let Err(e) = write_all(&mut f, bytes, check, progress) {
        drop(f);
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    drop(f);
    if req.fault == Some(Fault::AfterTemp) {
        // «Падение»: временный файл остаётся, цель не тронута.
        return Err(io::Error::other("fault injected after temp").into());
    }
    if exists {
        match crate::sys::replace_file(target, &temp) {
            Ok(()) => return Ok(How::Replaced),
            Err(e) => tracing::debug!(error = %e, "edit.save: ReplaceFileW failed, MoveFileExW"),
        }
    }
    match crate::sys::move_replace(&temp, target) {
        Ok(()) => Ok(How::Moved),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e.into())
        }
    }
}

/// На месте: truncate + write + flush (жёсткие ссылки и права файла остаются). После
/// truncate отмены нет: недописанный файл хуже дописанного.
fn in_place(
    target: &Path,
    bytes: &[u8],
    check: &dyn Fn() -> Result<(), Cancelled>,
    progress: &mut dyn FnMut(u64),
) -> Result<How, SaveError> {
    check()?;
    let mut f = OpenOptions::new().write(true).truncate(true).open(target)?;
    write_all(&mut f, bytes, &|| Ok(()), progress)?;
    Ok(How::InPlace)
}
