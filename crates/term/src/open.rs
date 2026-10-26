//! Enter по файлу (S6-19, S6 §7) — последнее звено цепочки `OpenResolver` (S0 v3 №9):
//! `term.open {path, verb?}`.
//!
//! 1. Классификация: расширение из `PATHEXT` (и `.ps1`); у `.exe`/`.com` — поле
//!    `Subsystem` PE-заголовка (≤1 КБ чтения, полоса Interactive). Прочие файлы — по
//!    расширению, без чтения: недоступный сетевой путь не держит полосу.
//!    (Только Windows: exec-бит, shebang, Mach-O и ELF — не нужны, S0 №57.)
//! 2. Консольная программа или скрипт — в активном сеансе, в синтаксисе shell, с cwd
//!    = каталог файла (ленивая синхронизация): pwsh `& '.\x.exe'`, cmd `".\x.exe"`,
//!    posix `./x`; `.ps1` вне PowerShell — `pwsh -File`.
//! 3. GUI-программы и остальные файлы — `ShellExecuteExW` на своём коротком потоке
//!    (ассоциация ОС, сеанс не занят). Варианты: `new_window` (Shift+Enter — новое
//!    окно консоли), `runas`, `openas`.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::quote::{self, Style};
use crate::shells::ShellKind;

/// Подсистема PE.
pub const SUBSYSTEM_GUI: u16 = 2;
pub const SUBSYSTEM_CONSOLE: u16 = 3;

/// Вызов `ShellExecuteExW`: verb (`None` — по умолчанию), файл, параметры, каталог.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub verb: Option<&'static str>,
    pub file: PathBuf,
    pub params: Option<std::ffi::OsString>,
    pub dir: Option<PathBuf>,
}

/// Что за файл.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Консольная программа: в сеансе.
    Console,
    /// GUI-программа: ассоциацией ОС, сеанс свободен.
    Gui,
    /// `.bat`/`.cmd`.
    Batch,
    /// `.ps1`.
    Ps1,
    /// Остальное — ассоциацией ОС.
    Document,
}

/// Вариант открытия (`verb`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Default,
    /// Shift+Enter: новое окно консоли ОС.
    NewWindow,
    RunAs,
    OpenAs,
}

impl Verb {
    pub fn parse(s: Option<&str>) -> Option<Verb> {
        Some(match s {
            None | Some("" | "open") => Verb::Default,
            Some("new_window") => Verb::NewWindow,
            Some("runas") => Verb::RunAs,
            Some("openas") => Verb::OpenAs,
            Some(_) => return None,
        })
    }
}

/// Поле `Subsystem` PE по началу файла; `None` — не PE или заголовок дальше буфера.
pub fn pe_subsystem(head: &[u8]) -> Option<u16> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(head.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(head.get(o..o + 4)?.try_into().ok()?));
    if head.get(..2)? != b"MZ" {
        return None;
    }
    let pe = usize::try_from(u32_at(0x3C)?).ok()?;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    // COFF-заголовок 20 байт, затем optional header: magic 0x10b (PE32) или 0x20b
    // (PE32+); Subsystem — на смещении 68 у обоих.
    let opt = pe + 24;
    match u16_at(opt)? {
        0x10b | 0x20b => u16_at(opt + 68),
        _ => None,
    }
}

/// Расширение в нижнем регистре.
fn ext(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Вид по расширению (без чтения); `None` — исполняемый PE, нужен заголовок.
pub fn kind_by_ext(path: &Path) -> Option<Kind> {
    match ext(path).as_str() {
        "exe" | "com" => None,
        "bat" | "cmd" => Some(Kind::Batch),
        "ps1" => Some(Kind::Ps1),
        // Прочие из PATHEXT (`.vbs`, `.js`, `.msc`) — их хозяин сам GUI (wscript, mmc).
        _ => Some(Kind::Document),
    }
}

/// Классифицировать (вне UI-потока): `.exe`/`.com` — по PE-заголовку.
pub fn classify(path: &Path) -> Kind {
    if let Some(k) = kind_by_ext(path) {
        return k;
    }
    let mut head = vec![0u8; 1024];
    let n = std::fs::File::open(path)
        .and_then(|mut f| {
            let mut n = 0;
            while n < head.len() {
                match f.read(&mut head[n..])? {
                    0 => break,
                    k => n += k,
                }
            }
            Ok(n)
        })
        .unwrap_or(0);
    head.truncate(n);
    match pe_subsystem(&head) {
        Some(SUBSYSTEM_GUI) => Kind::Gui,
        // Консоль, DOS-`.com`, не прочиталось — в сеансе (вывод покажет, что не так).
        _ => Kind::Console,
    }
}

/// Строка для сеанса: запуск файла `name` из его каталога в синтаксисе shell.
/// `pwsh` — чем запускать `.ps1` вне PowerShell (`pwsh` или `powershell`).
pub fn exec_line(name: &str, kind: Kind, shell: &ShellKind, pwsh: &str) -> String {
    let style = Style::of(shell);
    match (style, kind) {
        (Style::Pwsh, _) => format!("& '.\\{}'", name.replace('\'', "''")),
        (Style::Cmd, Kind::Ps1) => format!("{pwsh} -File \".\\{name}\""),
        // `.\`: с `NoDefaultCurrentDirectoryInExePath` cmd не ищет в текущем каталоге.
        (Style::Cmd, _) => format!("\".\\{name}\""),
        (Style::Posix, Kind::Ps1) => format!("{pwsh}.exe -File {}", quote::quote(name, style)),
        (Style::Posix, Kind::Batch) => format!("cmd.exe /c {}", quote::quote(name, style)),
        (Style::Posix, _) => format!("./{}", quote::quote(name, style)),
    }
}

/// Что вызвать в `ShellExecuteExW`: файл, параметры, verb.
pub fn shell_spec(path: &Path, kind: Kind, verb: Verb, pwsh: &str) -> Spec {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf);
    let plain = |verb| Spec {
        verb,
        file: path.to_path_buf(),
        params: None,
        dir: dir.clone(),
    };
    match (verb, kind) {
        // `.ps1` по умолчанию открывается в Блокноте: новое окно — через PowerShell.
        (Verb::NewWindow, Kind::Ps1) => Spec {
            verb: None,
            file: PathBuf::from(format!("{pwsh}.exe")),
            params: Some(format!("-NoExit -File \"{}\"", path.display()).into()),
            dir: dir.clone(),
        },
        (Verb::NewWindow | Verb::Default, _) => plain(None),
        (Verb::RunAs, _) => plain(Some("runas")),
        (Verb::OpenAs, _) => plain(Some("openas")),
    }
}

/// Строка cmdline в новом окне консоли ОС (`cmdline.exec detached`, Shift+Enter):
/// shell с ключом «выполнить и остаться».
pub fn detached_spec(program: &Path, kind: &ShellKind, line: &str, cwd: Option<PathBuf>) -> Spec {
    let params = match kind {
        ShellKind::Cmd => format!("/k {line}"),
        ShellKind::Pwsh | ShellKind::WinPowerShell => {
            format!("-NoExit -Command {}", quote::quote(line, Style::Pwsh))
        }
        _ => format!(
            "-c {}",
            quote::quote(&format!("{line}; exec bash"), Style::Posix)
        ),
    };
    Spec {
        verb: None,
        file: program.to_path_buf(),
        params: Some(params.into()),
        dir: cwd,
    }
}

// ---- `term.open` ----

/// `term.open {path?, verb?}`: без пути — файл под курсором активной панели.
pub(crate) fn handle(
    cx: &mut urfin_core::Cx<'_>,
    a: &urfin_core::Args,
) -> Result<(), urfin_core::CmdError> {
    let verb = Verb::parse(a.str("verb")).ok_or_else(|| urfin_core::CmdError::Arg {
        name: "verb".into(),
        msg: "new_window|runas|openas".into(),
    })?;
    let path = if let Some(p) = a.str("path") {
        PathBuf::from(p)
    } else {
        let t = cx.state.active_tab();
        match t.current() {
            Some(urfin_core::Row::Entry { entry, .. }) if t.loc.is_local() => {
                t.loc.path.join(entry.name.as_str())
            }
            _ => {
                return Err(urfin_core::CmdError::Rejected(
                    crate::manager::tr("term-open-nothing", &[]).into(),
                ));
            }
        }
    };
    match (verb, kind_by_ext(&path)) {
        // `.exe`/`.com`: GUI или консоль — по заголовку, вне UI-потока.
        (Verb::Default, None) => {
            let p = path.clone();
            cx.rt.spawn(
                urfin_core::Lane::Interactive,
                "term.open",
                cx.rt.token(),
                move |t| {
                    let kind = classify(&p);
                    let _ = t.sink.send(
                        urfin_core::msg("term.open", move |cx| apply(cx, &p, kind, Verb::Default)),
                        &t.cancel,
                    );
                },
            );
            Ok(())
        }
        (v, Some(kind)) => {
            apply(cx, &path, kind, v);
            Ok(())
        }
        // runas/openas/новое окно у программы — без классификации.
        (v, None) => {
            apply(cx, &path, Kind::Gui, v);
            Ok(())
        }
    }
}

/// Классифицированный файл: в сеанс или ассоциацией ОС.
fn apply(cx: &mut urfin_core::Cx<'_>, path: &Path, kind: Kind, verb: Verb) {
    let pwsh = cx.state.ext.get::<crate::TermState>().map_or("pwsh", |st| {
        if st.sessions.shells().iter().any(|s| s.id == "pwsh") {
            "pwsh"
        } else {
            "powershell"
        }
    });
    let in_session =
        verb == Verb::Default && matches!(kind, Kind::Console | Kind::Batch | Kind::Ps1);
    if in_session {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = path.parent().map(Path::to_path_buf);
        if let Err(e) = crate::run_file(cx, &name, kind, pwsh, dir) {
            cx.toast(urfin_core::Level::Error, e.to_string());
        }
        return;
    }
    spawn_shell(cx, shell_spec(path, kind, verb, pwsh), path);
}

/// `ShellExecuteExW` на своём коротком потоке (S6 §7): не пул Runtime — зависший
/// сетевой путь или расширение оболочки не держат ни полосы, ни shutdown. Ошибка —
/// тостом.
pub(crate) fn spawn_shell(cx: &urfin_core::Cx<'_>, spec: Spec, shown: &Path) {
    let sink = cx.rt.sink();
    let name = shown.file_name().map_or_else(
        || shown.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let run = move || {
        #[cfg(windows)]
        let r = crate::sys::shell_open::shell_execute(&spec, false);
        #[cfg(not(windows))]
        let r: std::io::Result<()> = {
            let _ = &spec;
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        };
        if let Err(e) = r {
            // Отмена пользователем окна UAC или «Открыть с помощью» — не ошибка.
            if e.raw_os_error() == Some(1223) {
                return;
            }
            tracing::warn!(error = %e, file = %name, "term: shell open");
            let msg = crate::manager::tr(
                "term-open-failed",
                &[("name", &name), ("error", &e.to_string())],
            );
            let _ = sink.send(
                urfin_core::msg("term.open_failed", move |cx| {
                    cx.toast(urfin_core::Level::Error, msg);
                }),
                &urfin_core::CancelToken::new(),
            );
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("term-open".into())
        .spawn(run)
    {
        tracing::error!(error = %e, "term: open thread");
    }
}

/// Кто открывает файлы по Enter (S0 v3 №9): локальный файл — `term.open`.
#[derive(Debug, Default)]
pub struct TermOpen;

impl urfin_core::OpenResolver for TermOpen {
    fn resolve(
        &self,
        dir: &urfin_core::Location,
        e: &urfin_core::Entry,
        _args: &urfin_core::Args,
    ) -> Option<urfin_core::Command> {
        if !dir.is_local() || e.kind.is_dir_like() {
            return None;
        }
        Some(
            urfin_core::Command::new("term.open")
                .arg("path", dir.path.join(e.name.as_str()).display().to_string()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Минимальный PE: MZ, `e_lfanew`, `PE\0\0`, COFF и optional header.
    fn pe(magic: u16, subsystem: u16, lfanew: usize) -> Vec<u8> {
        let mut b = vec![0u8; lfanew + 24 + 96];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&u32::try_from(lfanew).unwrap().to_le_bytes());
        b[lfanew..lfanew + 4].copy_from_slice(b"PE\0\0");
        let opt = lfanew + 24;
        b[opt..opt + 2].copy_from_slice(&magic.to_le_bytes());
        b[opt + 68..opt + 70].copy_from_slice(&subsystem.to_le_bytes());
        b
    }

    /// Тест PE-парсера на фикстурах (S6-19).
    #[test]
    fn pe_fixtures() {
        assert_eq!(pe_subsystem(&pe(0x20b, 2, 0x80)), Some(SUBSYSTEM_GUI));
        assert_eq!(pe_subsystem(&pe(0x10b, 3, 0xF8)), Some(SUBSYSTEM_CONSOLE));
        assert_eq!(pe_subsystem(&pe(0x107, 3, 0x80)), None, "ROM image");
        let mut bad = pe(0x20b, 2, 0x80);
        bad[0x80] = b'X';
        assert_eq!(pe_subsystem(&bad), None);
        assert_eq!(pe_subsystem(b"MZ"), None);
        assert_eq!(pe_subsystem(b"#!/bin/sh\n"), None);
        // Заголовок за пределами прочитанного — не PE для нас.
        assert_eq!(pe_subsystem(&pe(0x20b, 2, 0x80)[..0x90]), None);
    }

    /// Настоящие программы Windows: `notepad.exe` — GUI (не занимает сеанс),
    /// `cmd.exe` — консоль (в сеансе).
    #[cfg(windows)]
    #[test]
    fn system_binaries() {
        let sys =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
                .join("System32");
        assert_eq!(classify(&sys.join("notepad.exe")), Kind::Gui);
        assert_eq!(classify(&sys.join("cmd.exe")), Kind::Console);
        assert_eq!(classify(&sys.join("where.exe")), Kind::Console);
    }

    #[test]
    fn by_extension() {
        assert_eq!(kind_by_ext(Path::new(r"C:\x\a.CMD")), Some(Kind::Batch));
        assert_eq!(kind_by_ext(Path::new("a.ps1")), Some(Kind::Ps1));
        assert_eq!(kind_by_ext(Path::new("a.txt")), Some(Kind::Document));
        assert_eq!(kind_by_ext(Path::new("a.exe")), None);
        assert_eq!(Verb::parse(Some("runas")), Some(Verb::RunAs));
        assert_eq!(Verb::parse(None), Some(Verb::Default));
        assert_eq!(Verb::parse(Some("bogus")), None);
    }

    /// Приёмка: `.ps1` в cmd-сеансе — через `pwsh -File`; console-exe — в синтаксисе
    /// shell.
    #[test]
    fn exec_lines_per_shell() {
        let wsl = ShellKind::Wsl {
            distro: "Ubuntu".into(),
        };
        assert_eq!(
            exec_line("run.ps1", Kind::Ps1, &ShellKind::Cmd, "pwsh"),
            "pwsh -File \".\\run.ps1\""
        );
        assert_eq!(
            exec_line("my tool.exe", Kind::Console, &ShellKind::Pwsh, "pwsh"),
            "& '.\\my tool.exe'"
        );
        assert_eq!(
            exec_line("it's.bat", Kind::Batch, &ShellKind::WinPowerShell, "pwsh"),
            "& '.\\it''s.bat'"
        );
        assert_eq!(
            exec_line("x.exe", Kind::Console, &ShellKind::Cmd, "pwsh"),
            "\".\\x.exe\""
        );
        assert_eq!(exec_line("x.exe", Kind::Console, &wsl, "pwsh"), "./x.exe");
        assert_eq!(
            exec_line("b.cmd", Kind::Batch, &ShellKind::Bash, "pwsh"),
            "cmd.exe /c b.cmd"
        );
    }

    #[test]
    fn shell_specs() {
        let p = Path::new(r"C:\d\a.ps1");
        let s = shell_spec(p, Kind::Ps1, Verb::NewWindow, "pwsh");
        assert_eq!(s.file, PathBuf::from("pwsh.exe"));
        assert_eq!(s.params.unwrap(), r#"-NoExit -File "C:\d\a.ps1""#);
        assert_eq!(s.dir, Some(PathBuf::from(r"C:\d")));
        let t = shell_spec(
            Path::new(r"C:\d\a.txt"),
            Kind::Document,
            Verb::OpenAs,
            "pwsh",
        );
        assert_eq!((t.verb, t.params), (Some("openas"), None));
        let d = detached_spec(Path::new("cmd.exe"), &ShellKind::Cmd, "dir /s", None);
        assert_eq!(d.params.unwrap(), "/k dir /s");
        let d = detached_spec(Path::new("pwsh.exe"), &ShellKind::Pwsh, "ls 'a'", None);
        assert_eq!(d.params.unwrap(), "-NoExit -Command 'ls ''a'''");
    }
}
