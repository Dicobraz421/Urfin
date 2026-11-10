//! zip (S8 §3.3): оглавление — central directory (`zip` 9.4, ZIP64 — внутри крейта).
//!
//! - Имена: UTF-8 при флаге bit 11 или extra 0x7075 (Info-ZIP Unicode Path, применяет
//!   `zip`); иначе — кодовая страница `[arc] zip_codepage` (`auto` — OEM системы).
//!   Невалидное — `Attrs::ESCAPED_NAME`.
//! - Время: extra 0x000A (NTFS) и 0x5455 (UT) — UTC; иначе DOS-время как местное. Так же
//!   выбирает 7-Zip.
//! - Чтение: stored без шифрования — поддиапазон файла с `seek` (`Caps::SEEK`); сжатые и
//!   зашифрованные — поток (`Caps::SEQUENTIAL`), `len()` известен, `seek` — ошибка.
//!   Выборка ([`Format::plan`]) — до 4 независимых читателей, у каждого свой дескриптор.
//! - Шифрование (AES-1/2, ZipCrypto): пароль — [`crate::OpenCtx`] индекса (запрос —
//!   S8-35); без пароля — `arc-error-no-password`.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, PoisonError};

use ::zip::read::{ZipArchiveMetadata, ZipFileEntry, ZipReadOptions};
use ::zip::result::ZipError;
use ::zip::{CompressionMethod, DateTime, HasZipMetadata, System, ZipArchive};
use jiff::tz::TimeZone;
use oem_cp::code_table_type::TableType;
use urfin_core::{Attrs, CancelToken, EntryKind, Location};
use urfin_vfs::{Caps, ExtractStream, Extracted, VfsError, VfsRead};
use zeroize::Zeroizing;

use crate::config::ArcCfg;
use crate::format::{ArcSource, Format, Scan, corrupt, format_error, multivolume, no_password};
use crate::index::{ArchiveIndex, EntryLoc, IndexBuilder, NodeId, OuterKey, RawEntry};

/// Записей за один шаг разбора.
const STEP: usize = 8192;

/// Разобранных оглавлений в памяти формата (F3 и выборка не перечитывают их).
const METAS: usize = 4;

/// Порция потока сжатой записи.
const CHUNK: usize = 256 << 10;

/// Независимых читателей одного архива (SSD; на HDD ops ограничит сам).
const CHANNELS: u16 = 4;

/// Бит в `EntryLoc::Index`: запись stored и не зашифрована — читается с `seek`. Без него
/// `caps` пришлось бы читать оглавление из UI-потока.
const SEEKABLE: u32 = 1 << 31;

/// Раскладка `EntryLoc::Index` zip: номер записи в оглавлении и признак `seek`.
fn unpack(loc: EntryLoc) -> Option<(usize, bool)> {
    match loc {
        EntryLoc::Index(v) => Some(((v & !SEEKABLE) as usize, v & SEEKABLE != 0)),
        _ => None,
    }
}

/// Кодовая страница имён без флага UTF-8.
#[derive(Clone, Debug)]
enum Codec {
    Oem(TableType),
    Other(&'static encoding_rs::Encoding),
}

impl Codec {
    /// `auto` — OEM системы (реестр, иначе 437); номер — OEM-таблица или `encoding_rs`.
    fn of(setting: &str) -> Codec {
        let s = setting.trim().to_ascii_lowercase();
        let n = if s.is_empty() || s == "auto" {
            system_oem()
        } else {
            s.trim_start_matches("cp").parse().unwrap_or_else(|_| {
                tracing::warn!(setting, "[arc] zip_codepage: unknown, using OEM");
                system_oem()
            })
        };
        Codec::number(n)
    }

    fn number(n: u16) -> Codec {
        if let Some(t) = oem_cp::code_table::DECODING_TABLE_CP_MAP.get(&n) {
            return Codec::Oem(t.clone());
        }
        for label in [format!("windows-{n}"), format!("ibm{n}"), format!("cp{n}")] {
            if let Some(e) = encoding_rs::Encoding::for_label(label.as_bytes()) {
                return Codec::Other(e);
            }
        }
        tracing::warn!(codepage = n, "[arc] zip_codepage: no table, using 437");
        Codec::number(437)
    }

    fn decode(&self, raw: &[u8]) -> Option<String> {
        match self {
            Codec::Oem(t) => t.decode_string_checked(raw),
            Codec::Other(e) => e
                .decode_without_bom_handling_and_without_replacement(raw)
                .map(std::borrow::Cow::into_owned),
        }
    }
}

/// OEM-кодовая страница системы: `HKLM\SYSTEM\CurrentControlSet\Control\Nls\CodePage\OEMCP`.
fn system_oem() -> u16 {
    #[cfg(windows)]
    {
        if let Some(n) = windows_registry::LOCAL_MACHINE
            .open(r"SYSTEM\CurrentControlSet\Control\Nls\CodePage")
            .and_then(|k| k.get_string("OEMCP"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return n;
        }
    }
    437
}

/// Имя записи для индекса и признак экранирования.
fn decode_name(codec: &Codec, raw: &[u8], utf8: bool, unix: bool) -> (String, bool) {
    if raw.is_ascii() {
        return (String::from_utf8_lossy(raw).into_owned(), false);
    }
    // Без флага, но валидный UTF-8 из Unix (macOS, Info-ZIP) — UTF-8, как у 7-Zip.
    if (utf8 || unix)
        && let Ok(s) = std::str::from_utf8(raw)
    {
        return (s.to_owned(), false);
    }
    if !utf8 && let Some(s) = codec.decode(raw) {
        return (s, false);
    }
    let (s, escaped) = urfin_vfs::name::escape_bytes(raw);
    (s.into(), escaped)
}

/// Разность эпох FILETIME (1601) и Unix, в сотнях наносекунд.
const FILETIME_UNIX: i64 = 116_444_736_000_000_000;

/// Время из extra-полей записи central directory: NTFS (0x000A), затем UT (0x5455).
fn extra_mtime(extra: &[u8]) -> Option<i64> {
    let mut ut = None;
    let mut ntfs = None;
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[at], extra[at + 1]]);
        let len = usize::from(u16::from_le_bytes([extra[at + 2], extra[at + 3]]));
        let Some(data) = extra.get(at + 4..at + 4 + len) else {
            break;
        };
        match id {
            0x000A => ntfs = ntfs.or_else(|| ntfs_mtime(data)),
            0x5455 if data.len() >= 5 && data[0] & 1 != 0 => {
                let secs = i32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                ut = Some(i64::from(secs) * 1_000_000_000);
            }
            _ => {}
        }
        at += 4 + len;
    }
    ntfs.or(ut)
}

/// NTFS extra: 4 байта резерва, затем теги; тег 1 (24 байта) — mtime, atime, ctime.
fn ntfs_mtime(data: &[u8]) -> Option<i64> {
    let mut at = 4;
    while at + 4 <= data.len() {
        let tag = u16::from_le_bytes([data[at], data[at + 1]]);
        let len = usize::from(u16::from_le_bytes([data[at + 2], data[at + 3]]));
        let body = data.get(at + 4..at + 4 + len)?;
        if tag == 1 && len >= 8 {
            let ft = i64::try_from(u64::from_le_bytes(body[..8].try_into().ok()?)).ok()?;
            return ft.checked_sub(FILETIME_UNIX)?.checked_mul(100);
        }
        at += 4 + len;
    }
    None
}

/// DOS-время записи как местное.
fn dos_mtime(d: DateTime, tz: &TimeZone) -> Option<i64> {
    let dt = jiff::civil::DateTime::new(
        i16::try_from(d.year()).ok()?,
        i8::try_from(d.month()).ok()?,
        i8::try_from(d.day()).ok()?,
        i8::try_from(d.hour()).ok()?,
        i8::try_from(d.minute()).ok()?,
        i8::try_from(d.second()).ok()?,
        0,
    )
    .ok()?;
    let ts = tz.to_ambiguous_timestamp(dt).compatible().ok()?;
    i64::try_from(ts.as_nanosecond()).ok()
}

/// Атрибуты: DOS-биты младшего байта, у Unix — ещё «исполняемый».
fn attrs_of(e: &ZipFileEntry<'_>) -> Attrs {
    let ext = e.external_attributes();
    let mut a = 0;
    if ext & 0x01 != 0 {
        a |= Attrs::READONLY;
    }
    if ext & 0x02 != 0 {
        a |= Attrs::HIDDEN;
    }
    if ext & 0x04 != 0 {
        a |= Attrs::SYSTEM;
    }
    if ext & 0x20 != 0 {
        a |= Attrs::ARCHIVE;
    }
    if e.system() == System::Unix
        && let Some(mode) = e.unix_mode()
    {
        if mode & 0o111 != 0 && !e.is_dir() {
            a |= Attrs::EXEC;
        }
        if mode & 0o222 == 0 {
            a |= Attrs::READONLY;
        }
    }
    if e.encrypted() {
        a |= Attrs::ENCRYPTED;
    }
    Attrs(a)
}

/// Ошибка `zip` → ошибка VFS архива `outer`.
fn zip_error(outer: &Location, e: ZipError) -> VfsError {
    match e {
        ZipError::Io(e) => VfsError::from_io(outer.path.clone(), e),
        ZipError::InvalidPassword | ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED) => {
            no_password(outer)
        }
        e => corrupt(outer, e),
    }
}

/// То же для потока данных записи.
fn io_error(outer: &Location, e: ZipError) -> io::Error {
    match zip_error(outer, e) {
        VfsError::Io { source, .. } => source,
        other => io::Error::other(other.to_string()),
    }
}

/// Разобранные оглавления последних архивов: F3 и выборка не перечитывают их.
#[derive(Default)]
struct Metas(Mutex<VecDeque<(OuterKey, Arc<ZipArchiveMetadata>)>>);

impl Metas {
    fn get(&self, key: &OuterKey) -> Option<Arc<ZipArchiveMetadata>> {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let i = g.iter().position(|(k, _)| k == key)?;
        let hit = g.remove(i)?;
        let meta = Arc::clone(&hit.1);
        g.push_front(hit);
        Some(meta)
    }

    fn put(&self, key: &OuterKey, meta: &Arc<ZipArchiveMetadata>) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.retain(|(k, _)| k.loc != key.loc);
        g.push_front((key.clone(), Arc::clone(meta)));
        g.truncate(METAS);
    }

    /// Оглавление архива `src`: из памяти или разбором.
    fn of(&self, src: &ArcSource) -> Result<Arc<ZipArchiveMetadata>, VfsError> {
        if let Some(m) = self.get(src.key()) {
            return Ok(m);
        }
        let (archive, _) = open_archive(src)?;
        let meta = archive.metadata();
        self.put(src.key(), &meta);
        Ok(meta)
    }
}

/// Архив поверх читателя файла.
type Archive = ZipArchive<Box<dyn VfsRead>>;

/// Разбор central directory; второе — комментарий архива.
fn open_archive(src: &ArcSource) -> Result<(Archive, Box<[u8]>), VfsError> {
    let outer = src.outer();
    let mut r = src.open()?;
    let mut head = [0u8; 4];
    let got =
        read_up_to(&mut r, &mut head).map_err(|e| VfsError::from_io(outer.path.clone(), e))?;
    r.seek(SeekFrom::Start(0))
        .map_err(|e| VfsError::from_io(outer.path.clone(), e))?;
    match ZipArchive::new(r) {
        Ok(a) => {
            let comment = a.comment().into();
            Ok((a, comment))
        }
        Err(ZipError::Io(e)) => Err(VfsError::from_io(outer.path.clone(), e)),
        Err(e) => {
            // Первый том разрезанного архива начинается с `PK\x07\x08`; последний (.zip)
            // ссылается на тома `.z01`… рядом.
            let split = head == *b"PK\x07\x08" || outer.path.with_extension("z01").exists();
            if split {
                Err(multivolume(outer))
            } else if got < 2 || head[..2] != *b"PK" {
                Err(format_error(outer))
            } else {
                Err(corrupt(outer, e))
            }
        }
    }
}

fn read_up_to(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Формат zip.
pub struct ZipFormat {
    codec: Codec,
    metas: Arc<Metas>,
}

impl std::fmt::Debug for ZipFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZipFormat")
            .field("codec", &self.codec)
            .finish_non_exhaustive()
    }
}

impl ZipFormat {
    /// Кодовая страница имён — `[arc] zip_codepage`.
    pub fn new(cfg: &ArcCfg) -> ZipFormat {
        ZipFormat::with_codec(Codec::of(&cfg.zip_codepage))
    }

    /// С кодовой страницей `cp` (тесты, сцены).
    pub fn with_codepage(cp: u16) -> ZipFormat {
        ZipFormat::with_codec(Codec::number(cp))
    }

    fn with_codec(codec: Codec) -> ZipFormat {
        ZipFormat {
            codec,
            metas: Arc::default(),
        }
    }
}

struct ZipScan {
    outer: Location,
    meta: Arc<ZipArchiveMetadata>,
    /// Central directory целиком: extra-поля времени (`zip` их наружу не отдаёт).
    cd: Vec<u8>,
    cd_start: u64,
    at: usize,
    codec: Codec,
    tz: TimeZone,
}

impl ZipScan {
    /// Extra-поля записи `e` в central directory.
    fn extra(&self, e: &ZipFileEntry<'_>) -> Option<&[u8]> {
        let off = usize::try_from(e.central_header_start().checked_sub(self.cd_start)?).ok()?;
        let h = self.cd.get(off..off + 46)?;
        if h[..4] != *b"PK\x01\x02" {
            return None;
        }
        let name = usize::from(u16::from_le_bytes([h[28], h[29]]));
        let extra = usize::from(u16::from_le_bytes([h[30], h[31]]));
        self.cd.get(off + 46 + name..off + 46 + name + extra)
    }
}

impl Scan for ZipScan {
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let end = self.meta.len().min(self.at + STEP);
        for i in self.at..end {
            let e = self.meta.entry(i).map_err(|e| zip_error(&self.outer, e))?;
            // Bit 11 (UTF-8); `zip` ставит его и при принятом extra 0x7075.
            let utf8 = e.flags().as_u16() & 0x0800 != 0;
            let (path, escaped) =
                decode_name(&self.codec, e.name_raw(), utf8, e.system() == System::Unix);
            let mtime = self
                .extra(&e)
                .and_then(extra_mtime)
                .or_else(|| e.last_modified().and_then(|d| dos_mtime(d, &self.tz)))
                .unwrap_or(i64::MIN);
            let mut attrs = attrs_of(&e);
            if escaped {
                attrs.0 |= Attrs::ESCAPED_NAME;
            }
            let leaf = path
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default();
            if leaf.starts_with('.') && leaf != "." && leaf != ".." {
                attrs.0 |= Attrs::DOTFILE;
            }
            let Ok(n) = u32::try_from(i) else {
                return Err(corrupt(&self.outer, "too many entries"));
            };
            if n & SEEKABLE != 0 {
                return Err(corrupt(&self.outer, "too many entries"));
            }
            let seekable = !e.encrypted() && e.compression() == CompressionMethod::Stored;
            let kind = if e.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            b.push(RawEntry {
                path: &path,
                kind,
                attrs,
                size: e.size(),
                packed: e.compressed_size(),
                mtime,
                loc: EntryLoc::Index(if seekable { n | SEEKABLE } else { n }),
            });
        }
        self.at = end;
        Ok(end < self.meta.len())
    }
}

impl Format for ZipFormat {
    fn scheme(&self) -> &'static str {
        "zip"
    }

    fn scan(&self, src: &ArcSource) -> Result<Box<dyn Scan>, VfsError> {
        let outer = src.outer().clone();
        let (archive, comment) = open_archive(src)?;
        let meta = archive.metadata();
        let cd_start = archive.central_directory_start();
        let mut r = archive.into_inner();
        let mut cd = Vec::new();
        r.seek(SeekFrom::Start(cd_start))
            .and_then(|_| {
                r.take(src.len().saturating_sub(cd_start))
                    .read_to_end(&mut cd)
            })
            .map_err(|e| VfsError::from_io(outer.path.clone(), e))?;
        self.metas.put(src.key(), &meta);
        let codec = self.codec.clone();
        let mut scan = ZipScan {
            outer,
            meta,
            cd,
            cd_start,
            at: 0,
            codec,
            tz: TimeZone::system(),
        };
        if !comment.is_empty() {
            // Комментарий без флага: валидный UTF-8 — UTF-8, иначе кодовая страница.
            let (c, _) = decode_name(&scan.codec, &comment, false, true);
            scan.cd.shrink_to_fit();
            return Ok(Box::new(WithComment {
                scan,
                comment: Some(c),
            }));
        }
        Ok(Box::new(WithComment {
            scan,
            comment: None,
        }))
    }

    fn open_entry(
        &self,
        src: &ArcSource,
        idx: &ArchiveIndex,
        node: NodeId,
    ) -> Result<Box<dyn VfsRead>, VfsError> {
        let outer = src.outer();
        let (i, seekable) = unpack(idx.node(node).loc).ok_or_else(|| corrupt(outer, "no entry"))?;
        let meta = self.metas.of(src)?;
        let e = meta.entry(i).map_err(|e| zip_error(outer, e))?;
        let size = e.size();
        if seekable {
            let mut r = src.open()?;
            let start = e
                .get_metadata()
                .data_start(&mut *r)
                .map_err(|e| zip_error(outer, e))?;
            return Ok(Box::new(Slice {
                r,
                start,
                len: size,
                pos: 0,
                dirty: true,
            }));
        }
        let pw = idx.ctx().password();
        if e.encrypted() && pw.is_none() {
            return Err(no_password(outer));
        }
        Ok(Box::new(Pump::spawn(
            outer.clone(),
            Arc::clone(&meta),
            i,
            src.open()?,
            pw,
            size,
        )?))
    }

    fn entry_caps(&self, idx: &ArchiveIndex, node: NodeId) -> u32 {
        match unpack(idx.node(node).loc) {
            Some((_, true)) => Caps::SEEK,
            _ => Caps::SEQUENTIAL,
        }
    }

    /// Файлы по порядку оглавления (≈ порядку данных в файле), кусками подряд по сжатому
    /// размеру — до `parallel` читателей. Без I/O: оглавление читается в первом `next`.
    fn plan(
        &self,
        src: &ArcSource,
        idx: &Arc<ArchiveIndex>,
        files: &[(u32, NodeId)],
        parallel: u8,
    ) -> Option<Vec<Box<dyn ExtractStream>>> {
        let mut sorted = Vec::with_capacity(files.len());
        for &(input, node) in files {
            let (i, _) = unpack(idx.node(node).loc)?;
            sorted.push((i, input, idx.node(node).packed));
        }
        sorted.sort_unstable_by_key(|&(i, ..)| i);
        let k = usize::from(parallel.max(1))
            .min(usize::from(CHANNELS))
            .min(sorted.len().max(1));
        let total: u64 = sorted.iter().map(|&(.., p)| p.max(1)).sum();
        let share = total.div_ceil(k as u64).max(1);
        let mut todo: Vec<VecDeque<(u32, usize)>> = vec![VecDeque::new(); k];
        let mut acc = 0u64;
        for &(i, input, packed) in &sorted {
            let s = usize::try_from(acc / share).unwrap_or(k - 1).min(k - 1);
            todo[s].push_back((input, i));
            acc += packed.max(1);
        }
        Some(
            todo.into_iter()
                .filter(|t| !t.is_empty())
                .map(|todo| {
                    Box::new(ZipStream {
                        metas: Arc::clone(&self.metas),
                        src: src.clone(),
                        pw: idx.ctx().password(),
                        todo,
                        open: None,
                    }) as Box<dyn ExtractStream>
                })
                .collect(),
        )
    }

    fn transfer_channels(&self) -> u16 {
        CHANNELS
    }
}

/// Разбор с комментарием архива (ставится в первом шаге).
struct WithComment {
    scan: ZipScan,
    comment: Option<String>,
}

impl Scan for WithComment {
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError> {
        if let Some(c) = self.comment.take() {
            b.set_comment(c);
        }
        self.scan.step(b, cancel)
    }
}

/// Stored-запись: поддиапазон файла архива с `seek`.
struct Slice {
    r: Box<dyn VfsRead>,
    start: u64,
    len: u64,
    pos: u64,
    /// Позиция `r` не совпадает с `start + pos`.
    dirty: bool,
}

impl Read for Slice {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        if left == 0 || buf.is_empty() {
            return Ok(0);
        }
        if self.dirty {
            self.r.seek(SeekFrom::Start(self.start + self.pos))?;
            self.dirty = false;
        }
        let want = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let n = self.r.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Slice {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        if to != self.pos {
            self.pos = to;
            self.dirty = true;
        }
        Ok(to)
    }
}

impl VfsRead for Slice {
    fn len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// Сжатая или зашифрованная запись: распаковка в своём потоке, данные — порциями через
/// канал (у `zip` читатель записи заимствует архив). Закрыли читателя — поток кончается.
struct Pump {
    rx: crossbeam_channel::Receiver<io::Result<Vec<u8>>>,
    cur: Vec<u8>,
    at: usize,
    pos: u64,
    len: u64,
    done: bool,
}

impl Pump {
    fn spawn(
        outer: Location,
        meta: Arc<ZipArchiveMetadata>,
        i: usize,
        mut r: Box<dyn VfsRead>,
        pw: Option<Zeroizing<Vec<u8>>>,
        len: u64,
    ) -> Result<Pump, VfsError> {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let path = outer.path.clone();
        std::thread::Builder::new()
            .name("arc-zip-read".into())
            .spawn(move || {
                let opts = ZipReadOptions::new().password(pw.as_deref().map(Vec::as_slice));
                let file = meta.entry(i).and_then(|e| e.with_reader(&mut r, opts));
                let mut file = match file {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = tx.send(Err(io_error(&outer, e)));
                        return;
                    }
                };
                loop {
                    let mut buf = vec![0u8; CHUNK];
                    match read_up_to(&mut file, &mut buf) {
                        Ok(0) => return,
                        Ok(n) => {
                            buf.truncate(n);
                            if tx.send(Ok(buf)).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
            })
            .map_err(|e| VfsError::from_io(path, e))?;
        Ok(Pump {
            rx,
            cur: Vec::new(),
            at: 0,
            pos: 0,
            len,
            done: false,
        })
    }
}

impl Read for Pump {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.at < self.cur.len() {
                let n = buf.len().min(self.cur.len() - self.at);
                buf[..n].copy_from_slice(&self.cur[self.at..self.at + n]);
                self.at += n;
                self.pos += n as u64;
                return Ok(n);
            }
            if self.done || buf.is_empty() {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Ok(v)) => {
                    self.cur = v;
                    self.at = 0;
                }
                Ok(Err(e)) => {
                    self.done = true;
                    return Err(e);
                }
                Err(_) => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
    }
}

impl Seek for Pump {
    /// Только «где я»: сжатая запись читается подряд.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        if to == Some(self.pos) {
            return Ok(self.pos);
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "compressed zip entry is read sequentially",
        ))
    }
}

impl VfsRead for Pump {
    fn len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// Поток выборки: свой дескриптор архива, записи по порядку оглавления.
struct ZipStream {
    metas: Arc<Metas>,
    src: ArcSource,
    pw: Option<Zeroizing<Vec<u8>>>,
    /// (индекс во входном списке, номер записи).
    todo: VecDeque<(u32, usize)>,
    open: Option<(Arc<ZipArchiveMetadata>, Box<dyn VfsRead>)>,
}

impl ExtractStream for ZipStream {
    fn next(&mut self, cancel: &CancelToken) -> Result<Option<Extracted<'_>>, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let Some((input, i)) = self.todo.pop_front() else {
            return Ok(None);
        };
        if self.open.is_none() {
            let meta = self.metas.of(&self.src)?;
            self.open = Some((meta, self.src.open()?));
        }
        let outer = self.src.outer();
        let Some((meta, r)) = self.open.as_mut() else {
            return Ok(None);
        };
        let e = meta.entry(i).map_err(|e| zip_error(outer, e))?;
        if e.encrypted() && self.pw.is_none() {
            return Err(no_password(outer));
        }
        let opts = ZipReadOptions::new().password(self.pw.as_deref().map(Vec::as_slice));
        let f = e.with_reader(r, opts).map_err(|e| zip_error(outer, e))?;
        Ok(Some((input, Box::new(f))))
    }
}
