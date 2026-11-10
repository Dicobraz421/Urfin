//! Файловые датасеты вьювера и редактора (S7 §9, S7-00a): V1 — лог, V2 — строка
//! JSON, V3 — разреженный бинарник, V4 — UTF-16LE, V6 — растущий лог.
//!
//! Все детерминированы по `seed` и размеру: одинаковые аргументы — одинаковые байты.
//! Размер — точный (у V4 — чётный). Последняя строка дополняется пробелами до размера.

use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{Rng, datasets_root};

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// Версия генераторов файлов: меняется вместе с содержимым — старые датасеты пересоздаются.
const FILES_VERSION: u32 = 1;

/// Какой файл.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileSet {
    /// Лог UTF-8, LF, ≈ 91 байт на строку: 10 ГБ ≈ 120 млн строк.
    V1,
    /// Одна строка minified JSON без перевода строки.
    V2,
    /// Бинарный разреженный файл: заголовок, хвост и 8 блоков внутри, остальное — нули.
    V3,
    /// Тот же лог в UTF-16LE с BOM, CRLF.
    V4,
}

impl FileSet {
    pub const ALL: [FileSet; 4] = [FileSet::V1, FileSet::V2, FileSet::V3, FileSet::V4];

    /// Размер по S7 §9: 10 ГБ, 50 МБ, 4,5 ГБ, 1 ГБ.
    pub const fn default_size(self) -> u64 {
        match self {
            FileSet::V1 => 10 * GIB,
            FileSet::V2 => 50 * MIB,
            FileSet::V3 => 9 * GIB / 2,
            FileSet::V4 => GIB,
        }
    }

    pub const fn tag(self) -> &'static str {
        match self {
            FileSet::V1 => "v1",
            FileSet::V2 => "v2",
            FileSet::V3 => "v3",
            FileSet::V4 => "v4",
        }
    }

    const fn ext(self) -> &'static str {
        match self {
            FileSet::V1 => "log",
            FileSet::V2 => "json",
            FileSet::V3 => "bin",
            FileSet::V4 => "txt",
        }
    }

    pub fn parse(s: &str) -> Option<FileSet> {
        FileSet::ALL
            .into_iter()
            .find(|f| f.tag().eq_ignore_ascii_case(s))
    }
}

/// Файл датасета: что, сколько байт, сид.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FileSpec {
    pub set: FileSet,
    pub size: u64,
    pub seed: u64,
}

impl FileSpec {
    /// Размер по умолчанию, сид 1.
    pub const fn new(set: FileSet) -> Self {
        FileSpec {
            set,
            size: set.default_size(),
            seed: 1,
        }
    }

    /// Длина файла: `size`, у V4 — округлённая вниз до чётной.
    pub const fn len(&self) -> u64 {
        match self.set {
            FileSet::V4 => self.size & !1,
            _ => self.size,
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Имя файла в каталоге датасетов: `v1-10g-s1.log`, `v3-4.5g-s1.bin`, `v2-50m-s1.json`.
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}-s{}.{}",
            self.set.tag(),
            size_label(self.size),
            self.seed,
            self.set.ext()
        )
    }

    fn signature(&self) -> String {
        format!(
            "files v{FILES_VERSION} set={} size={} seed={}",
            self.set.tag(),
            self.size,
            self.seed
        )
    }
}

/// `10g`, `4.5g`, `50m`, `1234b`.
fn size_label(size: u64) -> String {
    if size >= GIB && size.is_multiple_of(GIB) {
        format!("{}g", size / GIB)
    } else if size >= GIB && size.is_multiple_of(GIB / 2) {
        format!("{}.5g", size / GIB)
    } else if size >= MIB && size.is_multiple_of(MIB) {
        format!("{}m", size / MIB)
    } else if size >= KIB && size.is_multiple_of(KIB) {
        format!("{}k", size / KIB)
    } else {
        format!("{size}b")
    }
}

/// Размер из строки: байты или с суффиксом `k`/`m`/`g` (двоичные), дробь — `4.5g`.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mul) = match s.as_bytes().last()? {
        b'k' => (&s[..s.len() - 1], KIB),
        b'm' => (&s[..s.len() - 1], MIB),
        b'g' => (&s[..s.len() - 1], GIB),
        _ => (&s[..], 1),
    };
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    let int: u64 = int.parse().ok()?;
    let mut bytes = int.checked_mul(mul)?;
    if !frac.is_empty() {
        let digits = u32::try_from(frac.len()).ok()?;
        let f: u64 = frac.parse().ok()?;
        bytes = bytes.checked_add(f.checked_mul(mul)? / 10u64.checked_pow(digits)?)?;
    }
    Some(bytes)
}

/// Создать файл `spec` по пути `path` (перезаписывается).
pub fn gen_file(path: &Path, spec: &FileSpec) -> io::Result<()> {
    match spec.set {
        FileSet::V1 => gen_log(path, spec.size, spec.seed, Enc::Utf8),
        FileSet::V2 => gen_json(path, spec.size, spec.seed),
        FileSet::V3 => gen_sparse(path, spec.size, spec.seed),
        FileSet::V4 => gen_log(path, spec.len(), spec.seed, Enc::Utf16Le),
    }
}

/// Файл `spec` как датасет приложения `app`: `%LOCALAPPDATA%\Pets\testdata\<app>\<имя>`.
/// Готовый берётся как есть, если метка (сид, размер, версия) и длина совпадают.
pub fn dataset_file(app: &str, spec: &FileSpec) -> io::Result<PathBuf> {
    file_in(
        &datasets_root(app),
        &spec.file_name(),
        &spec.signature(),
        spec.len(),
        |p| gen_file(p, spec),
    )
}

pub(crate) fn file_in(
    root: &Path,
    name: &str,
    signature: &str,
    len: u64,
    build: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let file = root.join(name);
    let mark = crate::marker(root, name);
    let ready = std::fs::read_to_string(&mark).is_ok_and(|s| s == signature)
        && std::fs::metadata(&file).is_ok_and(|m| m.is_file() && m.len() == len);
    if ready {
        return Ok(file);
    }
    let _ = std::fs::remove_file(&mark);
    std::fs::create_dir_all(root)?;
    build(&file)?;
    std::fs::write(&mark, signature)?;
    Ok(file)
}

/// Хэш первых и последних 64 КБ и длины (FNV-1a): сверка двух прогонов без чтения
/// всего файла.
pub fn edge_hash(path: &Path) -> io::Result<u64> {
    use std::io::Read;
    const EDGE: u64 = 64 * KIB;
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut h = Fnv::default();
    h.feed(&len.to_le_bytes());
    let mut buf = vec![0u8; usize::try_from(EDGE.min(len)).unwrap_or(0)];
    f.read_exact(&mut buf)?;
    h.feed(&buf);
    let tail = EDGE.min(len);
    f.seek(SeekFrom::Start(len - tail))?;
    buf.resize(usize::try_from(tail).unwrap_or(0), 0);
    f.read_exact(&mut buf)?;
    h.feed(&buf);
    Ok(h.0)
}

#[derive(Debug)]
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xCBF2_9CE4_8422_2325)
    }
}

impl Fnv {
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

// ---------------------------------------------------------------- лог (V1, V4, V6)

/// Кодировка строк лога.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Enc {
    /// UTF-8, LF.
    Utf8,
    /// UTF-16LE с BOM, CRLF.
    Utf16Le,
}

impl Enc {
    fn bom(self) -> &'static [u8] {
        match self {
            Enc::Utf8 => b"",
            Enc::Utf16Le => b"\xFF\xFE",
        }
    }

    fn eol(self) -> &'static [u8] {
        match self {
            Enc::Utf8 => b"\n",
            Enc::Utf16Le => b"\r\0\n\0",
        }
    }

    fn space(self) -> &'static [u8] {
        match self {
            Enc::Utf8 => b" ",
            Enc::Utf16Le => b" \0",
        }
    }
}

const METHODS: [&str; 5] = ["GET", "GET", "GET", "POST", "PUT"];
const RESOURCES: [&str; 6] = ["items", "users", "orders", "files", "reports", "sessions"];
const STATUSES: [u16; 8] = [200, 200, 200, 200, 201, 304, 404, 500];
const RU_NODES: [&str; 4] = ["узла", "сервера", "хранилища", "шлюза"];

/// 2026-11-01 в днях от 1970-01-01.
const BASE_DAYS: i64 = 20_758;

/// Строки лога: время растёт, состав — HTTP, DEBUG, кириллица, CJK, длинные ERROR.
#[derive(Debug)]
struct LogGen {
    rng: Rng,
    ms: u64,
    n: u64,
}

impl LogGen {
    fn new(seed: u64) -> Self {
        LogGen {
            rng: Rng::new(seed ^ 0x5631_4C4F_4721),
            ms: 0,
            n: 0,
        }
    }

    /// Строка без перевода строки (только BMP: UTF-16 без суррогатов).
    fn line(&mut self, out: &mut String) {
        use std::fmt::Write as _;
        self.ms += self.rng.below(3);
        self.n += 1;
        write_time(out, self.ms);
        let r = &mut self.rng;
        let roll = r.below(1000);
        let pick = |r: &mut Rng, list: &[&'static str]| list[r.below(list.len() as u64) as usize];
        if roll < 850 {
            let status = STATUSES[r.below(STATUSES.len() as u64) as usize];
            let level = if status >= 500 { "ERROR" } else { "INFO " };
            let _ = write!(
                out,
                "{level} [worker-{:02}] {} /api/{}/{} {status} {}ms req={:08x}",
                r.below(16),
                pick(r, &METHODS),
                pick(r, &RESOURCES),
                r.below(1_000_000),
                r.below(250) + 1,
                r.next_u64() as u32,
            );
        } else if roll < 930 {
            let _ = write!(
                out,
                "DEBUG [cache] hit ratio {}.{}% keys={} evicted={}",
                r.below(100),
                r.below(10),
                r.below(100_000),
                r.below(500),
            );
        } else if roll < 980 {
            let _ = write!(
                out,
                "WARN  [sync] медленный ответ от {} node-{}: {} мс, повтор {}",
                pick(r, &RU_NODES),
                r.below(64),
                r.below(5000) + 500,
                r.below(3) + 1,
            );
        } else if roll < 996 {
            let _ = write!(
                out,
                "INFO  [i18n] 翻訳ファイルを読み込みました: ja-JP, {} 件",
                r.below(10_000)
            );
        } else {
            let _ = write!(
                out,
                "ERROR [db] query failed after {} ms: SELECT id, name, size FROM files WHERE id IN (",
                r.below(30_000)
            );
            let n = r.below(120) + 10;
            for i in 0..n {
                let sep = if i == 0 { "" } else { ", " };
                let _ = write!(out, "{sep}{}", r.below(10_000_000));
            }
            out.push(')');
        }
    }
}

/// `2026-11-01 00:00:00.000 ` + `ms` от начала.
fn write_time(out: &mut String, ms: u64) {
    use std::fmt::Write as _;
    let (day, rem) = (ms / 86_400_000, ms % 86_400_000);
    let (year, month, mday) = civil_from_days(BASE_DAYS + i64::try_from(day).unwrap_or(0));
    let _ = write!(
        out,
        "{year:04}-{month:02}-{mday:02} {:02}:{:02}:{:02}.{:03} ",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    );
}

/// Дата по дням от 1970-01-01 (H. Hinnant, `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Поток строк лога в байтах кодировки: целые строки и последняя — подогнанная.
#[derive(Debug)]
struct LogStream {
    lines: LogGen,
    enc: Enc,
    text: String,
    /// Следующая строка в кодировке, с переводом строки; пустая — не создана.
    pending: Vec<u8>,
}

impl LogStream {
    fn new(seed: u64, enc: Enc) -> Self {
        LogStream {
            lines: LogGen::new(seed),
            enc,
            text: String::new(),
            pending: Vec::new(),
        }
    }

    fn pending(&mut self) -> &[u8] {
        if self.pending.is_empty() {
            self.text.clear();
            self.lines.line(&mut self.text);
            match self.enc {
                Enc::Utf8 => self.pending.extend_from_slice(self.text.as_bytes()),
                Enc::Utf16Le => {
                    for u in self.text.encode_utf16() {
                        self.pending.extend_from_slice(&u.to_le_bytes());
                    }
                }
            }
            self.pending.extend_from_slice(self.enc.eol());
        }
        &self.pending
    }

    /// Целые строки, пока влезают в `room`; сколько записано.
    fn take(&mut self, out: &mut Vec<u8>, room: u64) -> u64 {
        let mut written = 0;
        while self.pending().len() as u64 <= room - written {
            out.extend_from_slice(&self.pending);
            written += self.pending.len() as u64;
            self.pending.clear();
        }
        written
    }

    /// Ровно `room` байт: начало следующей строки по границе символа, пробелы, перевод.
    fn finish(&mut self, out: &mut Vec<u8>, room: u64) {
        let room = usize::try_from(room).unwrap_or(0);
        let (eol, space) = (self.enc.eol(), self.enc.space());
        self.pending();
        let start = out.len();
        if room >= eol.len() {
            let body = room - eol.len();
            let cut = match self.enc {
                Enc::Utf8 => {
                    let mut c = body.min(self.text.len());
                    while !self.text.is_char_boundary(c) {
                        c -= 1;
                    }
                    c
                }
                Enc::Utf16Le => body.min(self.pending.len() - eol.len()) & !1,
            };
            out.extend_from_slice(&self.pending[..cut]);
            while out.len() - start < body {
                out.extend_from_slice(space);
            }
            out.extend_from_slice(eol);
        } else {
            while out.len() - start < room {
                out.extend_from_slice(space);
            }
        }
        out.truncate(start + room);
        self.pending.clear();
    }

    /// Следующая порция до `room` байт; `last` — `room` ровно до конца файла.
    fn chunk(&mut self, out: &mut Vec<u8>, room: u64, last: bool) -> u64 {
        let w = self.take(out, room);
        if w == 0 && last && room > 0 {
            self.finish(out, room);
            return room;
        }
        w
    }
}

const CHUNK: u64 = 4 * MIB;

fn gen_log(path: &Path, len: u64, seed: u64, enc: Enc) -> io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let mut s = LogStream::new(seed, enc);
    let bom = enc.bom();
    let bom = &bom[..bom.len().min(usize::try_from(len).unwrap_or(0))];
    f.write_all(bom)?;
    let mut left = len - bom.len() as u64;
    let mut buf = Vec::with_capacity(usize::try_from(CHUNK).unwrap_or(0) + 4096);
    while left > 0 {
        buf.clear();
        let room = left.min(CHUNK);
        left -= s.chunk(&mut buf, room, room == left);
        f.write_all(&buf)?;
    }
    f.sync_all()
}

/// V6: растущий лог в `path` (файл пересоздаётся) — поток V1 с тем же сидом,
/// `rate` байт в секунду порциями раз в 20 мс. Стоп — по `limit` байт (последняя строка
/// подгоняется, как в V1: при одном сиде файл равен V1 того же размера) или по `stop`.
pub fn grow_log(
    path: &Path,
    seed: u64,
    rate: u64,
    limit: Option<u64>,
    stop: Option<Duration>,
) -> io::Result<u64> {
    const TICK: Duration = Duration::from_millis(20);
    let mut f = std::fs::File::create(path)?;
    let mut s = LogStream::new(seed, Enc::Utf8);
    let limit = limit.unwrap_or(u64::MAX);
    let t0 = Instant::now();
    let mut written = 0u64;
    let mut buf = Vec::new();
    while written < limit && stop.is_none_or(|d| t0.elapsed() < d) {
        let due = u64::try_from(u128::from(rate) * t0.elapsed().as_millis() / 1000)
            .unwrap_or(u64::MAX)
            .max(written);
        let room = (due - written).min(limit - written);
        buf.clear();
        written += s.chunk(&mut buf, room, room == limit - written);
        if !buf.is_empty() {
            f.write_all(&buf)?;
            f.flush()?;
        }
        if written < limit {
            std::thread::sleep(TICK);
        }
    }
    Ok(written)
}

// ---------------------------------------------------------------- V2

const JSON_WORDS: [&str; 8] = [
    "alpha", "beta", "gamma", "delta", "omega", "kappa", "sigma", "zeta",
];
const JSON_RU: [&str; 6] = ["отчёт", "смета", "договор", "счёт", "архив", "заметки"];

fn json_obj(r: &mut Rng, i: u64, out: &mut String) {
    use std::fmt::Write as _;
    let pick = |r: &mut Rng, list: &[&'static str]| list[r.below(list.len() as u64) as usize];
    let _ = write!(
        out,
        "{{\"id\":{i},\"name\":\"item-{i}\",\"title\":\"{} {}\",\"ru\":\"{} \\u2116{}\",\"tags\":[\"{}\",\"{}\"],\"price\":{}.{:02},\"ok\":{},\"nested\":{{\"x\":{},\"y\":[{},{},{}],\"note\":null}}}}",
        pick(r, &JSON_WORDS),
        pick(r, &JSON_WORDS),
        pick(r, &JSON_RU),
        r.below(1000),
        pick(r, &JSON_WORDS),
        pick(r, &JSON_WORDS),
        r.below(100_000),
        r.below(100),
        r.below(2) == 0,
        r.below(1 << 20),
        r.below(10),
        r.below(100),
        r.below(1000),
    );
}

/// Массив объектов в одну строку; хвост до размера — пробелы перед `]` (JSON валиден).
fn gen_json(path: &Path, size: u64, seed: u64) -> io::Result<()> {
    let mut f = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    if size < 2 {
        f.write_all(&b"["[..usize::try_from(size).unwrap_or(0)])?;
        return f
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all();
    }
    let mut r = Rng::new(seed ^ 0x5632_4A53_4F4E);
    let mut obj = String::new();
    f.write_all(b"[")?;
    let mut written = 1u64;
    for i in 0.. {
        obj.clear();
        if i > 0 {
            obj.push(',');
        }
        json_obj(&mut r, i, &mut obj);
        if written + obj.len() as u64 + 1 > size {
            break;
        }
        f.write_all(obj.as_bytes())?;
        written += obj.len() as u64;
    }
    let pad = vec![b' '; 4096];
    while written + 1 < size {
        let n = (size - 1 - written).min(pad.len() as u64);
        f.write_all(&pad[..usize::try_from(n).unwrap_or(0)])?;
        written += n;
    }
    f.write_all(b"]")?;
    f.into_inner()
        .map_err(io::IntoInnerError::into_error)?
        .sync_all()
}

// ---------------------------------------------------------------- V3

/// Заголовок и хвост — по 64 КБ данных, внутри — 8 блоков по 4 КБ на границах 64 КБ
/// (единица выделения NTFS для разреженного файла): на диске ≈ 640 КБ.
fn gen_sparse(path: &Path, size: u64, seed: u64) -> io::Result<()> {
    const EDGE: u64 = 64 * KIB;
    const BLOCK: u64 = 4 * KIB;
    let mut f = std::fs::File::create(path)?;
    crate::sys::set_sparse(path)?;
    f.set_len(size)?;
    let mut r = Rng::new(seed ^ 0x5633_5350_4152);
    let mut put = |f: &mut std::fs::File, at: u64, n: u64, label: &str| -> io::Result<()> {
        let n = n.min(size.saturating_sub(at));
        if n == 0 {
            return Ok(());
        }
        let mut data: Vec<u8> = (0..n.div_ceil(8))
            .flat_map(|_| r.next_u64().to_le_bytes())
            .take(usize::try_from(n).unwrap_or(0))
            .collect();
        // Текст в блоке — для поиска в hex-режиме.
        let tag = format!("URFIN-V3 {label} @{at:#x} seed={seed}\0");
        let k = tag.len().min(data.len());
        data[..k].copy_from_slice(&tag.as_bytes()[..k]);
        f.seek(SeekFrom::Start(at))?;
        f.write_all(&data)
    };
    put(&mut f, 0, EDGE, "head")?;
    if size >= 2 {
        // «MZ» в начале: детект — бинарный.
        f.seek(SeekFrom::Start(0))?;
        f.write_all(b"MZ")?;
    }
    for k in 1..=8u64 {
        let at = (size / 9 * k) & !(EDGE - 1);
        put(&mut f, at.max(EDGE), BLOCK, &format!("block {k}"))?;
    }
    let tail = size.saturating_sub(EDGE).max(EDGE.min(size));
    put(&mut f, tail, EDGE, "tail")?;
    f.sync_all()
}

/// Сколько файл занимает на диске (у разреженного — выделенные диапазоны).
pub fn allocated(path: &Path) -> io::Result<u64> {
    crate::sys::allocated(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("urfin-files-")
            .tempdir()
            .unwrap()
    }

    fn spec(set: FileSet, size: u64, seed: u64) -> FileSpec {
        FileSpec { set, size, seed }
    }

    /// Приёмка S7-00a: два прогона с одним сидом — одинаковые байты (хэш краёв), другой
    /// сид — другие; длина точная. Размеры уменьшены (полные — в `testdata v1…v4`).
    #[test]
    fn same_seed_same_bytes() {
        let dir = tmp();
        let cases = [
            spec(FileSet::V1, 3 * MIB + 17, 7),
            spec(FileSet::V2, MIB + 3, 7),
            spec(FileSet::V3, 16 * MIB, 7),
            spec(FileSet::V4, 2 * MIB + 6, 7),
        ];
        for s in cases {
            let a = dir.path().join(format!("a-{}", s.file_name()));
            let b = dir.path().join(format!("b-{}", s.file_name()));
            let c = dir.path().join(format!("c-{}", s.file_name()));
            gen_file(&a, &s).unwrap();
            gen_file(&b, &s).unwrap();
            gen_file(&c, &FileSpec { seed: 8, ..s }).unwrap();
            assert_eq!(std::fs::metadata(&a).unwrap().len(), s.len(), "{s:?}");
            assert_eq!(edge_hash(&a).unwrap(), edge_hash(&b).unwrap(), "{s:?}");
            assert_ne!(edge_hash(&a).unwrap(), edge_hash(&c).unwrap(), "{s:?}");
            assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        }
    }

    /// V1: UTF-8, строки с LF, ≈ 91 байт на строку (10 ГБ ≈ 120 млн), время растёт.
    #[test]
    fn v1_log_lines() {
        let dir = tmp();
        let p = dir.path().join("v1.log");
        gen_file(&p, &spec(FileSet::V1, 4 * MIB + 1, 1)).unwrap();
        let text = String::from_utf8(std::fs::read(&p).unwrap()).unwrap();
        assert!(text.ends_with('\n'));
        let lines: Vec<&str> = text.lines().collect();
        let avg = text.len() / lines.len();
        assert!((85..=95).contains(&avg), "avg {avg}");
        assert!(lines.iter().any(|l| l.contains("медленный")));
        assert!(lines.iter().any(|l| l.contains("翻訳")));
        assert!(lines.iter().any(|l| l.len() > 500));
        assert!(lines.windows(2).all(|w| w[0][..23] <= w[1][..23]));
    }

    /// V2: одна строка, валидный массив JSON (скобки, без перевода строки).
    #[test]
    fn v2_one_line_json() {
        let dir = tmp();
        let p = dir.path().join("v2.json");
        gen_file(&p, &spec(FileSet::V2, MIB, 1)).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(b.first(), Some(&b'['));
        assert_eq!(b.last(), Some(&b']'));
        assert!(!b.contains(&b'\n'));
        let depth = b.iter().fold(0i64, |d, &c| match c {
            b'{' | b'[' => d + 1,
            b'}' | b']' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
        assert!(std::str::from_utf8(&b).is_ok());
    }

    /// V3 полного размера (4,5 ГБ) занимает на диске < 1 МБ и помечен разреженным.
    #[test]
    fn v3_sparse_under_1mb() {
        let dir = tmp();
        let p = dir.path().join("v3.bin");
        let s = FileSpec::new(FileSet::V3);
        gen_file(&p, &s).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 9 * GIB / 2);
        let on_disk = allocated(&p).unwrap();
        assert!(on_disk > 0 && on_disk < 1_000_000, "on disk {on_disk}");
        assert!(crate::sys::is_sparse(&p).unwrap());
    }

    /// V4: BOM, чётная длина, UTF-16 без ошибок, CRLF.
    #[test]
    fn v4_utf16le() {
        let dir = tmp();
        let p = dir.path().join("v4.txt");
        gen_file(&p, &spec(FileSet::V4, MIB + 1, 1)).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(b.len() as u64, MIB);
        assert_eq!(&b[..2], b"\xFF\xFE");
        let units = b[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c));
        let text: String = char::decode_utf16(units).map(Result::unwrap).collect();
        assert!(text.ends_with("\r\n"));
        assert!(text.contains("медленный"));
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
    }

    /// V6 с лимитом равен V1 того же размера и сида; темп выдерживается.
    #[test]
    fn v6_grows_like_v1() {
        let dir = tmp();
        let (g, v1) = (dir.path().join("v6.log"), dir.path().join("v1.log"));
        let t = Instant::now();
        let n = grow_log(&g, 3, 10 * MIB, Some(MIB + 5), None).unwrap();
        let took = t.elapsed();
        assert_eq!(n, MIB + 5);
        assert!(took >= Duration::from_millis(80), "{took:?}");
        gen_file(&v1, &spec(FileSet::V1, MIB + 5, 3)).unwrap();
        assert_eq!(std::fs::read(&g).unwrap(), std::fs::read(&v1).unwrap());
    }

    /// Повторный запуск не пересоздаёт готовый файл; другой сид или длина — пересоздаёт.
    #[test]
    fn file_reused_by_signature_and_len() {
        let root = tmp();
        let mut builds = 0;
        let mut make = |sig: &str, len: u64| {
            file_in(root.path(), "f.log", sig, len, |p| {
                builds += 1;
                std::fs::write(p, vec![b'x'; usize::try_from(len).unwrap()])
            })
            .unwrap()
        };
        let a = make("s1", 10);
        make("s1", 10);
        make("s2", 10);
        std::fs::write(&a, b"short").unwrap();
        make("s2", 10);
        assert_eq!(builds, 3);
    }

    #[test]
    fn names_and_sizes() {
        assert_eq!(FileSpec::new(FileSet::V1).file_name(), "v1-10g-s1.log");
        assert_eq!(FileSpec::new(FileSet::V2).file_name(), "v2-50m-s1.json");
        assert_eq!(FileSpec::new(FileSet::V3).file_name(), "v3-4.5g-s1.bin");
        assert_eq!(FileSpec::new(FileSet::V4).file_name(), "v4-1g-s1.txt");
        assert_eq!(parse_size("4.5g"), Some(9 * GIB / 2));
        assert_eq!(parse_size("50M"), Some(50 * MIB));
        assert_eq!(parse_size("123"), Some(123));
        assert_eq!(parse_size("x"), None);
        assert_eq!(civil_from_days(BASE_DAYS), (2026, 11, 1));
        assert_eq!(civil_from_days(BASE_DAYS + 61), (2027, 1, 1));
    }
}
