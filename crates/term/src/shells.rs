//! Shells (S6 §4.1, S6-09): виды, описание и поиск на Windows — cmd, pwsh,
//! Windows PowerShell, Git Bash, дистрибутивы WSL (по реестру, без запуска
//! `wsl.exe`), свои из `[[term.shells]]`. Кэш — `state_dir/term/shells.toml`.

use std::path::{Path, PathBuf};

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::config::ShellConfig;

/// Вид shell: от него — интеграция (S6 §5) и стиль путей.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellKind {
    Cmd,
    Pwsh,
    WinPowerShell,
    Bash,
    Zsh,
    Fish,
    Wsl { distro: CompactString },
    Other,
}

/// Как shell пишет пути.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathStyle {
    Windows,
    /// Git Bash (MSYS): `/c/Users/...`.
    Posix,
    Wsl {
        distro: CompactString,
        mount_root: CompactString,
    },
}

/// Найденный или заданный shell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellSpec {
    pub id: CompactString,
    pub title: CompactString,
    pub kind: ShellKind,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub integration: bool,
    pub path_style: PathStyle,
}

impl ShellSpec {
    fn new(id: &str, title: &str, kind: ShellKind, program: PathBuf, args: &[&str]) -> ShellSpec {
        let path_style = match &kind {
            ShellKind::Wsl { distro } => PathStyle::Wsl {
                distro: distro.clone(),
                mount_root: "/mnt/".into(),
            },
            ShellKind::Bash | ShellKind::Zsh | ShellKind::Fish => PathStyle::Posix,
            _ => PathStyle::Windows,
        };
        ShellSpec {
            id: id.into(),
            title: title.into(),
            kind,
            program,
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            env: Vec::new(),
            integration: true,
            path_style,
        }
    }
}

/// Корень реестра (для [`Probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegRoot {
    CurrentUser,
    LocalMachine,
}

/// Окружение, ФС и реестр для поиска (подменяется в тестах).
pub trait Probe {
    fn env(&self, key: &str) -> Option<String>;
    fn is_file(&self, p: &Path) -> bool;
    fn dir_names(&self, p: &Path) -> Vec<String>;
    fn reg_string(&self, root: RegRoot, key: &str, value: &str) -> Option<String>;
    fn reg_subkeys(&self, root: RegRoot, key: &str) -> Option<Vec<String>>;
    /// `wsl.exe -l -q` с `WSL_UTF8=1` — только запасной путь без реестра.
    fn wsl_list(&self) -> Option<Vec<u8>>;
}

/// Настоящая система.
#[derive(Debug, Default)]
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn is_file(&self, p: &Path) -> bool {
        p.is_file()
    }

    fn dir_names(&self, p: &Path) -> Vec<String> {
        std::fs::read_dir(p).map_or_else(
            |_| Vec::new(),
            |rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            },
        )
    }

    fn reg_string(&self, root: RegRoot, key: &str, value: &str) -> Option<String> {
        #[cfg(windows)]
        {
            crate::sys::registry::string(reg_root(root), key, value)
        }
        #[cfg(not(windows))]
        {
            let _ = (root, key, value);
            None
        }
    }

    fn reg_subkeys(&self, root: RegRoot, key: &str) -> Option<Vec<String>> {
        #[cfg(windows)]
        {
            crate::sys::registry::subkeys(reg_root(root), key)
        }
        #[cfg(not(windows))]
        {
            let _ = (root, key);
            None
        }
    }

    fn wsl_list(&self) -> Option<Vec<u8>> {
        let out = std::process::Command::new("wsl.exe")
            .args(["-l", "-q"])
            .env("WSL_UTF8", "1")
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    }
}

#[cfg(windows)]
fn reg_root(r: RegRoot) -> crate::sys::registry::Root {
    match r {
        RegRoot::CurrentUser => crate::sys::registry::Root::CurrentUser,
        RegRoot::LocalMachine => crate::sys::registry::Root::LocalMachine,
    }
}

/// Вывод `wsl -l -q`: UTF-16LE (NUL-байты) или UTF-8 (`WSL_UTF8=1`); строки без
/// пустых, BOM и `\r`.
pub fn decode_wsl_list(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.len().is_multiple_of(2) && bytes.contains(&0) {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|l| l.trim().trim_matches('\0').to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

/// Скрытые дистрибутивы WSL (служебные).
fn hidden(distro: &str) -> bool {
    distro.to_ascii_lowercase().starts_with("docker-desktop")
}

/// Дистрибутивы WSL: реестр (по умолчанию — первым), без реестра — `wsl -l -q`.
fn wsl_distros(p: &dyn Probe, system: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(guids) = p.reg_subkeys(RegRoot::CurrentUser, LXSS) {
        let default = p.reg_string(RegRoot::CurrentUser, LXSS, "DefaultDistribution");
        let mut named: Vec<(bool, String)> = guids
            .iter()
            .filter_map(|g| {
                let name = p.reg_string(
                    RegRoot::CurrentUser,
                    &format!(r"{LXSS}\{g}"),
                    "DistributionName",
                )?;
                Some((default.as_deref() == Some(g.as_str()), name))
            })
            .collect();
        named.sort_by_key(|(is_default, _)| !*is_default);
        out.extend(named.into_iter().map(|(_, n)| n));
    } else if p.is_file(&system.join("wsl.exe")) {
        out = p
            .wsl_list()
            .map(|b| decode_wsl_list(&b))
            .unwrap_or_default();
    }
    out.retain(|d| !hidden(d));
    out
}

/// pwsh 7: PATH → `%ProgramFiles%\PowerShell\<версия>\pwsh.exe` (старшая, preview
/// позже) → Store-алиас.
fn find_pwsh(p: &dyn Probe) -> Option<PathBuf> {
    if let Some(path) = p.env("PATH") {
        for dir in path.split(';').filter(|d| !d.is_empty()) {
            let c = Path::new(dir).join("pwsh.exe");
            if p.is_file(&c) {
                return Some(c);
            }
        }
    }
    if let Some(pf) = p.env("ProgramFiles") {
        let root = Path::new(&pf).join("PowerShell");
        let mut vers = p.dir_names(&root);
        vers.sort_by_key(|v| {
            let major: u32 = v
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            (std::cmp::Reverse(major), v.contains("preview"))
        });
        for v in vers {
            let c = root.join(v).join("pwsh.exe");
            if p.is_file(&c) {
                return Some(c);
            }
        }
    }
    let local = p.env("LOCALAPPDATA")?;
    let c = Path::new(&local).join(r"Microsoft\WindowsApps\pwsh.exe");
    p.is_file(&c).then_some(c)
}

fn kind_of(s: &str) -> ShellKind {
    match s {
        "cmd" => ShellKind::Cmd,
        "pwsh" => ShellKind::Pwsh,
        "powershell" => ShellKind::WinPowerShell,
        "bash" => ShellKind::Bash,
        "zsh" => ShellKind::Zsh,
        "fish" => ShellKind::Fish,
        _ => ShellKind::Other,
    }
}

/// Поиск shells (Windows, S6 §4.1) и свои из конфига (одноимённые заменяют найденные).
pub fn discover(p: &dyn Probe, user: &[ShellConfig]) -> Vec<ShellSpec> {
    let root = p.env("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let system = Path::new(&root).join("System32");
    let mut out = Vec::new();
    let comspec = p
        .env("ComSpec")
        .map_or_else(|| system.join("cmd.exe"), PathBuf::from);
    if p.is_file(&comspec) {
        out.push(ShellSpec::new("cmd", "cmd", ShellKind::Cmd, comspec, &[]));
    }
    if let Some(pwsh) = find_pwsh(p) {
        out.push(ShellSpec::new(
            "pwsh",
            "pwsh",
            ShellKind::Pwsh,
            pwsh,
            &["-NoLogo"],
        ));
    }
    let ps = system.join(r"WindowsPowerShell\v1.0\powershell.exe");
    if p.is_file(&ps) {
        out.push(ShellSpec::new(
            "powershell",
            "Windows PowerShell",
            ShellKind::WinPowerShell,
            ps,
            &["-NoLogo"],
        ));
    }
    // Git for Windows: для всех (HKLM, Program Files) или для пользователя (HKCU,
    // `%LOCALAPPDATA%\Programs\Git`).
    let git = [RegRoot::LocalMachine, RegRoot::CurrentUser]
        .into_iter()
        .filter_map(|root| p.reg_string(root, r"SOFTWARE\GitForWindows", "InstallPath"))
        .map(|d| Path::new(&d).join(r"bin\bash.exe"))
        .chain(
            [
                ("ProgramFiles", r"Git\bin\bash.exe"),
                ("LOCALAPPDATA", r"Programs\Git\bin\bash.exe"),
            ]
            .into_iter()
            .filter_map(|(var, rel)| Some(Path::new(&p.env(var)?).join(rel))),
        )
        .find(|b| p.is_file(b));
    if let Some(bash) = git {
        out.push(ShellSpec::new(
            "gitbash",
            "Git Bash",
            ShellKind::Bash,
            bash,
            &["--login", "-i"],
        ));
    }
    for d in wsl_distros(p, &system) {
        let mut s = ShellSpec::new(
            &format!("wsl:{d}"),
            &format!("WSL {d}"),
            ShellKind::Wsl {
                distro: d.as_str().into(),
            },
            system.join("wsl.exe"),
            &["-d", &d],
        );
        s.integration = true;
        out.push(s);
    }
    apply_user(&mut out, user);
    out
}

/// Свои shells из конфига: одноимённые заменяют найденные.
fn apply_user(out: &mut Vec<ShellSpec>, user: &[ShellConfig]) {
    for u in user
        .iter()
        .filter(|u| !u.id.is_empty() && !u.program.is_empty())
    {
        out.retain(|s| s.id != u.id);
        let mut s = ShellSpec::new(
            &u.id,
            if u.title.is_empty() { &u.id } else { &u.title },
            kind_of(&u.kind),
            PathBuf::from(u.program.as_str()),
            &[],
        );
        s.args = u.args.iter().map(ToString::to_string).collect();
        s.integration = u.integration;
        out.push(s);
    }
}

/// Поиск shells на unix (L1-06): `$SHELL` — первым, дальше строки `/etc/shells`
/// (`etc_shells` — содержимое файла); id — имя файла, повторы имени пропускаются.
/// Свои из конфига — как в [`discover`].
pub fn discover_unix(p: &dyn Probe, etc_shells: &str, user: &[ShellConfig]) -> Vec<ShellSpec> {
    let login = p.env("SHELL");
    let listed = etc_shells
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'));
    let mut out: Vec<ShellSpec> = Vec::new();
    for path in login.as_deref().into_iter().chain(listed) {
        let program = PathBuf::from(path);
        let Some(name) = program.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if out.iter().any(|s| s.id == name) || !p.is_file(&program) {
            continue;
        }
        let mut s = ShellSpec::new(name, name, kind_of(name), program.clone(), &[]);
        s.path_style = PathStyle::Posix;
        out.push(s);
    }
    apply_user(&mut out, user);
    out
}

/// Shells этой системы: [`discover`] на Windows, [`discover_unix`] на unix.
pub fn discover_system(user: &[ShellConfig]) -> Vec<ShellSpec> {
    #[cfg(not(unix))]
    {
        discover(&SystemProbe, user)
    }
    #[cfg(unix)]
    {
        let etc = std::fs::read_to_string("/etc/shells").unwrap_or_default();
        discover_unix(&SystemProbe, &etc, user)
    }
}

/// Кэш найденного: `state_dir/term/shells.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    shells: Vec<ShellSpec>,
}

pub fn save_cache(path: &Path, shells: &[ShellSpec]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let text = toml::to_string(&Cache {
        shells: shells.to_vec(),
    })
    .map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

pub fn load_cache(path: &Path) -> Option<Vec<ShellSpec>> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str::<Cache>(&text).ok().map(|c| c.shells)
}

#[cfg(test)]
#[path = "shells_tests.rs"]
mod tests;
