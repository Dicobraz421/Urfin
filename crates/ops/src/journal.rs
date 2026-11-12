//! Журнал job (S5 §6.2–6.3, S5-12): по файлу на job в `state_dir/jobs`,
//! захваченному `try_lock` на всё время жизни job. Записи —
//! `[u32 len][u32 crc32][postcard Record]`, только дописываются; при чтении — до
//! первой битой (обрыв посреди записи не мешает).
//!
//! Заголовок — с `fsync`; временный файл записывается в журнал до его создания
//! и сразу сбрасывается в ОС (kill -9 его не теряет), остальное — буфером,
//! сброс раз в секунду из координатора. Итог — `Done`, затем файл удаляется.
//! Журнал без `Done` после падения или выхода с прерыванием — «Продолжить» или
//! «Очистить» при следующем старте ([`scan`]).
//!
//! Рядом — план последнего группового переименования ([`RenameLog`], S5-29): по нему
//! `ops.multi_rename.undo` возвращает имена.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::api::OpRequest;

/// Версия формата записей.
pub(crate) const VERSION: u16 = 1;
const EXT: &str = "jnl";
/// Запись больше — битая (защита от мусора в длине).
const MAX_RECORD: u32 = 16 << 20;

/// Что делала job: достаточно, чтобы повторить её.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Header {
    pub v: u16,
    pub req: OpRequest,
    /// Назначение (абсолютный путь) и текст, как его ввёл пользователь.
    pub dest: PathBuf,
    pub dest_raw: String,
    /// Секунды от эпохи Unix.
    pub started: i64,
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Record {
    Header(Box<Header>),
    /// Ход работы (раз в 5 с): для «выполнено ≈N%» в баннере.
    Counters {
        items: u64,
        bytes: u64,
        items_total: u64,
        bytes_total: u64,
    },
    /// Корень назначения.
    Root(PathBuf),
    /// Перед созданием временного файла.
    Temp(PathBuf),
    /// Временный файл стал файлом (или удалён).
    TempDone(PathBuf),
    Done {
        items: u64,
        bytes: u64,
    },
    /// Временный файл копируется с докачкой (S5-27): при «Продолжить» не удаляется.
    /// Новый вариант — в конце: старые журналы читаются как раньше.
    Resumable(PathBuf),
}

/// Журнал живой job. `Sync`: временные файлы пишут воркеры.
#[derive(Debug)]
pub(crate) struct Journal {
    path: PathBuf,
    w: Mutex<Option<BufWriter<File>>>,
}

impl Journal {
    /// Новый журнал в `dir` с заголовком (сброшен на диск).
    pub(crate) fn create(dir: &Path, header: Header) -> io::Result<Journal> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.{EXT}", unique()));
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        f.try_lock().map_err(lock_error)?;
        let mut w = BufWriter::new(f);
        write_record(&mut w, &Record::Header(Box::new(header)))?;
        w.flush()?;
        w.get_ref().sync_all()?;
        Ok(Journal {
            path,
            w: Mutex::new(Some(w)),
        })
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn with(&self, f: impl FnOnce(&mut BufWriter<File>) -> io::Result<()>) {
        let mut g = self.w.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(w) = g.as_mut()
            && let Err(e) = f(w)
        {
            tracing::warn!(journal = %self.path.display(), error = %e, "journal write");
        }
    }

    /// Дописать запись (в буфер).
    pub(crate) fn append(&self, r: &Record) {
        self.with(|w| write_record(w, r));
    }

    /// Дописать и сразу отдать ОС: переживает kill -9 (не отключение питания).
    pub(crate) fn append_now(&self, r: &Record) {
        self.with(|w| {
            write_record(w, r)?;
            w.flush()
        });
    }

    /// Сбросить буфер в ОС (раз в секунду из координатора).
    pub(crate) fn flush(&self) {
        self.with(Write::flush);
    }

    /// Job закончена: `Done` на диск, журнал удаляется.
    pub(crate) fn finish(&self, items: u64, bytes: u64) {
        let w = self.w.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(mut w) = w else {
            return;
        };
        let r = write_record(&mut w, &Record::Done { items, bytes })
            .and_then(|()| w.flush())
            .and_then(|()| w.get_ref().sync_all());
        drop(w);
        if let Err(e) = r.and_then(|()| std::fs::remove_file(&self.path)) {
            tracing::warn!(journal = %self.path.display(), error = %e, "journal finish");
        }
    }

    /// Job прервана для «Продолжить»: журнал остаётся (сброшен), блокировка снята.
    pub(crate) fn keep(&self) {
        let w = self.w.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(mut w) = w
            && let Err(e) = w.flush().and_then(|()| w.get_ref().sync_all())
        {
            tracing::warn!(journal = %self.path.display(), error = %e, "journal keep");
        }
    }
}

/// Имя журнала: процесс, время, счётчик (без зависимости от uuid).
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{:x}-{nanos:x}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn lock_error(e: std::fs::TryLockError) -> io::Error {
    match e {
        std::fs::TryLockError::Error(e) => e,
        std::fs::TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
    }
}

fn write_record<T: Serialize>(w: &mut impl Write, r: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(r).map_err(io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(io::Error::other)?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&crc32fast::hash(&body).to_le_bytes())?;
    w.write_all(&body)
}

/// Записи до первой битой (недописанная, с неверной суммой или не разбираемая).
pub(crate) fn read_records(bytes: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while rest.len() >= 8 {
        let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let crc = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
        let Ok(n) = usize::try_from(len) else {
            break;
        };
        if len > MAX_RECORD || rest.len() - 8 < n {
            break;
        }
        let body = &rest[8..8 + n];
        if crc32fast::hash(body) != crc {
            break;
        }
        let Ok(r) = postcard::from_bytes::<Record>(body) else {
            break;
        };
        out.push(r);
        rest = &rest[8 + n..];
    }
    out
}

/// Прерванная job из журнала.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Interrupted {
    pub path: PathBuf,
    pub header: Header,
    /// Последние `Counters`: (`items`, `bytes`, `items_total`, `bytes_total`).
    pub counters: Option<(u64, u64, u64, u64)>,
    /// Временные файлы без `TempDone` — удалить.
    pub temps: Vec<PathBuf>,
    /// Из них — с докачкой (S5-27): «Продолжить» дописывает, «Очистить» удаляет.
    pub resumable: Vec<PathBuf>,
}

impl Interrupted {
    /// Выполнено, % по байтам (если известно).
    pub(crate) fn percent(&self) -> Option<u64> {
        let (_, bytes, _, total) = self.counters?;
        (total > 0).then(|| bytes.min(total) * 100 / total)
    }

    /// Удалить временные файлы прерванной job; `keep` — с докачкой (оставить для
    /// «Продолжить»).
    pub(crate) fn remove_temps(&self, keep: bool) {
        for t in &self.temps {
            if keep && self.resumable.contains(t) {
                continue;
            }
            match std::fs::remove_file(t) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(temp = %t.display(), error = %e, "journal temp"),
            }
        }
    }
}

/// Разбор журнала: `None` — завершён (`Done`) или без заголовка (нечего продолжать).
fn interpret(path: &Path, records: Vec<Record>) -> Option<Interrupted> {
    let mut it = records.into_iter();
    let Some(Record::Header(header)) = it.next() else {
        return None;
    };
    let mut out = Interrupted {
        path: path.to_path_buf(),
        header: *header,
        counters: None,
        temps: Vec::new(),
        resumable: Vec::new(),
    };
    for r in it {
        match r {
            Record::Done { .. } => return None,
            Record::Counters {
                items,
                bytes,
                items_total,
                bytes_total,
            } => out.counters = Some((items, bytes, items_total, bytes_total)),
            Record::Temp(p) => out.temps.push(p),
            Record::TempDone(p) => {
                out.temps.retain(|t| *t != p);
                out.resumable.retain(|t| *t != p);
            }
            Record::Resumable(p) => out.resumable.push(p),
            Record::Header(_) | Record::Root(_) => {}
        }
    }
    Some(out)
}

/// Захватить журнал и прочитать его. `Ok(None)` — занят живой job (другой
/// экземпляр или эта же). Файл держит блокировку, пока жив.
pub(crate) fn take(path: &Path) -> io::Result<Option<(File, Option<Interrupted>)>> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    match f.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
        Err(std::fs::TryLockError::Error(e)) => return Err(e),
    }
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    let i = interpret(path, read_records(&bytes));
    Ok(Some((f, i)))
}

/// Сканирование при старте (S5 §6.3): завершённые и пустые журналы удаляются,
/// занятые живыми job пропускаются, прерванные — список для баннера.
pub(crate) fn scan(dir: &Path) -> Vec<Interrupted> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != EXT) {
            continue;
        }
        match take(&path) {
            Ok(Some((f, Some(i)))) => {
                drop(f);
                out.push(i);
            }
            Ok(Some((f, None))) => {
                drop(f);
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(journal = %path.display(), error = %e, "journal remove");
                }
            }
            Ok(None) => tracing::debug!(journal = %path.display(), "journal is live, skipped"),
            Err(e) => tracing::warn!(journal = %path.display(), error = %e, "journal read"),
        }
    }
    out.sort_by_key(|i| i.header.started);
    out
}

/// Файл плана последнего группового переименования в каталоге журналов. Расширение не
/// `jnl`: [`scan`] его не трогает — это не прерванная job.
const RENAME_FILE: &str = "multi-rename.undo";

/// Применённое групповое переименование (S5 §4.14, S5-29): каталог и строки
/// «было → стало», которые действительно переименованы, — для
/// `ops.multi_rename.undo`. Хранится последнее; переживает перезапуск.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RenameLog {
    pub v: u16,
    pub base: urfin_core::Location,
    pub rows: Vec<(String, String)>,
}

/// Записать план (тем же кадром, что записи журнала): во временный файл и
/// переименованием на место — недописанного файла плана не бывает.
pub(crate) fn save_rename(dir: &Path, log: &RenameLog) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut bytes = Vec::new();
    write_record(&mut bytes, log)?;
    let tmp = dir.join(format!("{RENAME_FILE}.{}", unique()));
    let put =
        std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, dir.join(RENAME_FILE)));
    if put.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    put
}

/// План последнего переименования; `None` — нет, битый или другой версии.
pub(crate) fn load_rename(dir: &Path) -> Option<RenameLog> {
    let bytes = match std::fs::read(dir.join(RENAME_FILE)) {
        Ok(b) => b,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %e, "rename log read");
            }
            return None;
        }
    };
    let len = usize::try_from(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    let crc = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
    let body = bytes.get(8..)?.get(..len)?;
    if crc32fast::hash(body) != crc {
        return None;
    }
    postcard::from_bytes::<RenameLog>(body)
        .ok()
        .filter(|l| l.v == VERSION)
}

/// Убрать план (откат выполнен).
pub(crate) fn clear_rename(dir: &Path) {
    match std::fs::remove_file(dir.join(RENAME_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "rename log remove"),
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
