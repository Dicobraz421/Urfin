//! Бенч архивов S8 §7 (S8-37): сценарии A1–A7 против 7-Zip, `tar.exe`, `UnRAR.exe` и
//! Проводника; сравнение 7z-крейтов `sevenz-rust2` и `zesven` на A3, A6. Метрики — JSON.
//!
//! ```text
//! cargo build --release -p urfin-arc --features rar --example arc_bench
//! target\release\examples\arc_bench.exe gen <каталог>
//! target\release\examples\arc_bench.exe run <каталог> [--runs 5] [--big-runs 3]
//!     [--only a1,a3] [--json <файл>] [--7z <7z.exe>] [--rar <Rar.exe>]
//!     [--unrar <UnRAR.exe>] [--tar <tar.exe>] [--zesven-a3]
//! ```
//!
//! `gen` (фиксированные сиды, данные — только свои; размеры — распакованные):
//! - `a1.zip` — 100 000 файлов 0–2 КБ в 1000 каталогах корня, deflate (крейт `zip`);
//! - `a2.zip` — `data/`: 10 000 файлов 1–32 КБ в 100 каталогах, deflate;
//! - дерево `src/data` ≈ 1 ГБ: файлы 4 КБ–2 МБ (лог-равномерно), 60 % текста, 25 % записей,
//!   15 % шума; из него `a3.7z` (`7z a`, настройки 7-Zip по умолчанию: LZMA2, solid),
//!   `a4.tar.zst` (крейты `tar` + `zstd`, уровень 3) и `a7.rar` (`rar a -ma5`, не solid);
//!   затем дерево удаляется;
//! - `a5.tar.gz` — 2 ГБ таких же файлов потоком в 20 каталогах, gzip «быстро» (`flate2`).
//!
//! `run`: каждый замер — в свежем процессе: Windows понижает приоритет (`QoS`) долгоживущему
//! фоновому процессу без окна (`docs/perf/s8.md`, #345). Урфин и сырые крейты — дочерний
//! `part`, утилиты — свой процесс (время — с его запуском). Первый прогон — прогрев, в медиану
//! не входит; инструменты в прогоне чередуются. Перед извлечением `out` очищается (вне замера),
//! после — считаются файлы и байты (сверка между инструментами).
//!
//! «urfin» — `ops.copy`; «urfin plan» — профиль без ops: обход индекса, `extract_plan` и
//! запись файлов `File::create` + `io::copy` (разница с «urfin» — цена движка копирования).
//! Дочерний замер дольше 10 мин снимается (`timeout=1`).
//!
//! Сценарии:
//! - A1 — вход в `a1.zip`: `first_ms` — первая порция корня, `ms` — весь корень (индекс
//!   построен), `again_ms` — повторный вход (индекс в кэше); 7-Zip — `7z l`;
//! - A2 — извлечение `a2.zip`: Урфин — `ops.copy` из панели в архиве (движок F5, `Core` без
//!   окна), 7-Zip — `7z x`, Проводник — `Shell.Application.CopyHere` (zipfldr);
//! - A3 — `a3.7z` целиком: Урфин, `7z x` (`-mmt` по умолчанию), сырой `sevenz-rust2`
//!   (`ArchiveReader`, потоков LZMA2 — ядра); `zesven` (`Archive::extract`) — по `--zesven-a3`;
//! - A4 — `a4.tar.zst`: Урфин, `tar.exe --zstd -x`;
//! - A5 — первая порция корня `a5.tar.gz` (поток листинга `tar`);
//! - A6 — F3 последнего файла solid-блока `a3.7z`: Урфин — `open_read` до конца, индекс уже
//!   в кэше, как в панели (`index_ms` — отдельно); 7-Zip FM ≈ `7z e -so` (то же: блок до файла),
//!   `7z t` — весь архив; сырые крейты — чтение файла в память;
//! - A7 — `a7.rar`: Урфин (нужна `--features rar`), `UnRAR x`.
//!
//! Far (arclite), 7-Zip FM и Проводник на A1 — только через GUI, синтетический ввод запрещён:
//! ручной замер.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    reason = "пример-бенч: мс в f64, длинные сценарии"
)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as Proc, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_arc::{ArcCfg, ArcVfs, SevenZFormat, TarFormat, ZipFormat, root_of};
use urfin_core::{
    Boot, CancelToken, Command, Core, JobState, JobSummary, Location, Services, Waker,
};
use urfin_vfs::{ListOpts, Vfs, VfsCfg, VfsListing, VfsRegistry};

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// Файлов в `a2.zip`.
const A2_FILES: usize = 10_000;

// ---------- данные ----------

/// `SplitMix64`.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn pick<'a, T: ?Sized>(&mut self, list: &[&'a T]) -> &'a T {
        list[self.below(list.len() as u64) as usize]
    }

    /// Лог-равномерно в `lo..hi`.
    fn span(&mut self, lo: u64, hi: u64) -> u64 {
        let u = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        (lo as f64 * (hi as f64 / lo as f64).powf(u)) as u64
    }
}

const WORDS: [&str; 32] = [
    "report",
    "file",
    "value",
    "result",
    "index",
    "buffer",
    "config",
    "state",
    "entry",
    "path",
    "name",
    "count",
    "items",
    "build",
    "main",
    "test",
    "отчёт",
    "договор",
    "значение",
    "строка",
    "файл",
    "каталог",
    "ошибка",
    "размер",
    "запись",
    "конец",
    "summary",
    "archive",
    "budget",
    "plan",
    "track",
    "chapter",
];

const TAGS: [&[u8; 10]; 8] = [
    b"alpha_____",
    b"beta______",
    b"gamma_____",
    b"delta_____",
    b"user:12345",
    b"order:0042",
    b"ru-RU_____",
    b"en-US_____",
];

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Records,
    Noise,
}

impl Kind {
    fn pick(rng: &mut Rng, noise: bool) -> Kind {
        match rng.below(100) {
            0..60 => Kind::Text,
            60..85 => Kind::Records,
            _ if noise => Kind::Noise,
            _ => Kind::Text,
        }
    }

    fn ext(self, rng: &mut Rng) -> &'static str {
        match self {
            Kind::Text => rng.pick(&["txt", "rs", "md", "log", "json"]),
            Kind::Records => rng.pick(&["dat", "bin", "db"]),
            Kind::Noise => rng.pick(&["jpg", "png", "zip"]),
        }
    }
}

/// Содержимое файла вида `kind` размером `size` в `out`.
fn fill(rng: &mut Rng, kind: Kind, size: usize, out: &mut Vec<u8>) {
    out.clear();
    match kind {
        Kind::Text => {
            while out.len() < size {
                let n = 3 + rng.below(10);
                for i in 0..n {
                    if i > 0 {
                        out.push(b' ');
                    }
                    if rng.below(8) == 0 {
                        out.extend_from_slice(rng.below(100_000).to_string().as_bytes());
                    } else {
                        out.extend_from_slice(rng.pick(&WORDS).as_bytes());
                    }
                }
                out.extend_from_slice(b"\r\n");
            }
        }
        Kind::Records => {
            let mut id = 0u32;
            while out.len() < size {
                out.extend_from_slice(&id.to_le_bytes());
                id += 1;
                out.extend_from_slice(&(rng.below(4) as u16).to_le_bytes());
                let tag: &[u8; 10] = rng.pick(&TAGS);
                out.extend_from_slice(tag);
                out.extend_from_slice(&rng.below(1_000_000).to_le_bytes());
                out.extend_from_slice(&[0; 8]);
            }
        }
        Kind::Noise => {
            while out.len() < size {
                out.extend_from_slice(&rng.next().to_le_bytes());
            }
        }
    }
    out.truncate(size);
}

/// Файлы 4 КБ–2 МБ общим размером ≥ `total`: `f(относительный путь, данные)`; `dir(i)` —
/// каталог `i`-го файла.
fn files(
    seed: u64,
    total: u64,
    dir: impl Fn(usize) -> String,
    mut f: impl FnMut(&str, &[u8]) -> io::Result<()>,
) -> io::Result<(usize, u64)> {
    let mut rng = Rng(seed);
    let mut buf = Vec::new();
    let (mut n, mut bytes) = (0, 0);
    while bytes < total {
        let kind = Kind::pick(&mut rng, true);
        let size = rng.span(4 * KIB, 2 * MIB);
        fill(&mut rng, kind, size as usize, &mut buf);
        let word = rng.pick(&WORDS);
        let ext = kind.ext(&mut rng);
        f(&format!("{}/{word}_{n:05}.{ext}", dir(n)), &buf)?;
        n += 1;
        bytes += size;
    }
    Ok((n, bytes))
}

fn zip_opts() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::from_date_and_time(2025, 3, 4, 5, 6, 8).unwrap())
}

fn zip_err(e: zip::result::ZipError) -> io::Error {
    io::Error::other(e)
}

/// `a1.zip`: 100 000 файлов в 1000 каталогах корня.
fn gen_a1(p: &Path) -> io::Result<()> {
    let mut z = zip::ZipWriter::new(BufWriter::new(File::create(p)?));
    let mut rng = Rng(1);
    let mut buf = Vec::new();
    for d in 0..1000 {
        let dir = format!("dir {d:04}/");
        z.add_directory(dir.as_str(), zip_opts()).map_err(zip_err)?;
        for i in 0..100 {
            let size = if rng.below(10) < 3 {
                0
            } else {
                rng.span(16, 2 * KIB)
            };
            fill(&mut rng, Kind::Text, size as usize, &mut buf);
            let name = format!("{dir}{}{}.txt", rng.pick(&WORDS), d * 100 + i);
            z.start_file(name, zip_opts()).map_err(zip_err)?;
            z.write_all(&buf)?;
        }
    }
    z.finish().map_err(zip_err)?.flush()
}

/// `a2.zip`: `data/` — 10 000 файлов 1–32 КБ в 100 каталогах.
fn gen_a2(p: &Path) -> io::Result<()> {
    let mut z = zip::ZipWriter::new(BufWriter::new(File::create(p)?));
    let mut rng = Rng(2);
    let mut buf = Vec::new();
    z.add_directory("data/", zip_opts()).map_err(zip_err)?;
    for d in 0..100 {
        let dir = format!("data/part {d:02}/");
        z.add_directory(dir.as_str(), zip_opts()).map_err(zip_err)?;
        for i in 0..A2_FILES / 100 {
            let kind = Kind::pick(&mut rng, false);
            let size = rng.span(KIB, 32 * KIB);
            fill(&mut rng, kind, size as usize, &mut buf);
            let ext = kind.ext(&mut rng);
            let name = format!("{dir}{}_{:05}.{ext}", rng.pick(&WORDS), d * 100 + i);
            z.start_file(name, zip_opts()).map_err(zip_err)?;
            z.write_all(&buf)?;
        }
    }
    z.finish().map_err(zip_err)?.flush()
}

/// Дерево `src/data` ≈ 1 ГБ: 8 каталогов по 6 подкаталогов.
fn gen_src(src: &Path) -> io::Result<(usize, u64)> {
    let dir = |i: usize| format!("data/{} {}/sub {}", WORDS[i % 8], i % 8, (i / 8) % 6);
    files(3, GIB, dir, |rel, data| {
        let p = src.join(rel);
        fs::create_dir_all(p.parent().unwrap())?;
        fs::write(p, data)
    })
}

/// `a4.tar.zst` из `src/data`.
fn gen_a4(src: &Path, p: &Path) -> io::Result<()> {
    let enc = zstd::Encoder::new(BufWriter::new(File::create(p)?), 3)?;
    let mut b = tar::Builder::new(enc);
    b.append_dir_all("data", src.join("data"))?;
    b.into_inner()?.finish()?.flush()
}

/// `a5.tar.gz`: 2 ГБ файлов в 20 каталогах корня.
fn gen_a5(p: &Path) -> io::Result<(usize, u64)> {
    let gz = flate2::write::GzEncoder::new(
        BufWriter::new(File::create(p)?),
        flate2::Compression::fast(),
    );
    let mut b = tar::Builder::new(gz);
    let header = |kind: tar::EntryType, size: u64| {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(kind);
        h.set_size(size);
        h.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
        h.set_mtime(1_741_064_768);
        h
    };
    for d in 0..20 {
        let mut h = header(tar::EntryType::Directory, 0);
        b.append_data(&mut h, format!("part {d:02}/"), io::empty())?;
    }
    let made = files(
        5,
        2 * GIB,
        |i| format!("part {:02}", i % 20),
        |rel, data| {
            let mut h = header(tar::EntryType::Regular, data.len() as u64);
            b.append_data(&mut h, rel, data)
        },
    )?;
    b.into_inner()?.finish()?.flush()?;
    Ok(made)
}

/// Внешняя программа: успех или паника с её выводом.
fn exec(cmd: &mut Proc) {
    let out = cmd.output().expect("запуск");
    assert!(
        out.status.success(),
        "{cmd:?}: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout)
    );
}

fn gen_all(dir: &Path, t: &Tools) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let t0 = Instant::now();
    gen_a1(&dir.join("a1.zip"))?;
    eprintln!("a1.zip: {:.1} с", t0.elapsed().as_secs_f64());
    gen_a2(&dir.join("a2.zip"))?;
    eprintln!("a2.zip: {:.1} с", t0.elapsed().as_secs_f64());
    let src = dir.join("src");
    if src.exists() {
        fs::remove_dir_all(&src)?;
    }
    let (n, bytes) = gen_src(&src)?;
    eprintln!("src: {n} файлов, {} МиБ", bytes / MIB);
    for f in ["a3.7z", "a7.rar"] {
        let _ = fs::remove_file(dir.join(f));
    }
    exec(
        Proc::new(&t.sz)
            .args(["a", "-t7z", "-bso0", "-bsp0", r"..\a3.7z", "data"])
            .current_dir(&src),
    );
    eprintln!("a3.7z: {:.1} с", t0.elapsed().as_secs_f64());
    gen_a4(&src, &dir.join("a4.tar.zst"))?;
    eprintln!("a4.tar.zst: {:.1} с", t0.elapsed().as_secs_f64());
    exec(
        Proc::new(&t.rar)
            .args(["a", "-ma5", "-r", "-idq", r"..\a7.rar", "data"])
            .current_dir(&src),
    );
    eprintln!("a7.rar: {:.1} с", t0.elapsed().as_secs_f64());
    fs::remove_dir_all(&src)?;
    let (n, bytes) = gen_a5(&dir.join("a5.tar.gz"))?;
    eprintln!("a5.tar.gz: {n} файлов, {} МиБ", bytes / MIB);
    for f in [
        "a1.zip",
        "a2.zip",
        "a3.7z",
        "a4.tar.zst",
        "a5.tar.gz",
        "a7.rar",
    ] {
        let len = fs::metadata(dir.join(f))?.len();
        eprintln!("{f}: {:.1} МиБ", len as f64 / MIB as f64);
    }
    let (name, block, blocks) = last_in_block(&dir.join("a3.7z"));
    eprintln!(
        "a3.7z: блоков {blocks}; последний файл {name}, его блок {} МиБ",
        block / MIB
    );
    eprintln!("всё: {:.1} с", t0.elapsed().as_secs_f64());
    Ok(())
}

/// Последний файл с данными в `a3.7z` (имя в архиве), распакованный размер его блока и
/// число блоков.
fn last_in_block(arc: &Path) -> (String, u64, usize) {
    let a = sevenz_rust2::Archive::open(arc).expect("a3.7z");
    let (i, e) = a
        .files
        .iter()
        .enumerate()
        .rev()
        .find(|(_, e)| e.has_stream() && !e.is_directory())
        .expect("файл");
    let b = a.stream_map.file_block_index[i].expect("блок");
    (
        e.name().to_owned(),
        a.blocks[b].get_unpack_size(),
        a.blocks.len(),
    )
}

// ---------- замеры в дочернем процессе ----------

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Пик private-памяти процесса, МБ.
fn peak_mib() -> f64 {
    urfin_vfs::diag::peak_memory()
        .and_then(|(_, p)| p)
        .map_or(0.0, |v| v as f64 / MIB as f64)
}

type Sample = BTreeMap<String, f64>;

/// Корень архива: первая порция, весь листинг, записей.
fn list_dir(v: &dyn Vfs, dir: &Location) -> (f64, f64, usize) {
    let t0 = Instant::now();
    let mut s = v.list(dir, &ListOpts::default()).expect("list");
    let mut out = Vec::new();
    let cancel = CancelToken::new();
    let mut first = None;
    loop {
        let more = s.next_batch(&mut out, &cancel).expect("batch");
        if first.is_none() && !out.is_empty() {
            first = Some(ms(t0.elapsed()));
        }
        if !more {
            break;
        }
    }
    (first.unwrap_or(0.0), ms(t0.elapsed()), out.len())
}

fn part_a1(arc: &Path) -> Sample {
    let cfg = ArcCfg::default();
    let v = ArcVfs::new(ZipFormat::new(&cfg), urfin_arc::cache(&cfg));
    let root = root_of("zip", Location::local(arc));
    let (first, all, entries) = list_dir(&v, &root);
    let (_, again, _) = list_dir(&v, &root);
    let sub = Location {
        path: PathBuf::from("/dir 0500"),
        ..root.clone()
    };
    let (_, dir, dir_entries) = list_dir(&v, &sub);
    // Профиль: разбор central directory одним крейтом `zip` — через буфер и без него.
    let t0 = Instant::now();
    let parsed =
        zip::ZipArchive::new(io::BufReader::new(File::open(arc).expect("a1"))).expect("zip");
    let zip_buf = ms(t0.elapsed());
    assert_eq!(parsed.len(), 101_000);
    let t0 = Instant::now();
    zip::ZipArchive::new(File::open(arc).expect("a1")).expect("zip");
    let zip_raw = ms(t0.elapsed());
    let mut s = Sample::new();
    s.insert("zip_buf_ms".into(), zip_buf);
    s.insert("zip_raw_ms".into(), zip_raw);
    s.insert("peak_mib".into(), peak_mib());
    s.insert("first_ms".into(), first);
    s.insert("ms".into(), all);
    s.insert("again_ms".into(), again);
    s.insert("dir_ms".into(), dir);
    s.insert("entries".into(), entries as f64);
    s.insert("dir_entries".into(), dir_entries as f64);
    s
}

fn part_a5(arc: &Path) -> Sample {
    let v = ArcVfs::new(TarFormat::new(), urfin_arc::cache(&ArcCfg::default()));
    let root = root_of("tar", Location::local(arc));
    let t0 = Instant::now();
    let mut s = v.list(&root, &ListOpts::default()).expect("list");
    let mut out = Vec::new();
    let cancel = CancelToken::new();
    while out.is_empty() && s.next_batch(&mut out, &cancel).expect("batch") {}
    let first = ms(t0.elapsed());
    cancel.cancel();
    let mut r = Sample::new();
    r.insert("ms".into(), first);
    r.insert("entries".into(), out.len() as f64);
    r
}

fn part_a6(arc: &Path, name: &str) -> Sample {
    let v = ArcVfs::new(SevenZFormat::new(), urfin_arc::cache(&ArcCfg::default()));
    let root = root_of("7z", Location::local(arc));
    let t0 = Instant::now();
    v.index(&root, &CancelToken::new()).expect("index");
    let index = ms(t0.elapsed());
    let p = Location {
        path: PathBuf::from(format!("/{name}")),
        ..root.clone()
    };
    let t1 = Instant::now();
    let mut r = v.open_read(&p).expect("open_read");
    let mut buf = vec![0u8; 256 << 10];
    let mut ttfb = None;
    let mut bytes = 0u64;
    loop {
        let n = r.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        ttfb.get_or_insert_with(|| ms(t1.elapsed()));
        bytes += n as u64;
    }
    let mut s = Sample::new();
    s.insert("ms".into(), ms(t1.elapsed()));
    s.insert("ttfb_ms".into(), ttfb.unwrap_or(0.0));
    s.insert("index_ms".into(), index);
    s.insert("bytes".into(), bytes as f64);
    s.insert("peak_mib".into(), peak_mib());
    s
}

fn cores() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| u32::try_from(n.get()).unwrap_or(1))
}

/// Сырой `sevenz-rust2`: весь архив в `out` (`a3`) или файл `name` в память (`a6`).
fn part_sevenz(arc: &Path, out: &Path, name: Option<&str>) -> Sample {
    let t0 = Instant::now();
    let mut r =
        sevenz_rust2::ArchiveReader::open(arc, sevenz_rust2::Password::empty()).expect("7z");
    r.set_thread_count(cores());
    let mut bytes = 0u64;
    if let Some(name) = name {
        bytes = r.read_file(name).expect("read_file").len() as u64;
    } else {
        r.for_each_entries(|e, rd| {
            let p = out.join(e.name());
            if e.is_directory() {
                fs::create_dir_all(&p)?;
            } else {
                fs::create_dir_all(p.parent().unwrap())?;
                bytes += io::copy(rd, &mut File::create(&p)?)?;
            }
            Ok(true)
        })
        .expect("for_each_entries");
    }
    let mut s = Sample::new();
    s.insert("ms".into(), ms(t0.elapsed()));
    s.insert("bytes".into(), bytes as f64);
    s.insert("peak_mib".into(), peak_mib());
    s
}

/// Сырой `zesven`: весь архив в `out` или файл `name` в память.
fn part_zesven(arc: &Path, out: &Path, name: Option<&str>) -> Sample {
    let t0 = Instant::now();
    let mut a = zesven::Archive::open_path(arc).expect("zesven");
    let bytes = if let Some(name) = name {
        let want = name.replace('\\', "/");
        let idx = a
            .entries()
            .iter()
            .position(|e| e.path.as_str().replace('\\', "/") == want)
            .expect("файл в zesven");
        a.extract_entry_to_vec_by_index(idx)
            .expect("zesven read")
            .len() as u64
    } else {
        a.extract(out, (), &zesven::ExtractOptions::default())
            .expect("zesven extract")
            .bytes_extracted
    };
    let mut s = Sample::new();
    s.insert("ms".into(), ms(t0.elapsed()));
    s.insert("bytes".into(), bytes as f64);
    s.insert("peak_mib".into(), peak_mib());
    s
}

/// Урфин: панель внутри архива (`scheme`), курсор на `data`, `ops.copy` в `out` — как F5.
/// `enter_ms` — до листинга корня, `copy_ms` — задача копирования, `ms` — всё вместе.
fn part_ops(scheme: &str, arc: &Path, out: &Path) -> Sample {
    let t0 = Instant::now();
    let cfg = ArcCfg::default();
    let cache = urfin_arc::cache(&cfg);
    let mut reg = VfsRegistry::with_local();
    for b in urfin_arc::backends(&cache, &cfg) {
        reg.register(b);
    }
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let left = root_of(scheme, Location::local(arc));
    let mut core = Core::new(
        Boot::new(left, Location::local(out)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, Arc::clone(&reg), VfsCfg::default());
    urfin_arc::register(&mut core, Arc::clone(&reg), Arc::clone(&cache), &cfg);
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
    let pump = |core: &mut Core| {
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    };
    while !core
        .state()
        .active_tab()
        .listing
        .entries
        .iter()
        .any(|e| e.name == "data")
    {
        assert!(t0.elapsed() < Duration::from_mins(10), "листинг архива");
        pump(&mut core);
    }
    core.dispatch(Command::new("panel.cursor.to").arg("name", "data"));
    core.pump(Duration::from_millis(2));
    let enter = ms(t0.elapsed());
    let t1 = Instant::now();
    let dest = format!("{}\\", out.display());
    core.dispatch(Command::new("ops.copy").arg("dest", dest.as_str()));
    let sum: JobSummary = loop {
        let done = core
            .state()
            .jobs
            .iter()
            .find(|j| j.kind == "copy" && !j.state.is_active())
            .map(|j| j.state.clone());
        match done {
            Some(JobState::Done(s)) => break s,
            Some(other) => panic!("copy: {other:?}"),
            None => {}
        }
        assert!(t1.elapsed() < Duration::from_mins(30), "копирование");
        pump(&mut core);
    };
    assert_eq!((sum.errors, sum.skipped), (0, 0), "{sum:?}");
    let copy = ms(t1.elapsed());
    let mut s = Sample::new();
    s.insert("ms".into(), ms(t0.elapsed()));
    s.insert("enter_ms".into(), enter);
    s.insert("copy_ms".into(), copy);
    s.insert("items".into(), sum.items as f64);
    s.insert("peak_mib".into(), peak_mib());
    s
}

/// Профиль Урфина без ops: обход индекса, `extract_plan` на все файлы (`parallel` —
/// `transfer_channels`), поток плана — свой поток, файлы — `File::create` + `io::copy`.
fn part_plan(scheme: &str, arc: &Path, out: &Path) -> Sample {
    let t0 = Instant::now();
    let cfg = ArcCfg::default();
    let cache = urfin_arc::cache(&cfg);
    let v = urfin_arc::backends(&cache, &cfg)
        .into_iter()
        .find(|b| b.scheme() == scheme)
        .expect("схема");
    let root = root_of(scheme, Location::local(arc));
    let cancel = CancelToken::new();
    let mut files = Vec::new();
    let mut rels = Vec::new();
    let mut todo = vec![String::new()];
    while let Some(dir) = todo.pop() {
        let at = Location {
            path: PathBuf::from(if dir.is_empty() { "/" } else { dir.as_str() }),
            ..root.clone()
        };
        let mut st = v.list(&at, &ListOpts::default()).expect("list");
        let mut batch = Vec::new();
        while st.next_batch(&mut batch, &cancel).expect("batch") {}
        for e in batch {
            let rel = format!("{dir}/{}", e.name);
            if e.kind.is_dir_like() {
                fs::create_dir_all(out.join(&rel[1..])).expect("mkdir");
                todo.push(rel);
            } else {
                files.push(Location {
                    path: PathBuf::from(&rel),
                    ..root.clone()
                });
                rels.push(out.join(&rel[1..]));
            }
        }
    }
    let walk = ms(t0.elapsed());
    let parallel = u8::try_from(v.transfer_channels()).unwrap_or(4);
    let streams = v.extract_plan(&files, parallel).expect("extract_plan");
    let n_streams = streams.len();
    let bytes: u64 = std::thread::scope(|sc| {
        let hs: Vec<_> = streams
            .into_iter()
            .map(|mut st| {
                let rels = &rels;
                let cancel = &cancel;
                sc.spawn(move || {
                    let mut b = 0;
                    while let Some((i, mut rd)) = st.next(cancel).expect("next") {
                        let mut f = File::create(&rels[i as usize]).expect("create");
                        b += io::copy(&mut rd, &mut f).expect("copy");
                    }
                    b
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("поток")).sum()
    });
    let mut s = Sample::new();
    s.insert("ms".into(), ms(t0.elapsed()));
    s.insert("walk_ms".into(), walk);
    s.insert("streams".into(), n_streams as f64);
    s.insert("bytes".into(), bytes as f64);
    s.insert("peak_mib".into(), peak_mib());
    s
}

fn part(args: &[String]) -> Sample {
    let a = |i: usize| args.get(i).map(String::as_str).expect("аргументы part");
    let p = |i: usize| PathBuf::from(a(i));
    match a(0) {
        "a1" => part_a1(&p(1)),
        "a5" => part_a5(&p(1)),
        "a6" => part_a6(&p(1), a(2)),
        "ops" => part_ops(a(1), &p(2), &p(3)),
        "plan" => part_plan(a(1), &p(2), &p(3)),
        "sevenz" => part_sevenz(&p(1), &p(2), args.get(3).map(String::as_str)),
        "zesven" => part_zesven(&p(1), &p(2), args.get(3).map(String::as_str)),
        other => panic!("part {other}"),
    }
}

// ---------- прогон ----------

struct Tools {
    sz: PathBuf,
    rar: PathBuf,
    unrar: PathBuf,
    tar: PathBuf,
}

/// Инструмент сценария: имя и замер.
type Tool<'a> = (&'static str, Box<dyn Fn() -> Sample + 'a>);

/// Предел дочернего замера: дольше — процесс снимается, в замере `timeout=1`.
const PART_LIMIT: Duration = Duration::from_mins(10);

fn spawn_part(args: &[&str]) -> Sample {
    let exe = std::env::current_exe().expect("путь бенча");
    let t0 = Instant::now();
    let mut child = Proc::new(exe)
        .arg("part")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("дочерний бенч");
    let mut stdout = child.stdout.take().expect("stdout");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    loop {
        if let Some(st) = child.try_wait().expect("wait") {
            assert!(st.success(), "part {args:?}: {st}");
            break;
        }
        if t0.elapsed() > PART_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("part {args:?}: дольше {} с — снят", PART_LIMIT.as_secs());
            return Sample::from([
                ("ms".to_owned(), ms(t0.elapsed())),
                ("timeout".to_owned(), 1.0),
            ]);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    reader
        .join()
        .expect("stdout")
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.to_owned(), v.trim().parse().ok()?))
        })
        .collect()
}

/// Утилита: время от запуска до выхода, вывод — в `NUL`.
fn timed(cmd: &mut Proc) -> Sample {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let t0 = Instant::now();
    let st = cmd.status().expect("запуск утилиты");
    let t = ms(t0.elapsed());
    assert!(st.success(), "{cmd:?}: {st}");
    Sample::from([("ms".to_owned(), t)])
}

/// Проводник: `Shell.Application.CopyHere` (тот же zipfldr, что «Извлечь» и перетаскивание).
/// Время — снаружи, от запуска PowerShell; если копирование ещё идёт после выхода — до
/// появления всех `want` файлов.
fn explorer(ps1: &Path, zip: &Path, out: &Path, want: usize) -> Sample {
    let t0 = Instant::now();
    let st = Proc::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(ps1)
        .arg(zip)
        .arg(out)
        .stdout(Stdio::null())
        .status()
        .expect("powershell");
    assert!(st.success(), "CopyHere: {st}");
    let exit = ms(t0.elapsed());
    while count(out).0 < want {
        assert!(
            t0.elapsed() < Duration::from_mins(30),
            "CopyHere не дописал"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Sample::from([
        ("ms".to_owned(), ms(t0.elapsed())),
        ("exit_ms".to_owned(), exit),
    ])
}

const EXPLORER_PS1: &str = "param([string]$zip, [string]$out)\r\n\
$sh = New-Object -ComObject Shell.Application\r\n\
$sh.NameSpace($out).CopyHere($sh.NameSpace($zip).Items(), 1556)\r\n";

/// Файлов и байт под `root`.
fn count(root: &Path) -> (usize, u64) {
    let (mut files, mut bytes) = (0, 0);
    let Ok(rd) = fs::read_dir(root) else {
        return (0, 0);
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            let (n, b) = count(&e.path());
            files += n;
            bytes += b;
        } else {
            files += 1;
            bytes += e.metadata().map_or(0, |m| m.len());
        }
    }
    (files, bytes)
}

fn reset(out: &Path) {
    if out.exists() {
        fs::remove_dir_all(out).expect("очистка out");
    }
    fs::create_dir_all(out).expect("out");
}

/// Прогоны сценария: прогрев + `runs`, инструменты чередуются; `out` — каталог извлечения.
fn scenario(
    name: &str,
    runs: usize,
    out: Option<&Path>,
    tools: &[Tool<'_>],
) -> Vec<(&'static str, Vec<Sample>)> {
    let mut res: Vec<(&'static str, Vec<Sample>)> =
        tools.iter().map(|(t, _)| (*t, Vec::new())).collect();
    for r in 0..=runs {
        for (k, (t, f)) in tools.iter().enumerate() {
            if let Some(o) = out {
                reset(o);
            }
            let mut s = f();
            if let Some(o) = out {
                let (n, b) = count(o);
                s.insert("files".into(), n as f64);
                s.insert("out_mib".into(), b as f64 / MIB as f64);
            }
            let line: Vec<String> = s.iter().map(|(k, v)| format!("{k}={v:.1}")).collect();
            let tag = if r == 0 {
                "прогрев"
            } else {
                "прогон"
            };
            eprintln!("{name} {t} {tag} {r}: {}", line.join(" "));
            if r > 0 {
                res[k].1.push(s);
            }
        }
    }
    if let Some(o) = out {
        let _ = fs::remove_dir_all(o);
    }
    res
}

/// Медиана, минимум, максимум.
fn stats(v: &[f64]) -> (f64, f64, f64) {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n == 0 {
        return (0.0, 0.0, 0.0);
    }
    let med = if n % 2 == 1 {
        s[n / 2]
    } else {
        f64::midpoint(s[n / 2 - 1], s[n / 2])
    };
    (med, s[0], s[n - 1])
}

fn json_scenario(res: &[(&str, Vec<Sample>)]) -> String {
    let tools: Vec<String> = res
        .iter()
        .map(|(t, samples)| {
            let mut keys: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
            for s in samples {
                for (k, v) in s {
                    keys.entry(k.as_str()).or_default().push(*v);
                }
            }
            let fields: Vec<String> = keys
                .iter()
                .map(|(k, v)| {
                    let (med, lo, hi) = stats(v);
                    let all: Vec<String> = v.iter().map(|x| format!("{x:.1}")).collect();
                    format!(
                        "\"{k}\":{{\"median\":{med:.1},\"min\":{lo:.1},\"max\":{hi:.1},\"runs\":[{}]}}",
                        all.join(",")
                    )
                })
                .collect();
            format!("\"{t}\":{{{}}}", fields.join(","))
        })
        .collect();
    format!("{{{}}}", tools.join(","))
}

/// Строка сводки: медиана `ms` и разброс у каждого инструмента.
fn summary(name: &str, res: &[(&str, Vec<Sample>)]) -> String {
    let cells: Vec<String> = res
        .iter()
        .map(|(t, samples)| {
            let v: Vec<f64> = samples
                .iter()
                .filter_map(|s| s.get("ms").copied())
                .collect();
            let (med, lo, hi) = stats(&v);
            format!("{t} {med:.0} мс ({lo:.0}–{hi:.0})")
        })
        .collect();
    format!("{name}: {}", cells.join("; "))
}

fn run(
    dir: &Path,
    t: &Tools,
    (runs, big): (usize, usize),
    only: Option<&[String]>,
    zesven_a3: bool,
) -> String {
    let want = |s: &str| only.is_none_or(|o| o.iter().any(|x| x == s));
    let out = dir.join("out");
    let path = |f: &str| dir.join(f);
    let s = |p: &Path| p.display().to_string();
    let mut json = Vec::new();
    let mut lines = Vec::new();
    let mut put = |name: &str, res: Vec<(&'static str, Vec<Sample>)>| {
        lines.push(summary(name, &res));
        eprintln!("{}", lines.last().unwrap());
        json.push(format!("\"{name}\":{}", json_scenario(&res)));
    };
    let arg_out = format!("-o{}", out.display());
    if want("a1") {
        let a = s(&path("a1.zip"));
        let tools: Vec<Tool<'_>> = vec![
            ("urfin", Box::new(|| spawn_part(&["a1", &a]))),
            ("7z l", Box::new(|| timed(Proc::new(&t.sz).args(["l", &a])))),
        ];
        put("A1", scenario("A1", runs, None, &tools));
    }
    if want("a2") {
        let a = s(&path("a2.zip"));
        let o = s(&out);
        let ps1 = dir.join("explorer.ps1");
        fs::write(&ps1, EXPLORER_PS1).expect("ps1");
        let tools: Vec<Tool<'_>> = vec![
            ("urfin", Box::new(|| spawn_part(&["ops", "zip", &a, &o]))),
            (
                "urfin plan",
                Box::new(|| spawn_part(&["plan", "zip", &a, &o])),
            ),
            (
                "7z x",
                Box::new(|| {
                    timed(Proc::new(&t.sz).args(["x", "-y", "-bso0", "-bsp0", &arg_out, &a]))
                }),
            ),
            (
                "explorer",
                Box::new(|| explorer(&ps1, &path("a2.zip"), &out, A2_FILES)),
            ),
        ];
        put("A2", scenario("A2", runs, Some(&out), &tools));
    }
    if want("a5") {
        let a = s(&path("a5.tar.gz"));
        let tools: Vec<Tool<'_>> = vec![("urfin", Box::new(|| spawn_part(&["a5", &a])))];
        put("A5", scenario("A5", runs, None, &tools));
    }
    if want("a6") {
        let a = s(&path("a3.7z"));
        let (name, block, blocks) = last_in_block(&path("a3.7z"));
        eprintln!("A6: блоков {blocks}, файл {name}, блок {} МиБ", block / MIB);
        let win = name.replace('/', "\\");
        let o = s(&out);
        let tools: Vec<Tool<'_>> = vec![
            ("urfin", Box::new(|| spawn_part(&["a6", &a, &name]))),
            (
                "7z e -so",
                Box::new(|| timed(Proc::new(&t.sz).args(["e", "-so", &a, &win]))),
            ),
            ("7z t", Box::new(|| timed(Proc::new(&t.sz).args(["t", &a])))),
            (
                "sevenz-rust2",
                Box::new(|| spawn_part(&["sevenz", &a, &o, &name])),
            ),
            (
                "zesven",
                Box::new(|| spawn_part(&["zesven", &a, &o, &name])),
            ),
        ];
        put("A6", scenario("A6", runs, None, &tools));
    }
    if want("a3") {
        let a = s(&path("a3.7z"));
        let o = s(&out);
        let mut tools: Vec<Tool<'_>> = vec![
            ("urfin", Box::new(|| spawn_part(&["ops", "7z", &a, &o]))),
            (
                "urfin plan",
                Box::new(|| spawn_part(&["plan", "7z", &a, &o])),
            ),
            (
                "7z x",
                Box::new(|| {
                    timed(Proc::new(&t.sz).args(["x", "-y", "-bso0", "-bsp0", &arg_out, &a]))
                }),
            ),
            ("sevenz-rust2", Box::new(|| spawn_part(&["sevenz", &a, &o]))),
        ];
        // `zesven::Archive::extract` на solid-архиве квадратичен: каждый файл читает блок
        // в память и распаковывает его с начала (`extract_from_solid_block`); на `a3.7z`
        // не кончил за 3,5 ч. Только по `--zesven-a3`, под пределом `PART_LIMIT`.
        if zesven_a3 {
            tools.push(("zesven", Box::new(|| spawn_part(&["zesven", &a, &o]))));
        }
        put("A3", scenario("A3", big, Some(&out), &tools));
    }
    if want("a4") {
        let a = s(&path("a4.tar.zst"));
        let o = s(&out);
        let tools: Vec<Tool<'_>> = vec![
            ("urfin", Box::new(|| spawn_part(&["ops", "tar", &a, &o]))),
            (
                "urfin plan",
                Box::new(|| spawn_part(&["plan", "tar", &a, &o])),
            ),
            (
                "tar --zstd -x",
                Box::new(|| timed(Proc::new(&t.tar).args(["--zstd", "-xf", &a, "-C", &o]))),
            ),
        ];
        put("A4", scenario("A4", big, Some(&out), &tools));
    }
    if want("a7") {
        let a = s(&path("a7.rar"));
        let o = s(&out);
        let dest = format!("{o}\\");
        let mut tools: Vec<Tool<'_>> = Vec::new();
        if cfg!(feature = "rar") {
            tools.push(("urfin", Box::new(|| spawn_part(&["ops", "rar", &a, &o]))));
            tools.push((
                "urfin plan",
                Box::new(|| spawn_part(&["plan", "rar", &a, &o])),
            ));
        } else {
            eprintln!("A7: Урфин пропущен — пример собран без `--features rar`");
        }
        tools.push((
            "unrar x",
            Box::new(|| timed(Proc::new(&t.unrar).args(["x", "-y", "-idq", &a, &dest]))),
        ));
        put("A7", scenario("A7", big, Some(&out), &tools));
    }
    for l in &lines {
        eprintln!("{l}");
    }
    format!(
        "{{\"dir\":{:?},\"runs\":{runs},\"big_runs\":{big},\"cpus\":{},{}}}",
        dir.display().to_string(),
        cores(),
        json.join(",")
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();
    if cmd == "part" {
        let s = part(&rest);
        let mut o = io::stdout().lock();
        for (k, v) in &s {
            writeln!(o, "{k}={v}").expect("stdout");
        }
        o.flush().expect("stdout");
        // Поток листинга A5 ещё читает архив: выход, не дожидаясь его.
        std::process::exit(0);
    }
    let mut tools = Tools {
        sz: PathBuf::from(r"C:\Program Files\7-Zip\7z.exe"),
        rar: PathBuf::from(r"C:\Program Files\WinRAR\Rar.exe"),
        unrar: PathBuf::from(r"C:\Program Files\WinRAR\UnRAR.exe"),
        tar: PathBuf::from(r"C:\Windows\System32\tar.exe"),
    };
    let (mut runs, mut big, mut only, mut json, mut dir) = (5, 3, None, None, None);
    let mut zesven_a3 = false;
    let mut it = rest.into_iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{a}: нет значения"));
        match a.as_str() {
            "--runs" => runs = value().parse().expect("--runs <n>"),
            "--big-runs" => big = value().parse().expect("--big-runs <n>"),
            "--only" => only = Some(value().split(',').map(str::to_owned).collect::<Vec<_>>()),
            "--json" => json = Some(PathBuf::from(value())),
            "--7z" => tools.sz = PathBuf::from(value()),
            "--rar" => tools.rar = PathBuf::from(value()),
            "--unrar" => tools.unrar = PathBuf::from(value()),
            "--tar" => tools.tar = PathBuf::from(value()),
            "--zesven-a3" => zesven_a3 = true,
            _ => dir = Some(PathBuf::from(&a)),
        }
    }
    let dir = std::path::absolute(dir.expect("каталог данных")).expect("путь");
    match cmd.as_str() {
        "gen" => gen_all(&dir, &tools).expect("gen"),
        "run" => {
            let out = run(&dir, &tools, (runs, big), only.as_deref(), zesven_a3);
            println!("{out}");
            if let Some(p) = json {
                fs::write(&p, format!("{out}\n")).expect("json");
            }
        }
        _ => panic!("gen <каталог> | run <каталог> [--runs N] [--big-runs N] [--only a1,a2]"),
    }
}
