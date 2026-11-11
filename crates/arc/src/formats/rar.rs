//! RAR (S8 §3.3): RAR 1.5–4.x и RAR5, solid, многотомные, шифрование — `UnRAR` 7.1.0
//! (`unrar_sys`, feature `rar`, №37).
//!
//! - `UnRAR` открывает архив по пути: только локальный файл; вложенный rar и rar на сервере
//!   — `Unsupported` до спилла (S8-31).
//! - На диск `UnRAR` не пишет: данные — `RAR_TEST` и колбэк `UCM_PROCESSDATA` в наш поток
//!   (`sys`), имя из архива в `UnRAR` не уходит (S8-33).
//! - Оглавления нет (`catalog_first = false`): заголовки читаются по порядку
//!   (`RAR_OM_LIST_INCSPLIT`), корень листается потоком. Запись — порядковый номер
//!   заголовка (`EntryLoc::Index`); продолжения файла в следующих томах (`SPLITBEFORE`)
//!   номера не получают, их сжатый размер прибавляется к файлу.
//! - Многотомные: `UnRAR` сам переходит к следующему тому; нет тома — листинг кончается на
//!   нём (предупреждение в журнал), чтение файла с продолжением — ошибка с именем тома.
//!   Вход через любой том и склейка набора — S8-44.
//! - «Сжато» — сжатый размер записи (у solid тоже по записи, как `unrar lt`); каталоги —
//!   `<DIR>` без размеров. Время — `FILETIME` `UnRAR` (UTC), атрибуты — Windows-биты или
//!   режим Unix.
//! - Ссылки RAR5: symlink и junction — `LinkFile` (на каталог — `LinkDir`) с целью, без
//!   данных; hardlink и копия файла — `LinkFile` с данными цели, без цели — `LinkBroken`.
//! - Чтение: распаковка в своём потоке, данные порциями через канал (`Pump`,
//!   `Caps::SEQUENTIAL`); записи до нужной — `RAR_SKIP` (у solid — распаковка в никуда).
//!   Выборка ([`Format::plan`]) — один поток и один проход по порядку записей.
//! - Шифрование: пароль записей — [`crate::OpenCtx`] индекса, зашифрованные заголовки
//!   (`-hp`) — пароль до листинга ([`crate::ArcCache::set_password`]). Шифр — AES-256 у
//!   RAR5, AES-128 у RAR 3–4.

// Лицензия UnRAR (полностью — `THIRD_PARTY.md`):
//
// UnRAR source code may be used in any software to handle
// RAR archives without limitations free of charge, but cannot be
// used to develop RAR (WinRAR) compatible archiver and to
// re-create RAR compression algorithm, which is proprietary.
// Distribution of modified UnRAR source code in separate form
// or as a part of other software is permitted, provided that
// full text of this paragraph, starting from "UnRAR source code"
// words, is included in license, or in documentation if license
// is not available, and in source code comments of resulting package.

#[allow(
    unsafe_code,
    reason = "FFI UnRAR: открытие, заголовки, колбэк данных и пароля (S8-33)"
)]
mod sys;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use urfin_core::{Attrs, CancelToken, EntryKind, Location};
use urfin_vfs::{Caps, ExtractStream, Extracted, VfsError, VfsRead};
use zeroize::Zeroizing;

use super::{CHUNK, Empty, Pump};
use crate::format::{ArcSource, Format, Scan, corrupt, format_error, no_password};
use crate::index::{ArchiveIndex, EntryLoc, IndexBuilder, NodeId, RawEntry};
use sys::{Fail, Header, Mode};

/// Заголовков за один шаг разбора.
const STEP: usize = 1024;

/// Как часто ожидающий следующий файл выборки проверяет отмену.
const WAIT: Duration = Duration::from_millis(20);

/// Разность эпох `FILETIME` (1601) и Unix, в сотнях наносекунд.
const FILETIME_UNIX: i64 = 116_444_736_000_000_000;

/// `FSREDIR_*` заголовка RAR5.
const REDIR_UNIX_SYMLINK: u32 = 1;
const REDIR_WIN_SYMLINK: u32 = 2;
const REDIR_JUNCTION: u32 = 3;
const REDIR_HARDLINK: u32 = 4;
const REDIR_FILECOPY: u32 = 5;

/// Пароль `UnRAR` (UTF-16) из байт UTF-8 `OpenCtx`.
fn wide(pw: Option<&[u8]>) -> Option<Zeroizing<Vec<u16>>> {
    let s = std::str::from_utf8(pw?).ok()?;
    Some(Zeroizing::new(s.encode_utf16().collect()))
}

/// `FILETIME` → наносекунды Unix; 0 — времени нет.
fn nt_mtime(ft: u64) -> i64 {
    if ft == 0 {
        return i64::MIN;
    }
    i64::try_from(ft)
        .ok()
        .and_then(|t| t.checked_sub(FILETIME_UNIX))
        .and_then(|t| t.checked_mul(100))
        .unwrap_or(i64::MIN)
}

/// Шифр записи: RAR5 (версия распаковки от 5.0) — AES-256, RAR 3–4 — AES-128.
fn cipher(h: &Header) -> &'static str {
    if h.unp_ver >= 50 {
        "AES-256"
    } else {
        "AES-128"
    }
}

/// Путь архива на диске: `UnRAR` открывает только его.
fn local(src: &ArcSource) -> Result<&Path, VfsError> {
    src.local_path()
        .ok_or(VfsError::Unsupported("archive inside a container: S8-31"))
}

/// Нет следующего тома.
fn missing_volume(outer: &Location, name: &str) -> VfsError {
    let name = Path::new(name)
        .file_name()
        .map_or_else(|| name.to_owned(), |n| n.to_string_lossy().into_owned());
    corrupt(outer, format!("missing volume {name}"))
}

/// Ошибка `UnRAR` → ошибка VFS архива `outer`; `enc` — запись или заголовки зашифрованы.
fn vfs_error(outer: &Location, f: &Fail, enc: bool) -> VfsError {
    if let Some(v) = &f.missing {
        return missing_volume(outer, v);
    }
    match f.code {
        sys::ERAR_MISSING_PASSWORD | sys::ERAR_BAD_PASSWORD => no_password(outer),
        // RAR 3–4 проверки пароля не имеет: неверный пароль — ошибка CRC.
        sys::ERAR_BAD_DATA if enc || f.asked => no_password(outer),
        sys::ERAR_BAD_DATA => corrupt(outer, "CRC"),
        sys::ERAR_BAD_ARCHIVE | sys::ERAR_UNKNOWN_FORMAT => format_error(outer),
        sys::ERAR_EOPEN => match std::fs::File::open(&outer.path) {
            Err(e) => VfsError::from_io(outer.path.clone(), e),
            Ok(_) => corrupt(outer, "cannot open"),
        },
        sys::ERAR_EREAD => VfsError::from_io(
            outer.path.clone(),
            io::Error::new(io::ErrorKind::UnexpectedEof, "read error"),
        ),
        sys::ERAR_NO_MEMORY => {
            VfsError::from_io(outer.path.clone(), io::ErrorKind::OutOfMemory.into())
        }
        sys::ERAR_LARGE_DICT => corrupt(outer, "dictionary over 4 GiB"),
        code => corrupt(outer, format!("UnRAR error {code}")),
    }
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

/// Атрибуты записи: Windows-биты или режим Unix.
fn attrs_of(h: &Header, dir: bool) -> Attrs {
    let mut a = 0;
    if h.host_os == sys::HOST_WIN32 {
        let w = h.attr;
        if w & 0x01 != 0 {
            a |= Attrs::READONLY;
        }
        if w & 0x02 != 0 {
            a |= Attrs::HIDDEN;
        }
        if w & 0x04 != 0 {
            a |= Attrs::SYSTEM;
        }
        if w & 0x20 != 0 {
            a |= Attrs::ARCHIVE;
        }
    } else if h.attr != 0 {
        if !dir && h.attr & 0o111 != 0 {
            a |= Attrs::EXEC;
        }
        if !dir && h.attr & 0o222 == 0 {
            a |= Attrs::READONLY;
        }
    }
    Attrs(a)
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

/// Формат RAR.
#[derive(Debug, Default)]
pub struct RarFormat;

impl RarFormat {
    pub fn new() -> RarFormat {
        RarFormat
    }
}

/// Запись из заголовка; ждёт, пока не станет ясно, нет ли у неё продолжений в томах.
struct Rec {
    path: String,
    kind: EntryKind,
    attrs: Attrs,
    size: u64,
    packed: u64,
    mtime: i64,
    loc: EntryLoc,
    /// Цель hardlink и копии файла — путь в архиве.
    link: Option<String>,
    /// Цель symlink и junction как в заголовке.
    symlink: Option<String>,
}

fn rec_of(h: &Header, ord: u32) -> Rec {
    let (name, escaped) = urfin_vfs::name::escape_wide(&h.name);
    let path: String = name.into();
    let dir = h.flags & sys::RHDF_DIRECTORY != 0;
    let target = (!h.redir_name.is_empty()).then(|| h.redir_name.clone());
    let (kind, link, symlink) = match h.redir {
        REDIR_UNIX_SYMLINK | REDIR_WIN_SYMLINK | REDIR_JUNCTION => {
            (EntryKind::LinkFile, None, target)
        }
        REDIR_HARDLINK | REDIR_FILECOPY => (EntryKind::LinkFile, target, None),
        _ if dir => (EntryKind::Dir, None, None),
        _ => (EntryKind::File, None, None),
    };
    let mut attrs = attrs_of(h, dir);
    if kind == EntryKind::File && h.flags & sys::RHDF_ENCRYPTED != 0 {
        attrs.0 |= Attrs::ENCRYPTED;
    }
    if escaped {
        attrs.0 |= Attrs::ESCAPED_NAME;
    }
    let leaf = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    if urfin_vfs::name::is_dotfile(leaf) {
        attrs.0 |= Attrs::DOTFILE;
    }
    let file = kind == EntryKind::File;
    Rec {
        path,
        kind,
        attrs,
        size: if file { h.unp } else { 0 },
        packed: if kind == EntryKind::Dir { 0 } else { h.pack },
        mtime: nt_mtime(h.mtime),
        loc: if file {
            EntryLoc::Index(ord)
        } else {
            EntryLoc::None
        },
        link,
        symlink,
    }
}

/// Разбор заголовков `UnRAR`.
struct RarScan {
    outer: Location,
    /// `None` — архив дочитан.
    arc: Option<sys::Archive>,
    pw: Option<Zeroizing<Vec<u16>>>,
    first: bool,
    /// Порядковый номер следующей записи.
    ord: u32,
    pending: Option<Rec>,
    /// Файлы по пути: цель hardlink — (данные, размер).
    files: HashMap<u64, (EntryLoc, u64)>,
}

impl RarScan {
    fn flush(&mut self, b: &mut IndexBuilder) {
        let Some(r) = self.pending.take() else {
            return;
        };
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

    /// Конец разбора: последняя запись — в индекс.
    fn end(&mut self, b: &mut IndexBuilder) -> bool {
        self.flush(b);
        self.arc = None;
        false
    }

    /// Ошибка разбора; нет тома — конец листинга на прочитанном.
    fn fail(&mut self, b: &mut IndexBuilder, f: &Fail, enc: bool) -> Result<bool, VfsError> {
        if let Some(v) = &f.missing {
            tracing::warn!(target: "arc", volume = %v, "rar: no next volume, listing ends here");
            return Ok(self.end(b));
        }
        self.arc = None;
        Err(vfs_error(&self.outer, f, enc))
    }
}

impl Scan for RarScan {
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError> {
        let Some(mut arc) = self.arc.take() else {
            return Ok(false);
        };
        let enc_headers = arc.flags & sys::ROADF_ENCHEADERS != 0;
        if std::mem::take(&mut self.first) {
            b.set_solid(arc.flags & sys::ROADF_SOLID != 0);
            b.set_encrypted(enc_headers);
            if let Some(c) = arc.comment.take() {
                b.set_comment(c);
            }
        }
        let pw = self.pw.clone();
        let pw = pw.as_deref().map(Vec::as_slice);
        for _ in 0..STEP {
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            let h = match arc.read_header(pw) {
                Ok(Some(h)) => h,
                Ok(None) => return Ok(self.end(b)),
                Err(f) => return self.fail(b, &f, enc_headers),
            };
            let enc = h.flags & sys::RHDF_ENCRYPTED != 0;
            if enc || enc_headers {
                b.set_cipher(cipher(&h));
            }
            if h.flags & sys::RHDF_SPLITBEFORE != 0 {
                // Продолжение файла из прошлого тома: его сжатые байты.
                if let Some(p) = &mut self.pending {
                    p.packed += h.pack;
                }
            } else {
                self.flush(b);
                self.pending = Some(rec_of(&h, self.ord));
                self.ord = self
                    .ord
                    .checked_add(1)
                    .ok_or_else(|| corrupt(&self.outer, "too many entries"))?;
            }
            if let Err(f) = arc.skip(pw) {
                return self.fail(b, &f, enc || enc_headers);
            }
        }
        self.arc = Some(arc);
        Ok(true)
    }
}

impl Format for RarFormat {
    fn scheme(&self) -> &'static str {
        "rar"
    }

    fn catalog_first(&self) -> bool {
        false
    }

    fn scan(&self, src: &ArcSource) -> Result<Box<dyn Scan>, VfsError> {
        let outer = src.outer();
        let path = local(src)?;
        let pw = wide(src.password().map(str::as_bytes));
        let arc = sys::Archive::open(path, Mode::ListSplit, pw.as_deref().map(Vec::as_slice))
            .map_err(|f| vfs_error(outer, &f, pw.is_some()))?;
        Ok(Box::new(RarScan {
            outer: outer.clone(),
            arc: Some(arc),
            pw,
            first: true,
            ord: 0,
            pending: None,
            files: HashMap::new(),
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
        let EntryLoc::Index(ord) = n.loc else {
            // Ссылка без данных.
            return Ok(Box::new(Empty));
        };
        if n.size == 0 {
            return Ok(Box::new(Empty));
        }
        let pw = idx.ctx().password();
        if n.attrs.has(Attrs::ENCRYPTED) && pw.is_none() {
            return Err(no_password(outer));
        }
        let path = local(src)?.to_path_buf();
        let (tx, rx) = crossbeam_channel::bounded(4);
        Worker {
            outer: outer.clone(),
            path,
            pw: wide(pw.as_deref().map(Vec::as_slice)),
            rounds: vec![vec![Want {
                ord,
                input: 0,
                size: n.size,
                enc: n.attrs.has(Attrs::ENCRYPTED),
            }]],
            sink: Sink::One(tx),
        }
        .spawn()?;
        Ok(Box::new(Pump::new(rx, n.size)))
    }

    fn entry_caps(&self, idx: &ArchiveIndex, node: NodeId) -> u32 {
        let n = idx.node(node);
        match n.loc {
            EntryLoc::Index(_) if n.size > 0 => Caps::SEQUENTIAL,
            _ => Caps::SEEK,
        }
    }

    /// Один читатель: записи по порядку одним проходом. Файл дважды во входном списке
    /// (hardlink и его цель) — повторный проход. Без I/O: архив открывает поток читателя.
    fn plan(
        &self,
        src: &ArcSource,
        idx: &Arc<ArchiveIndex>,
        files: &[(u32, NodeId)],
        _parallel: u8,
    ) -> Option<Vec<Box<dyn ExtractStream>>> {
        let path = src.local_path()?.to_path_buf();
        let mut empty = VecDeque::new();
        let mut data = Vec::with_capacity(files.len());
        for &(input, node) in files {
            let n = idx.node(node);
            match n.loc {
                EntryLoc::Index(ord) if n.size > 0 => data.push(Want {
                    ord,
                    input,
                    size: n.size,
                    enc: n.attrs.has(Attrs::ENCRYPTED),
                }),
                _ => empty.push_back(input),
            }
        }
        data.sort_unstable_by_key(|w| (w.ord, w.input));
        // Проходы: в каждом номер записи — не больше одного раза.
        let mut rounds: Vec<Vec<Want>> = Vec::new();
        let mut prev = None;
        let mut round = 0;
        for w in data {
            round = if prev == Some(w.ord) { round + 1 } else { 0 };
            prev = Some(w.ord);
            if rounds.len() <= round {
                rounds.push(Vec::new());
            }
            rounds[round].push(w);
        }
        let start = (!rounds.is_empty()).then(|| PlanStart {
            path,
            pw: wide(idx.ctx().password().as_deref().map(Vec::as_slice)),
            rounds,
        });
        Some(vec![Box::new(RarStream {
            outer: src.outer().clone(),
            empty,
            start,
            rx: None,
        })])
    }
}

/// Нужный файл: номер записи, индекс во входном списке, размер, зашифрован.
#[derive(Copy, Clone, Debug)]
struct Want {
    ord: u32,
    input: u32,
    size: u64,
    enc: bool,
}

/// Порции данных файла.
type Chunks = Receiver<io::Result<Vec<u8>>>;

/// Файл выборки: индекс во входном списке, размер, данные.
type Next = io::Result<(u32, u64, Chunks)>;

/// Куда распаковщик отдаёт данные.
enum Sink {
    /// Один файл (`open_entry`).
    One(Sender<io::Result<Vec<u8>>>),
    /// Выборка: файл за файлом; ошибка прохода — `Err`.
    Many(Sender<Next>),
}

impl Sink {
    fn fail(&self, e: io::Error) {
        match self {
            Sink::One(tx) => {
                let _ = tx.send(Err(e));
            }
            Sink::Many(tx) => {
                let _ = tx.send(Err(e));
            }
        }
    }
}

/// Порции по [`CHUNK`] в канал файла.
struct Feeder<'a> {
    tx: &'a Sender<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    /// Читатель файла ушёл.
    gone: bool,
}

impl Feeder<'_> {
    /// `false` — читатель файла ушёл.
    fn put(&mut self, mut data: &[u8]) -> bool {
        while !self.gone && !data.is_empty() {
            let k = (CHUNK - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..k]);
            data = &data[k..];
            if self.buf.len() == CHUNK {
                self.flush();
            }
        }
        !self.gone
    }

    fn flush(&mut self) {
        if self.gone || self.buf.is_empty() {
            return;
        }
        let full = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        self.gone = self.tx.send(Ok(full)).is_err();
    }
}

/// Чем кончился файл.
enum Fed {
    /// Отдан (или читатель файла ушёл раньше) — дальше.
    Ok,
    /// Читатель ушёл — стоп.
    Gone,
    /// Распаковка упала: ошибка — у читателя файла.
    Failed(io::Error),
}

/// Распаковщик в своём потоке: свой дескриптор `UnRAR`.
struct Worker {
    outer: Location,
    path: PathBuf,
    pw: Option<Zeroizing<Vec<u16>>>,
    rounds: Vec<Vec<Want>>,
    sink: Sink,
}

impl Worker {
    fn spawn(self) -> Result<(), VfsError> {
        let path = self.outer.path.clone();
        std::thread::Builder::new()
            .name("arc-rar-read".into())
            .spawn(move || self.run())
            .map(drop)
            .map_err(|e| VfsError::from_io(path, e))
    }

    fn run(self) {
        for want in &self.rounds {
            match self.pass(want) {
                Ok(true) => {}
                Ok(false) => return,
                Err(e) => {
                    self.sink.fail(e);
                    return;
                }
            }
        }
    }

    /// Один проход архива: нужные файлы — в `sink`, прочие — `RAR_SKIP`, после последнего
    /// нужного — стоп. `false` — читатель ушёл или ошибка уже у него.
    fn pass(&self, want: &[Want]) -> io::Result<bool> {
        let enc_any = want.iter().any(|w| w.enc);
        let pw = self.pw.as_deref().map(Vec::as_slice);
        let mut arc = sys::Archive::open(&self.path, Mode::Extract, pw)
            .map_err(|f| into_io(vfs_error(&self.outer, &f, enc_any)))?;
        let enc_headers = arc.flags & sys::ROADF_ENCHEADERS != 0;
        #[cfg(test)]
        tests::note_pass(&self.outer.path);
        let mut ord = 0u32;
        let mut at = 0;
        while let Some(&w) = want.get(at) {
            let h = arc
                .read_header(pw)
                .map_err(|f| into_io(vfs_error(&self.outer, &f, enc_headers)))?
                .ok_or_else(|| into_io(corrupt(&self.outer, "no entry")))?;
            let enc = enc_headers || h.flags & sys::RHDF_ENCRYPTED != 0;
            if h.flags & sys::RHDF_SPLITBEFORE != 0 {
                arc.skip(pw)
                    .map_err(|f| into_io(vfs_error(&self.outer, &f, enc)))?;
                continue;
            }
            let cur = ord;
            ord = ord.wrapping_add(1);
            if cur != w.ord {
                arc.skip(pw)
                    .map_err(|f| into_io(vfs_error(&self.outer, &f, enc)))?;
                continue;
            }
            at += 1;
            match self.feed(&mut arc, pw, w, enc) {
                Fed::Ok => {}
                Fed::Gone => return Ok(false),
                Fed::Failed(e) => {
                    if let Sink::Many(tx) = &self.sink {
                        let _ = tx.send(Err(e));
                    }
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Данные файла `w` — читателю.
    fn feed(&self, arc: &mut sys::Archive, pw: Option<&[u16]>, w: Want, enc: bool) -> Fed {
        let tx = match &self.sink {
            Sink::One(tx) => tx.clone(),
            Sink::Many(next) => {
                let (tx, rx) = crossbeam_channel::bounded(4);
                if next.send(Ok((w.input, w.size, rx))).is_err() {
                    return Fed::Gone;
                }
                tx
            }
        };
        let one = matches!(self.sink, Sink::One(_));
        let mut f = Feeder {
            tx: &tx,
            buf: Vec::with_capacity(CHUNK),
            gone: false,
        };
        // Читатель выборки ушёл с файла — остаток распаковывается в никуда; читатель
        // одного файла ушёл — стоп.
        let res = arc.test(pw, &mut |d| f.put(d) || !one);
        match res {
            Ok(true) => {
                f.flush();
                Fed::Ok
            }
            Ok(false) => Fed::Gone,
            Err(fail) => {
                let e = into_io(vfs_error(&self.outer, &fail, enc));
                let copy = copy_error(&e);
                f.flush();
                let _ = tx.send(Err(e));
                Fed::Failed(copy)
            }
        }
    }
}

/// Что нужно читателю выборки, чтобы начать: поток — в первом `next`.
struct PlanStart {
    path: PathBuf,
    pw: Option<Zeroizing<Vec<u16>>>,
    rounds: Vec<Vec<Want>>,
}

/// Поток выборки: файлы без данных, затем записи своим распаковщиком.
struct RarStream {
    outer: Location,
    empty: VecDeque<u32>,
    start: Option<PlanStart>,
    rx: Option<Receiver<Next>>,
}

impl ExtractStream for RarStream {
    fn next(&mut self, cancel: &CancelToken) -> Result<Option<Extracted<'_>>, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if let Some(input) = self.empty.pop_front() {
            return Ok(Some((input, Box::new(io::empty()))));
        }
        if let Some(s) = self.start.take() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            Worker {
                outer: self.outer.clone(),
                path: s.path,
                pw: s.pw,
                rounds: s.rounds,
                sink: Sink::Many(tx),
            }
            .spawn()?;
            self.rx = Some(rx);
        }
        let Some(rx) = &self.rx else {
            return Ok(None);
        };
        loop {
            match rx.recv_timeout(WAIT) {
                Ok(Ok((input, len, data))) => {
                    return Ok(Some((input, Box::new(Pump::new(data, len)))));
                }
                Ok(Err(e)) => {
                    self.rx = None;
                    return Err(VfsError::from_io(self.outer.path.clone(), e));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.rx = None;
                    return Ok(None);
                }
                Err(RecvTimeoutError::Timeout) if cancel.is_cancelled() => {
                    return Err(VfsError::Cancelled);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    use urfin_core::{CancelToken, Location};
    use urfin_vfs::Vfs;

    use super::RarFormat;
    use crate::{ArcCache, ArcVfs, root_of};

    /// Проходов распаковщика на файл архива.
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

    /// Выборка в обратном порядке — один читатель и один проход по порядку записей;
    /// файл дважды во входном списке — второй проход. Фикстура — первый том RAR4
    /// (`tests/fixtures/rar/archive.part1.rar`), файлы без продолжения во втором томе.
    #[test]
    fn plan_is_one_pass_in_archive_order() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("archive.part1.rar");
        let src =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rar/archive.part1.rar");
        std::fs::copy(src, &file).unwrap();
        let v = ArcVfs::new(RarFormat::new(), ArcCache::new(64 << 20));
        let root = root_of("rar", Location::local(file.clone()));
        let cancel = CancelToken::new();
        v.index(&root, &cancel).unwrap();
        let names = [
            ("/vendor/unrar/arccmt.cpp", 4090),
            ("/src/lib.rs", 7870),
            ("/build.rs", 2396),
            ("/Cargo.toml", 313),
            ("/examples/lister.rs", 2693),
            ("/vendor/unrar/acknow.txt", 4280),
        ];
        let at = |p: &str| Location {
            path: PathBuf::from(p),
            ..root.clone()
        };
        let files: Vec<Location> = names.iter().map(|(p, _)| at(p)).collect();
        let streams = v.extract_plan(&files, 4).unwrap();
        assert_eq!(streams.len(), 1, "rar — один читатель");
        let mut order = Vec::new();
        for mut s in streams {
            while let Some((i, mut r)) = s.next(&cancel).unwrap() {
                let mut got = Vec::new();
                std::io::Read::read_to_end(&mut r, &mut got).unwrap();
                let (p, size) = names[i as usize];
                assert_eq!(got.len(), size, "{p}");
                let mut whole = Vec::new();
                std::io::Read::read_to_end(&mut v.open_read(&at(p)).unwrap(), &mut whole).unwrap();
                assert_eq!(got, whole, "{p}");
                order.push(i);
            }
        }
        // Порядок записей в томе: build.rs, Cargo.toml, lister.rs, lib.rs, acknow.txt, arccmt.cpp.
        assert_eq!(order, [2, 3, 4, 1, 5, 0]);
        let before = passes(&file);
        let twice = [at("/build.rs"), at("/build.rs"), at("/Cargo.toml")];
        let mut seen = Vec::new();
        for mut s in v.extract_plan(&twice, 1).unwrap() {
            while let Some((i, mut r)) = s.next(&cancel).unwrap() {
                let mut got = Vec::new();
                std::io::Read::read_to_end(&mut r, &mut got).unwrap();
                assert_eq!(got.len(), if i == 2 { 313 } else { 2396 });
                seen.push(i);
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, [0, 1, 2]);
        assert_eq!(passes(&file) - before, 2, "повтор — второй проход");
    }
}
