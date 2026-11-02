//! Досчёт с правами администратора (F01-12; ADR-0021, решение владельца 2026-11-02
//! `docs/decisions/2026-11-02-du-elevated-same-exe.md`).
//!
//! Тот же `urfin.exe` с флагом [`FLAG`] запускается через UAC (`runas`): считает итоги
//! недоступных папок тем же обходом [`super::scan`] (только чтение метаданных), пишет
//! один файл результата и выходит. Параметры — только путь файла результата и корни.
//! Файл лежит в `state\du\elevated\` Урфина (не в общем `%TEMP%`), имя —
//! `du-<nonce>.dures`; создаётся только новым и только по итоговому пути (ссылки в
//! пути — отказ, `crate::sys::win_elev`).
//!
//! Обычный процесс ([`collect`], задача `du.elevate`) готовит каталог и имя, запускает
//! через [`Elevator`] (`runas` — `urfin_ui::sys`), ждёт выхода (отмена — файл
//! `*.cancel` и снятие процесса), читает файл и проверяет его ([`decode`]: формат,
//! версия, контрольная сумма, nonce, пути — только запрошенные корни). В снимок
//! попадают итоги корня — размер, число файлов и каталогов, суммы по типам; имён
//! внутри недоступных папок нет ни в файле, ни в снимке, ни в кэше.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use urfin_core::Location;
use xxhash_rust::xxh3::xxh3_64;

use super::model::{DuSnapshot, DuTree, DuType};
use super::scan::{ScanCfg, scan};

/// Флаг повышенного режима: `urfin.exe --du-elevated --out <файл> <корень>…`.
pub const FLAG: &str = "--du-elevated";
/// Флаг пути файла результата.
pub const OUT: &str = "--out";
/// Корней за один запуск не больше.
pub const MAX_ROOTS: usize = 64;
/// Файл результата не длиннее.
pub const MAX_RESULT: u64 = 1 << 20;
/// Версия формата файла результата.
pub const VERSION: u16 = 1;
/// Расширение файла результата.
pub const EXT: &str = "dures";
/// Каталог файлов результата внутри `state\du\`.
pub const SUBDIR: &str = "elevated";
const MAGIC: &[u8; 8] = b"URFDUE\0\0";
const MAX_PATH: usize = 32_767;
/// Командная строка Windows — до 32 767 символов: корни сверх — следующим запуском.
const MAX_ARGS: usize = 30_000;
const CANCEL_POLL: Duration = Duration::from_millis(200);
const WAIT_STEP: Duration = Duration::from_millis(100);
/// Брошенные файлы результата (Урфин закрыт посреди досчёта) старше — удаляются.
const STALE: Duration = Duration::from_secs(3600);

/// Коды выхода повышенного режима.
pub mod exit {
    pub const OK: u8 = 0;
    /// Аргументы не прошли проверку.
    pub const BAD_ARGS: u8 = 2;
    /// Досчёт запрещён политикой (`DuElevatedHelper` = 0).
    pub const POLICY: u8 = 3;
    /// Отменён (файл `*.cancel`).
    pub const CANCELLED: u8 = 4;
    /// Файл результата не записан.
    pub const WRITE: u8 = 5;
}

/// Ошибка аргументов или файла результата.
#[derive(Debug, thiserror::Error)]
pub enum ElevError {
    #[error("bad argument: {0}")]
    Args(&'static str),
    #[error("bad result: {0}")]
    Result(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Запрос повышенному процессу: файл результата и корни.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub out: PathBuf,
    pub roots: Vec<PathBuf>,
}

impl Request {
    /// Аргументы `urfin.exe` (без имени exe).
    pub fn args(&self) -> Vec<OsString> {
        let mut a = vec![
            OsString::from(FLAG),
            OsString::from(OUT),
            self.out.clone().into_os_string(),
        ];
        a.extend(self.roots.iter().map(|r| r.clone().into_os_string()));
        a
    }

    /// Разбор `--du-elevated --out <файл> <корень>…` (без имени exe) с проверкой путей:
    /// корни — абсолютные пути на дисках с буквой (не UNC, не `\\?\`, без `..` и потоков
    /// `:`), не больше [`MAX_ROOTS`], без повторов; файл — `…\du\elevated\du-<nonce>.dures`.
    pub fn parse(args: &[OsString]) -> Result<Request, ElevError> {
        let mut it = args.iter().map(OsString::as_os_str);
        if it.next() != Some(OsStr::new(FLAG)) {
            return Err(ElevError::Args("flag"));
        }
        if it.next() != Some(OsStr::new(OUT)) {
            return Err(ElevError::Args("--out"));
        }
        let out = PathBuf::from(it.next().ok_or(ElevError::Args("out"))?);
        check_out(&out)?;
        let roots: Vec<PathBuf> = it.map(PathBuf::from).collect();
        if roots.is_empty() || roots.len() > MAX_ROOTS {
            return Err(ElevError::Args("roots count"));
        }
        let mut seen = FxHashSet::default();
        for r in &roots {
            let s = r.to_str().ok_or(ElevError::Args("root"))?;
            if !drive_path(s) {
                return Err(ElevError::Args("root"));
            }
            if !seen.insert(s.to_lowercase()) {
                return Err(ElevError::Args("duplicate root"));
            }
        }
        Ok(Request { out, roots })
    }

    /// Nonce из имени файла результата (`du-<nonce>.dures`).
    pub fn nonce(&self) -> &str {
        self.out
            .file_stem()
            .and_then(OsStr::to_str)
            .and_then(|s| s.strip_prefix("du-"))
            .unwrap_or("")
    }
}

/// Абсолютный путь на диске с буквой: `C:\` или `C:\a\b` — без пустых звеньев, `.` и
/// `..`, `/`, `:` (потоки), шаблонов и управляющих символов.
pub fn drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    if s.len() > MAX_PATH || b.len() < 3 || !b[0].is_ascii_alphabetic() || &b[1..3] != b":\\" {
        return false;
    }
    let rest = &s[3..];
    rest.is_empty()
        || (!rest.contains([':', '/', '*', '?', '"', '<', '>', '|'])
            && !rest.chars().any(char::is_control)
            && rest
                .split('\\')
                .all(|c| !c.is_empty() && c != "." && c != ".."))
}

fn is_nonce(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Файл результата: `…\du\elevated\du-<32 hex>.dures` на диске с буквой.
fn check_out(p: &Path) -> Result<(), ElevError> {
    let bad = || ElevError::Args("out");
    let s = p.to_str().ok_or_else(bad)?;
    if !drive_path(s) {
        return Err(bad());
    }
    let name = p.file_name().and_then(OsStr::to_str).ok_or_else(bad)?;
    let nonce = name
        .strip_prefix("du-")
        .and_then(|n| n.strip_suffix(&format!(".{EXT}")))
        .ok_or_else(bad)?;
    let dir = p.parent().ok_or_else(bad)?;
    let named = |d: &Path, want: &str| {
        d.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|n| n.eq_ignore_ascii_case(want))
    };
    let up = dir.parent().ok_or_else(bad)?;
    if !is_nonce(nonce) || !named(dir, SUBDIR) || !named(up, "du") {
        return Err(bad());
    }
    Ok(())
}

/// Файл результата с `nonce` в каталоге `dir`.
pub fn out_path(dir: &Path, nonce: &str) -> PathBuf {
    dir.join(format!("du-{nonce}.{EXT}"))
}

/// Знак отмены рядом с файлом результата: повышенный процесс проверяет его и выходит.
pub fn cancel_path(out: &Path) -> PathBuf {
    out.with_extension("cancel")
}

/// Новый nonce: 128 бит, 32 hex. Не секрет — связь ответа с запросом.
pub fn new_nonce() -> String {
    use std::fmt::Write as _;
    use std::hash::{BuildHasher, Hasher};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut out = String::with_capacity(32);
    for k in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(k);
        h.write_u64(SEQ.fetch_add(1, Ordering::Relaxed));
        h.write_u128(now);
        h.write_u32(std::process::id());
        let _ = write!(out, "{:016x}", h.finish());
    }
    out
}

/// Итоги одного корня.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootSum {
    pub path: String,
    /// Корень прочитан целиком (каталог, не ссылка, обход не прерван).
    pub ok: bool,
    pub disk: u64,
    pub logical: u64,
    pub files: u64,
    /// Каталогов внутри (без корня).
    pub dirs: u64,
    /// Каталогов, недоступных и с правами (без `SeBackupPrivilege`, чужой ACL).
    pub denied: u32,
    pub types_disk: [u64; DuType::COUNT],
    pub types_files: [u64; DuType::COUNT],
}

/// Ответ повышенного процесса.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub nonce: String,
    pub roots: Vec<RootSum>,
}

/// Файл результата: magic `URFDUE\0\0` · `ver: u16` · postcard [`Answer`] · `xxh3_64` всего
/// предыдущего (little-endian).
pub fn encode(a: &Answer) -> Vec<u8> {
    let mut b = MAGIC.to_vec();
    b.extend_from_slice(&VERSION.to_le_bytes());
    // Сериализация простых полей в память не отказывает.
    b.extend(postcard::to_stdvec(a).unwrap_or_default());
    let sum = xxh3_64(&b);
    b.extend_from_slice(&sum.to_le_bytes());
    b
}

/// Разбор и проверка файла результата для запроса `req`: длина, magic, версия,
/// контрольная сумма, nonce запроса; каждый корень — один из запрошенных, без повторов.
pub fn decode(b: &[u8], req: &Request) -> Result<Answer, ElevError> {
    let head = MAGIC.len() + 2;
    if b.len() as u64 > MAX_RESULT {
        return Err(ElevError::Result("too long"));
    }
    if b.len() < head + 8 {
        return Err(ElevError::Result("short"));
    }
    if &b[..MAGIC.len()] != MAGIC {
        return Err(ElevError::Result("magic"));
    }
    if u16::from_le_bytes([b[8], b[9]]) != VERSION {
        return Err(ElevError::Result("version"));
    }
    let end = b.len() - 8;
    let mut sum = [0u8; 8];
    sum.copy_from_slice(&b[end..]);
    if xxh3_64(&b[..end]) != u64::from_le_bytes(sum) {
        return Err(ElevError::Result("checksum"));
    }
    let (a, rest): (Answer, _) =
        postcard::take_from_bytes(&b[head..end]).map_err(|_| ElevError::Result("body"))?;
    if !rest.is_empty() {
        return Err(ElevError::Result("trailing bytes"));
    }
    if a.nonce != req.nonce() {
        return Err(ElevError::Result("nonce"));
    }
    if a.roots.len() > req.roots.len() {
        return Err(ElevError::Result("roots count"));
    }
    let asked: FxHashSet<String> = req
        .roots
        .iter()
        .map(|r| r.to_string_lossy().to_lowercase())
        .collect();
    let mut seen = FxHashSet::default();
    for r in &a.roots {
        let key = r.path.to_lowercase();
        if !asked.contains(&key) {
            return Err(ElevError::Result("root not requested"));
        }
        if !seen.insert(key) {
            return Err(ElevError::Result("duplicate root"));
        }
    }
    Ok(a)
}

/// Повышенный режим целиком: разбор, политика, укрепление процесса, обход, файл
/// результата; код выхода — [`exit`]. Только для процесса `urfin.exe --du-elevated`:
/// токен и политика процесса меняются необратимо. `allowed` — политика
/// `DuElevatedHelper`, перечитанная этим процессом.
pub fn main(args: &[OsString], allowed: bool) -> u8 {
    let Ok(req) = Request::parse(args) else {
        return exit::BAD_ARGS;
    };
    if !allowed {
        return exit::POLICY;
    }
    harden();
    serve(&req)
}

/// Привилегии: Backup — включить, прочие — удалить; дочерние процессы — запрет. Ошибки
/// не останавливают: режим и без них только читает.
fn harden() {
    #[cfg(windows)]
    {
        let _ = crate::sys::win_elev::no_child_processes();
        let _ = crate::sys::win_elev::restrict_privileges();
    }
}

/// Обход корней и файл результата — без укрепления процесса (тесты: тот же код без
/// `runas`). Отмена — появился [`cancel_path`]: код [`exit::CANCELLED`], файла нет.
pub fn serve(req: &Request) -> u8 {
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let watch = {
        let (stop, done, mark) = (Arc::clone(&stop), Arc::clone(&done), cancel_path(&req.out));
        std::thread::Builder::new()
            .name("du-elevated-cancel".into())
            .spawn(move || {
                while !done.load(Ordering::Acquire) {
                    if mark.exists() {
                        stop.store(true, Ordering::Release);
                        return;
                    }
                    std::thread::sleep(CANCEL_POLL);
                }
            })
            .ok()
    };
    let halted = || stop.load(Ordering::Acquire);
    let answer = run(req, &halted);
    done.store(true, Ordering::Release);
    if let Some(w) = watch {
        let _ = w.join();
    }
    if halted() {
        return exit::CANCELLED;
    }
    match write_new(&req.out, &encode(&answer)) {
        Ok(()) => exit::OK,
        Err(_) => exit::WRITE,
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        crate::sys::win_elev::write_new(path, bytes)
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        f.write_all(bytes)
    }
}

/// Итоги корней запроса (останов — недосчитанные не попадают).
pub fn run(req: &Request, halted: &(dyn Fn() -> bool + Sync)) -> Answer {
    let mut roots = Vec::with_capacity(req.roots.len());
    for r in &req.roots {
        if halted() {
            break;
        }
        roots.push(sum_root(r, halted));
    }
    Answer {
        nonce: req.nonce().to_owned(),
        roots,
    }
}

fn is_reparse(md: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        md.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        md.file_type().is_symlink()
    }
}

/// Итоги одного корня обходом анализа места; крупные файлы не собираются (порог —
/// максимум), имена остаются в памяти этого процесса.
fn sum_root(root: &Path, halted: &(dyn Fn() -> bool + Sync)) -> RootSum {
    let path = root.to_string_lossy().into_owned();
    let bad = RootSum {
        path: path.clone(),
        ..RootSum::default()
    };
    match std::fs::symlink_metadata(root) {
        Ok(md) if md.is_dir() && !is_reparse(&md) => {}
        _ => return bad,
    }
    let tree = DuTree::new();
    let cfg = ScanCfg {
        track_min: u64::MAX,
        ..ScanCfg::default()
    };
    let fs = crate::LocalFs::new();
    let out = scan(
        &Location::local(root),
        &fs,
        &cfg,
        &tree,
        halted,
        None,
        |_| {},
    );
    let s = &out.snapshot;
    let Some(n) = s.dirs.first().copied() else {
        return bad;
    };
    RootSum {
        path,
        ok: s.flags & DuSnapshot::PARTIAL == 0 && !s.denied.contains(&0) && !halted(),
        disk: n.disk,
        logical: n.logical,
        files: u64::from(n.files),
        dirs: s.dirs.len().saturating_sub(1) as u64,
        denied: u32::try_from(s.denied.len()).unwrap_or(u32::MAX),
        types_disk: s.types.disk,
        types_files: s.types.files,
    }
}

/// Отказ в запуске.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchError {
    /// Пользователь отказал в окне UAC (`ERROR_CANCELLED`).
    Declined,
    Failed(String),
}

/// Запуск `urfin.exe` с повышением (`runas` — `urfin_ui::sys`; тесты — подставка).
pub trait Elevator: Send + Sync {
    /// Запустить `urfin.exe <args>`; блокирует до решения в окне UAC.
    fn launch(&self, args: &[OsString]) -> Result<Box<dyn Elevated>, LaunchError>;
}

/// Запущенный повышенный процесс.
pub trait Elevated: Send {
    /// Ждать выхода не дольше `t`: код выхода или `None` — ещё работает.
    fn wait(&mut self, t: Duration) -> Option<u32>;
    /// Снять процесс (отмена). Права могут не позволить — тогда процесс выйдет по
    /// знаку отмены.
    fn kill(&mut self);
}

/// Досчёт с правами: чем запускать и куда писать ответ (`state\du\elevated\`).
#[derive(Clone)]
pub struct Elevation {
    pub runner: Arc<dyn Elevator>,
    pub dir: PathBuf,
}

impl std::fmt::Debug for Elevation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Elevation")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Elevation {
    /// Файлы результата — в `<state_dir>\du\elevated\`.
    pub fn new(runner: Arc<dyn Elevator>, state_dir: &Path) -> Elevation {
        Elevation {
            runner,
            dir: state_dir.join("du").join(SUBDIR),
        }
    }
}

/// Почему досчёт не удался.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// Повышенный процесс вышел с кодом (см. [`exit`]).
    Exit(u32),
    /// Каталог, запуск или чтение файла.
    Io(String),
    /// Файл результата не прошёл проверку.
    Result(String),
}

/// Итог досчёта.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done(Answer),
    /// Отказ в окне UAC: ничего не досчитано.
    Declined,
    Cancelled,
    Failed(Fail),
}

/// `\\?\C:\…` → `C:\…` (итог `canonicalize`); прочее — как есть.
fn plain(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if drive_path(rest) => PathBuf::from(rest),
        _ => p,
    }
}

/// Брошенные файлы результата и знаки отмены старше часа.
fn sweep(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in rd.flatten() {
        let name = e.file_name();
        let ours = name.to_str().is_some_and(|n| {
            n.starts_with("du-") && (n.ends_with(&format!(".{EXT}")) || n.ends_with(".cancel"))
        });
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).is_ok_and(|d| d > STALE));
        if ours && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Досчёт `roots` (в обычном процессе, поток задачи): каталог ответа, запуск через UAC,
/// ожидание выхода (`halted` — отмена: знак отмены и снятие процесса), чтение и
/// проверка ответа. `launched` — UAC пройден, процесс считает.
pub fn collect(
    el: &Elevation,
    roots: Vec<PathBuf>,
    halted: &dyn Fn() -> bool,
    launched: &dyn Fn(),
) -> Outcome {
    if let Err(e) = std::fs::create_dir_all(&el.dir) {
        return Outcome::Failed(Fail::Io(e.to_string()));
    }
    // Итоговый путь каталога: короткие имена и законные ссылки раскрыты заранее —
    // повышенный процесс сверяет с ним путь созданного файла.
    let dir = match std::fs::canonicalize(&el.dir) {
        Ok(d) => plain(d),
        Err(e) => return Outcome::Failed(Fail::Io(e.to_string())),
    };
    sweep(&dir);
    let req = Request {
        out: out_path(&dir, &new_nonce()),
        roots,
    };
    let mut proc = match el.runner.launch(&req.args()) {
        Ok(p) => p,
        Err(LaunchError::Declined) => return Outcome::Declined,
        Err(LaunchError::Failed(e)) => return Outcome::Failed(Fail::Io(e)),
    };
    launched();
    let code = loop {
        if halted() {
            let _ = std::fs::write(cancel_path(&req.out), b"");
            proc.kill();
            return Outcome::Cancelled;
        }
        if let Some(c) = proc.wait(WAIT_STEP) {
            break c;
        }
    };
    if code != u32::from(exit::OK) {
        let _ = std::fs::remove_file(&req.out);
        return Outcome::Failed(Fail::Exit(code));
    }
    let read = (|| -> Result<Answer, ElevError> {
        let mut b = Vec::new();
        std::fs::File::open(&req.out)?
            .take(MAX_RESULT + 1)
            .read_to_end(&mut b)?;
        decode(&b, &req)
    })();
    let _ = std::fs::remove_file(&req.out);
    match read {
        Ok(a) => Outcome::Done(a),
        Err(ElevError::Io(e)) => Outcome::Failed(Fail::Io(e.to_string())),
        Err(e) => Outcome::Failed(Fail::Result(e.to_string())),
    }
}

/// Ещё один корень `next` к `paths` укладывается в [`MAX_ROOTS`] и длину командной
/// строки (с кавычками и пробелом).
pub fn fits(paths: &[PathBuf], next: &Path) -> bool {
    let len: usize =
        paths.iter().map(|p| p.as_os_str().len() + 3).sum::<usize>() + next.as_os_str().len() + 3;
    paths.len() < MAX_ROOTS && len <= MAX_ARGS
}

#[cfg(test)]
#[path = "elevated_tests.rs"]
mod tests;
