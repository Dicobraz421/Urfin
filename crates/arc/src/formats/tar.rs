//! tar и компрессоры (S8 §3.3): `.tar`, `.tar.gz`/`.tgz`, `.tar.bz2`, `.tar.xz`,
//! `.tar.zst` и одиночные `.gz`, `.bz2`, `.xz`, `.zst` — одна схема `tar`.
//!
//! - Что внутри, решает сигнатура (`detect::by_head`), а у сжатого потока — первый блок
//!   после распаковки: заголовок tar с верной суммой — сжатый tar, иначе одиночный файл
//!   (псевдо-архив из одной записи, имя — без расширения компрессора).
//! - Заголовки разбирает `tar` (PAX, GNU long names, GNU sparse) в своём потоке: его
//!   читатель записи заимствует архив. Оглавления нет (`catalog_first = false`): корень
//!   листается по мере разбора.
//! - Plain tar: данные записи — поддиапазон файла (`EntryLoc::Offset`, `Caps::SEEK`).
//!   Сжатые и sparse — номер записи по порядку (`EntryLoc::Index`, `Caps::SEQUENTIAL`):
//!   чтение распаковывает архив с начала до записи, выборка ([`Format::plan`]) — все
//!   файлы одним проходом.
//! - Многочленные `.gz` (`MultiGzDecoder`), `.bz2`, `.xz` и `.zst` читаются целиком.
//! - Ссылки: symlink — `LinkFile` (на каталог архива — `LinkDir`) без данных, с целью
//!   ([`ArchiveIndex::link_target`]): ops делает ссылку по `LinkMode`; hardlink —
//!   `LinkFile` с данными цели (цель в tar всегда раньше ссылки), без цели — `LinkBroken`.
//! - Сжатый размер записи сжатого tar — оценка: байты файла, прочитанные распаковщиком
//!   между концами соседних файлов; сумма — размер архива.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::tar::{Archive, EntryType};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use urfin_core::{Attrs, CancelToken, EntryKind, Location};
use urfin_vfs::{Caps, ExtractStream, Extracted, VfsError, VfsRead};

use super::{CHUNK, Pump, Slice, pump_into, read_up_to};
use crate::detect::{self, ArcKind, HEAD_LEN};
use crate::format::{ArcSource, Format, Scan, corrupt, format_error};
use crate::index::{ArchiveIndex, EntryLoc, IndexBuilder, NodeId, OuterKey, RawEntry};

/// Записей за один шаг разбора.
const STEP: usize = 4096;

/// Записей в порции от потока разбора.
const BATCH: usize = 256;

/// Порция уходит не реже: первые строки видны, даже если записи крупные.
const FLUSH_EVERY: Duration = Duration::from_millis(30);

/// Ожидание порции между проверками отмены.
const POLL: Duration = Duration::from_millis(100);

/// Распакованных байт одиночного файла за шаг разбора.
const SINGLE_STEP: u64 = 32 << 20;

/// Распознанных архивов в памяти формата (метка не открывает файл заново).
const KINDS: usize = 8;

/// Буфер чтения файла архива под распаковщиком.
const BUF: usize = 64 << 10;

/// Компрессор.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Comp {
    None,
    Gz,
    Bz2,
    Xz,
    Zst,
}

/// Что лежит в файле: компрессор и tar ли внутри.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Kind {
    comp: Comp,
    tar: bool,
}

impl Kind {
    fn arc(self) -> ArcKind {
        match (self.comp, self.tar) {
            (Comp::None, _) => ArcKind::Tar,
            (Comp::Gz, true) => ArcKind::TarGz,
            (Comp::Bz2, true) => ArcKind::TarBz2,
            (Comp::Xz, true) => ArcKind::TarXz,
            (Comp::Zst, true) => ArcKind::TarZst,
            (Comp::Gz, false) => ArcKind::Gz,
            (Comp::Bz2, false) => ArcKind::Bz2,
            (Comp::Xz, false) => ArcKind::Xz,
            (Comp::Zst, false) => ArcKind::Zst,
        }
    }
}

/// Имя файла архива.
fn file_name(outer: &Location) -> String {
    outer
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Имя говорит «tar» (`.tar`, `.tgz`, `.tar.zst`…).
fn named_tar(outer: &Location) -> bool {
    matches!(
        detect::by_name(&file_name(outer)),
        Some(ArcKind::Tar | ArcKind::TarGz | ArcKind::TarBz2 | ArcKind::TarXz | ArcKind::TarZst)
    )
}

/// Первый блок — заголовок tar: сумма сходится. Нулевой блок — пустой tar, если имя
/// говорит «tar».
fn is_tar_block(block: &[u8], named: bool) -> bool {
    if block.len() < 512 {
        return false;
    }
    let block = &block[..512];
    if block.iter().all(|&b| b == 0) {
        return named;
    }
    let sum: u32 = block[..148]
        .iter()
        .chain(&block[156..])
        .map(|&b| u32::from(b))
        .sum::<u32>()
        + 8 * u32::from(b' ');
    ::tar::Header::from_byte_slice(block)
        .cksum()
        .is_ok_and(|c| c == sum)
}

/// Имя одиночного файла: имя архива без расширения компрессора (`dump.sql.gz` →
/// `dump.sql`, `a.tgz` → `a.tar`).
fn single_name(outer: &Location) -> String {
    let name = file_name(outer);
    let exts = ["gz", "bz2", "xz", "zst", "tgz", "tbz2", "txz", "tzst"];
    let stem = match detect::ext_in(&name, exts) {
        Some(ext) => {
            let stem = &name[..name.len() - ext.len() - 1];
            if ext.starts_with('t') {
                format!("{stem}.tar")
            } else {
                stem.to_owned()
            }
        }
        None => name,
    };
    if stem.is_empty() { "data".into() } else { stem }
}

/// Счётчик байт, прочитанных из файла архива (оценка сжатого размера записи).
struct Counted {
    r: Box<dyn VfsRead>,
    n: Arc<AtomicU64>,
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let k = self.r.read(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
}

/// Распаковщик поверх файла архива.
fn decoder(comp: Comp, r: Box<dyn Read + Send>) -> io::Result<Box<dyn Read + Send>> {
    Ok(match comp {
        Comp::None => r,
        Comp::Gz => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Comp::Bz2 => Box::new(bzip2::read::MultiBzDecoder::new(r)),
        Comp::Xz => Box::new(lzma_rust2::XzReader::new(
            io::BufReader::with_capacity(BUF, r),
            true,
        )),
        Comp::Zst => Box::new(zstd::stream::read::Decoder::new(r)?),
    })
}

/// Распакованный поток: `seek` только вперёд (пропуск данных записи для `tar`) и «где я».
struct Forward {
    r: Box<dyn Read + Send>,
    pos: u64,
    len: Option<u64>,
}

impl Read for Forward {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.r.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Forward {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.len.and_then(|l| l.checked_add_signed(d)),
        };
        let Some(to) = to.filter(|&t| t >= self.pos) else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "compressed stream is read sequentially",
            ));
        };
        let skip = to - self.pos;
        let got = io::copy(&mut (&mut self.r).take(skip), &mut io::sink())?;
        self.pos += got;
        if got < skip {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected end of compressed stream",
            ));
        }
        Ok(self.pos)
    }
}

impl VfsRead for Forward {
    fn len(&self) -> Option<u64> {
        self.len
    }
}

/// Флаг «стоп» для потока разбора: ставится, когда читатель ушёл.
#[derive(Clone, Default)]
struct Stop(Arc<AtomicBool>);

impl Stop {
    fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Ставит [`Stop`] при сбросе.
struct StopOnDrop(Stop);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.0.store(true, Ordering::Relaxed);
    }
}

/// Запись tar из потока разбора.
struct Rec {
    path: String,
    kind: EntryKind,
    attrs: Attrs,
    size: u64,
    packed: u64,
    mtime: i64,
    loc: EntryLoc,
    /// Цель hardlink — путь в архиве.
    link: Option<String>,
    /// Цель symlink как в заголовке.
    symlink: Option<String>,
}

/// Путь внутри архива без `.`, `..` и префиксов — ключ цели hardlink.
fn norm(path: &str) -> u64 {
    let mut s = String::with_capacity(path.len());
    for c in crate::path::components(path) {
        s.push('/');
        s.push_str(c);
    }
    xxhash_rust::xxh3::xxh3_64(s.as_bytes())
}

/// Имя записи: UTF-8 (PAX — всегда), иначе экранирование.
fn decode(raw: &[u8]) -> (String, bool) {
    let (s, escaped) = urfin_vfs::name::escape_bytes(raw);
    (s.into(), escaped)
}

/// `mtime` из PAX: секунды с долями (`1700000000.25`).
fn pax_mtime(v: &str) -> Option<i64> {
    let (secs, frac) = v.split_once('.').unwrap_or((v, ""));
    let secs: i64 = secs.parse().ok()?;
    let mut ns = 0i64;
    for (i, c) in frac.chars().take(9).enumerate() {
        ns += i64::from(c.to_digit(10)?) * 10i64.pow(8 - i as u32);
    }
    let ns = if secs < 0 { -ns } else { ns };
    secs.checked_mul(1_000_000_000)?.checked_add(ns)
}

type TarEntry<'a> = ::tar::Entry<'a, Box<dyn VfsRead>>;

/// Запись tar → [`Rec`]; `None` — служебная (глобальный PAX-заголовок).
fn rec_of(e: &mut TarEntry<'_>, ord: u32, plain: bool) -> io::Result<Option<Rec>> {
    let ty = e.header().entry_type();
    if ty.is_pax_global_extensions() || ty.is_pax_local_extensions() {
        return Ok(None);
    }
    let (path, escaped) = decode(&e.path_bytes());
    let mode = e.header().mode().unwrap_or(0o644);
    let mut mtime = e
        .header()
        .mtime()
        .ok()
        .and_then(|s| i64::try_from(s).ok())
        .and_then(|s| s.checked_mul(1_000_000_000))
        .unwrap_or(i64::MIN);
    if let Some(exts) = e.pax_extensions()? {
        for x in exts.flatten() {
            if x.key() == Ok("mtime")
                && let Some(t) = x.value().ok().and_then(pax_mtime)
            {
                mtime = t;
            }
        }
    }
    let size = e.size();
    let stored = e.header().entry_size().unwrap_or(size);
    let mut attrs = 0;
    let dir_name = path.ends_with('/') || path.ends_with('\\');
    let mut symlink = None;
    let (kind, link) = match ty {
        EntryType::Directory => (EntryKind::Dir, None),
        EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse if dir_name => {
            (EntryKind::Dir, None)
        }
        EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => {
            (EntryKind::File, None)
        }
        EntryType::Symlink => {
            symlink = e.link_name_bytes().map(|b| decode(&b).0);
            (EntryKind::LinkFile, None)
        }
        EntryType::Link => {
            let target = e.link_name_bytes().map(|b| decode(&b).0);
            (EntryKind::LinkFile, target)
        }
        _ => (EntryKind::Other, None),
    };
    if kind == EntryKind::File && mode & 0o111 != 0 {
        attrs |= Attrs::EXEC;
    }
    if mode & 0o222 == 0 && kind != EntryKind::Dir {
        attrs |= Attrs::READONLY;
    }
    if escaped {
        attrs |= Attrs::ESCAPED_NAME;
    }
    let leaf = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    if leaf.starts_with('.') && leaf != "." && leaf != ".." {
        attrs |= Attrs::DOTFILE;
    }
    let data = kind == EntryKind::File;
    let loc = if !data {
        EntryLoc::None
    } else if plain && !ty.is_gnu_sparse() {
        EntryLoc::Offset(e.raw_file_position())
    } else {
        EntryLoc::Index(ord)
    };
    Ok(Some(Rec {
        path,
        kind,
        attrs: Attrs(attrs),
        size: if data { size } else { 0 },
        packed: if data && plain { stored } else { 0 },
        mtime,
        loc,
        link,
        symlink,
    }))
}

/// Обход записей tar по порядку; `f` — номер записи и запись, `Ok(false)` — хватит.
fn walk(
    r: Box<dyn VfsRead>,
    stop: &Stop,
    mut f: impl FnMut(u32, &mut TarEntry<'_>) -> io::Result<bool>,
) -> io::Result<()> {
    let mut ar = Archive::new(r);
    let mut ord = 0u32;
    for e in ar.entries_with_seek()? {
        if stop.is_set() {
            return Ok(());
        }
        let mut e = e?;
        if !f(ord, &mut e)? {
            return Ok(());
        }
        ord = ord
            .checked_add(1)
            .ok_or_else(|| io::Error::other("too many entries"))?;
    }
    Ok(())
}

/// Ошибка разбора или чтения → ошибка VFS архива `outer`.
fn tar_error(outer: &Location, e: io::Error) -> VfsError {
    match e.kind() {
        io::ErrorKind::InvalidData
        | io::ErrorKind::InvalidInput
        | io::ErrorKind::UnexpectedEof
        | io::ErrorKind::Other => corrupt(outer, e),
        _ => VfsError::from_io(outer.path.clone(), e),
    }
}

/// То же для потока данных записи.
fn io_error(outer: &Location, e: io::Error) -> io::Error {
    match tar_error(outer, e) {
        VfsError::Io { source, .. } => source,
        other => io::Error::other(other.to_string()),
    }
}

fn spawn(name: &str, outer: &Location, f: impl FnOnce() + Send + 'static) -> Result<(), VfsError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(drop)
        .map_err(|e| VfsError::from_io(outer.path.clone(), e))
}

/// Поток с начала архива: распакованный (у plain tar — сам файл). `raw` — счётчик
/// прочитанных байт файла, `len` — размер распакованного, если известен.
fn open_input(
    passes: &AtomicU64,
    src: &ArcSource,
    kind: Kind,
    raw: Option<Arc<AtomicU64>>,
    len: Option<u64>,
) -> Result<Box<dyn VfsRead>, VfsError> {
    passes.fetch_add(1, Ordering::Relaxed);
    let r = src.open()?;
    if kind.comp == Comp::None {
        return Ok(r);
    }
    let n = raw.unwrap_or_default();
    let d =
        decoder(kind.comp, Box::new(Counted { r, n })).map_err(|e| tar_error(src.outer(), e))?;
    Ok(Box::new(Forward { r: d, pos: 0, len }))
}

/// Индекс во входном списке, данные записи и её размер.
type Grant = (u32, Receiver<io::Result<Vec<u8>>>, u64);

/// Данные записей `want` — (номер записи, индекс во входном списке, размер) по
/// возрастанию номера, без повторов — одним проходом в своём потоке; в `tx` — по
/// [`Grant`] на запись.
fn spawn_pass(
    passes: &AtomicU64,
    src: &ArcSource,
    kind: Kind,
    mut want: VecDeque<(u32, u32, u64)>,
    tx: Sender<io::Result<Grant>>,
    stop: Stop,
) -> Result<(), VfsError> {
    let input = open_input(passes, src, kind, None, None)?;
    let outer = src.outer().clone();
    spawn("arc-tar-read", src.outer(), move || {
        let res = walk(input, &stop, |ord, e| {
            if want.front().is_some_and(|w| w.0 == ord)
                && let Some((_, input, len)) = want.pop_front()
            {
                let (ftx, frx) = crossbeam_channel::bounded(4);
                if tx.send(Ok((input, frx, len))).is_err() {
                    return Ok(false);
                }
                // Читатель закрыл запись раньше конца — остаток пропустит `tar`.
                pump_into(e, &ftx);
            }
            Ok(!want.is_empty() && !stop.is_set())
        });
        match res {
            Err(e) => {
                let _ = tx.send(Err(io_error(&outer, e)));
            }
            Ok(()) if !want.is_empty() && !stop.is_set() => {
                let e = io::Error::new(io::ErrorKind::InvalidData, "entry not found");
                let _ = tx.send(Err(io_error(&outer, e)));
            }
            Ok(()) => {}
        }
    })
}

/// Вид архива по метке индекса (`tar.gz`, `gz`): чтение без I/O на распознавание.
fn kind_of_label(label: &str) -> Option<Kind> {
    let comps = [Comp::None, Comp::Gz, Comp::Bz2, Comp::Xz, Comp::Zst];
    comps
        .into_iter()
        .flat_map(|comp| [Kind { comp, tar: true }, Kind { comp, tar: false }])
        .filter(|k| k.comp != Comp::None || k.tar)
        .find(|k| k.arc().label() == label)
}

/// Формат tar.
#[derive(Default)]
pub struct TarFormat {
    /// Распознанные архивы: метка не открывает файл заново.
    kinds: Mutex<VecDeque<(OuterKey, Kind)>>,
    /// Проходов по архиву с начала (разбор, чтение, выборка).
    passes: Arc<AtomicU64>,
}

impl std::fmt::Debug for TarFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TarFormat")
            .field("passes", &self.passes())
            .finish_non_exhaustive()
    }
}

impl TarFormat {
    pub fn new() -> TarFormat {
        TarFormat::default()
    }

    /// Проходов по архиву с начала: разбор оглавления, чтение записи, поток выборки.
    /// Выборка из сжатого tar — один проход на всю выборку.
    pub fn passes(&self) -> u64 {
        self.passes.load(Ordering::Relaxed)
    }

    fn kinds(&self) -> std::sync::MutexGuard<'_, VecDeque<(OuterKey, Kind)>> {
        self.kinds.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Распознать архив `src` и запомнить вид (для метки).
    fn sniff(&self, src: &ArcSource) -> Result<Kind, VfsError> {
        let k = sniff(src)?;
        let mut g = self.kinds();
        g.retain(|(key, _)| key.loc != src.key().loc);
        g.push_front((src.key().clone(), k));
        g.truncate(KINDS);
        Ok(k)
    }
}

/// Вид архива по первым байтам файла и, у сжатого, — по первому блоку распаковки.
fn sniff(src: &ArcSource) -> Result<Kind, VfsError> {
    let outer = src.outer();
    let io = |e| VfsError::from_io(outer.path.clone(), e);
    let mut r = src.open()?;
    let mut head = [0u8; HEAD_LEN];
    let n = read_up_to(&mut r, &mut head).map_err(io)?;
    let comp = match detect::by_head(&head[..n]) {
        Some(ArcKind::Tar) => Comp::None,
        Some(ArcKind::Gz) => Comp::Gz,
        Some(ArcKind::Bz2) => Comp::Bz2,
        Some(ArcKind::Xz) => Comp::Xz,
        Some(ArcKind::Zst) => Comp::Zst,
        // Старый tar (v7) без `ustar`, пустой tar.
        None if is_tar_block(&head[..n], named_tar(outer)) => Comp::None,
        _ => return Err(format_error(outer)),
    };
    if comp == Comp::None {
        return Ok(Kind { comp, tar: true });
    }
    r.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut d = decoder(
        comp,
        Box::new(Counted {
            r,
            n: Arc::default(),
        }),
    )
    .map_err(|e| tar_error(outer, e))?;
    let mut block = [0u8; 512];
    let k = read_up_to(&mut d, &mut block).map_err(|e| tar_error(outer, e))?;
    Ok(Kind {
        comp,
        tar: is_tar_block(&block[..k], named_tar(outer)),
    })
}

/// Разбор tar: записи приходят порциями из потока разбора.
struct TarScan {
    outer: Location,
    rx: Receiver<io::Result<Vec<Rec>>>,
    _stop: StopOnDrop,
    /// Файлы по пути: цель hardlink — (данные, размер).
    files: HashMap<u64, (EntryLoc, u64)>,
}

impl TarScan {
    fn push(&mut self, b: &mut IndexBuilder, r: &Rec) {
        let mut kind = r.kind;
        let (mut loc, mut size) = (r.loc, r.size);
        if let Some(target) = &r.link {
            match self.files.get(&norm(target)) {
                Some(&(l, s)) => (loc, size) = (l, s),
                None => kind = EntryKind::LinkBroken,
            }
        }
        if !matches!(loc, EntryLoc::None) {
            self.files.insert(norm(&r.path), (loc, size));
        }
        let id = b.push(RawEntry {
            path: &r.path,
            kind,
            attrs: r.attrs,
            size,
            packed: r.packed,
            mtime: r.mtime,
            loc,
        });
        if let (Some(id), Some(t)) = (id, &r.symlink) {
            b.set_link(id, t);
        }
    }
}

impl Scan for TarScan {
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError> {
        let first = loop {
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            match self.rx.recv_timeout(POLL) {
                Ok(v) => break v,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(false),
            }
        };
        let mut n = 0;
        let mut next = Some(first);
        while let Some(batch) = next.take() {
            let batch = batch.map_err(|e| tar_error(&self.outer, e))?;
            n += batch.len();
            for r in &batch {
                self.push(b, r);
            }
            if n < STEP {
                next = self.rx.try_recv().ok();
            }
        }
        Ok(true)
    }
}

/// Поток разбора: записи порциями в `tx`. Сжатый размер записи — байты файла от её
/// заголовка до следующего.
fn scan_thread(
    input: Box<dyn VfsRead>,
    plain: bool,
    raw: &AtomicU64,
    total: u64,
    tx: &Sender<io::Result<Vec<Rec>>>,
    stop: &Stop,
) {
    let mut batch = Vec::with_capacity(BATCH);
    let mut last = Instant::now();
    // Сжатый tar: запись ждёт следующего заголовка, чтобы узнать свой сжатый размер.
    // Байты файла между концами соседних файлов — сжатый размер; сумма — весь архив.
    let mut pending: Option<Rec> = None;
    let mut mark = 0u64;
    let res = walk(input, stop, |ord, e| {
        let now = raw.load(Ordering::Relaxed);
        if let Some(mut r) = pending.take() {
            r.packed = now.saturating_sub(mark);
            mark = now;
            batch.push(r);
        }
        if let Some(r) = rec_of(e, ord, plain)? {
            if plain || r.kind != EntryKind::File {
                batch.push(r);
            } else {
                pending = Some(r);
            }
        }
        if batch.len() >= BATCH || (!batch.is_empty() && last.elapsed() >= FLUSH_EVERY) {
            last = Instant::now();
            if tx.send(Ok(std::mem::take(&mut batch))).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    });
    if let Some(mut r) = pending.take() {
        r.packed = total.saturating_sub(mark);
        batch.push(r);
    }
    if !batch.is_empty() && tx.send(Ok(batch)).is_err() {
        return;
    }
    if let Err(e) = res {
        let _ = tx.send(Err(e));
    }
}

/// Одиночный сжатый файл: размер узнаётся распаковкой целиком.
struct SingleScan {
    outer: Location,
    d: Box<dyn VfsRead>,
    name: String,
    size: u64,
    packed: u64,
    mtime: i64,
    buf: Vec<u8>,
}

impl Scan for SingleScan {
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError> {
        let mut got = 0u64;
        while got < SINGLE_STEP {
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            let n = self
                .d
                .read(&mut self.buf)
                .map_err(|e| tar_error(&self.outer, e))?;
            if n == 0 {
                b.push(RawEntry {
                    path: &self.name,
                    kind: EntryKind::File,
                    attrs: Attrs::default(),
                    size: self.size,
                    packed: self.packed,
                    mtime: self.mtime,
                    loc: EntryLoc::Index(0),
                });
                return Ok(false);
            }
            got += n as u64;
            self.size += n as u64;
        }
        Ok(true)
    }
}

impl Format for TarFormat {
    fn scheme(&self) -> &'static str {
        "tar"
    }

    /// `tar.gz`, `tar.zst`, `gz`…: по распознанному виду, иначе по имени.
    fn label(&self, outer: &Location) -> &'static str {
        if let Some(&(_, k)) = self.kinds().iter().find(|(key, _)| key.loc == *outer) {
            return k.arc().label();
        }
        detect::by_name(&file_name(outer))
            .filter(|k| k.scheme() == "tar")
            .map_or("tar", ArcKind::label)
    }

    fn catalog_first(&self) -> bool {
        false
    }

    fn scan(&self, src: &ArcSource) -> Result<Box<dyn Scan>, VfsError> {
        let kind = self.sniff(src)?;
        let outer = src.outer().clone();
        if !kind.tar {
            let d = open_input(&self.passes, src, kind, None, None)?;
            return Ok(Box::new(SingleScan {
                name: single_name(&outer),
                outer,
                d,
                size: 0,
                packed: src.len(),
                mtime: src.key().mtime,
                buf: vec![0u8; CHUNK],
            }));
        }
        let raw = Arc::new(AtomicU64::new(0));
        let input = open_input(&self.passes, src, kind, Some(Arc::clone(&raw)), None)?;
        let (tx, rx) = crossbeam_channel::bounded(4);
        let stop = Stop::default();
        let plain = kind.comp == Comp::None;
        let total = src.len();
        let s = stop.clone();
        spawn("arc-tar-scan", &outer, move || {
            scan_thread(input, plain, &raw, total, &tx, &s);
        })?;
        Ok(Box::new(TarScan {
            outer,
            rx,
            _stop: StopOnDrop(stop),
            files: HashMap::new(),
        }))
    }

    fn open_entry(
        &self,
        src: &ArcSource,
        idx: &ArchiveIndex,
        node: NodeId,
    ) -> Result<Box<dyn VfsRead>, VfsError> {
        let n = idx.node(node);
        let outer = src.outer();
        match n.loc {
            EntryLoc::Offset(off) => Ok(Box::new(Slice::new(src.open()?, off, n.size))),
            EntryLoc::Index(ord) => {
                let kind = kind_of_label(idx.format).ok_or_else(|| format_error(outer))?;
                if !kind.tar {
                    return open_input(&self.passes, src, kind, None, Some(n.size));
                }
                let (tx, rx) = crossbeam_channel::bounded(1);
                let stop = Stop::default();
                let want = VecDeque::from([(ord, 0, n.size)]);
                spawn_pass(&self.passes, src, kind, want, tx, stop.clone())?;
                Ok(Box::new(EntryRead {
                    rx,
                    pump: None,
                    len: n.size,
                    _stop: StopOnDrop(stop),
                }))
            }
            // Symlink и hardlink без цели: данных нет.
            _ => Ok(Box::new(Forward {
                r: Box::new(io::empty()),
                pos: 0,
                len: Some(0),
            })),
        }
    }

    fn entry_caps(&self, idx: &ArchiveIndex, node: NodeId) -> u32 {
        match idx.node(node).loc {
            EntryLoc::Offset(_) | EntryLoc::None => Caps::SEEK,
            _ => Caps::SEQUENTIAL,
        }
    }

    /// Plain tar — по смещению одним дескриптором; сжатые и sparse — одним проходом по
    /// порядку записей (hardlink вместе с целью — второй проход). Одиночный файл —
    /// `None` (`open_entry`). Без I/O: файл открывает первый `next`.
    fn plan(
        &self,
        src: &ArcSource,
        idx: &Arc<ArchiveIndex>,
        files: &[(u32, NodeId)],
        parallel: u8,
    ) -> Option<Vec<Box<dyn ExtractStream>>> {
        let _ = parallel;
        let kind = kind_of_label(idx.format)?;
        if !kind.tar {
            return None;
        }
        let mut slices = Vec::new();
        let mut ords = Vec::new();
        // Без данных (symlink, hardlink без цели) — первому потоку.
        let mut empty = VecDeque::new();
        for &(input, node) in files {
            let n = idx.node(node);
            match n.loc {
                EntryLoc::Offset(off) => slices.push((off, input, n.size)),
                EntryLoc::Index(ord) => ords.push((ord, input, n.size)),
                EntryLoc::None | EntryLoc::Folder { .. } => empty.push_back(input),
            }
        }
        ords.sort_by_key(|&(ord, ..)| ord);
        let mut passes: Vec<VecDeque<(u32, u32, u64)>> = Vec::new();
        let mut prev = None;
        let mut rank = 0;
        for w in ords {
            rank = if prev == Some(w.0) { rank + 1 } else { 0 };
            prev = Some(w.0);
            if passes.len() <= rank {
                passes.push(VecDeque::new());
            }
            passes[rank].push_back(w);
        }
        let mut out: Vec<Box<dyn ExtractStream>> = Vec::new();
        if !slices.is_empty() || passes.is_empty() {
            slices.sort_unstable_by_key(|&(off, ..)| off);
            out.push(Box::new(SliceStream {
                passes: Arc::clone(&self.passes),
                src: src.clone(),
                empty: std::mem::take(&mut empty),
                todo: slices.into(),
                r: None,
            }));
        }
        for want in passes {
            out.push(Box::new(PassStream {
                passes: Arc::clone(&self.passes),
                src: src.clone(),
                kind,
                empty: std::mem::take(&mut empty),
                want: Some(want),
                rx: None,
                cur: None,
                stop: None,
            }));
        }
        Some(out)
    }
}

/// Запись сжатого tar для `open_read`: проход до неё начинается сразу, данные — по
/// первому чтению.
struct EntryRead {
    rx: Receiver<io::Result<Grant>>,
    pump: Option<Pump>,
    len: u64,
    _stop: StopOnDrop,
}

impl EntryRead {
    fn pump(&mut self) -> io::Result<&mut Pump> {
        if self.pump.is_none() {
            let (_, frx, len) = self.rx.recv().map_err(|_| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "archive stream ended")
            })??;
            self.pump = Some(Pump::new(frx, len));
        }
        self.pump
            .as_mut()
            .ok_or_else(|| io::Error::other("no entry stream"))
    }
}

impl Read for EntryRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.pump()?.read(buf)
    }
}

impl Seek for EntryRead {
    /// Только «где я».
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match &mut self.pump {
            Some(p) => p.seek(pos),
            None => Pump::new(crossbeam_channel::never(), self.len).seek(pos),
        }
    }
}

impl VfsRead for EntryRead {
    fn len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// Выборка из plain tar: по смещению, один дескриптор.
struct SliceStream {
    passes: Arc<AtomicU64>,
    src: ArcSource,
    /// Индексы во входном списке файлов без данных.
    empty: VecDeque<u32>,
    /// (смещение, индекс во входном списке, размер).
    todo: VecDeque<(u64, u32, u64)>,
    r: Option<Box<dyn VfsRead>>,
}

impl ExtractStream for SliceStream {
    fn next(&mut self, cancel: &CancelToken) -> Result<Option<Extracted<'_>>, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if let Some(input) = self.empty.pop_front() {
            return Ok(Some((input, Box::new(io::empty()))));
        }
        let Some((off, input, len)) = self.todo.pop_front() else {
            return Ok(None);
        };
        if len == 0 {
            return Ok(Some((input, Box::new(io::empty()))));
        }
        if self.r.is_none() {
            let plain = Kind {
                comp: Comp::None,
                tar: true,
            };
            self.r = Some(open_input(&self.passes, &self.src, plain, None, None)?);
        }
        let path = self.src.outer().path.clone();
        let Some(r) = self.r.as_mut() else {
            return Ok(None);
        };
        r.seek(SeekFrom::Start(off))
            .map_err(|e| VfsError::from_io(path, e))?;
        Ok(Some((input, Box::new(Read::take(&mut **r, len)))))
    }
}

/// Выборка из сжатого tar: один проход в своём потоке.
struct PassStream {
    passes: Arc<AtomicU64>,
    src: ArcSource,
    kind: Kind,
    /// Индексы во входном списке файлов без данных.
    empty: VecDeque<u32>,
    /// Ещё не начатый проход.
    want: Option<VecDeque<(u32, u32, u64)>>,
    rx: Option<Receiver<io::Result<Grant>>>,
    cur: Option<Pump>,
    stop: Option<StopOnDrop>,
}

impl ExtractStream for PassStream {
    fn next(&mut self, cancel: &CancelToken) -> Result<Option<Extracted<'_>>, VfsError> {
        // Прошлую запись закрываем: поток разбора пропустит её остаток.
        self.cur = None;
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if let Some(input) = self.empty.pop_front() {
            return Ok(Some((input, Box::new(io::empty()))));
        }
        if let Some(want) = self.want.take() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let stop = Stop::default();
            spawn_pass(&self.passes, &self.src, self.kind, want, tx, stop.clone())?;
            self.rx = Some(rx);
            self.stop = Some(StopOnDrop(stop));
        }
        let Some(rx) = &self.rx else {
            return Ok(None);
        };
        loop {
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            match rx.recv_timeout(POLL) {
                Ok(Ok((input, frx, len))) => {
                    let p = self.cur.insert(Pump::new(frx, len));
                    return Ok(Some((input, Box::new(p))));
                }
                Ok(Err(e)) => return Err(VfsError::from_io(self.src.outer().path.clone(), e)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }
}
