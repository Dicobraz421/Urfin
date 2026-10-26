//! Автокомплит (S6-17): пути, PATH, история, слияние, квотирование, ghost.

use super::*;
use crate::history::Entry;

fn names(list: &[(&str, bool)]) -> Vec<Name> {
    let mut v: Vec<Name> = list.iter().map(|(n, d)| Name::new(n, *d)).collect();
    v.sort_by(|a, b| a.lower.cmp(&b.lower));
    v
}

fn root() -> Vec<Name> {
    names(&[
        ("Program Files", true),
        ("Program Files (x86)", true),
        ("ProgramData", true),
        ("Users", true),
        ("pagefile.sys", false),
    ])
}

fn q<'a>(line: &'a str, style: Style, cwd: &'a Path) -> Query<'a> {
    Query {
        line,
        cursor: line.len(),
        style,
        cwd,
    }
}

/// Приёмка: Tab по `C:\Pro` → `'C:\Program Files'` (pwsh) и `"C:\Program Files"` (cmd).
#[test]
fn program_files_quoted_per_shell() {
    let cwd = Path::new(r"D:\work");
    let line = r"cd C:\Pro";
    assert_eq!(path_query(r"C:\Pro", cwd).dir, PathBuf::from(r"C:\"));
    let pwsh = candidates(&q(line, Style::Pwsh, cwd), &root(), None, None);
    assert_eq!(pwsh[0].insert, r"'C:\Program Files'");
    assert_eq!(pwsh.len(), 3);
    assert_eq!(apply(line, &pwsh[0]).0, r"cd 'C:\Program Files'");
    let cmd = candidates(&q(line, Style::Cmd, cwd), &root(), None, None);
    assert_eq!(cmd[0].insert, r#""C:\Program Files""#);
    assert_eq!(cmd[2].insert, r"C:\ProgramData");
}

/// Настоящий корень диска: `C:\Program Files` есть на любом Windows.
#[cfg(windows)]
#[test]
fn program_files_on_disk() {
    let cwd = Path::new(r"C:\");
    let pq = path_query(r"C:\Pro", cwd);
    let listing = read_dir(&pq.dir);
    let c = candidates(&q(r"C:\Pro", Style::Pwsh, cwd), &listing, None, None);
    assert!(c.iter().any(|c| c.insert == r"'C:\Program Files'"), "{c:?}");
}

#[test]
fn relative_paths_and_cd_dirs_only() {
    let cwd = Path::new(r"C:\src");
    let ls = names(&[("app", true), ("apply.txt", false), ("b", true)]);
    let c = candidates(&q("type ap", Style::Cmd, cwd), &ls, None, None);
    assert_eq!(
        c.iter().map(|c| c.insert.as_str()).collect::<Vec<_>>(),
        ["app", "apply.txt"]
    );
    let c = candidates(&q("cd ap", Style::Cmd, cwd), &ls, None, None);
    assert_eq!(c.len(), 1);
    assert!(c[0].dir);
    assert_eq!(path_query(r"sub\x", cwd).dir, PathBuf::from(r"C:\src\sub"));
    assert_eq!(path_query("x", cwd).typed_dir, "");
}

#[test]
fn merge_paths_commands_history() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["gitk.exe", "git.exe", "gizmo.cmd", "readme.txt"] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    let mut cmds = PathCommands::default();
    cmds.refresh(
        &dir.path().display().to_string(),
        &[".exe".into(), ".cmd".into()],
    );
    assert_eq!(cmds.matching("gi"), ["git", "gitk", "gizmo.cmd"]);
    // Неизменный каталог (тот же mtime) не перечитывается.
    let before = cmds.dirs[0].2.len();
    cmds.refresh(&dir.path().display().to_string(), &[".exe".into()]);
    assert_eq!(cmds.dirs[0].2.len(), before);

    let mut h = History::default();
    h.push(Entry::new("git status", 1), 10);
    h.push(Entry::new("git", 1), 10);
    let cwd = Path::new(r"C:\x");
    let ls = names(&[("git-notes", false)]);
    let c = candidates(&q("git", Style::Pwsh, cwd), &ls, Some(&cmds), Some(&h));
    let got: Vec<(&str, Source)> = c.iter().map(|c| (c.label.as_str(), c.source)).collect();
    assert_eq!(
        got,
        [
            ("git-notes", Source::Path),
            ("git", Source::Command),
            ("gitk", Source::Command),
            ("git status", Source::History),
        ]
    );
    // История заменяет всю строку.
    assert_eq!(apply("git", &c[3]), ("git status".to_owned(), 10));
}

/// Приёмка: ghost — хвост последней строки истории; принимается → (в handler).
#[test]
fn ghost_from_history() {
    let mut h = History::default();
    h.push(Entry::new("cargo build", 1), 10);
    h.push(Entry::new("cargo test -p x", 2), 10);
    assert_eq!(ghost(&h, "cargo "), Some("test -p x"));
    assert_eq!(ghost(&h, "cargo b"), Some("uild"));
    assert_eq!(ghost(&h, "cargo test -p x"), None);
    assert_eq!(ghost(&h, ""), None);
    assert_eq!(ghost(&h, "Cargo"), None);
}

/// Приёмка: каталог на 10k файлов — ответ после кэша <30 мс (в release; в debug —
/// с запасом).
#[test]
fn ten_thousand_files_fast() {
    let ls: Vec<Name> = (0..10_000)
        .map(|i| Name::new(&format!("file_{i:05}.txt"), false))
        .collect();
    let cwd = Path::new(r"C:\big");
    let t = std::time::Instant::now();
    let c = candidates(&q("type file_09", Style::Pwsh, cwd), &ls, None, None);
    let took = t.elapsed();
    assert_eq!(c.len(), 1000);
    let limit = if cfg!(debug_assertions) { 300 } else { 30 };
    assert!(took.as_millis() < limit, "{took:?}");
}
