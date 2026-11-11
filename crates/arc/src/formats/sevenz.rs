//! 7z (S8 §3.3): оглавление — заголовок в конце файла (`sevenz-rust2` 0.25).
//!
//! - Записи: `EntryLoc::Folder { folder, index }` — блок (folder, solid-блок) и номер
//!   файла в оглавлении архива. У пустого файла данных нет: `folder` = [`NO_BLOCK`],
//!   чтение — пустой поток с `seek`.
//! - «Сжато»: сжатый размер блока — у его первого файла, у остальных файлов блока — 0
//!   (как `7z l`); сумма — сжатые данные архива. Каталоги — `<DIR>` без размеров.
//! - Время — FILETIME (UTC), атрибуты — Windows-биты, у Unix-архивов ещё «исполняемый».
//! - Чтение: распаковка в своём потоке, данные — порциями через канал (`Pump`,
//!   `Caps::SEQUENTIAL`). Файл в середине solid-блока — после распаковки в никуда
//!   всего, что перед ним (бенч A6, S8-37); у блока из одного LZMA2 (без фильтров и
//!   шифрования) — с последнего сброса словаря перед файлом ([`lzma2_restart`]): чанки
//!   до него не распаковываются, потоков — по числу нужных независимых кусков (#719).
//! - Выборка ([`Format::plan`]): файлы по блокам; блоки делятся по сжатому размеру между
//!   до 4 распаковщиками со своими дескрипторами, внутри блока — один проход по порядку,
//!   ненужные файлы распаковываются в никуда, после последнего нужного блок бросается.
//!   Распаковщик идёт впереди на бюджет, его файлы разбирают несколько писателей
//!   ([`super::relay`]): один solid-блок — до 4 файлов пишутся параллельно (бенч A3).
//! - Шифрование (7zAES, AES-256): пароль записей — [`crate::OpenCtx`] индекса; без
//!   него — `arc-error-no-password`. Зашифрованные имена (`-mhe`): пароль до листинга —
//!   [`crate::ArcCache::set_password`], после разбора он же — в `OpenCtx`.
//! - Многотомные `.7z.001` — M5b (S8-44): пока `arc-error-multivolume`.

use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, PoisonError};

use crossbeam_channel::Sender;
use lzma_rust2::{Lzma2Reader, Lzma2ReaderMt};
use sevenz_rust2::{
    Archive, BlockDecoder, EncoderMethod, Error as SzError, Password, SIGNATURE_HEADER_SIZE,
};
use urfin_core::{Attrs, CancelToken, EntryKind, Location};
use urfin_vfs::{Caps, ExtractStream, VfsError, VfsRead};
use zeroize::Zeroizing;

use super::relay::{self, Fed, Relay};
use super::{CHUNK, Empty, Pump, read_up_to};
use crate::format::{ArcSource, Format, Scan, corrupt, format_error, multivolume, no_password};
use crate::index::{ArchiveIndex, EntryLoc, IndexBuilder, NodeId, OuterKey, RawEntry};

/// Записей за один шаг разбора.
const STEP: usize = 8192;

/// Разобранных оглавлений в памяти формата (F3 и выборка не перечитывают их).
const METAS: usize = 4;

/// Независимых читателей одного архива (SSD; на HDD ops ограничит сам).
const CHANNELS: u16 = 4;

/// `folder` у файла без данных (пустой файл).
pub const NO_BLOCK: u32 = u32::MAX;

/// Шифр 7z — подпись у поля пароля.
const CIPHER: &str = "AES-256";

/// Буфер разбора заголовков чанков LZMA2: между ними — `seek` по данным чанка.
const SCAN_BUF: usize = 4 << 10;

/// Буфер чтения сжатого потока под распаковщиком LZMA2.
const PACK_BUF: usize = 64 << 10;

/// Разность эпох FILETIME (1601) и Unix, в сотнях наносекунд.
const FILETIME_UNIX: i64 = 116_444_736_000_000_000;

/// Раскладка `EntryLoc::Folder`: блок (или [`NO_BLOCK`]) и номер файла.
fn unpack(loc: EntryLoc) -> Option<(u32, usize)> {
    match loc {
        EntryLoc::Folder { folder, index } => Some((folder, index as usize)),
        _ => None,
    }
}

/// Пароль `sevenz-rust2` (UTF-16LE) из байт UTF-8 `OpenCtx`.
fn password(pw: Option<&[u8]>) -> Password {
    pw.and_then(|p| std::str::from_utf8(p).ok())
        .map_or_else(Password::empty, Password::new)
}

/// FILETIME → наносекунды Unix.
fn nt_mtime(ft: u64) -> Option<i64> {
    i64::try_from(ft)
        .ok()?
        .checked_sub(FILETIME_UNIX)?
        .checked_mul(100)
}

/// Атрибуты: Windows-биты; при расширении Unix (0x8000) старшие 16 бит — режим.
fn attrs_of(win: u32, dir: bool) -> Attrs {
    let mut a = 0;
    if win & 0x01 != 0 {
        a |= Attrs::READONLY;
    }
    if win & 0x02 != 0 {
        a |= Attrs::HIDDEN;
    }
    if win & 0x04 != 0 {
        a |= Attrs::SYSTEM;
    }
    if win & 0x20 != 0 {
        a |= Attrs::ARCHIVE;
    }
    if win & 0x8000 != 0 {
        let mode = win >> 16;
        if mode & 0o111 != 0 && !dir {
            a |= Attrs::EXEC;
        }
    }
    Attrs(a)
}

/// Том многотомного архива: `.7z.001` и т. п.
fn is_volume(outer: &Location) -> bool {
    outer
        .path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.len() == 3 && e.bytes().all(|b| b.is_ascii_digit()))
}

/// Ошибка разбора заголовка → ошибка VFS архива `outer`; `pw` — с паролем.
fn header_error(outer: &Location, e: SzError, pw: bool) -> VfsError {
    if is_volume(outer) {
        return multivolume(outer);
    }
    match e {
        SzError::BadSignature(_) => format_error(outer),
        SzError::PasswordRequired | SzError::MaybeBadPassword(_) => no_password(outer),
        SzError::Io(e, _) | SzError::FileOpen(e, _) if !decode_kind(e.kind()) => {
            VfsError::from_io(outer.path.clone(), e)
        }
        // Неверный пароль заголовка — мусор вместо оглавления.
        _ if pw => no_password(outer),
        e => corrupt(outer, e),
    }
}

/// Ошибка, которую даёт распаковка мусора: неверный пароль или битые данные.
fn decode_kind(k: io::ErrorKind) -> bool {
    matches!(
        k,
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::Other
    )
}

/// Ошибка распаковки блока; `enc` — блок зашифрован.
fn data_error(outer: &Location, e: SzError, enc: bool) -> io::Error {
    let v = match e {
        SzError::PasswordRequired | SzError::MaybeBadPassword(_) => no_password(outer),
        SzError::Io(e, _) => return read_error(outer, e, enc),
        _ if enc => no_password(outer),
        e => corrupt(outer, e),
    };
    into_io(v)
}

/// Ошибка чтения данных записи; у зашифрованной мусор — неверный пароль.
fn read_error(outer: &Location, e: io::Error, enc: bool) -> io::Error {
    if !decode_kind(e.kind()) {
        return e;
    }
    into_io(if enc {
        no_password(outer)
    } else {
        corrupt(outer, e)
    })
}

fn into_io(v: VfsError) -> io::Error {
    match v {
        VfsError::Io { source, .. } => source,
        other => io::Error::other(other.to_string()),
    }
}

/// Копия ошибки: одна — читателю файла, другая — потоку выборки.
fn copy_error(e: &io::Error) -> io::Error {
    io::Error::new(e.kind(), e.to_string())
}

/// Разобранное оглавление: второе — имена зашифрованы.
fn read_archive(src: &ArcSource, pw: Option<&str>) -> Result<(Archive, bool), VfsError> {
    let outer = src.outer();
    let mut r = src.open()?;
    match Archive::read(&mut r, &Password::empty()) {
        Ok(a) => Ok((a, false)),
        Err(SzError::PasswordRequired) => {
            let Some(pw) = pw else {
                return Err(no_password(outer));
            };
            Archive::read(&mut r, &Password::new(pw))
                .map(|a| (a, true))
                .map_err(|e| header_error(outer, e, true))
        }
        Err(e) => Err(header_error(outer, e, false)),
    }
}

/// Разобранные оглавления последних архивов: F3 и выборка не перечитывают их.
#[derive(Default)]
struct Metas(Mutex<VecDeque<(OuterKey, Arc<Archive>)>>);

impl Metas {
    fn get(&self, key: &OuterKey) -> Option<Arc<Archive>> {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let i = g.iter().position(|(k, _)| k == key)?;
        let hit = g.remove(i)?;
        let a = Arc::clone(&hit.1);
        g.push_front(hit);
        Some(a)
    }

    fn put(&self, key: &OuterKey, a: &Arc<Archive>) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.retain(|(k, _)| k.loc != key.loc);
        g.push_front((key.clone(), Arc::clone(a)));
        g.truncate(METAS);
    }

    /// Оглавление архива `src`: из памяти или разбором; `pw` — пароль имён (байты UTF-8).
    fn of(&self, src: &ArcSource, pw: Option<&[u8]>) -> Result<Arc<Archive>, VfsError> {
        if let Some(a) = self.get(src.key()) {
            return Ok(a);
        }
        let pw = pw.and_then(|p| std::str::from_utf8(p).ok());
        let (a, _) = read_archive(src, pw)?;
        let a = Arc::new(a);
        self.put(src.key(), &a);
        Ok(a)
    }
}

/// Сжатый размер каждого блока: сумма его pack-потоков (у BCJ2 их несколько).
fn block_packed(a: &Archive) -> Vec<u64> {
    let first = a.stream_map.block_first_pack_stream_index();
    let sizes = a.pack_sizes();
    (0..a.blocks.len())
        .map(|b| {
            let from = first.get(b).copied().unwrap_or(sizes.len());
            let to = first.get(b + 1).copied().unwrap_or(sizes.len()).max(from);
            sizes
                .get(from..to.min(sizes.len()))
                .map_or(0, |s| s.iter().sum())
        })
        .collect()
}

/// Формат 7z.
#[derive(Default)]
pub struct SevenZFormat {
    metas: Arc<Metas>,
}

impl std::fmt::Debug for SevenZFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SevenZFormat").finish_non_exhaustive()
    }
}

impl SevenZFormat {
    pub fn new() -> SevenZFormat {
        SevenZFormat::default()
    }
}

struct SevenZScan {
    outer: Location,
    archive: Arc<Archive>,
    packed: Vec<u64>,
    /// Блок уже отдал свой сжатый размер первому файлу.
    counted: Vec<bool>,
    names_encrypted: bool,
    at: usize,
}

impl Scan for SevenZScan {
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if self.at == 0 {
            b.set_solid(self.archive.is_solid);
            if self.names_encrypted {
                b.set_encrypted(true);
                b.set_cipher(CIPHER);
            }
        }
        let files = &self.archive.files;
        let end = files.len().min(self.at + STEP);
        for (i, e) in files.iter().enumerate().take(end).skip(self.at) {
            if e.is_anti_item {
                continue;
            }
            let Ok(n) = u32::try_from(i) else {
                return Err(corrupt(&self.outer, "too many entries"));
            };
            let dir = e.is_directory;
            let mut attrs = if e.has_windows_attributes {
                attrs_of(e.windows_attributes, dir)
            } else {
                Attrs::default()
            };
            let leaf = e
                .name
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default();
            if leaf.starts_with('.') && leaf != "." && leaf != ".." {
                attrs.0 |= Attrs::DOTFILE;
            }
            let mtime = if e.has_last_modified_date {
                nt_mtime(e.last_modified_date.into()).unwrap_or(i64::MIN)
            } else {
                i64::MIN
            };
            if dir {
                b.push(RawEntry {
                    attrs,
                    mtime,
                    ..RawEntry::dir(&e.name)
                });
                continue;
            }
            let block = if e.has_stream {
                self.archive
                    .stream_map
                    .file_block_index
                    .get(i)
                    .copied()
                    .flatten()
            } else {
                None
            };
            let mut packed = 0;
            if let Some(blk) = block {
                if self.archive.is_file_encrypted(i) {
                    attrs.0 |= Attrs::ENCRYPTED;
                    b.set_cipher(CIPHER);
                }
                if let Some(c) = self.counted.get_mut(blk)
                    && !*c
                {
                    *c = true;
                    packed = self.packed.get(blk).copied().unwrap_or(0);
                }
            }
            let folder = match block.map(u32::try_from) {
                Some(Ok(f)) if f != NO_BLOCK => f,
                Some(_) => return Err(corrupt(&self.outer, "too many blocks")),
                None => NO_BLOCK,
            };
            b.push(RawEntry {
                path: &e.name,
                kind: EntryKind::File,
                attrs,
                size: if e.has_stream { e.size } else { 0 },
                packed,
                mtime,
                loc: EntryLoc::Folder { folder, index: n },
            });
        }
        self.at = end;
        Ok(end < files.len())
    }
}

impl Format for SevenZFormat {
    fn scheme(&self) -> &'static str {
        "7z"
    }

    fn scan(&self, src: &ArcSource) -> Result<Box<dyn Scan>, VfsError> {
        let (archive, names_encrypted) = read_archive(src, src.password())?;
        let archive = Arc::new(archive);
        self.metas.put(src.key(), &archive);
        let packed = block_packed(&archive);
        Ok(Box::new(SevenZScan {
            outer: src.outer().clone(),
            counted: vec![false; packed.len()],
            packed,
            archive,
            names_encrypted,
            at: 0,
        }))
    }

    fn open_entry(
        &self,
        src: &ArcSource,
        idx: &ArchiveIndex,
        node: NodeId,
    ) -> Result<Box<dyn VfsRead>, VfsError> {
        let outer = src.outer();
        let n = idx.node(node);
        let (block, i) = unpack(n.loc).ok_or_else(|| corrupt(outer, "no entry"))?;
        if block == NO_BLOCK {
            return Ok(Box::new(Empty));
        }
        let pw = idx.ctx().password();
        if n.attrs.has(Attrs::ENCRYPTED) && pw.is_none() {
            return Err(no_password(outer));
        }
        let (tx, rx) = crossbeam_channel::bounded(4);
        Worker {
            metas: Arc::clone(&self.metas),
            src: src.clone(),
            pw,
            todo: vec![(block as usize, vec![(i, 0)])],
            sink: Sink::One(tx),
            threads: cores(),
        }
        .spawn()?;
        Ok(Box::new(Pump::new(rx, n.size)))
    }

    fn entry_caps(&self, idx: &ArchiveIndex, node: NodeId) -> u32 {
        match unpack(idx.node(node).loc) {
            Some((NO_BLOCK, _)) => Caps::SEEK,
            _ => Caps::SEQUENTIAL,
        }
    }

    /// Файлы по блокам, блоки по порядку и кусками подряд по сжатому размеру — до
    /// `parallel` читателей. Без I/O: оглавление читается в потоке читателя.
    fn plan(
        &self,
        src: &ArcSource,
        idx: &Arc<ArchiveIndex>,
        files: &[(u32, NodeId)],
        parallel: u8,
    ) -> Option<Vec<Box<dyn ExtractStream>>> {
        let mut empty = VecDeque::new();
        // (блок, номер файла, индекс во входном списке, размер).
        let mut data = Vec::with_capacity(files.len());
        for &(input, node) in files {
            let n = idx.node(node);
            let (block, i) = unpack(n.loc)?;
            if block == NO_BLOCK {
                empty.push_back(input);
            } else {
                data.push((block as usize, i, input, n.size));
            }
        }
        data.sort_unstable();
        // Блоки с нужными файлами; вес блока — сколько распаковать (до последнего нужного
        // файла — оценка снизу: размер нужных).
        let mut blocks: Todo = Vec::new();
        let mut weight: Vec<u64> = Vec::new();
        for (block, i, input, size) in data {
            match (blocks.last_mut(), weight.last_mut()) {
                (Some((b, files)), Some(w)) if *b == block => {
                    files.push((i, input));
                    *w += size;
                }
                _ => {
                    blocks.push((block, vec![(i, input)]));
                    weight.push(size);
                }
            }
        }
        let k = usize::from(parallel.max(1))
            .min(usize::from(CHANNELS))
            .min(blocks.len().max(1));
        let total: u64 = weight.iter().map(|&w| w.max(1)).sum();
        let share = total.div_ceil(k as u64).max(1);
        let mut todo: Vec<Todo> = vec![Vec::new(); k];
        let mut acc = 0u64;
        for (b, w) in blocks.into_iter().zip(weight) {
            let s = usize::try_from(acc / share).unwrap_or(k - 1).min(k - 1);
            todo[s].push(b);
            acc += w.max(1);
        }
        let threads = (cores() / k as u32).max(1);
        let mut out: Vec<Box<dyn ExtractStream>> = Vec::with_capacity(usize::from(parallel));
        for todo in todo {
            if todo.is_empty() {
                continue;
            }
            let files = todo.iter().map(|(_, f)| f.len()).sum();
            let worker = Worker {
                metas: Arc::clone(&self.metas),
                src: src.clone(),
                pw: idx.ctx().password(),
                todo,
                sink: Sink::Many(None),
                threads,
            };
            out.extend(relay::streams(
                src.outer(),
                relay::writers(parallel, k, files),
                relay::BUDGET / k,
                std::mem::take(&mut empty),
                Box::new(move |relay| {
                    Worker {
                        sink: Sink::Many(Some(relay)),
                        ..worker
                    }
                    .spawn()
                }),
            ));
        }
        if !empty.is_empty() {
            // Только пустые файлы: проход без блоков.
            out.extend(relay::streams(
                src.outer(),
                1,
                relay::ONE,
                empty,
                Box::new(|_| Ok(())),
            ));
        }
        Some(out)
    }

    fn transfer_channels(&self) -> u16 {
        CHANNELS
    }
}

/// Потоков распаковки LZMA2 (многопоточные блоки) на читателя.
fn cores() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
}

/// Блоки по порядку; в каждом — (номер файла, индекс во входном списке) по порядку.
type Todo = Vec<(usize, Vec<(usize, u32)>)>;

/// Куда распаковщик отдаёт данные.
enum Sink {
    /// Один файл (`open_entry`).
    One(Sender<io::Result<Vec<u8>>>),
    /// Выборка: файл за файлом в конвейер; `None` — до старта прохода.
    Many(Option<Relay>),
}

impl Sink {
    fn fail(&self, e: io::Error) {
        match self {
            Sink::One(tx) => {
                let _ = tx.send(Err(e));
            }
            Sink::Many(Some(relay)) => relay.fail(e),
            Sink::Many(None) => {}
        }
    }
}

/// Распаковщик блоков в своём потоке.
struct Worker {
    metas: Arc<Metas>,
    src: ArcSource,
    pw: Option<Zeroizing<Vec<u8>>>,
    todo: Todo,
    sink: Sink,
    threads: u32,
}

impl Worker {
    fn spawn(self) -> Result<(), VfsError> {
        let path = self.src.outer().path.clone();
        std::thread::Builder::new()
            .name("arc-7z-read".into())
            .spawn(move || self.run())
            .map(drop)
            .map_err(|e| VfsError::from_io(path, e))
    }

    fn run(self) {
        if let Err(e) = self.blocks() {
            self.sink.fail(e);
        }
    }

    fn blocks(&self) -> io::Result<()> {
        let archive = self
            .metas
            .of(&self.src, self.pw.as_deref().map(Vec::as_slice));
        let archive = archive.map_err(into_io)?;
        let mut r = self.src.open().map_err(into_io)?;
        if let (Sink::One(tx), [(block, want)]) = (&self.sink, self.todo.as_slice())
            && let [(i, _)] = want.as_slice()
            && let Some(d) = Direct::of(&archive, *block, *i)
            && let Some(at) = d.restart(&mut r)
        {
            #[cfg(test)]
            tests::note_pass(&self.src.outer().path);
            return self.direct(&mut r, &d, &at, tx);
        }
        let pw = password(self.pw.as_deref().map(Vec::as_slice));
        for (block, want) in &self.todo {
            if !self.block(&archive, &pw, &mut r, *block, want)? {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Один проход блока: нужные файлы — в `sink`, прочие — в никуда, после последнего
    /// нужного — стоп. `false` — читатель выборки ушёл.
    fn block(
        &self,
        a: &Archive,
        pw: &Password,
        r: &mut Box<dyn VfsRead>,
        block: usize,
        want: &[(usize, u32)],
    ) -> io::Result<bool> {
        let outer = self.src.outer();
        let first = a.stream_map.block_first_file_index.get(block).copied();
        let Some(mut cursor) = first else {
            return Err(into_io(corrupt(outer, "no block")));
        };
        let enc = want.first().is_some_and(|&(i, _)| a.is_file_encrypted(i));
        #[cfg(test)]
        tests::note_pass(&outer.path);
        let mut at = 0;
        let mut fed = Fed::Ok;
        let res = BlockDecoder::new(self.threads, block, a, pw, r).for_each_entries(
            &mut |entry, data| {
                // Номер файла: записи блока идут по порядку оглавления.
                while cursor < a.files.len() && !std::ptr::eq(&raw const a.files[cursor], entry) {
                    cursor += 1;
                }
                let Some(&(i, input)) = want.get(at) else {
                    return Ok(false);
                };
                if i != cursor {
                    return Ok(true);
                }
                // Один файл дважды во входном списке: повтор — обычным путём.
                while want.get(at).is_some_and(|w| w.0 == i) {
                    at += 1;
                }
                fed = self.feed(input, entry.size, data, enc);
                match &fed {
                    Fed::Ok => Ok(at < want.len()),
                    Fed::Gone | Fed::Failed(_) => Err(SzError::Other("stop".into())),
                }
            },
        );
        match (res, fed) {
            (_, Fed::Gone) => Ok(false),
            (_, Fed::Failed(e)) => {
                // Ошибка уже у читателя файла; выборка — с ошибкой прохода.
                if let Sink::Many(Some(relay)) = &self.sink {
                    relay.fail(e);
                }
                Ok(false)
            }
            (Ok(_), Fed::Ok) => Ok(true),
            (Err(e), Fed::Ok) => Err(data_error(outer, e, enc)),
        }
    }

    /// Файл `d` с точки `at` (без распаковки чанков перед ней) — читателю `tx`.
    fn direct(
        &self,
        r: &mut Box<dyn VfsRead>,
        d: &Direct,
        at: &Restart,
        tx: &Sender<io::Result<Vec<u8>>>,
    ) -> io::Result<()> {
        let outer = self.src.outer();
        let bad = |e: io::Error| read_error(outer, e, false);
        r.seek(SeekFrom::Start(d.offset + at.pos)).map_err(bad)?;
        let input = BufReader::with_capacity(PACK_BUF, Read::take(&mut **r, d.pack - at.pos));
        let threads = at.units.min(self.threads);
        let mut lz: Box<dyn Read + '_> = if threads < 2 {
            Box::new(Lzma2Reader::new(input, d.dict, None))
        } else {
            Box::new(Lzma2ReaderMt::new(input, d.dict, None, threads))
        };
        let gap = d.skip - at.out;
        let got = io::copy(&mut (&mut lz).take(gap), &mut io::sink()).map_err(bad)?;
        let short = || bad(io::Error::new(io::ErrorKind::UnexpectedEof, "short block"));
        if got < gap {
            return Err(short());
        }
        let mut crc = flate2::Crc::new();
        let mut left = d.size;
        while left > 0 {
            let mut buf = vec![0u8; usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK))];
            let n = read_up_to(&mut lz, &mut buf).map_err(bad)?;
            if n == 0 {
                return Err(short());
            }
            buf.truncate(n);
            crc.update(&buf);
            left -= n as u64;
            if left == 0
                && let Some(want) = d.crc
                && crc.sum() != want
            {
                // Данные отданы, ошибка — в конце, как у проверки CRC `sevenz-rust2`.
                let _ = tx.send(Ok(buf));
                return Err(bad(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "CRC mismatch",
                )));
            }
            if tx.send(Ok(buf)).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Данные файла `input` — читателю.
    fn feed(&self, input: u32, len: u64, data: &mut dyn Read, enc: bool) -> Fed {
        let tx = match &self.sink {
            Sink::One(tx) => tx.clone(),
            Sink::Many(Some(relay)) => {
                let outer = self.src.outer();
                return relay.feed(input, len, data, |e| read_error(outer, e, enc));
            }
            Sink::Many(None) => return Fed::Gone,
        };
        loop {
            let mut buf = vec![0u8; CHUNK];
            match read_up_to(data, &mut buf) {
                Ok(0) => return Fed::Ok,
                Ok(n) => {
                    buf.truncate(n);
                    if tx.send(Ok(buf)).is_err() {
                        // Читатель файла ушёл: стоп.
                        return Fed::Gone;
                    }
                }
                Err(e) => {
                    let e = read_error(self.src.outer(), e, enc);
                    let copy = copy_error(&e);
                    let _ = tx.send(Err(e));
                    return Fed::Failed(copy);
                }
            }
        }
    }
}

/// Файл блока из одного LZMA2: где сжатый поток и сколько распакованного перед файлом.
struct Direct {
    /// Начало сжатого потока блока в файле архива.
    offset: u64,
    pack: u64,
    dict: u32,
    /// Распакованного в блоке перед файлом.
    skip: u64,
    size: u64,
    crc: Option<u32>,
}

impl Direct {
    /// Файл `i` блока `block`; `None` — блок не «один LZMA2» (фильтры, шифрование, LZMA,
    /// несколько потоков): обычный путь.
    fn of(a: &Archive, block: usize, i: usize) -> Option<Direct> {
        let b = a.blocks.get(block)?;
        let [coder] = b.coders.as_slice() else {
            return None;
        };
        if coder.encoder_method_id() != EncoderMethod::ID_LZMA2 {
            return None;
        }
        let dict = lzma2_dict(*coder.properties().first()?)?;
        let first_pack = *a.stream_map.block_first_pack_stream_index().get(block)?;
        let offset = SIGNATURE_HEADER_SIZE
            .checked_add(a.pack_pos())?
            .checked_add(*a.stream_map.pack_stream_offsets().get(first_pack)?)?;
        let pack = *a.pack_sizes().get(first_pack)?;
        let first = *a.stream_map.block_first_file_index.get(block)?;
        let e = a.files.get(i)?;
        let skip = a
            .files
            .get(first..i)?
            .iter()
            .filter(|f| f.has_stream)
            .try_fold(0u64, |acc, f| acc.checked_add(f.size))?;
        let crc = if e.has_crc {
            u32::try_from(e.crc).ok()
        } else if b.has_crc && skip == 0 && e.size == b.get_unpack_size() {
            u32::try_from(b.crc).ok()
        } else {
            None
        };
        Some(Direct {
            offset,
            pack,
            dict,
            skip,
            size: e.size,
            crc,
        })
    }

    /// Точка входа: разбор заголовков чанков. Ошибка разбора — обычный путь (там же и
    /// ошибка битого архива).
    fn restart(&self, r: &mut Box<dyn VfsRead>) -> Option<Restart> {
        r.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut br = BufReader::with_capacity(SCAN_BUF, &mut **r);
        let end = self.skip.checked_add(self.size)?;
        lzma2_restart(&mut br, self.pack, self.skip, end).ok()?
    }
}

/// Словарь LZMA2 по байту свойств (как у `sevenz-rust2`).
fn lzma2_dict(bits: u8) -> Option<u32> {
    match bits {
        40 => Some(u32::MAX),
        0..40 => Some((2 | (u32::from(bits) & 1)) << (bits / 2 + 11)),
        _ => None,
    }
}

/// Откуда распаковывать поток LZMA2 ради диапазона `skip..end`.
#[derive(Debug, PartialEq, Eq)]
struct Restart {
    /// Смещение чанка со сбросом словаря в сжатом потоке.
    pos: u64,
    /// Распакованного до него.
    out: u64,
    /// Независимых кусков (от сброса до сброса), задевающих диапазон: потоков столько.
    units: u32,
}

/// Последний чанк со сбросом словаря (`0x01`, `0xE0..`) не дальше `skip` — с него поток
/// LZMA2 распаковывается без предыдущего; данные чанков пропускаются `seek`. Так же поток
/// делит на куски многопоточный распаковщик `lzma-rust2`. `None` — первый чанк без сброса.
fn lzma2_restart<R: Read + Seek>(
    r: &mut BufReader<R>,
    pack: u64,
    skip: u64,
    end: u64,
) -> io::Result<Option<Restart>> {
    let (mut pos, mut out) = (0u64, 0u64);
    let mut best: Option<Restart> = None;
    while pos < pack && (out <= skip || out < end) {
        let mut c = [0u8; 1];
        r.read_exact(&mut c)?;
        let control = c[0];
        if control == 0 {
            break;
        }
        if control == 0x01 || control >= 0xE0 {
            if out <= skip {
                best = Some(Restart { pos, out, units: 1 });
            } else if let Some(b) = &mut best {
                b.units += 1;
            }
        }
        let (unpacked, packed, head) = if control >= 0x80 {
            let head = if control >= 0xC0 { 5 } else { 4 };
            let mut h = [0u8; 5];
            r.read_exact(&mut h[..head])?;
            let low = u64::from(u16::from_be_bytes([h[0], h[1]]));
            let unpacked = ((u64::from(control & 0x1F) << 16) | low) + 1;
            (
                unpacked,
                u64::from(u16::from_be_bytes([h[2], h[3]])) + 1,
                head,
            )
        } else if control <= 0x02 {
            let mut h = [0u8; 2];
            r.read_exact(&mut h)?;
            let n = u64::from(u16::from_be_bytes(h)) + 1;
            (n, n, 2)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid LZMA2 control byte",
            ));
        };
        let jump = i64::try_from(packed).map_err(io::Error::other)?;
        r.seek_relative(jump)?;
        pos += 1 + head as u64 + packed;
        out += unpacked;
    }
    Ok(best)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    use urfin_core::{CancelToken, Location};
    use urfin_vfs::Vfs;

    use super::SevenZFormat;
    use crate::{ArcCache, ArcVfs, root_of};

    /// Проходов по блокам на файл архива.
    static PASSES: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    pub(crate) fn note_pass(path: &Path) {
        let mut g = PASSES.lock().unwrap_or_else(PoisonError::into_inner);
        *g.get_or_insert_default()
            .entry(path.to_path_buf())
            .or_default() += 1;
    }

    fn passes(path: &Path) -> usize {
        let g = PASSES.lock().unwrap_or_else(PoisonError::into_inner);
        g.as_ref().and_then(|m| m.get(path).copied()).unwrap_or(0)
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Выборка 1000 файлов одного solid-блока — один проход блока. Файл
    /// `fNNNN.txt` фикстуры — строка `file N` с CRLF, N % 7 + 1 раз.
    #[test]
    fn plan_of_one_block_is_one_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("solid1000.7z");
        std::fs::copy(fixture("solid1000.7z"), &file).unwrap();
        let v = ArcVfs::new(SevenZFormat::new(), ArcCache::new(64 << 20));
        let root = root_of("7z", Location::local(file.clone()));
        let cancel = CancelToken::new();
        v.index(&root, &cancel).unwrap();
        let files: Vec<Location> = (0..1000)
            .rev()
            .map(|i| Location {
                path: PathBuf::from(format!("/f{i:04}.txt")),
                ..root.clone()
            })
            .collect();
        let streams = v.extract_plan(&files, 4).unwrap();
        // Один распаковщик, его файлы разбирают 4 писателя (#719).
        assert_eq!(streams.len(), 4, "один блок — один проход, четыре писателя");
        let mut seen = vec![false; files.len()];
        for mut s in streams {
            while let Some((i, mut r)) = s.next(&cancel).unwrap() {
                let mut got = String::new();
                std::io::Read::read_to_string(&mut r, &mut got).unwrap();
                let n = 999 - i as usize;
                assert_eq!(got, format!("file {n}\r\n").repeat(n % 7 + 1), "f{n:04}");
                assert!(!std::mem::replace(&mut seen[i as usize], true));
            }
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(passes(&file), 1, "блок распакован один раз");
    }

    /// Поток LZMA2 из несжатых чанков: `0x01` — со сбросом словаря, `0x02` — без.
    fn stream(chunks: &[(u8, &[u8])]) -> Vec<u8> {
        let mut v = Vec::new();
        for &(c, data) in chunks {
            v.push(c);
            v.extend_from_slice(&u16::try_from(data.len() - 1).unwrap().to_be_bytes());
            v.extend_from_slice(data);
        }
        v.push(0);
        v
    }

    /// Точка входа — последний сброс словаря не дальше начала файла; кусков — сколько
    /// задевает файл; с точки поток распаковывается `Lzma2Reader` без начала.
    #[test]
    fn lzma2_restart_skips_to_last_dictionary_reset() {
        use std::io::{BufReader, Cursor, Read};

        let xa = vec![b'a'; 300];
        let xb = vec![b'b'; 200];
        let xc = vec![b'c'; 100];
        let xd = vec![b'd'; 50];
        // Куски: [a b] с 0, [c d] с 500.
        let raw = stream(&[(0x01, &xa), (0x02, &xb), (0x01, &xc), (0x02, &xd)]);
        let pack = raw.len() as u64;
        let at = |skip, end| {
            let mut r = BufReader::with_capacity(16, Cursor::new(raw.clone()));
            super::lzma2_restart(&mut r, pack, skip, end)
                .unwrap()
                .unwrap()
        };
        let second = 1 + 2 + 300 + 1 + 2 + 200;
        assert_eq!(
            at(10, 20),
            super::Restart {
                pos: 0,
                out: 0,
                units: 1
            }
        );
        assert_eq!(
            at(450, 520),
            super::Restart {
                pos: 0,
                out: 0,
                units: 2
            }
        );
        let tail = at(520, 650);
        assert_eq!(
            tail,
            super::Restart {
                pos: second,
                out: 500,
                units: 1
            }
        );
        assert_eq!(at(500, 501).pos, second, "файл с начала куска");
        let mut lz = lzma_rust2::Lzma2Reader::new(&raw[second as usize..], 4096, None);
        let mut got = Vec::new();
        lz.read_to_end(&mut got).unwrap();
        assert_eq!(got, [xc, xd].concat(), "кусок распаковывается сам");

        let bad = [0x80u8, 0, 0, 0, 0, 0];
        let mut r = BufReader::new(Cursor::new(bad.to_vec()));
        assert_eq!(
            super::lzma2_restart(&mut r, 6, 0, 1).unwrap(),
            None,
            "без сброса"
        );
    }

    /// F3 файла solid-блока LZMA2 — с точки входа, а не распаковкой блока с начала
    /// (A6, #719); данные — как у полного прохода.
    #[test]
    fn open_entry_of_solid_lzma2_starts_at_restart_point() {
        use std::io::Read;

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("solid1000-f3.7z");
        std::fs::copy(fixture("solid1000.7z"), &file).unwrap();
        let arch = sevenz_rust2::Archive::open(&file).unwrap();
        let i = arch
            .files
            .iter()
            .position(|e| e.name == "f0999.txt")
            .unwrap();
        let block = arch.stream_map.file_block_index[i].unwrap();
        let direct = super::Direct::of(&arch, block, i).expect("один LZMA2");
        assert!(direct.skip > 0 && direct.crc.is_some());
        let v = ArcVfs::new(SevenZFormat::new(), ArcCache::new(64 << 20));
        let root = root_of("7z", Location::local(file.clone()));
        v.index(&root, &CancelToken::new()).unwrap();
        for n in [999usize, 0, 500] {
            let p = Location {
                path: PathBuf::from(format!("/f{n:04}.txt")),
                ..root.clone()
            };
            let mut got = String::new();
            v.open_read(&p).unwrap().read_to_string(&mut got).unwrap();
            assert_eq!(got, format!("file {n}\r\n").repeat(n % 7 + 1), "f{n:04}");
        }
        assert_eq!(passes(&file), 3, "по точке входа на файл");
    }
}
