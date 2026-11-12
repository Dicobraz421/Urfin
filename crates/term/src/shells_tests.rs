//! Поиск shells: подменная система и настоящий стенд (приёмка S6-09).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::*;

#[derive(Default)]
struct Fake {
    env: HashMap<&'static str, &'static str>,
    files: HashSet<PathBuf>,
    dirs: HashMap<PathBuf, Vec<String>>,
    reg: HashMap<(String, String), String>,
    subkeys: Option<Vec<String>>,
    wsl_out: Option<Vec<u8>>,
    wsl_called: Cell<bool>,
}

impl Probe for Fake {
    fn env(&self, key: &str) -> Option<String> {
        self.env.get(key).map(|s| (*s).to_owned())
    }
    fn is_file(&self, p: &Path) -> bool {
        self.files.contains(p)
    }
    fn dir_names(&self, p: &Path) -> Vec<String> {
        self.dirs.get(p).cloned().unwrap_or_default()
    }
    fn reg_string(&self, _root: RegRoot, key: &str, value: &str) -> Option<String> {
        self.reg.get(&(key.to_owned(), value.to_owned())).cloned()
    }
    fn reg_subkeys(&self, _root: RegRoot, _key: &str) -> Option<Vec<String>> {
        self.subkeys.clone()
    }
    fn wsl_list(&self) -> Option<Vec<u8>> {
        self.wsl_called.set(true);
        self.wsl_out.clone()
    }
}

fn base() -> Fake {
    let mut f = Fake::default();
    f.env.insert("SystemRoot", r"C:\Windows");
    f.env.insert("ComSpec", r"C:\Windows\System32\cmd.exe");
    f.env.insert("ProgramFiles", r"C:\Program Files");
    f.env.insert("PATH", r"C:\Windows\System32;C:\Tools");
    for p in [
        r"C:\Windows\System32\cmd.exe",
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        r"C:\Windows\System32\wsl.exe",
    ] {
        f.files.insert(PathBuf::from(p));
    }
    f
}

fn ids(v: &[ShellSpec]) -> Vec<&str> {
    v.iter().map(|s| s.id.as_str()).collect()
}

#[test]
#[cfg_attr(not(windows), ignore = "пути и оболочки Windows")]
fn registry_wsl_default_first_docker_hidden() {
    let mut f = base();
    let lxss = r"Software\Microsoft\Windows\CurrentVersion\Lxss";
    f.subkeys = Some(vec!["{a}".into(), "{b}".into(), "{c}".into()]);
    for (g, n) in [
        ("{a}", "Ubuntu"),
        ("{b}", "Debian"),
        ("{c}", "docker-desktop-data"),
    ] {
        f.reg.insert(
            (format!(r"{lxss}\{g}"), "DistributionName".into()),
            n.into(),
        );
    }
    f.reg
        .insert((lxss.into(), "DefaultDistribution".into()), "{b}".into());
    let s = discover(&f, &[]);
    assert_eq!(ids(&s), ["cmd", "powershell", "wsl:Debian", "wsl:Ubuntu"]);
    assert!(
        !f.wsl_called.get(),
        "wsl.exe is not started when the registry is there"
    );
    let wsl = &s[2];
    assert_eq!(wsl.args, ["-d", "Debian"]);
    assert!(matches!(&wsl.path_style, PathStyle::Wsl { distro, .. } if distro == "Debian"));
}

#[test]
#[cfg_attr(not(windows), ignore = "пути и оболочки Windows")]
fn pwsh_highest_version_and_git_from_registry() {
    let mut f = base();
    let ps = PathBuf::from(r"C:\Program Files\PowerShell");
    f.dirs
        .insert(ps.clone(), vec!["7-preview".into(), "6".into(), "7".into()]);
    for v in ["7-preview", "6", "7"] {
        f.files.insert(ps.join(v).join("pwsh.exe"));
    }
    f.reg.insert(
        (r"SOFTWARE\GitForWindows".into(), "InstallPath".into()),
        r"D:\Git".into(),
    );
    f.files.insert(PathBuf::from(r"D:\Git\bin\bash.exe"));
    let s = discover(&f, &[]);
    assert_eq!(ids(&s), ["cmd", "pwsh", "powershell", "gitbash"]);
    assert_eq!(s[1].program, ps.join("7").join("pwsh.exe"));
    assert_eq!(s[3].program, PathBuf::from(r"D:\Git\bin\bash.exe"));
    assert_eq!(s[3].path_style, PathStyle::Posix);
}

#[test]
#[cfg_attr(not(windows), ignore = "пути и оболочки Windows")]
fn wsl_fallback_decodes_utf16_and_user_shells_replace() {
    let mut f = base();
    let text = "\u{feff}Ubuntu\r\ndocker-desktop\r\nAlpine\r\n";
    f.wsl_out = Some(text.encode_utf16().flat_map(u16::to_le_bytes).collect());
    let user = [ShellConfig {
        id: "cmd".into(),
        title: "My cmd".into(),
        program: r"C:\tools\tcc.exe".into(),
        args: vec!["/k".into()],
        kind: "cmd".into(),
        integration: false,
    }];
    let s = discover(&f, &user);
    assert!(f.wsl_called.get());
    assert_eq!(ids(&s), ["powershell", "wsl:Ubuntu", "wsl:Alpine", "cmd"]);
    let mine = s.last().unwrap();
    assert_eq!((mine.title.as_str(), mine.integration), ("My cmd", false));
}

#[test]
fn decode_utf8_and_utf16() {
    assert_eq!(decode_wsl_list(b"Ubuntu\nDebian\n"), ["Ubuntu", "Debian"]);
    let u16: Vec<u8> = "Ubuntu\r\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert_eq!(decode_wsl_list(&u16), ["Ubuntu"]);
}

#[test]
fn cache_roundtrip() {
    let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
    let p = d.path().join("term").join("shells.toml");
    let s = discover(&base(), &[]);
    save_cache(&p, &s).unwrap();
    assert_eq!(load_cache(&p).unwrap(), s);
}

/// Приёмка S6-09 на стенде: найдены cmd и Windows PowerShell; реестр — быстро,
/// `wsl.exe` не запускается, если WSL есть в реестре.
#[test]
#[cfg_attr(not(windows), ignore = "нужны ConPTY и оболочки Windows")]
fn stand_discovery() {
    struct Counting(SystemProbe, Cell<bool>);
    impl Probe for Counting {
        fn env(&self, k: &str) -> Option<String> {
            self.0.env(k)
        }
        fn is_file(&self, p: &Path) -> bool {
            self.0.is_file(p)
        }
        fn dir_names(&self, p: &Path) -> Vec<String> {
            self.0.dir_names(p)
        }
        fn reg_string(&self, r: RegRoot, k: &str, v: &str) -> Option<String> {
            self.0.reg_string(r, k, v)
        }
        fn reg_subkeys(&self, r: RegRoot, k: &str) -> Option<Vec<String>> {
            self.0.reg_subkeys(r, k)
        }
        fn wsl_list(&self) -> Option<Vec<u8>> {
            self.1.set(true);
            self.0.wsl_list()
        }
    }
    let p = Counting(SystemProbe, Cell::new(false));
    let t0 = Instant::now();
    let s = discover(&p, &[]);
    let dt = t0.elapsed();
    eprintln!("shells: {:?} in {dt:?}", ids(&s));
    assert!(ids(&s).contains(&"cmd") && ids(&s).contains(&"powershell"));
    let lxss = SystemProbe.reg_subkeys(RegRoot::CurrentUser, LXSS);
    if lxss.is_some() {
        assert!(!p.1.get(), "registry present: wsl.exe must not run");
    }
    let t1 = Instant::now();
    let _ = SystemProbe.reg_subkeys(RegRoot::CurrentUser, LXSS);
    assert!(
        t1.elapsed().as_millis() < 5,
        "registry read {:?}",
        t1.elapsed()
    );
}

/// L1-06: `$SHELL` первым, `/etc/shells` без комментариев, повторов имени и
/// отсутствующих файлов; свои из конфига заменяют одноимённые.
#[test]
fn unix_login_shell_first_then_etc_shells() {
    let mut f = Fake::default();
    f.env.insert("SHELL", "/usr/bin/zsh");
    for p in ["/usr/bin/zsh", "/bin/sh", "/bin/bash", "/usr/bin/bash"] {
        f.files.insert(PathBuf::from(p));
    }
    let etc = "# /etc/shells\n/bin/sh\n\n/bin/bash\n/usr/bin/bash\n/usr/bin/fish\n/usr/bin/zsh\n";
    let v = discover_unix(&f, etc, &[]);
    assert_eq!(ids(&v), ["zsh", "sh", "bash"]);
    assert_eq!(v[0].kind, ShellKind::Zsh);
    assert_eq!(v[1].kind, ShellKind::Other);
    assert_eq!(v[2].program, PathBuf::from("/bin/bash"));
    assert!(v.iter().all(|s| s.path_style == PathStyle::Posix));
    assert!(!f.wsl_called.get());

    let user = [ShellConfig {
        id: "bash".into(),
        program: "/opt/bash".into(),
        ..ShellConfig::default()
    }];
    let v = discover_unix(&f, etc, &user);
    assert_eq!(ids(&v), ["zsh", "sh", "bash"]);
    assert_eq!(v[2].program, PathBuf::from("/opt/bash"));
}
