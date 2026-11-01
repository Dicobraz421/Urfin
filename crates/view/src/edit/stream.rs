//! Сохранение документа на движке потоком (ADR-0023 §8; S7 §5.4). Карточка S7-31.
//!
//! - Снимок кусков → временный файл блоками ≤ 4 МБ (`write::blocks`, `read_at`
//!   источников, не кэш страниц): память постоянная. Та же кодировка — байты кусков как
//!   есть (без правок — те же байты), другая — `Recode` потоком + BOM; непредставимые
//!   считаются по ходу записи временного файла — цель при вопросе не тронута.
//! - Перед записью — свободное место на томе ≥ размера ([`super::Outcome::NoSpace`]).
//! - Замена — дисковый слой S7-17 (`ReplaceFileW` → `MoveFileExW`, атрибуты, symlink).
//!   Проверено на стенде (S7-31): `ReplaceFileW` проходит при нашем handle с запретом
//!   записи другим, прежний handle читает старые байты; отказ — `ReplaceFileW` с
//!   резервной копией, копия сразу удаляется (handle держит её байты до закрытия).
//! - «На месте» (жёсткие ссылки, нет прав на каталог) — временный файл во `%TEMP%`,
//!   затем копия в цель.
//! - Записанный файл открывается заново ([`Reopened`]): ≤ 16 МБ — байты записи в памяти,
//!   больше — handle с запретом записи другим; индекс строк — по ходу записи (готов).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use urfin_core::Cancelled;

use super::super::open::{Guard, MEM_MAX};
use super::super::piece::TextSnap;
use super::super::write::{Recode, blocks};
use super::{
    ATTR_ARCHIVE, Fault, How, Outcome, ReadOnly, SaveError, SaveReq, Unmappable, probe, replace,
    same_stamp, target_of, temp_path, temp_refused,
};
use crate::detect::Eol;
use crate::enc::Enc;
use crate::index::{LineIndex, MARK_EVERY, Sep, scan};
use crate::source::{ByteSource, FileSource, OwnedSource};
use crate::sys::{ATTR_KEEP, ATTR_READONLY};

/// Снимок документа на движке.
#[derive(Clone, Debug)]
pub struct SnapBody {
    pub snap: TextSnap,
    /// Кодировка байт снимка (`bom_len` = 0).
    pub from: Enc,
    /// Концы строк документа: разделитель индекса нового файла при перекодировании.
    pub eol: Eol,
    /// Сверять свободное место на томе (ответ «Сохранить всё равно» — `false`).
    pub space: bool,
    /// Открыть записанный файл заново ([`Reopened`]).
    pub reopen: bool,
}

/// Записанный файл, открытый заново: исходник документа после сохранения.
pub struct Reopened {
    pub src: Arc<dyn ByteSource>,
    pub guard: Guard,
    /// Индекс строк нового файла (готов: посчитан по ходу записи).
    pub index: LineIndex,
    /// Байт BOM в начале файла.
    pub bom: u64,
}

impl std::fmt::Debug for Reopened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reopened")
            .field("len", &self.src.len())
            .field("guard", &self.guard)
            .field("lines", &self.index.lines)
            .field("bom", &self.bom)
            .finish()
    }
}

/// Итог сохранения и файл, открытый заново.
#[derive(Debug)]
pub struct Saved {
    pub outcome: Outcome,
    pub reopened: Option<Reopened>,
}

impl From<Outcome> for Saved {
    fn from(outcome: Outcome) -> Saved {
        Saved {
            outcome,
            reopened: None,
        }
    }
}

/// BOM кодировки (только UTF-8 и UTF-16).
fn bom_bytes(enc: &'static Encoding, bom: bool) -> &'static [u8] {
    if !bom {
        &[]
    } else if enc == UTF_8 {
        &[0xEF, 0xBB, 0xBF]
    } else if enc == UTF_16LE {
        &[0xFF, 0xFE]
    } else if enc == UTF_16BE {
        &[0xFE, 0xFF]
    } else {
        &[]
    }
}

/// Индекс строк файла по ходу записи (координаты файла, BOM — в первой строке).
struct IndexBuild {
    sep: Sep,
    lines: u64,
    last: u64,
    marks: Vec<(u64, u64)>,
    /// UTF-16: первый байт единицы на границе кусков записи и его смещение.
    carry: Option<(u64, u8)>,
}

impl IndexBuild {
    fn new(sep: Sep) -> IndexBuild {
        IndexBuild {
            sep,
            lines: 0,
            last: 0,
            marks: vec![(0, 0)],
            carry: None,
        }
    }

    fn feed(&mut self, b: &[u8], base: u64) {
        if b.is_empty() {
            return;
        }
        let sep = self.sep;
        let (lines, last, marks) = (&mut self.lines, &mut self.last, &mut self.marks);
        let mut on = |s: u64| {
            *lines += 1;
            if *lines - *last >= MARK_EVERY {
                marks.push((s, *lines));
                *last = *lines;
            }
        };
        if let Some((at, c)) = self.carry.take() {
            scan(&[c, b[0]], at, sep, &mut on);
        }
        scan(b, base, sep, &mut on);
        let end = base + b.len() as u64;
        if sep.unit() == 2 && end % 2 == 1 {
            self.carry = Some((end - 1, b[b.len() - 1]));
        }
    }

    fn finish(self, len: u64) -> LineIndex {
        LineIndex {
            marks: self.marks,
            lines: self.lines,
            upto: len,
            done: true,
        }
    }
}

/// Записанное потоком.
struct Wrote {
    len: u64,
    unmappable: usize,
    /// Байты файла, если он не больше [`MEM_MAX`] (исходник документа после сохранения).
    mem: Option<Vec<u8>>,
    index: LineIndex,
}

/// Поток записи: файл, копия в памяти до порога, индекс, прогресс.
struct Sink<'a> {
    file: &'a mut File,
    len: u64,
    mem: Option<Vec<u8>>,
    ix: IndexBuild,
    progress: &'a mut dyn FnMut(u64),
}

impl Sink<'_> {
    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        if b.is_empty() {
            return Ok(());
        }
        self.file.write_all(b)?;
        self.ix.feed(b, self.len);
        self.len += b.len() as u64;
        if self.len > MEM_MAX {
            self.mem = None;
        } else if let Some(m) = &mut self.mem {
            m.extend_from_slice(b);
        }
        (self.progress)(self.len);
        Ok(())
    }
}

/// Снимок → файл: BOM, куски блоками (та же кодировка — как есть, другая — `Recode`),
/// `sync_all`.
fn write_snap(
    file: &mut File,
    b: &SnapBody,
    enc: &'static Encoding,
    bom: &[u8],
    check: &dyn Fn() -> Result<(), Cancelled>,
    progress: &mut dyn FnMut(u64),
) -> Result<Wrote, SaveError> {
    let same = b.from.enc == enc;
    let sep = if same {
        b.snap.sep()
    } else {
        Sep::of(&Enc::new(enc), b.eol)
    };
    let mut sink = Sink {
        file,
        len: 0,
        // Копия в памяти — только если файл заведомо не больше порога: пик памяти записи
        // большого файла — блок чтения, не 16 МБ.
        mem: (b.snap.len() + bom.len() as u64 <= MEM_MAX).then(Vec::new),
        ix: IndexBuild::new(sep),
        progress,
    };
    sink.put(bom)?;
    let mut unmappable = 0;
    if same {
        blocks(&b.snap, check, &mut |_, data| sink.put(data))?;
    } else {
        let total = b.snap.len();
        let mut rc = Recode::new(b.from, enc, None);
        blocks(&b.snap, check, &mut |base, data| {
            let last = base + data.len() as u64 == total;
            rc.feed(data, last, &mut |o| sink.put(o))
        })?;
        if total == 0 {
            rc.feed(&[], true, &mut |o| sink.put(o))?;
        }
        unmappable = rc.unmappable;
    }
    sink.file.flush()?;
    sink.file.sync_all()?;
    let len = sink.len;
    Ok(Wrote {
        len,
        unmappable,
        mem: sink.mem,
        index: sink.ix.finish(len),
    })
}

/// Временный файл во `%TEMP%\Urfin` (запись «на месте»: сначала сюда, затем в цель).
fn spare_temp() -> io::Result<(File, PathBuf)> {
    let dir = std::env::temp_dir().join("Urfin");
    std::fs::create_dir_all(&dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = dir.join(format!("save-{}-{nanos}.tmp", std::process::id()));
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok((f, path))
}

/// Временный файл → цель на месте (truncate + копия + flush): жёсткие ссылки и права
/// файла остаются. Цель под нашим запретом записи (документ > 16 МБ) — ошибка с
/// подсказкой «Сохранить как».
fn copy_in_place(temp: &Path, target: &Path) -> Result<(), SaveError> {
    let mut src = File::open(temp)?;
    let mut f = match OpenOptions::new().write(true).truncate(true).open(target) {
        Ok(f) => f,
        Err(e) if crate::sys::sharing_violation(&e) => {
            let msg = format!("{e} (hard-linked file open with a write lock: use Save As)");
            return Err(io::Error::new(e.kind(), msg).into());
        }
        Err(e) => return Err(e.into()),
    };
    io::copy(&mut src, &mut f)?;
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

/// Записанный файл — заново: ≤ [`MEM_MAX`] — байты записи (handle не нужен), больше —
/// handle с запретом записи другим. Файл уже другой (пишут) — `None`.
fn reopen(target: &Path, w: Wrote, bom: u64) -> Option<Reopened> {
    if let Some(mem) = w.mem {
        return Some(Reopened {
            src: Arc::new(OwnedSource::new(mem)),
            guard: Guard::Memory,
            index: w.index,
            bom,
        });
    }
    let f = match crate::sys::open_locked(target) {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(error = %e, "edit.save: saved file not reopened with a lock");
            return None;
        }
    };
    let src = FileSource::new(f, target.to_path_buf(), false);
    if src.len() != w.len {
        tracing::debug!("edit.save: saved file changed before reopen");
        return None;
    }
    Some(Reopened {
        src: Arc::new(src),
        guard: Guard::Locked,
        index: w.index,
        bom,
    })
}

/// Снимок движка (ADR-0023 §8): поток во временный файл, затем замена (или копия на
/// место) и файл — заново.
#[allow(
    clippy::too_many_lines,
    reason = "проверки, запись и замена — один путь сверху вниз, как `save_rope`"
)]
pub(super) fn save_snap(
    req: &SaveReq,
    body: &SnapBody,
    check: &dyn Fn() -> Result<(), Cancelled>,
    progress: &mut dyn FnMut(u64),
) -> Result<Saved, SaveError> {
    let _span =
        tracing::debug_span!("edit.save", path = %req.path.display(), len = body.snap.len())
            .entered();
    let target = target_of(&req.path)?;
    let now = probe(&target)?;
    if let Some(want) = &req.check {
        match &now {
            Some((s, _)) if same_stamp(want, s) => {}
            _ => return Ok(Outcome::Changed.into()),
        }
    }
    let attrs = now.as_ref().map_or(0, |(_, i)| i.attrs);
    let ro = attrs & ATTR_READONLY != 0;
    if ro && req.readonly == ReadOnly::Ask {
        return Ok(Outcome::ReadOnly.into());
    }
    check()?;
    let bom = bom_bytes(req.enc, req.bom);
    if body.space {
        let need = body.snap.len() + bom.len() as u64;
        let dir = target
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        match crate::sys::free_space(dir) {
            Ok(free) if free < need => return Ok(Outcome::NoSpace { need, free }.into()),
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "edit.save: free space unknown"),
        }
    }
    let exists = now.is_some();
    // Временный файл: рядом с целью; жёсткие ссылки или нет прав на каталог — во
    // `%TEMP%`, затем копия на место.
    let links = now.as_ref().is_some_and(|(_, i)| i.nlink > 1);
    let (mut file, temp, in_place) = if links {
        let (f, p) = spare_temp()?;
        (f, p, true)
    } else {
        let t = temp_path(&target);
        match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&t)
        {
            Ok(f) => (f, t, false),
            Err(e) if exists && temp_refused(&e) => {
                tracing::debug!(error = %e, "edit.save: no temp file, writing in place");
                let (f, p) = spare_temp()?;
                (f, p, true)
            }
            Err(e) => return Err(e.into()),
        }
    };
    let w = match write_snap(&mut file, body, req.enc, bom, check, progress) {
        Ok(w) => w,
        Err(e) => {
            drop(file);
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
    };
    drop(file);
    if w.unmappable > 0 && req.unmappable == Unmappable::Ask {
        let _ = std::fs::remove_file(&temp);
        return Ok(Outcome::Unmappable(w.unmappable).into());
    }
    if req.fault == Some(Fault::AfterTemp) {
        // «Падение»: временный файл остаётся, цель не тронута.
        return Err(io::Error::other("fault injected after temp").into());
    }
    let keep_ro = ro && req.readonly == ReadOnly::Keep;
    let after = exists.then(|| {
        let ro_bit = if keep_ro { ATTR_READONLY } else { 0 };
        (attrs & ATTR_KEEP & !ATTR_READONLY) | ATTR_ARCHIVE | ro_bit
    });
    if ro {
        crate::sys::set_attrs(&target, attrs & ATTR_KEEP & !ATTR_READONLY)?;
    }
    let res = if in_place {
        let r = copy_in_place(&temp, &target).map(|()| How::InPlace);
        let _ = std::fs::remove_file(&temp);
        r
    } else {
        replace(&target, &temp, exists)
    };
    let how = match res {
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
    let bytes = w.len;
    let reopened = if body.reopen {
        reopen(&target, w, bom.len() as u64)
    } else {
        None
    };
    let stamp = probe(&target)?
        .map(|(s, _)| s)
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    Ok(Saved {
        outcome: Outcome::Saved {
            stamp,
            bytes,
            how,
            readonly: keep_ro,
        },
        reopened,
    })
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
