//! Изолированный каталог данных `--screens`: конфиг, состояние, логи и кэш прогона
//! (portable-раскладка, как `urfin.portable`) — во временном каталоге, который
//! удаляется в конце. Профиль владельца (`%APPDATA%\Urfin`, `%LOCALAPPDATA%\Urfin`)
//! снимки не читают и не пишут: сцены идут с умолчаниями конфига на любой машине.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use urfin_core::Paths;

/// Каталоги прогонов, брошенные упавшим прогоном (`exit(1)` сцены), удаляются при
/// следующем запуске, если старше этого.
const STALE: Duration = Duration::from_hours(6);

/// Префикс каталога прогона в `<temp>\urfin-screens`.
const PREFIX: &str = "run-";

/// Временный каталог данных прогона; удаляется при `Drop`.
#[derive(Debug)]
pub struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    /// `<temp>\urfin-screens\run-<pid>` — пустой, рядом с демо-деревом сцен.
    pub fn create() -> std::io::Result<Self> {
        Self::create_in(&crate::demo_tree::base().join("urfin-screens"))
    }

    pub(crate) fn create_in(root: &Path) -> std::io::Result<Self> {
        sweep(root);
        let dir = root.join(format!("{PREFIX}{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(Sandbox { dir })
    }

    /// Каталоги приложения внутри песочницы (portable-раскладка).
    pub fn paths(&self) -> Paths {
        Paths::in_dir(self.dir.join("data"))
    }

    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            eprintln!("screens: sandbox {} not removed: {e}", self.dir.display());
        }
    }
}

/// Удалить старые каталоги прогонов (упавший прогон выходит без `Drop`).
fn sweep(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for e in rd.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().starts_with(PREFIX) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STALE);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Sandbox;

    #[test]
    fn paths_inside_and_removed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let sb = Sandbox::create_in(tmp.path()).unwrap();
        let dir = sb.dir().to_path_buf();
        let p = sb.paths();
        assert!(p.portable);
        for d in [&p.config_dir, &p.state_dir, &p.log_dir, &p.cache_dir] {
            assert!(d.starts_with(&dir), "{} вне песочницы", d.display());
        }
        p.ensure().unwrap();
        std::fs::write(p.config_file(), "x").unwrap();
        drop(sb);
        assert!(!dir.exists());
    }
}
