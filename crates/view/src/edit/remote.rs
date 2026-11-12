//! F3/F4 удалённых файлов (F03 §4.3, F03-08): правка файла схемы с записью (`sftp://`).
//!
//! Документ редактора — временная копия на диске
//! `<temp>\<схема>\<сервер>\<hash(адрес)>\<имя>` (`temp` — `%LOCALAPPDATA%\Urfin\temp`):
//! скачивание задачей (больше 1 `МиБ` — в job-баре), затем обычное открытие и сохранение
//! локального файла. После сохранения (F2) копия закачивается обратно — только методами
//! `Vfs` схемы (решение 2026-11-04 «vfs-write»): `<имя>.urfin-part` рядом с целью,
//! `finish` (`fsync@openssh.com`, если есть), права файла, `rename` поверх
//! (`posix-rename`, иначе — решает backend). Перед закачкой — сверка размера и mtime на
//! сервере с отметкой скачивания: изменился — строка конфликта. Обрыв связи — «ждёт
//! связи» и повтор, когда сеанс вернулся. Временная папка удаляется при закрытии
//! документа, остатки старше 7 дней — при старте.
//!
//! Вьювер (F3) временной копии не делает: страницы читает источник `Seek` по запросу
//! (S7 §0), большой файл с сервера целиком не тянется.

use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use urfin_core::{CancelToken, EntryKind, Location};
use urfin_vfs::{Caps, Link, Metadata, Vfs, VfsError, VfsRegistry};

use crate::inbox::After;

/// Суффикс недописанного файла на сервере (F03 §4.2).
pub const PART: &str = ".urfin-part";
/// Скачивание больше — задачей в job-баре (F03 §4.3 п. 1).
pub const FETCH_JOB_MIN: u64 = 1 << 20;
/// Остатки временных папок старше — удаляются при старте (F03 §4.3 п. 5).
pub const KEEP: Duration = Duration::from_hours(7 * 24);
/// Буфер передачи.
const BUF: usize = 256 << 10;
/// Ссылок подряд при поиске цели — не больше.
const LINK_HOPS: usize = 8;
/// Шаг ожидания связи.
const LINK_POLL: Duration = Duration::from_millis(100);
/// Обрыв виден не сразу: столько ждём, что канал сочтут потерянным.
const LINK_GRACE: Duration = Duration::from_millis(500);
/// Прогресс в UI — не чаще.
const TICK: Duration = Duration::from_millis(100);

/// Отметка файла на сервере: размер и mtime (нс Unix; у SFTP — точность секунды).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteStamp {
    pub size: u64,
    pub mtime: i64,
}

impl RemoteStamp {
    fn of(m: &Metadata) -> RemoteStamp {
        RemoteStamp {
            size: m.size,
            mtime: m.mtime,
        }
    }
}

/// Состояние строки «Закачка».
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpState {
    Running,
    /// Связь потеряна, сеанс переподключается: закачка повторится.
    Waiting,
    Done,
    /// Не закачано: текст ошибки.
    Failed(String),
}

/// Закачка сохранённой копии (строка над статусом, `screen-sftpedit`).
#[derive(Clone, Debug)]
pub struct Upload {
    pub state: UpState,
    pub done: u64,
    pub total: u64,
    /// Сколько шла (для «готово · 2,3 КБ · 0,2 с»).
    pub elapsed: Duration,
    pub(crate) gen_: u32,
    pub(crate) cancel: CancelToken,
}

impl Upload {
    /// Доля для полосы.
    pub fn frac(&self) -> f32 {
        if self.state == UpState::Done {
            return 1.0;
        }
        #[allow(clippy::cast_precision_loss, reason = "доля")]
        let f = self.done as f32 / self.total.max(1) as f32;
        f.clamp(0.0, 1.0)
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, UpState::Running | UpState::Waiting)
    }
}

/// Документ — копия файла схемы с записью (`EditorDoc::remote`).
#[derive(Debug)]
pub struct Remote {
    /// Адрес файла, как открыт (`sftp://srv1/etc/nginx/app.conf`): шапка, история, чип.
    pub loc: Location,
    /// Куда пишется: сам файл или цель ссылки.
    pub target: Location,
    /// Временная папка копии.
    pub dir: PathBuf,
    /// Отметка на сервере после скачивания или последней закачки; `None` — файла не было
    /// (новый файл) или ещё скачивается.
    pub stamp: Option<RemoteStamp>,
    /// Права файла на сервере: сохраняются при закачке.
    pub mode: Option<u32>,
    /// Идёт скачивание.
    pub fetching: bool,
    /// Копия сохранена, но не закачана (конфликт отменён, ошибка, отмена).
    pub unsynced: bool,
    pub up: Option<Upload>,
    /// Что сделать после закачки (закрыть документ — `SaveClose`).
    pub(crate) then: Option<After>,
    pub(crate) up_gen: u32,
}

impl Remote {
    /// Новая копия адреса `loc` в папке `dir` (ещё не скачана).
    pub fn new(loc: Location, dir: PathBuf) -> Remote {
        Remote {
            target: loc.clone(),
            loc,
            dir,
            stamp: None,
            mode: None,
            fetching: true,
            unsynced: false,
            up: None,
            then: None,
            up_gen: 0,
        }
    }

    /// Подпись чипа у таба и строк: сервер (профиль `sftp`).
    pub fn label(&self) -> String {
        label_of(&self.loc).unwrap_or_default()
    }

    /// Закачка идёт или ждёт связи.
    pub fn uploading(&self) -> bool {
        self.up.as_ref().is_some_and(Upload::is_active)
    }
}

/// Подпись сервера адреса для чипа таба: `authority` не-локальной схемы вне архива.
pub fn label_of(loc: &Location) -> Option<String> {
    if loc.is_local() || loc.outer.is_some() {
        return None;
    }
    loc.authority
        .as_deref()
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
}

/// Файл схемы правится через временную копию: не локальный, не внутри архива, схема
/// пишет (без `READ_ONLY` и `VIRTUAL`).
pub fn editable(reg: &VfsRegistry, loc: &Location) -> bool {
    if loc.is_local() || loc.outer.is_some() {
        return false;
    }
    reg.get(loc).is_ok_and(|v| {
        let c = v.caps(loc);
        !c.has(Caps::READ_ONLY) && !c.has(Caps::VIRTUAL)
    })
}

/// Временная папка копии: `<root>\<схема>\<сервер>\<hash(адрес)>`.
pub fn temp_dir(root: &Path, loc: &Location) -> PathBuf {
    let mut h = rustc_hash::FxHasher::default();
    loc.to_uri().hash(&mut h);
    let server = loc
        .authority
        .as_deref()
        .map_or_else(String::new, local_name);
    root.join(local_name(&loc.backend))
        .join(if server.is_empty() { "_" } else { &server })
        .join(format!("{:016x}", h.finish()))
}

/// Имя файла, допустимое в Windows: запрещённые знаки — `_`, точки и пробелы в конце
/// убираются, зарезервированные имена (`CON`, `NUL`, `COM1`…) — с `_` впереди.
pub fn local_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if u32::from(c) < 0x20 => '_',
            c => c,
        })
        .collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    if s.is_empty() {
        return "_".to_owned();
    }
    let stem = s.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    s
}

/// Имя файла адреса.
fn name_of(loc: &Location) -> String {
    loc.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Путь сервера без `.` и `..`, от корня.
fn normalize(p: &Path) -> PathBuf {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => parts.clear(),
            Component::CurDir => {}
        }
    }
    PathBuf::from(format!("/{}", parts.join("/")))
}

/// Цель записи: сам файл или, если это ссылка, файл, на который она указывает (запись
/// `rename` поверх ссылки заменила бы её обычным файлом). Метаданные — цели.
pub fn resolve_target(vfs: &dyn Vfs, loc: &Location) -> Result<(Location, Metadata), VfsError> {
    let mut at = loc.clone();
    for _ in 0..LINK_HOPS {
        let m = vfs.stat(&at, false)?;
        let link = matches!(m.kind, EntryKind::LinkFile | EntryKind::LinkBroken);
        if let (true, Some(t)) = (link, m.link_target.as_ref()) {
            let base = at.path.parent().map(Path::to_path_buf).unwrap_or_default();
            at = Location {
                path: normalize(&base.join(t)),
                ..at.clone()
            };
        } else {
            let m = if link { vfs.stat(&at, true)? } else { m };
            return Ok((at, m));
        }
    }
    // Петля ссылок: пусть ответит сервер.
    Ok((loc.clone(), vfs.stat(loc, true)?))
}

/// Скачанная копия.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub target: Location,
    pub stamp: RemoteStamp,
    pub mode: Option<u32>,
}

/// Копирует поток в `out` блоками: отмена между блоками, прогресс — `on(done)`.
fn pump(
    r: &mut dyn Read,
    w: &mut dyn Write,
    cancel: &CancelToken,
    on: &mut dyn FnMut(u64),
) -> io::Result<u64> {
    let mut buf = vec![0u8; BUF];
    let mut done = 0u64;
    let mut tick = Instant::now();
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        w.write_all(&buf[..n])?;
        done += n as u64;
        if tick.elapsed() >= TICK {
            tick = Instant::now();
            on(done);
        }
    }
    on(done);
    Ok(done)
}

fn io_err(path: &Path, e: io::Error, cancel: &CancelToken) -> VfsError {
    if cancel.is_cancelled() || e.kind() == io::ErrorKind::Interrupted {
        VfsError::Cancelled
    } else {
        VfsError::from_io(path, e)
    }
}

/// Скачивает файл `loc` (или цель ссылки) в `file`; `on(done, total)` — прогресс.
pub fn fetch(
    vfs: &dyn Vfs,
    loc: &Location,
    file: &Path,
    cancel: &CancelToken,
    on: &mut dyn FnMut(u64, u64),
) -> Result<Fetched, VfsError> {
    let _span = tracing::debug_span!("edit.fetch").entered();
    let (target, m) = resolve_target(vfs, loc)?;
    if m.kind.is_dir_like() {
        return Err(VfsError::Unsupported("directory"));
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| VfsError::from_io(dir, e))?;
    }
    let mut r = vfs.open_read(&target)?;
    let total = r.len().unwrap_or(m.size);
    on(0, total);
    let mut out =
        io::BufWriter::new(std::fs::File::create(file).map_err(|e| VfsError::from_io(file, e))?);
    pump(&mut r, &mut out, cancel, &mut |n| on(n, total))
        .and_then(|_| out.flush())
        .map_err(|e| io_err(file, e, cancel))?;
    tracing::debug!(size = m.size, target = %target.to_uri(), "remote copy fetched");
    Ok(Fetched {
        target,
        stamp: RemoteStamp::of(&m),
        mode: m.mode,
    })
}

/// Что закачать.
#[derive(Debug, Clone)]
pub struct UpReq {
    /// Сохранённая копия.
    pub local: PathBuf,
    /// Файл на сервере.
    pub target: Location,
    /// Сверка перед записью: `None` — без сверки («Перезаписать»); `Some(None)` — файла
    /// не было; `Some(Some(s))` — отметка должна совпасть.
    pub expect: Option<Option<RemoteStamp>>,
    /// Права файла (младшие 12 бит).
    pub mode: Option<u32>,
}

/// Итог закачки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpOutcome {
    Done {
        stamp: RemoteStamp,
        bytes: u64,
    },
    /// Файл на сервере изменился с момента скачивания: ничего не записано.
    Changed,
}

/// `<имя>.urfin-part` рядом с `target`.
pub fn part_of(target: &Location) -> Location {
    Location {
        path: target
            .path
            .with_file_name(format!("{}{PART}", name_of(target))),
        ..target.clone()
    }
}

/// Отметка сервера изменилась с `want`.
fn changed(vfs: &dyn Vfs, target: &Location, want: Option<RemoteStamp>) -> Result<bool, VfsError> {
    match (vfs.stat(target, true), want) {
        (Ok(m), Some(w)) => Ok(RemoteStamp::of(&m) != w),
        (Ok(_), None) | (Err(VfsError::NotFound(_)), Some(_)) => Ok(true),
        (Err(VfsError::NotFound(_)), None) => Ok(false),
        (Err(e), _) => Err(e),
    }
}

/// Закачивает копию атомарно (F03 §4.2): `.urfin-part`, `finish`, права, `rename`
/// поверх; затем новая отметка. Ошибка до `rename` — `part` удаляется (если связь есть).
pub fn upload(
    vfs: &dyn Vfs,
    req: &UpReq,
    cancel: &CancelToken,
    on: &mut dyn FnMut(u64),
) -> Result<UpOutcome, VfsError> {
    let _span = tracing::debug_span!("edit.upload").entered();
    if let Some(want) = req.expect
        && changed(vfs, &req.target, want)?
    {
        tracing::info!(target = %req.target.to_uri(), "remote file changed since download");
        return Ok(UpOutcome::Changed);
    }
    let part = part_of(&req.target);
    let mut src = std::fs::File::open(&req.local).map_err(|e| VfsError::from_io(&req.local, e))?;
    let res = (|| {
        let mut w = vfs.open_write(&part, None)?;
        let bytes =
            pump(&mut src, &mut w, cancel, on).map_err(|e| io_err(&part.path, e, cancel))?;
        w.finish()?;
        if let Some(mode) = req.mode
            && let Err(e) = vfs.set_meta(&part, Some(mode & 0o7777), None)
        {
            // Права — по возможности: сервер может не дать (чужой файл, ACL).
            tracing::warn!(error = %e, "remote edit: mode not kept");
        }
        vfs.rename(&part, &req.target, true)?;
        Ok(bytes)
    })();
    let bytes = match res {
        Ok(b) => b,
        Err(e) => {
            if vfs.link(&part) == Link::Up {
                let _ = vfs.remove(&part);
            }
            return Err(e);
        }
    };
    let stamp = RemoteStamp::of(&vfs.stat(&req.target, true)?);
    tracing::debug!(bytes, target = %req.target.to_uri(), "remote copy uploaded");
    Ok(UpOutcome::Done { stamp, bytes })
}

/// Ошибка, при которой стоит проверить связь (не «нет файла», не «нет прав», не отмена).
pub fn maybe_net(e: &VfsError) -> bool {
    matches!(e, VfsError::Io { .. })
}

/// После ошибки: связь потеряна и сеанс переподключается — ждём (`on_wait` — один раз);
/// `true` — связь вернулась, повторить; `false` — ждать нечего или отменено.
pub fn wait_link(
    vfs: &dyn Vfs,
    at: &Location,
    cancel: &CancelToken,
    on_wait: &mut dyn FnMut(),
) -> bool {
    let t0 = Instant::now();
    loop {
        match vfs.link(at) {
            Link::Up if t0.elapsed() >= LINK_GRACE => return false,
            Link::Up => {}
            Link::Down => return false,
            Link::Waiting => break,
        }
        if cancel.is_cancelled() {
            return false;
        }
        std::thread::sleep(LINK_POLL);
    }
    tracing::warn!(at = %at.to_uri(), "remote edit: waiting for connection");
    on_wait();
    loop {
        if cancel.is_cancelled() {
            return false;
        }
        match vfs.link(at) {
            Link::Up => return true,
            Link::Down => return false,
            Link::Waiting => std::thread::sleep(LINK_POLL),
        }
    }
}

/// Удаляет временную папку копии; файл ещё может быть открыт фоновой задачей документа —
/// несколько попыток.
pub fn remove_dir(dir: &Path) {
    for k in 0..20 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) if k == 19 => {
                tracing::warn!(error = %e, dir = %dir.display(), "remote edit: temp dir kept");
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Самое позднее время изменения папки и файлов в ней.
fn newest(dir: &Path) -> Option<SystemTime> {
    let mut t = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            t = Some(t.map_or(m, |t| t.max(m)));
        }
    }
    t
}

/// Остатки временных копий (`<root>\<схема>\<сервер>\<hash>`) старше `older` — удаляются;
/// возвращает число удалённых папок.
pub fn sweep(root: &Path, older: Duration) -> usize {
    let now = SystemTime::now();
    let mut n = 0;
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    for scheme in dirs(root) {
        for server in dirs(&scheme) {
            for copy in dirs(&server) {
                let stale = newest(&copy)
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age >= older);
                if stale && std::fs::remove_dir_all(&copy).is_ok() {
                    n += 1;
                }
            }
            let _ = std::fs::remove_dir(&server);
        }
    }
    n
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
