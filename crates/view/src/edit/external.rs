//! Внешний редактор (S7 §5.6, Alt+Shift+F4; `editor.default = "external"` — F4).
//!
//! Порядок: `[editor] external` (шаблон `code -w {path}`, `{line}` — строка) → `$VISUAL`
//! → `$EDITOR` → программа Windows для типа файла (`file.open` / `term.open`). Запуск:
//! консольный редактор (`external_console = "auto"`: vim, nvim, vi, nano, micro, hx, kak,
//! `emacs -nw`; или `true`) — строкой в сеансе терминала S6 (`term.exec`), GUI —
//! отсоединённым процессом (`term.open` принимает только путь, без параметров шаблона).

use std::path::{Path, PathBuf};

use urfin_core::{Command, Cx, Lane, Level};

/// Как запустить.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// Программа Windows для типа файла.
    Assoc,
    /// Консольный редактор: строка команды в сеансе терминала.
    Console { line: String },
    /// GUI-редактор: отсоединённый процесс (`argv[0]` ищется по `PATH`/`PATHEXT`).
    Detached { argv: Vec<String> },
}

/// Консольные редакторы для `external_console = "auto"` (имя программы без расширения).
const CONSOLE: [&str; 9] = [
    "vim", "nvim", "vi", "nano", "micro", "hx", "helix", "kak", "ne",
];

/// Резолв: шаблон конфига, затем `$VISUAL` и `$EDITOR` (`env`), иначе — программа Windows.
/// `line` — строка с 1 (`{line}`; нет — 1).
pub fn resolve(
    template: &str,
    console: &str,
    env: &dyn Fn(&str) -> Option<String>,
    path: &Path,
    line: Option<u32>,
) -> Launch {
    let tpl = Some(template.trim().to_owned())
        .filter(|t| !t.is_empty())
        .or_else(|| env("VISUAL").filter(|v| !v.trim().is_empty()))
        .or_else(|| env("EDITOR").filter(|v| !v.trim().is_empty()));
    let Some(tpl) = tpl else {
        return Launch::Assoc;
    };
    let line = expand(tpl.trim(), path, line.unwrap_or(1));
    let argv = split(&line);
    if argv.is_empty() {
        return Launch::Assoc;
    }
    let is_console = match console.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "always" | "console" => true,
        "false" | "no" | "never" | "gui" => false,
        _ => is_console_editor(&argv),
    };
    if is_console {
        Launch::Console { line }
    } else {
        Launch::Detached { argv }
    }
}

/// `{path}` → путь в кавычках (`"{path}"` — тоже), `{line}` → строка; без `{path}` путь
/// дописывается в конец.
pub fn expand(tpl: &str, path: &Path, line: u32) -> String {
    let quoted = format!("\"{}\"", path.display());
    let mut s = tpl
        .replace("\"{path}\"", "{path}")
        .replace("{line}", &line.to_string());
    if s.contains("{path}") {
        s = s.replace("{path}", &quoted);
    } else {
        s.push(' ');
        s.push_str(&quoted);
    }
    s
}

/// Командная строка → аргументы: пробелы разделяют, двойные кавычки группируют.
pub fn split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Консольный ли редактор по имени программы и ключам (`emacs -nw`, `emacsclient -t`).
pub fn is_console_editor(argv: &[String]) -> bool {
    let Some(prog) = argv.first() else {
        return false;
    };
    let stem = Path::new(prog)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if CONSOLE.contains(&stem.as_str()) {
        return true;
    }
    let flag = |f: &[&str]| argv[1..].iter().any(|a| f.contains(&a.as_str()));
    match stem.as_str() {
        "emacs" => flag(&["-nw", "--no-window-system"]),
        "emacsclient" => flag(&["-nw", "-t", "--tty"]),
        _ => false,
    }
}

/// Программа по имени: путь с расширением — как есть; иначе `PATHEXT` по каталогу
/// программы или по `PATH`. `None` — не найдена.
pub fn which(prog: &str, path_var: &str, pathext: &str) -> Option<PathBuf> {
    let exts: Vec<String> = pathext
        .split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let p = Path::new(prog);
    let try_in = |base: &Path| -> Option<PathBuf> {
        if base.extension().is_some() && base.is_file() {
            return Some(base.to_path_buf());
        }
        exts.iter().find_map(|e| {
            let mut s = base.as_os_str().to_owned();
            s.push(e);
            let c = PathBuf::from(s);
            c.is_file().then_some(c)
        })
    };
    if p.is_absolute() || p.components().count() > 1 {
        return try_in(p);
    }
    std::env::split_paths(path_var).find_map(|dir| try_in(&dir.join(p)))
}

/// `DETACHED_PROCESS`: GUI-программа без консоли.
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: Ctrl+C Урфина не доходит до редактора.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_NO_WINDOW`: `.cmd`/`.bat` (как `code.cmd`) — консоль без окна.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Отсоединённый процесс (вне UI-потока): программа по `PATH`/`PATHEXT`, каталог — папка
/// файла, стандартные потоки закрыты.
pub fn spawn_detached(argv: &[String], cwd: Option<&Path>) -> std::io::Result<u32> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let path_var = std::env::var("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let exe = which(program, &path_var, &pathext).unwrap_or_else(|| PathBuf::from(program));
    let batch = exe
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut c = std::process::Command::new(&exe);
    c.args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let flags = if batch {
            CREATE_NO_WINDOW
        } else {
            DETACHED_PROCESS
        };
        c.creation_flags(flags | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(not(windows))]
    let _ = (
        batch,
        DETACHED_PROCESS,
        CREATE_NEW_PROCESS_GROUP,
        CREATE_NO_WINDOW,
    );
    // Процесс не ждём: `Child` без `wait` ничего не держит.
    Ok(c.spawn()?.id())
}

/// Запуск внешнего редактора для локального `path`: `from_cursor` — файл под курсором
/// панели (для `file.open`), `line` — строка с 1.
pub(crate) fn launch(cx: &mut Cx<'_>, path: &Path, from_cursor: bool, line: Option<u32>) {
    let Some(cfg) = cx
        .state
        .ext
        .get::<crate::ViewState>()
        .map(|v| v.editor.clone())
    else {
        return;
    };
    let env = |k: &str| std::env::var(k).ok();
    let cwd = path.parent().map(Path::to_path_buf);
    match resolve(&cfg.external, &cfg.external_console, &env, path, line) {
        Launch::Assoc if from_cursor => cx.dispatch(Command::new("file.open")),
        Launch::Assoc => {
            cx.dispatch(Command::new("term.open").arg("path", path.to_string_lossy().as_ref()));
        }
        Launch::Console { line } => {
            let mut c = Command::new("term.exec").arg("cmd", line);
            if let Some(d) = &cwd {
                c = c.arg("cwd", d.to_string_lossy().as_ref());
            }
            cx.dispatch(c);
        }
        Launch::Detached { argv } => {
            cx.rt.spawn(
                Lane::Interactive,
                "edit.external",
                cx.rt.token(),
                move |t| match spawn_detached(&argv, cwd.as_deref()) {
                    Ok(pid) => tracing::debug!(pid, "external editor started"),
                    Err(e) => {
                        tracing::warn!(error = %e, prog = ?argv.first(), "external editor failed");
                        let mut a = urfin_i18n::FluentArgs::new();
                        a.set("error", e.to_string());
                        let text = urfin_i18n::current().fmt("ui-edit-external-failed", &a);
                        let _ = t.sink.send(
                            urfin_core::msg("edit.external", move |cx| {
                                cx.toast(Level::Error, text);
                                cx.keep_rev();
                            }),
                            &t.cancel,
                        );
                    }
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn resolve_order_and_kind() {
        let p = Path::new(r"C:\w\a b.txt");
        let none = env_of(&[]);
        assert_eq!(resolve("", "auto", &none, p, None), Launch::Assoc);
        // $EDITOR=vim → консольный, строка с путём в кавычках.
        let vim = env_of(&[("EDITOR", "vim")]);
        assert_eq!(
            resolve("", "auto", &vim, p, None),
            Launch::Console {
                line: r#"vim "C:\w\a b.txt""#.into()
            }
        );
        // $VISUAL раньше $EDITOR; шаблон конфига раньше обоих.
        let both = env_of(&[("VISUAL", "code -w"), ("EDITOR", "vim")]);
        assert_eq!(
            resolve("", "auto", &both, p, None),
            Launch::Detached {
                argv: vec!["code".into(), "-w".into(), r"C:\w\a b.txt".into()]
            }
        );
        assert_eq!(
            resolve("nvim +{line} {path}", "auto", &both, p, Some(41)),
            Launch::Console {
                line: r#"nvim +41 "C:\w\a b.txt""#.into()
            }
        );
        // Явно консольный / явно GUI.
        assert!(matches!(
            resolve("myed {path}", "true", &none, p, None),
            Launch::Console { .. }
        ));
        assert!(matches!(
            resolve("vim {path}", "false", &none, p, None),
            Launch::Detached { .. }
        ));
        assert!(matches!(
            resolve("emacs -nw", "auto", &none, p, None),
            Launch::Console { .. }
        ));
        assert!(matches!(
            resolve("emacs", "auto", &none, p, None),
            Launch::Detached { .. }
        ));
        assert!(matches!(
            resolve(
                r#""C:\Program Files\Vim\vim91\vim.exe" "{path}""#,
                "auto",
                &none,
                p,
                None
            ),
            Launch::Console { .. }
        ));
    }

    #[test]
    fn split_and_which() {
        assert_eq!(
            split(r#""C:\Program Files\x.exe" -a  "b c" d"#),
            vec![r"C:\Program Files\x.exe", "-a", "b c", "d"]
        );
        assert_eq!(split(r#"x """#), vec!["x", ""]);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fake.cmd"), "@echo off\r\n").unwrap();
        let pv = dir.path().to_string_lossy().into_owned();
        let found = which("fake", &pv, ".COM;.EXE;.BAT;.CMD").unwrap();
        assert_eq!(found, dir.path().join("fake.cmd"));
        assert!(which("nope", &pv, ".EXE;.CMD").is_none());
        let full = dir.path().join("fake.cmd");
        assert_eq!(
            which(&full.to_string_lossy(), "", ".EXE").as_deref(),
            Some(full.as_path())
        );
    }
}
