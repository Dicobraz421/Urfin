//! Фоновая проверка обновлений (F02 §4): `urfin.exe --pkg-check` по расписанию и
//! `pkg.check_now` в открытом Урфине. Порядок:
//!
//! 1. эксклюзивный `state/pkg/check.lock` (занят — проверка уже идёт, выход);
//! 2. обнаружение и запросы всех найденных менеджеров параллельно, с таймаутами §2.3 —
//!    установленные и обновления, чтобы дубли сводились как во вкладке (§2.4);
//! 3. атомарно — кэш вкладки `cache.json`, `managers.toml` и итог `check.json`
//!    (его читает баннер). Ни один менеджер не ответил — прежние файлы остаются.
//!
//! Сам Урфин в сеть не обращается: сеть трогают только менеджеры.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use compact_str::CompactString;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use urfin_core::CancelToken;
use urfin_core::pkg::Notice;

use crate::api::{Mgr, PkgId, RunCx, Update};
use crate::book::{CACHE_FILE, Filter, PkgBook, nanos};
use crate::config::PkgConfig;
use crate::detect::{MANAGERS_FILE, Managers};
use crate::handler::PkgSource;

/// Имя файла в `state_dir/pkg`.
pub const CHECK_FILE: &str = "check.json";
/// Замок проверки в `state_dir/pkg`.
pub const LOCK_FILE: &str = "check.lock";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub checked_at: Timestamp,
    pub managers: Vec<MgrStatus>,
    /// Обновления без дублей (как строки «Обновлений»).
    pub updates: Vec<Update>,
    /// «Важные» из `updates`.
    pub important: Vec<PkgId>,
    /// Баннер этой проверки закрыт («Показать» или «Позже»).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_at: Option<Timestamp>,
    /// «Проверить сейчас», а не по расписанию.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
}

/// Ответ одного менеджера.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MgrStatus {
    pub mgr: Mgr,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CompactString>,
}

impl CheckReport {
    /// Нет файла — `None`; битый — `None` с предупреждением в логе.
    pub fn load(path: &Path) -> Option<CheckReport> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes)
            .inspect_err(|e| tracing::warn!(error = %e, path = %path.display(), "pkg: check.json"))
            .ok()
    }

    /// Атомарно: временный файл рядом и переименование.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Хоть один менеджер ответил.
    pub fn ok(&self) -> bool {
        self.managers.iter().any(|m| m.ok)
    }

    /// Баннер: обновления есть и баннер этой проверки ещё не закрывали.
    pub fn notice(&self) -> Option<Notice> {
        if self.updates.is_empty() || self.dismissed_at.is_some() {
            return None;
        }
        Some(Notice {
            updates: self.updates.len(),
            important: self.important.len(),
            checked_at: nanos(self.checked_at),
            scheduled: !self.manual,
        })
    }
}

/// Ошибка проверки.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// `check.lock` занят: проверка уже идёт (по расписанию или «сейчас»).
    #[error("an update check is already running")]
    Busy,
    /// Менеджеров нет или ни один не ответил: первая ошибка.
    #[error("no package manager answered: {0}")]
    NoAnswer(CompactString),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Эксклюзивный `check.lock`: одна проверка на каталог состояния, и между процессами.
/// Отпускается с закрытием файла — и со смертью процесса.
#[derive(Debug)]
pub struct CheckLock {
    _file: File,
}

impl CheckLock {
    /// `None` — замок держит другая проверка.
    pub fn try_take(dir: &Path) -> io::Result<Option<CheckLock>> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(CheckLock { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }
}

/// Проверка в каталоге `dir` (`state_dir/pkg`): менеджеры `source`, «важные» и
/// таймауты — из `cfg`; `manual` — «Проверить сейчас». Ждёт ответа всех менеджеров —
/// не на UI-потоке.
pub fn run(
    dir: &Path,
    cfg: &PkgConfig,
    source: &dyn PkgSource,
    cancel: &CancelToken,
    manual: bool,
) -> Result<CheckReport, CheckError> {
    let Some(_lock) = CheckLock::try_take(dir)? else {
        return Err(CheckError::Busy);
    };
    let t0 = Instant::now();
    let cached = Managers::load(&dir.join(MANAGERS_FILE)).unwrap_or_default();
    let found = source.detect(&cached);
    let book = PkgBook::new(cfg);
    book.start();
    book.found(found.clone(), source.arp());
    let t = cfg.timeouts;
    let rc = |secs: u32| RunCx {
        cancel: cancel.clone(),
        timeout: Some(Duration::from_secs(u64::from(secs))),
        lines: None,
    };
    std::thread::scope(|s| {
        for d in &found {
            let adapter = source.adapter(d, cfg);
            let (book, rc) = (&book, &rc);
            let spawned = std::thread::Builder::new()
                .name(format!("pkg.check.{}", d.mgr))
                .spawn_scoped(s, move || {
                    let res = adapter.list_installed(&rc(t.list_s)).and_then(|list| {
                        book.set_installed(d.mgr, list);
                        adapter.list_updates(&rc(t.updates_s))
                    });
                    match res {
                        Ok(ups) => book.set_updates(d.mgr, ups, adapter.detect()),
                        Err(e) => {
                            tracing::warn!(target: "pkg", mgr = %d.mgr, error = %e, "pkg: check");
                            book.failed(d.mgr, e.to_string().into());
                        }
                    }
                });
            if let Err(e) = spawned {
                book.failed(d.mgr, e.to_string().into());
            }
        }
    });
    let now = Timestamp::now();
    book.finish(now);
    let cache = book.cache();
    let view = book.view();
    let rows = view.rows(Filter::Updates);
    let report = CheckReport {
        checked_at: now,
        managers: cache.managers.clone(),
        updates: rows
            .iter()
            .map(|r| Update {
                pkg: r.pkg.clone(),
                available: r.available.clone().unwrap_or_default(),
                pinned: r.pinned,
            })
            .collect(),
        important: rows
            .iter()
            .filter(|r| r.important)
            .map(|r| r.pkg.id.clone())
            .collect(),
        dismissed_at: None,
        manual,
    };
    tracing::info!(
        name: "pkg.check",
        target: "pkg",
        ms = t0.elapsed().as_millis(),
        managers = found.len(),
        updates = report.updates.len(),
        important = report.important.len(),
        manual,
    );
    if !report.ok() {
        let first = report
            .managers
            .iter()
            .find_map(|m| m.error.clone())
            .unwrap_or_else(|| "no package managers found".into());
        return Err(CheckError::NoAnswer(first));
    }
    cache.save(&dir.join(CACHE_FILE))?;
    Managers {
        managers: book.detected(),
    }
    .save(&dir.join(MANAGERS_FILE))?;
    report.save(&dir.join(CHECK_FILE))?;
    Ok(report)
}

/// Баннер проверки `checked_at` (Unix, нс) закрыт: `dismissed_at` в `check.json`. Идёт
/// новая проверка (замок занят) или итог уже другой — ничего: баннер покажет новый.
pub fn dismiss(dir: &Path, checked_at: i64) -> io::Result<()> {
    let Some(_lock) = CheckLock::try_take(dir)? else {
        return Ok(());
    };
    let path = dir.join(CHECK_FILE);
    let Some(mut r) = CheckReport::load(&path) else {
        return Ok(());
    };
    if nanos(r.checked_at) != checked_at || r.dismissed_at.is_some() {
        return Ok(());
    }
    r.dismissed_at = Some(Timestamp::now());
    r.save(&path)
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
