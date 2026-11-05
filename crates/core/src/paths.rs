//! Каталоги приложения (S1 §2.8, решение владельца №1): профиль ОС, portable — при
//! файле `urfin.portable` рядом с exe.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Маркер portable-режима рядом с exe.
pub const PORTABLE_MARKER: &str = "urfin.portable";

/// Каталоги конфига, состояния, логов и кэша.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub log_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub portable: bool,
}

/// Базовые каталоги ОС (из `dirs`); отдельно — чтобы тестировать без окружения.
#[derive(Clone, Debug, Default)]
#[allow(dead_code, reason = "набор нужных каталогов зависит от ОС")]
pub(crate) struct OsDirs {
    pub(crate) config: Option<PathBuf>,
    pub(crate) data_local: Option<PathBuf>,
    pub(crate) state: Option<PathBuf>,
    pub(crate) cache: Option<PathBuf>,
}

impl OsDirs {
    fn current() -> Self {
        OsDirs {
            config: dirs::config_dir(),
            data_local: dirs::data_local_dir(),
            state: dirs::state_dir(),
            cache: dirs::cache_dir(),
        }
    }
}

impl Paths {
    /// Portable, если рядом с exe есть `urfin.portable`: тогда всё в `<exe>/data/`.
    /// Иначе — профиль ОС:
    ///
    /// | ОС | `config_dir` | `state_dir`, `log_dir`, `cache_dir` |
    /// |---|---|---|
    /// | Windows | `%APPDATA%\Urfin` | `%LOCALAPPDATA%\Urfin\{state,logs,cache}` |
    /// | macOS | `~/Library/Application Support/Urfin` | там же `/{state,logs}`, кэш — `~/Library/Caches/Urfin` |
    /// | Linux | `$XDG_CONFIG_HOME/urfin` | `$XDG_STATE_HOME/urfin{,/logs}`, кэш — `$XDG_CACHE_HOME/urfin` |
    ///
    /// Если каталог ОС не определяется, используется portable-раскладка рядом с exe.
    pub fn detect() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        Self::detect_with(exe_dir.as_deref(), &OsDirs::current())
    }

    pub(crate) fn detect_with(exe_dir: Option<&Path>, os: &OsDirs) -> Self {
        let base = exe_dir.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        if base.join(PORTABLE_MARKER).is_file() {
            return Self::in_dir(base.join("data"));
        }
        Self::profile(os).unwrap_or_else(|| Paths {
            portable: false,
            ..Self::in_dir(base.join("data"))
        })
    }

    #[cfg(windows)]
    fn profile(os: &OsDirs) -> Option<Self> {
        let config = os.config.as_ref()?.join("Urfin");
        let local = os.data_local.as_ref()?.join("Urfin");
        Some(Paths {
            config_dir: config,
            state_dir: local.join("state"),
            log_dir: local.join("logs"),
            cache_dir: local.join("cache"),
            portable: false,
        })
    }

    #[cfg(target_os = "macos")]
    fn profile(os: &OsDirs) -> Option<Self> {
        let support = os.config.as_ref()?.join("Urfin");
        let cache = os.cache.as_ref()?.join("Urfin");
        Some(Paths {
            state_dir: support.join("state"),
            log_dir: support.join("logs"),
            config_dir: support,
            cache_dir: cache,
            portable: false,
        })
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn profile(os: &OsDirs) -> Option<Self> {
        let state = os.state.as_ref()?.join("urfin");
        Some(Paths {
            config_dir: os.config.as_ref()?.join("urfin"),
            log_dir: state.join("logs"),
            state_dir: state,
            cache_dir: os.cache.as_ref()?.join("urfin"),
            portable: false,
        })
    }

    /// Всё внутри `base` (portable-раскладка; тесты, `--smoke`).
    pub fn in_dir(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        Paths {
            state_dir: base.join("state"),
            log_dir: base.join("logs"),
            cache_dir: base.join("cache"),
            config_dir: base,
            portable: true,
        }
    }

    /// `config_dir/config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// `state_dir/session.toml`.
    pub fn session_file(&self) -> PathBuf {
        self.state_dir.join("session.toml")
    }

    /// Создать все каталоги.
    pub fn ensure(&self) -> io::Result<()> {
        for d in [
            &self.config_dir,
            &self.state_dir,
            &self.log_dir,
            &self.cache_dir,
        ] {
            fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Атомарная запись: временный файл рядом, fsync, rename поверх.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    // Уникальное имя: параллельные записи одного файла не делят временный файл.
    let tmp = dir.join(format!(
        ".{}.{}-{}.tmp",
        name.as_deref().unwrap_or("file"),
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::{OsDirs, PORTABLE_MARKER, Paths, write_atomic};
    use std::fs;

    fn os(root: &std::path::Path) -> OsDirs {
        OsDirs {
            config: Some(root.join("cfg")),
            data_local: Some(root.join("local")),
            state: Some(root.join("st")),
            cache: Some(root.join("cache")),
        }
    }

    #[test]
    fn portable_marker_switches_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("app");
        fs::create_dir_all(&exe).unwrap();
        let profile = Paths::detect_with(Some(&exe), &os(tmp.path()));
        assert!(!profile.portable);
        assert!(!profile.config_dir.starts_with(&exe));

        fs::write(exe.join(PORTABLE_MARKER), "").unwrap();
        let p = Paths::detect_with(Some(&exe), &os(tmp.path()));
        assert!(p.portable);
        assert_eq!(p.config_dir, exe.join("data"));
        assert_eq!(p.state_dir, exe.join("data").join("state"));
        assert_eq!(p.config_file(), exe.join("data").join("config.toml"));
        assert_eq!(
            p.session_file(),
            exe.join("data").join("state").join("session.toml")
        );
        p.ensure().unwrap();
        assert!(p.log_dir.is_dir() && p.cache_dir.is_dir());
    }

    #[test]
    fn profile_layout_per_os() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::detect_with(Some(tmp.path()), &os(tmp.path()));
        if cfg!(windows) {
            assert_eq!(p.config_dir, tmp.path().join("cfg").join("Urfin"));
            assert_eq!(
                p.state_dir,
                tmp.path().join("local").join("Urfin").join("state")
            );
            assert_eq!(
                p.log_dir,
                tmp.path().join("local").join("Urfin").join("logs")
            );
        } else if cfg!(target_os = "macos") {
            assert_eq!(p.config_dir, tmp.path().join("cfg").join("Urfin"));
            assert_eq!(p.log_dir, p.config_dir.join("logs"));
        } else {
            assert_eq!(p.config_dir, tmp.path().join("cfg").join("urfin"));
            assert_eq!(p.state_dir, tmp.path().join("st").join("urfin"));
            assert_eq!(p.log_dir, p.state_dir.join("logs"));
        }
    }

    #[test]
    fn missing_os_dirs_fall_back_next_to_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::detect_with(Some(tmp.path()), &OsDirs::default());
        assert!(!p.portable);
        assert_eq!(p.config_dir, tmp.path().join("data"));
    }

    #[test]
    fn atomic_write_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("sub").join("a.toml");
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"two");
        assert_eq!(fs::read_dir(f.parent().unwrap()).unwrap().count(), 1);
    }
}
