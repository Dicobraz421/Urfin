//! Открытие документа на движке (ADR-0023 §1, §3, §8; S7 §5.1). Карточка S7-29.
//!
//! Открытие = открытие вьювера: stat → первые 64 КБ → детект (§3.2) → документ из
//! одного куска `Original`. Ни декодирования всего файла, ни Rope; лимита размера нет.
//! - До [`MEM_MAX`] файл читается в память целиком и индексируется здесь же
//!   ([`Guard::Memory`]): ссылок на диск у документа нет, внешнее изменение — только
//!   конфликт `DiskStamp` при сохранении.
//! - Больше — источник `File` (`seek_read`, не mmap) по handle с share `READ | DELETE`
//!   ([`Guard::Locked`]): чужая запись получает «файл занят». Файл уже пишет другая
//!   программа (`ERROR_SHARING_VIOLATION`) — без запрета ([`Guard::Unlocked`]), первая
//!   правка спрашивает «Править копию» ([`Guard::Copy`]). Индекс строк — фоновый
//!   `Indexer` вьювера.
//!
//! Концы строк и `lossy` у файла в памяти — по всем байтам, у большого — по выборке.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use encoding_rs::DecoderResult;
use urfin_core::{CancelToken, Location};
use urfin_vfs::VfsRegistry;

use super::DiskStamp;
use super::load::{EolCount, LoadError, LoadOpts, eol_new, eol_of};
use super::piece::{AddBuf, TextDoc};
use crate::detect::{Eol, detect};
use crate::enc::Enc;
use crate::index::{LineIndex, Sep};
use crate::source::{
    ByteSource, FileSource, MmapPolicy, OwnedSource, SourceKind, SourceOpts, open_source, read_full,
};
use crate::viewer::{SAMPLE, bom_len};

/// Файл до этого размера — в памяти целиком, без запрета записи (ADR-0023 §1).
pub const MEM_MAX: u64 = 16 << 20;

/// Защита исходника документа (ADR-0023 §8).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Guard {
    /// Файл в памяти (≤ 16 МБ) или новый.
    #[default]
    Memory,
    /// Свой handle без `FILE_SHARE_WRITE`: другие не пишут.
    Locked,
    /// Файл пишет другая программа: запрет не получен, первая правка спрашивает.
    Unlocked,
    /// Куски ссылаются на свою копию во `%TEMP%` («Править копию»).
    Copy,
}

/// Открытый документ: текст и его свойства.
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые признаки документа"
)]
#[derive(Debug)]
pub struct Opened {
    pub text: TextDoc,
    /// Кодировка документа; `bom_len` — BOM файла (куски начинаются после него).
    pub enc: Enc,
    /// Что дал детект (кольцо F8: «авто»).
    pub detected: Enc,
    pub bom: bool,
    pub eol: Eol,
    pub eol_new: Eol,
    pub lossy: bool,
    pub lang: Option<&'static str>,
    pub guard: Guard,
    pub stamp: DiskStamp,
    pub readonly: bool,
    /// Файла нет: новый документ (`editor.new`).
    pub new: bool,
}

/// Концы строк в байтах кодировки `enc` (по единицам кода).
pub fn count_eols(b: &[u8], enc: &Enc) -> EolCount {
    let mut c = EolCount::default();
    let mut tally = |cr: bool, lf: bool, prev_cr: &mut bool| {
        if *prev_cr && lf {
            c.crlf += 1;
        } else {
            if *prev_cr {
                c.cr += 1;
            }
            if lf {
                c.lf += 1;
            }
        }
        *prev_cr = cr;
    };
    let mut prev = false;
    if enc.is_utf16() {
        let be = enc.is_utf16be();
        for u in b.as_chunks::<2>().0 {
            let (k, z) = if be { (u[1], u[0]) } else { (u[0], u[1]) };
            let z = z == 0;
            tally(z && k == b'\r', z && k == b'\n', &mut prev);
        }
    } else {
        for &x in b {
            tally(x == b'\r', x == b'\n', &mut prev);
        }
    }
    if prev {
        c.cr += 1;
    }
    c
}

/// Есть ли в байтах недекодируемые в `enc`; `last` — байты кончаются концом файла (иначе
/// недописанный хвостовой символ не в счёт).
pub fn has_bad(b: &[u8], enc: &Enc, last: bool) -> bool {
    if enc.is_utf8() {
        return match std::str::from_utf8(b) {
            Ok(_) => false,
            Err(e) => e.error_len().is_some() || last,
        };
    }
    let mut dec = enc.enc.new_decoder_without_bom_handling();
    let mut out = vec![0u8; 64 << 10];
    let mut input = b;
    loop {
        let (r, read, _) = dec.decode_to_utf8_without_replacement(input, &mut out, last);
        input = &input[read..];
        match r {
            DecoderResult::InputEmpty => return false,
            DecoderResult::OutputFull => {}
            DecoderResult::Malformed(..) => return true,
        }
    }
}

/// Пустой документ нового файла (`editor.new`).
fn new_doc(doc: u32, o: &LoadOpts) -> Opened {
    let n = o.new.expect("new document options");
    let unicode = n.enc == encoding_rs::UTF_8
        || n.enc == encoding_rs::UTF_16LE
        || n.enc == encoding_rs::UTF_16BE;
    let bom = n.bom && unicode;
    let bom_len = match (bom, n.enc == encoding_rs::UTF_8) {
        (false, _) => 0,
        (true, true) => 3,
        (true, false) => 2,
    };
    let enc = Enc {
        bom_len,
        ..Enc::new(n.enc)
    };
    let eol = eol_new(o.eol, EolCount::default());
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(Vec::new()));
    let sep = Sep::of(&enc, eol);
    let text = TextDoc::new(src, 0, sep, AddBuf::new(doc), LineIndex::default());
    Opened {
        text,
        enc,
        detected: enc,
        bom,
        eol,
        eol_new: eol,
        lossy: false,
        lang: crate::lang::detect(&o.name, &[]),
        guard: Guard::Memory,
        stamp: DiskStamp::default(),
        readonly: false,
        new: true,
    }
}

/// Документ над открытым источником: выборка, детект, индекс файла в памяти.
fn build(
    src: Arc<dyn ByteSource>,
    doc: u32,
    o: &LoadOpts,
    guard: Guard,
    stamp: DiskStamp,
    readonly: bool,
) -> Result<Opened, LoadError> {
    let len = src.len();
    let mut head = vec![0u8; SAMPLE.min(usize::try_from(len).unwrap_or(SAMPLE))];
    let n = read_full(&*src, 0, &mut head)?;
    head.truncate(n);
    let at_eof = n as u64 >= len;
    let det = detect(&head, at_eof, o.tld);
    let enc = match o.enc {
        Some(e) => Enc {
            bom_len: bom_len(e, &head),
            ..Enc::new(e)
        },
        None => det.enc,
    };
    let lang = if det.binary {
        None
    } else {
        crate::lang::detect(&o.name, &head)
    };
    let bom = u64::from(enc.bom_len).min(len);
    let (count, lossy) = if let Some(all) = src.mapped() {
        let body = &all[bom as usize..];
        (count_eols(body, &enc), has_bad(body, &enc, true))
    } else {
        let body = &head[(bom as usize).min(head.len())..];
        (count_eols(body, &enc), has_bad(body, &enc, at_eof))
    };
    let eol = eol_of(count);
    let sep = Sep::of(&enc, eol);
    let index = if src.mapped().is_some() {
        super::lines::index_now(&*src, sep)?
    } else {
        LineIndex::default()
    };
    let text = TextDoc::new(src, bom, sep, AddBuf::new(doc), index);
    Ok(Opened {
        text,
        enc,
        detected: det.enc,
        bom: enc.bom_len > 0,
        eol,
        eol_new: eol_new(o.eol, count),
        lossy,
        lang,
        guard,
        stamp,
        readonly,
        new: false,
    })
}

fn stamp_of(f: &File, meta: &std::fs::Metadata) -> DiskStamp {
    DiskStamp {
        size: meta.len(),
        mtime: super::save::mtime_ns(meta),
        file_id: crate::sys::file_id(f).ok(),
    }
}

/// Локальный файл (вне UI-потока, Interactive lane). `doc` — номер документа (имя
/// временного файла правок).
pub fn open_path(
    path: &Path,
    doc: u32,
    o: &LoadOpts,
    cancel: &CancelToken,
) -> Result<Opened, LoadError> {
    let _span = tracing::debug_span!("edit.open", path = %path.display()).entered();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound && o.new.is_some() => {
            return Ok(new_doc(doc, o));
        }
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    let readonly = meta.permissions().readonly();
    cancel.check()?;
    if meta.len() <= MEM_MAX {
        let stamp = stamp_of(&file, &meta);
        let src = Arc::new(OwnedSource::read(file, meta.len())?);
        return build(src, doc, o, Guard::Memory, stamp, readonly);
    }
    let (file, guard) = match crate::sys::open_locked(path) {
        Ok(locked) => (locked, Guard::Locked),
        Err(e) if crate::sys::sharing_violation(&e) => {
            tracing::debug!("edit.open: file is being written, no write lock");
            (file, Guard::Unlocked)
        }
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    let stamp = stamp_of(&file, &meta);
    let src = Arc::new(FileSource::new(file, path.to_path_buf(), false));
    build(src, doc, o, guard, stamp, readonly)
}

/// Любой VFS: локальный путь — [`open_path`]; иначе источник вьювера (Owned, Seek; своя
/// копия — Spool, S8), без запрета записи.
pub fn open(
    reg: &VfsRegistry,
    loc: &Location,
    doc: u32,
    o: &LoadOpts,
    cancel: &CancelToken,
) -> Result<Opened, LoadError> {
    let vfs = reg.get(loc)?;
    if let Some(path) = vfs.local_path(loc) {
        return open_path(&path, doc, o, cancel);
    }
    let meta = vfs.stat(loc, true).ok();
    let so = SourceOpts {
        mmap: MmapPolicy::Off,
        ..SourceOpts::default()
    };
    let opened = open_source(reg, loc, &so, cancel)?;
    let stamp = DiskStamp {
        size: opened.src.len(),
        mtime: meta.as_ref().map_or(i64::MIN, |m| m.mtime),
        file_id: None,
    };
    let guard = if opened.src.kind() == SourceKind::Owned {
        Guard::Memory
    } else {
        Guard::Copy
    };
    build(opened.src, doc, o, guard, stamp, false)
}

#[cfg(test)]
#[path = "open_tests.rs"]
pub(crate) mod tests;
