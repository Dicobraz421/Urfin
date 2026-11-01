//! Обнаружение менеджеров (F02 §2.2): без процессов — окружение и файлы, ≤5 мс.
//! Не найден — `None`, без записи в лог. Версия — лениво, процессом (`--version`);
//! найденное с версиями — кэш `state/pkg/managers.toml`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::api::{Detected, Mgr, MgrCaps};

/// Имя кэша в `state_dir/pkg`.
pub const MANAGERS_FILE: &str = "managers.toml";

/// Окружение и ФС (подменяются в тестах).
pub trait Probe {
    fn env(&self, key: &str) -> Option<String>;
    fn is_file(&self, p: &Path) -> bool;
}

/// Настоящая система.
#[derive(Debug, Default)]
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    /// Алиас App Installer (`winget.exe`, reparse `APPEXECLINK`) не открывается как
    /// файл — тогда метаданные самой точки повторной обработки.
    fn is_file(&self, p: &Path) -> bool {
        std::fs::metadata(p).map_or_else(
            |_| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()),
            |m| m.is_file(),
        )
    }
}

fn join(base: &str, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(PathBuf::from(base), |p, part| p.join(part))
}

/// winget: PATH, иначе `%LOCALAPPDATA%\Microsoft\WindowsApps`.
fn find_winget(p: &dyn Probe) -> Option<PathBuf> {
    let path = p.env("PATH").unwrap_or_default();
    path.split(';')
        .map(|d| d.trim().trim_matches('"'))
        .filter(|d| !d.is_empty())
        .map(|d| join(d, &["winget.exe"]))
        .chain(
            p.env("LOCALAPPDATA")
                .map(|l| join(&l, &["Microsoft", "WindowsApps", "winget.exe"])),
        )
        .find(|c| p.is_file(c))
}

/// scoop: `%SCOOP%\shims\scoop.ps1`, иначе `%USERPROFILE%\scoop\shims`.
fn find_scoop(p: &dyn Probe) -> Option<PathBuf> {
    p.env("SCOOP")
        .map(|s| join(&s, &["shims", "scoop.ps1"]))
        .into_iter()
        .chain(
            p.env("USERPROFILE")
                .map(|u| join(&u, &["scoop", "shims", "scoop.ps1"])),
        )
        .find(|c| p.is_file(c))
}

/// choco: `%ChocolateyInstall%\bin`, иначе `%ProgramData%\chocolatey\bin`.
fn find_choco(p: &dyn Probe) -> Option<PathBuf> {
    let data = p
        .env("ProgramData")
        .unwrap_or_else(|| r"C:\ProgramData".into());
    p.env("ChocolateyInstall")
        .map(|c| join(&c, &["bin", "choco.exe"]))
        .into_iter()
        .chain(Some(join(&data, &["chocolatey", "bin", "choco.exe"])))
        .find(|c| p.is_file(c))
}

/// PowerShell для скриптов scoop (F02 §2.2): `pwsh.exe` из PATH, иначе Windows
/// PowerShell 5.1 из `%SystemRoot%`. Приложение передаёт найденный S6
/// (`term/shells.toml`); это — запасной путь.
pub fn find_powershell(p: &dyn Probe) -> PathBuf {
    let path = p.env("PATH").unwrap_or_default();
    path.split(';')
        .map(|d| d.trim().trim_matches('"'))
        .filter(|d| !d.is_empty())
        .map(|d| join(d, &["pwsh.exe"]))
        .find(|c| p.is_file(c))
        .unwrap_or_else(|| windows_powershell(p))
}

/// Windows PowerShell 5.1: есть в каждой Windows 10/11.
pub fn windows_powershell(p: &dyn Probe) -> PathBuf {
    let root = p.env("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    join(
        &root,
        &["System32", "WindowsPowerShell", "v1.0", "powershell.exe"],
    )
}

/// Менеджер `mgr` на машине; версия ещё не известна.
pub fn detect(mgr: Mgr, p: &dyn Probe) -> Option<Detected> {
    let exe = match mgr {
        Mgr::Winget => find_winget(p),
        Mgr::Scoop => find_scoop(p),
        Mgr::Choco => find_choco(p),
    }?;
    Some(Detected {
        mgr,
        exe,
        version: None,
        caps: MgrCaps::empty(),
    })
}

/// Все найденные менеджеры в порядке `Mgr::ALL`.
pub fn detect_all() -> Vec<Detected> {
    detect_all_in(&SystemProbe)
}

pub fn detect_all_in(p: &dyn Probe) -> Vec<Detected> {
    Mgr::ALL.into_iter().filter_map(|m| detect(m, p)).collect()
}

/// argv запроса версии. scoop — скрипт PowerShell: его запуск — у адаптера (F02-05).
pub fn version_argv(d: &Detected) -> Option<Vec<OsString>> {
    match d.mgr {
        Mgr::Winget | Mgr::Choco => Some(vec![d.exe.clone().into(), "--version".into()]),
        Mgr::Scoop => None,
    }
}

/// Версия из вывода `--version`: первое слово, начинающееся с цифры (`v` впереди
/// снимается): winget `v1.9.25200`, choco `2.3.0`, `Chocolatey v0.12.1`.
pub fn parse_version(lines: &[String]) -> Option<CompactString> {
    lines
        .iter()
        .flat_map(|l| l.split_whitespace())
        .map(|w| w.strip_prefix(['v', 'V']).unwrap_or(w))
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .map(CompactString::from)
}

/// Возможности, известные по версии: choco 0.x/1.x — синтаксис v1 (F02 §2.3).
pub fn caps_for(mgr: Mgr, version: &str) -> MgrCaps {
    let major: Option<u32> = version
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|m| m.parse().ok());
    match (mgr, major) {
        (Mgr::Choco, Some(m)) if m < 2 => MgrCaps::CHOCO_V1,
        _ => MgrCaps::empty(),
    }
}

/// Версия менеджера: из `d`, иначе процессом (таймаут — `cx.timeout`); найденное
/// пишется в `d` вместе с возможностями.
#[cfg(windows)]
pub fn query_version(
    d: &mut Detected,
    cx: &crate::api::RunCx,
) -> Result<CompactString, crate::api::PkgError> {
    use crate::api::PkgError;
    if let Some(v) = &d.version {
        return Ok(v.clone());
    }
    let argv = version_argv(d).ok_or(PkgError::NotFound)?;
    let out = crate::proc::run_query(&argv, cx)?.ok()?;
    let v = parse_version(&out.lines).ok_or_else(|| PkgError::Parse {
        line: out
            .lines
            .first()
            .map(String::as_str)
            .unwrap_or_default()
            .into(),
    })?;
    d.caps |= caps_for(d.mgr, &v);
    d.version = Some(v.clone());
    Ok(v)
}

/// Кэш найденного (`state/pkg/managers.toml`): версии не перезапрашиваются на
/// каждом открытии `pkg:`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Managers {
    #[serde(default)]
    pub managers: Vec<Detected>,
}

impl Managers {
    /// Нет файла или он битый — `None` (обнаружение заново).
    pub fn load(path: &Path) -> Option<Managers> {
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok()
    }

    /// Атомарно: временный файл рядом и переименование.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = toml::to_string(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    pub fn get(&self, mgr: Mgr) -> Option<&Detected> {
        self.managers.iter().find(|d| d.mgr == mgr)
    }

    /// Свежее обнаружение: пропавшие уходят, версия и возможности остаются у
    /// менеджера с тем же exe (другой exe — версия заново).
    pub fn refresh(&mut self, fresh: Vec<Detected>) {
        self.managers = fresh
            .into_iter()
            .map(|mut d| {
                if let Some(old) = self.get(d.mgr).filter(|o| o.exe == d.exe) {
                    d.version.clone_from(&old.version);
                    d.caps |= old.caps;
                }
                d
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;

    #[derive(Default)]
    struct Fake {
        env: HashMap<&'static str, String>,
        files: HashSet<PathBuf>,
    }

    impl Fake {
        fn env(mut self, k: &'static str, v: &str) -> Fake {
            self.env.insert(k, v.into());
            self
        }

        fn file(mut self, base: &str, parts: &[&str]) -> Fake {
            self.files.insert(join(base, parts));
            self
        }
    }

    impl Probe for Fake {
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }

        fn is_file(&self, p: &Path) -> bool {
            self.files.contains(p)
        }
    }

    fn exes(p: &Fake) -> Vec<(Mgr, PathBuf)> {
        detect_all_in(p)
            .into_iter()
            .map(|d| (d.mgr, d.exe))
            .collect()
    }

    #[test]
    fn nothing_installed() {
        let p = Fake::default().env("PATH", r"C:\Windows;C:\Tools");
        assert_eq!(exes(&p), []);
        assert_eq!(detect(Mgr::Choco, &p), None);
    }

    #[test]
    fn winget_in_path_first() {
        let p = Fake::default()
            .env(
                "PATH",
                r#"C:\Windows;"C:\Tools";C:\Users\me\AppData\Local\Microsoft\WindowsApps"#,
            )
            .env("LOCALAPPDATA", r"C:\Users\me\AppData\Local")
            .file(r"C:\Tools", &["winget.exe"])
            .file(
                r"C:\Users\me\AppData\Local",
                &["Microsoft", "WindowsApps", "winget.exe"],
            );
        assert_eq!(
            exes(&p),
            [(Mgr::Winget, join(r"C:\Tools", &["winget.exe"]))]
        );
    }

    #[test]
    fn winget_alias_without_path() {
        let p = Fake::default()
            .env("LOCALAPPDATA", r"C:\Users\me\AppData\Local")
            .file(
                r"C:\Users\me\AppData\Local",
                &["Microsoft", "WindowsApps", "winget.exe"],
            );
        assert_eq!(
            exes(&p),
            [(
                Mgr::Winget,
                join(
                    r"C:\Users\me\AppData\Local",
                    &["Microsoft", "WindowsApps", "winget.exe"]
                )
            )]
        );
    }

    #[test]
    fn scoop_env_then_profile() {
        let profile = Fake::default()
            .env("USERPROFILE", r"C:\Users\me")
            .file(r"C:\Users\me", &["scoop", "shims", "scoop.ps1"]);
        assert_eq!(
            exes(&profile),
            [(
                Mgr::Scoop,
                join(r"C:\Users\me", &["scoop", "shims", "scoop.ps1"])
            )]
        );
        // `%SCOOP%` задан — профиль не смотрится раньше него.
        let custom = profile
            .env("SCOOP", r"D:\scoop")
            .file(r"D:\scoop", &["shims", "scoop.ps1"]);
        assert_eq!(
            exes(&custom),
            [(Mgr::Scoop, join(r"D:\scoop", &["shims", "scoop.ps1"]))]
        );
    }

    #[test]
    fn choco_env_then_program_data() {
        let default = Fake::default().file(r"C:\ProgramData", &["chocolatey", "bin", "choco.exe"]);
        assert_eq!(
            exes(&default),
            [(
                Mgr::Choco,
                join(r"C:\ProgramData", &["chocolatey", "bin", "choco.exe"])
            )]
        );
        let custom = Fake::default()
            .env("ChocolateyInstall", r"D:\choco")
            .file(r"D:\choco", &["bin", "choco.exe"]);
        assert_eq!(
            exes(&custom),
            [(Mgr::Choco, join(r"D:\choco", &["bin", "choco.exe"]))]
        );
    }

    #[test]
    fn powershell_pwsh_then_builtin() {
        let p = Fake::default()
            .env("PATH", r"C:\Windows;C:\Program Files\PowerShell\7")
            .env("SystemRoot", r"D:\Win")
            .file(r"C:\Program Files\PowerShell\7", &["pwsh.exe"]);
        assert_eq!(
            find_powershell(&p),
            PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe")
        );
        let p = Fake::default().env("SystemRoot", r"D:\Win");
        assert_eq!(
            find_powershell(&p),
            PathBuf::from(r"D:\Win\System32\WindowsPowerShell\v1.0\powershell.exe")
        );
    }

    #[test]
    fn all_three_in_order() {
        let p = Fake::default()
            .env("PATH", r"C:\bin")
            .env("USERPROFILE", r"C:\Users\me")
            .file(r"C:\bin", &["winget.exe"])
            .file(r"C:\Users\me", &["scoop", "shims", "scoop.ps1"])
            .file(r"C:\ProgramData", &["chocolatey", "bin", "choco.exe"]);
        let mgrs: Vec<Mgr> = exes(&p).into_iter().map(|(m, _)| m).collect();
        assert_eq!(mgrs, Mgr::ALL);
    }

    #[test]
    fn versions() {
        let v = |s: &str| parse_version(&[String::new(), s.to_owned()]);
        assert_eq!(v("v1.9.25200").as_deref(), Some("1.9.25200"));
        assert_eq!(v("2.3.0").as_deref(), Some("2.3.0"));
        assert_eq!(v("Chocolatey v0.12.1").as_deref(), Some("0.12.1"));
        assert_eq!(v("no version here"), None);
        assert_eq!(caps_for(Mgr::Choco, "1.4.0"), MgrCaps::CHOCO_V1);
        assert_eq!(caps_for(Mgr::Choco, "0.12.1"), MgrCaps::CHOCO_V1);
        assert_eq!(caps_for(Mgr::Choco, "2.3.0"), MgrCaps::empty());
        assert_eq!(caps_for(Mgr::Winget, "1.2"), MgrCaps::empty());
    }

    fn det(mgr: Mgr, exe: &str, version: Option<&str>) -> Detected {
        Detected {
            mgr,
            exe: exe.into(),
            version: version.map(Into::into),
            caps: if version == Some("1.4.0") {
                MgrCaps::CHOCO_V1
            } else {
                MgrCaps::empty()
            },
        }
    }

    #[test]
    fn refresh_keeps_versions_of_same_exe() {
        let mut m = Managers {
            managers: vec![
                det(Mgr::Winget, "a/winget.exe", Some("1.9")),
                det(Mgr::Scoop, "s/scoop.ps1", None),
                det(Mgr::Choco, "c/choco.exe", Some("1.4.0")),
            ],
        };
        m.refresh(vec![
            det(Mgr::Winget, "b/winget.exe", None),
            det(Mgr::Choco, "c/choco.exe", None),
        ]);
        assert_eq!(
            m.managers,
            [
                det(Mgr::Winget, "b/winget.exe", None),
                det(Mgr::Choco, "c/choco.exe", Some("1.4.0")),
            ]
        );
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg").join(MANAGERS_FILE);
        assert_eq!(Managers::load(&path), None);
        let m = Managers {
            managers: vec![
                det(Mgr::Winget, r"C:\winget.exe", Some("1.9.25200")),
                det(Mgr::Choco, r"C:\choco.exe", Some("1.4.0")),
            ],
        };
        m.save(&path).unwrap();
        assert_eq!(Managers::load(&path), Some(m.clone()));
        insta::assert_snapshot!(toml::to_string(&m).unwrap());
        std::fs::write(&path, "managers = 5").unwrap();
        assert_eq!(Managers::load(&path), None);
    }

    /// Стенд: все установленные менеджеры находятся, `detect_all` ≤5 мс.
    #[test]
    #[ignore = "стенд: настоящие менеджеры"]
    fn stand_detect_all() {
        let t = std::time::Instant::now();
        let found = detect_all();
        let took = t.elapsed();
        eprintln!("{took:?}: {found:#?}");
        assert!(took <= std::time::Duration::from_millis(5), "{took:?}");
    }
}
