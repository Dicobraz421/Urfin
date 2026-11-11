//! Спилл (S8 §3.4): архив, который нельзя читать на месте, — копия на диске.
//!
//! Вложенный архив со сжатой записью (zip → tar.gz), вложенный rar (`UnRAR` открывает
//! только путь) и архив на сервере (`sftp://`) распаковываются или скачиваются в
//! `<cache_dir>/arc/<xxh3>.<ext>` ([`crate::ArcCache::set_spill_dir`]). Копия живёт, пока
//! жив индекс архива ([`crate::OpenCtx`]): индекс вышел из LRU и его никто не держит —
//! файл удалён. Остатки прошлых запусков старше [`STALE`] удаляются при старте.
//!
//! Лимиты против zip-бомб — `[arc] spill_max` и `ratio_max`: больше — вопрос
//! «Распаковать N во временный каталог?» ([`SpillQuestion`]); пишется не больше
//! заявленного размера записи.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use urfin_core::{Anchor, CancelToken, Choice, Location, PromptSpec};
use urfin_vfs::VfsError;

use crate::detect;
use crate::index::OuterKey;

/// Остатки спилла старше — удаляются при старте.
pub const STALE: Duration = Duration::from_hours(24);

/// Кусок копирования: между кусками — проверка отмены.
const CHUNK: usize = 1 << 20;

/// Копия архива на диске; файл удаляется со сбросом последней ссылки.
#[derive(Debug)]
pub struct Spill {
    path: PathBuf,
}

impl Spill {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(target: "arc", error = %e, path = %self.path.display(), "spill not removed");
        }
    }
}

/// Почему спилл спрашивает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Over {
    /// Больше `[arc] spill_max` (лимит, байты).
    Size(u64),
    /// Степень сжатия записи больше `[arc] ratio_max` (`N:1`).
    Ratio(u64),
}

/// Превышен ли лимит: `packed` — сжатый размер записи архива (у файла сервера — `None`).
pub(crate) fn over(size: u64, packed: Option<u64>, max: u64, ratio_max: u32) -> Option<Over> {
    if let Some(p) = packed.filter(|&p| p > 0) {
        let ratio = size / p;
        if ratio > u64::from(ratio_max) {
            return Some(Over::Ratio(ratio));
        }
    }
    (size > max).then_some(Over::Size(max))
}

/// Вопрос перед спиллом сверх лимита.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpillQuestion {
    /// Имя вложенного архива.
    pub name: String,
    /// Сколько распаковать, байты.
    pub size: u64,
    pub over: Over,
}

fn reason(over: Over) -> String {
    let i18n = urfin_i18n::current();
    let mut a = urfin_i18n::FluentArgs::new();
    match over {
        Over::Size(limit) => {
            a.set("limit", urfin_i18n::fmt::bytes(limit));
            i18n.fmt("arc-spill-big", &a)
        }
        Over::Ratio(r) => {
            a.set("ratio", r.to_string());
            i18n.fmt("arc-spill-ratio", &a)
        }
    }
}

impl SpillQuestion {
    /// «Распаковать 5 ГБ во временный каталог?» с причиной; по умолчанию — «Отмена».
    pub fn spec(&self, anchor: Anchor) -> PromptSpec {
        let i18n = urfin_i18n::current();
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("size", urfin_i18n::fmt::bytes(self.size));
        let choice = |id: &str, key: &str| Choice {
            id: id.into(),
            label: i18n.get(key).into(),
            key: '\0',
        };
        PromptSpec {
            anchor,
            title: i18n.fmt("arc-spill-ask", &a),
            body: format!("{} · {}", self.name, reason(self.over)),
            choices: vec![
                choice(GO, "arc-spill-go"),
                choice("cancel", "ui-button-cancel"),
            ]
            .into(),
            // Enter по привычке не распаковывает бомбу.
            default: 1,
            input: None,
            allow_all: false,
            detail: None,
            secret: false,
        }
    }
}

/// Ответ «Распаковать».
pub(crate) const GO: &str = "go";

/// Отказ от спилла (или спросить некому): причина — ошибкой листинга.
pub(crate) fn refused(outer: &Location, over: Over) -> VfsError {
    VfsError::Io {
        path: outer.path.clone(),
        source: io::Error::other(reason(over)),
    }
}

/// Имя копии: `xxh3` адреса и ключа, `tag` (у каждой копии своё имя: новая не затирает
/// ту, которую ещё читают) и расширение архива.
pub(crate) fn file_name(key: &OuterKey, tag: &str) -> String {
    let uri = format!("{}|{}|{}", key.loc.to_uri(), key.size, key.mtime);
    let h = xxhash_rust::xxh3::xxh3_64(uri.as_bytes());
    let name = key
        .loc
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = detect::ext_in(&name, detect::archive_exts())
        .map(str::to_owned)
        .or_else(|| {
            Path::new(&name)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
        })
        .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'))
        .unwrap_or_else(|| "bin".into());
    format!(
        "{h:016x}-{tag}.{}",
        ext.trim_start_matches('.').to_lowercase()
    )
}

/// Копирует `r` (ровно `size` байт: больше или меньше — архив повреждён) в `dir`.
/// Отмена и ошибка — без следа.
pub(crate) fn write(
    dir: &Path,
    key: &OuterKey,
    mut r: impl Read,
    cancel: &CancelToken,
) -> Result<Spill, VfsError> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let io_err = |e| VfsError::from_io(dir.to_path_buf(), e);
    fs::create_dir_all(dir).map_err(io_err)?;
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = file_name(key, &format!("{}-{seq}", std::process::id()));
    let part = dir.join(format!("{name}.part"));
    let res = copy(&mut r, &part, key, cancel).and_then(|()| {
        let path = dir.join(&name);
        fs::rename(&part, &path).map_err(io_err)?;
        Ok(Spill { path })
    });
    if res.is_err() {
        let _ = fs::remove_file(&part);
    }
    res
}

fn copy(
    r: &mut impl Read,
    part: &Path,
    key: &OuterKey,
    cancel: &CancelToken,
) -> Result<(), VfsError> {
    let werr = |e| VfsError::from_io(part.to_path_buf(), e);
    let rerr = |e| VfsError::from_io(key.loc.path.clone(), e);
    let mut f = fs::File::create(part).map_err(werr)?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        // Запас в байт: поток длиннее заявленного — бомба с ложным размером.
        let want = (key.size + 1 - done).min(CHUNK as u64) as usize;
        let n = match r.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(rerr(e)),
        };
        done += n as u64;
        if done > key.size {
            return Err(crate::format::corrupt(
                &key.loc,
                "entry is larger than declared",
            ));
        }
        f.write_all(&buf[..n]).map_err(werr)?;
    }
    if done != key.size {
        return Err(crate::format::corrupt(
            &key.loc,
            "entry is shorter than declared",
        ));
    }
    f.flush().map_err(werr)
}

/// Удаляет из `dir` файлы старше `older` (остатки прошлых запусков). Сколько удалено.
pub fn sweep(dir: &Path, older: Duration) -> usize {
    let Ok(rd) = fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut n = 0;
    for e in rd.flatten() {
        let Ok(m) = e.metadata() else { continue };
        let old = m
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= older);
        if m.is_file() && old && fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    if n > 0 {
        tracing::info!(target: "arc", removed = n, dir = %dir.display(), "stale spill removed");
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, size: u64) -> OuterKey {
        OuterKey {
            loc: Location {
                backend: "zip".into(),
                authority: None,
                path: PathBuf::from(format!("/{name}")),
                outer: Some(std::sync::Arc::new(Location::local(r"C:\a.zip"))),
            },
            size,
            mtime: 1,
        }
    }

    #[test]
    fn limits() {
        assert_eq!(over(10, Some(10), 100, 1000), None);
        assert_eq!(over(101, Some(100), 100, 1000), Some(Over::Size(100)));
        assert_eq!(over(5000, Some(2), 1 << 30, 1000), Some(Over::Ratio(2500)));
        assert_eq!(
            over(5000, Some(0), 1 << 30, 1000),
            None,
            "без сжатого размера"
        );
        assert_eq!(over(5000, None, 1 << 30, 1000), None, "файл сервера");
    }

    #[test]
    fn name_keeps_the_archive_extension() {
        let n = file_name(&key("inner.tar.gz", 3), "1-2");
        assert!(n.ends_with("-1-2.tar.gz"), "{n}");
        assert_eq!(n.len(), 16 + "-1-2.tar.gz".len());
        assert_ne!(
            n,
            file_name(&key("inner.tar.gz", 4), "1-2"),
            "другой ключ — другой файл"
        );
        assert_eq!(
            Path::new(&file_name(&key("x", 3), "1"))
                .extension()
                .unwrap(),
            "bin"
        );
    }

    #[test]
    fn writes_exact_size_and_removes_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let k = key("in.zip", 5);
        let s = write(dir.path(), &k, &b"hello"[..], &CancelToken::new()).unwrap();
        assert_eq!(fs::read(s.path()).unwrap(), b"hello");
        let p = s.path().to_path_buf();
        drop(s);
        assert!(!p.exists());

        for data in [&b"hello!"[..], &b"hell"[..]] {
            let e = write(dir.path(), &k, data, &CancelToken::new()).unwrap_err();
            assert!(matches!(e, VfsError::Io { .. }), "{e:?}");
        }
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            write(dir.path(), &k, &b"hello"[..], &cancel),
            Err(VfsError::Cancelled)
        ));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "без следа");
    }

    #[test]
    fn sweep_removes_only_old_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.zip"), b"x").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(sweep(dir.path(), STALE), 0, "свежий файл остаётся");
        assert_eq!(sweep(dir.path(), Duration::ZERO), 1);
        assert!(dir.path().join("sub").is_dir(), "каталоги не трогаем");
        assert_eq!(sweep(&dir.path().join("none"), STALE), 0);
    }

    #[test]
    fn question_names_the_reason() {
        urfin_i18n::set_language("ru");
        let q = SpillQuestion {
            name: "bomb.zip".into(),
            size: 5 << 30,
            over: Over::Ratio(2500),
        };
        let spec = q.spec(Anchor::Cmdline);
        assert_eq!(spec.title, "Распаковать 5,0 ГБ во временный каталог?");
        assert!(spec.body.contains("2500:1"), "{}", spec.body);
        assert_eq!(spec.choices[usize::from(spec.default)].id, "cancel");
        let big = SpillQuestion {
            over: Over::Size(4 << 30),
            ..q
        };
        assert!(big.spec(Anchor::Cmdline).body.contains("4,0 ГБ"));
    }
}
