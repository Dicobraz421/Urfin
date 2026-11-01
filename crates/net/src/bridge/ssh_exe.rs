//! Поиск и версия `ssh.exe` (F03 §2.7): `%SystemRoot%\System32\OpenSSH\ssh.exe`, затем
//! PATH. Версия — `ssh -V`, лениво, с кэшем `state/ssh/ssh_exe.toml` (путь, размер,
//! время изменения). Мост (`SSH_ASKPASS_REQUIRE`) — с OpenSSH 8.9.

use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
#[cfg(windows)]
use urfin_term::proc::{self, PipedFlags};

/// Наименьшая версия для моста.
pub const MIN_VERSION: (u32, u32) = (8, 9);

/// Найденный `ssh.exe` и его версия.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshExe {
    pub path: PathBuf,
    /// Первая строка `ssh -V`.
    pub version: String,
    size: u64,
    modified: u64,
}

impl SshExe {
    /// `(major, minor)`; `None` — не разобрали.
    pub fn parsed(&self) -> Option<(u32, u32)> {
        parse_version(&self.version)
    }

    /// Мост к `ssh.exe` возможен; иначе — подпись «обновите OpenSSH», пароль — в терминале.
    pub fn bridge_ok(&self) -> bool {
        self.parsed().is_some_and(|v| v >= MIN_VERSION)
    }
}

/// `ssh.exe`: системный OpenSSH, затем PATH.
pub fn find(system_root: Option<&Path>, path: Option<&OsStr>) -> Option<PathBuf> {
    let system = system_root.map(|r| r.join("System32").join("OpenSSH").join("ssh.exe"));
    let in_path = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("ssh.exe"));
    system.into_iter().chain(in_path).find(|p| p.is_file())
}

/// `OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2` → `(9, 5)`.
pub fn parse_version(s: &str) -> Option<(u32, u32)> {
    let i = s.find("OpenSSH_")?;
    let rest = &s[i + "OpenSSH_".len()..];
    let rest = rest.strip_prefix("for_Windows_").unwrap_or(rest);
    let mut it = rest.split(|c: char| !c.is_ascii_digit());
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

fn stamp(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((m.len(), t))
}

/// `ssh.exe` с версией. Блокирует (запуск `ssh -V`): только вне UI.
pub fn detect(cache: &Path) -> Option<SshExe> {
    let path = find(
        std::env::var_os("SystemRoot").as_deref().map(Path::new),
        std::env::var_os("PATH").as_deref(),
    )?;
    let (size, modified) = stamp(&path)?;
    if let Some(c) = std::fs::read_to_string(cache)
        .ok()
        .and_then(|t| toml::from_str::<SshExe>(&t).ok())
        .filter(|c| c.path == path && c.size == size && c.modified == modified)
    {
        return Some(c);
    }
    let version = version_of(&path)?;
    let exe = SshExe {
        path,
        version,
        size,
        modified,
    };
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(t) = toml::to_string(&exe) {
        let _ = std::fs::write(cache, t);
    }
    Some(exe)
}

/// `ssh -V` пишет версию в stderr.
#[cfg(windows)]
fn version_of(exe: &Path) -> Option<String> {
    let argv = [exe.as_os_str().to_owned(), OsString::from("-V")];
    let flags = PipedFlags {
        no_window: true,
        merge_stderr: false,
        kill_on_drop: true,
    };
    let mut child = proc::spawn_piped(&argv, None, &[], flags).ok()?;
    let mut err = child.stderr.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    if child.wait_timeout(Some(Duration::from_secs(5))).is_none() {
        let _ = child.kill_tree();
    }
    let text = reader.join().ok()?;
    text.lines().next().map(|l| l.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2"),
            Some((9, 5))
        );
        assert_eq!(
            parse_version("OpenSSH_for_Windows_8.1p1, LibreSSL 3.0.2"),
            Some((8, 1))
        );
        assert_eq!(
            parse_version("OpenSSH_8.9p1 Ubuntu-3, OpenSSL 3.0.2"),
            Some((8, 9))
        );
        assert_eq!(parse_version("garbage"), None);
        let exe = |v: &str| SshExe {
            path: PathBuf::new(),
            version: v.into(),
            size: 0,
            modified: 0,
        };
        assert!(exe("OpenSSH_for_Windows_9.5p1").bridge_ok());
        assert!(exe("OpenSSH_8.9p1").bridge_ok());
        assert!(!exe("OpenSSH_for_Windows_8.1p1").bridge_ok());
        assert!(!exe("").bridge_ok());
    }

    #[test]
    fn system_first_then_path() {
        let root = tempfile::tempdir().unwrap();
        let sys = root.path().join("System32").join("OpenSSH");
        let other = root.path().join("bin");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("ssh.exe"), "").unwrap();
        let path = std::env::join_paths([&other]).unwrap();
        assert_eq!(
            find(Some(root.path()), Some(&path)),
            Some(other.join("ssh.exe"))
        );
        std::fs::write(sys.join("ssh.exe"), "").unwrap();
        assert_eq!(
            find(Some(root.path()), Some(&path)),
            Some(sys.join("ssh.exe"))
        );
        assert_eq!(find(None, None), None);
    }
}
