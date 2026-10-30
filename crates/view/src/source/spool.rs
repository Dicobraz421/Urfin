//! `Spool`: поток без seek (архивы S8) — фоновая копия во временный файл
//! `cache_dir/view-spool`, который растёт; читать можно то, что уже скопировано.
//!
//! Файл открыт с `FILE_FLAG_DELETE_ON_CLOSE`: ОС удаляет его, когда закрыт последний
//! handle (документ закрыт и насос остановлен, или процесс упал). Остатки старше часа
//! [`sweep_spool`] убирает при первом Spool в процессе.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, SystemTime};

use urfin_core::CancelToken;
use urfin_vfs::VfsRead;

use super::{ByteSource, SourceKind, assert_off_ui, clamp};
use crate::sys;

/// Расширение временных файлов Spool.
const EXT: &str = "spool";
/// Блок копирования.
const BLOCK: usize = 1 << 20;
/// Остатки старше этого убирает [`sweep_spool`].
const STALE: Duration = Duration::from_secs(3600);

/// Каталог Spool в каталоге кэша приложения.
pub fn spool_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join(concat!("view", "-spool"))
}

/// Удаляет файлы Spool, не менявшиеся дольше `older`; возвращает число удалённых.
pub fn sweep_spool(dir: &Path, older: Duration) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != EXT) {
            continue;
        }
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= older);
        if stale && std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    n
}

#[derive(Debug)]
struct Shared {
    file: File,
    copied: AtomicU64,
    done: AtomicBool,
}

/// Источник поверх растущего временного файла.
#[derive(Debug)]
pub struct SpoolSource {
    sh: Arc<Shared>,
    cancel: CancelToken,
}

/// Насос копирования: `run` — долгий воркер (`Runtime::spawn_dedicated`).
pub struct SpoolPump {
    reader: Box<dyn VfsRead>,
    sh: Arc<Shared>,
    cancel: CancelToken,
}

impl std::fmt::Debug for SpoolPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpoolPump")
            .field("copied", &self.sh.copied)
            .finish_non_exhaustive()
    }
}

impl SpoolSource {
    /// Создаёт временный файл в `dir`; копирование начнётся, когда вызывающий запустит
    /// `SpoolPump::run`. `cancel` останавливает насос (его же отменяет `Drop` источника).
    pub fn start(
        reader: Box<dyn VfsRead>,
        dir: &Path,
        cancel: CancelToken,
    ) -> io::Result<(SpoolSource, SpoolPump)> {
        static SWEEP: Once = Once::new();
        static SEQ: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(dir)?;
        SWEEP.call_once(|| {
            let n = sweep_spool(dir, STALE);
            if n > 0 {
                tracing::debug!(n, "stale spool files removed");
            }
        });
        let pid = std::process::id();
        let mut tries = 0;
        let file = loop {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{pid}-{seq}.{EXT}"));
            match sys::open_spool(&path) {
                Ok(f) => break f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 16 => tries += 1,
                Err(e) => return Err(e),
            }
        };
        let sh = Arc::new(Shared {
            file,
            copied: AtomicU64::new(0),
            done: AtomicBool::new(false),
        });
        let pump = SpoolPump {
            reader,
            sh: sh.clone(),
            cancel: cancel.clone(),
        };
        Ok((SpoolSource { sh, cancel }, pump))
    }
}

impl Drop for SpoolSource {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl SpoolPump {
    /// Копирует поток до конца, ошибки или отмены. Файл удалится, когда закроются оба
    /// конца (насос и источник).
    pub fn run(mut self) {
        let _span = tracing::debug_span!("view.spool").entered();
        let mut buf = vec![0u8; BLOCK];
        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            match self.reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let at = self.sh.copied.load(Ordering::Relaxed);
                    if let Err(e) = sys::write_all_at(&self.sh.file, &buf[..n], at) {
                        tracing::warn!(error = %e, "spool write failed");
                        break;
                    }
                    self.sh.copied.store(at + n as u64, Ordering::Release);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    tracing::warn!(error = %e, "spool read failed");
                    break;
                }
            }
        }
        self.sh.done.store(true, Ordering::Release);
    }
}

impl ByteSource for SpoolSource {
    fn len(&self) -> u64 {
        self.sh.copied.load(Ordering::Acquire)
    }

    fn is_final(&self) -> bool {
        self.sh.done.load(Ordering::Acquire)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        assert_off_ui();
        let n = clamp(off, buf.len(), self.len());
        if n == 0 {
            return Ok(0);
        }
        sys::read_at(&self.sh.file, &mut buf[..n], off)
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Spool
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len())
    }
}
