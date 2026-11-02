//! Интеграционные тесты старта: бинарь копируется во временный каталог рядом с
//! маркером `urfin.portable`, поэтому всё пишется в `<tmp>/data`, а профиль
//! пользователя не трогается.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct App {
    _tmp: tempfile::TempDir,
    exe: PathBuf,
    data: PathBuf,
}

impl App {
    fn new() -> App {
        let tmp = tempfile::tempdir().unwrap();
        let src = PathBuf::from(env!("CARGO_BIN_EXE_urfin"));
        let exe = tmp.path().join(src.file_name().unwrap());
        fs::copy(&src, &exe).unwrap();
        fs::write(tmp.path().join("urfin.portable"), "").unwrap();
        let data = tmp.path().join("data");
        App {
            _tmp: tmp,
            exe,
            data,
        }
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(&self.exe);
        c.args(args).env_remove("URFIN_LOG");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    fn log_text(&self) -> String {
        files(&self.logs(), "urfin.")
            .iter()
            .map(|f| fs::read_to_string(f).unwrap())
            .collect()
    }

    fn write_config(&self, text: &str) {
        fs::create_dir_all(&self.data).unwrap();
        fs::write(self.data.join("config.toml"), text).unwrap();
    }
}

fn files(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(prefix))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn smoke_creates_portable_layout_and_default_config() {
    let app = App::new();
    let o = app.run(&["--smoke"], &[]);
    assert!(o.status.success(), "{o:?}");
    assert!(stdout(&o).contains("smoke ok"));
    assert!(app.data.join("config.toml").is_file());
    assert!(
        !app.data.join("state").join("session.toml").exists(),
        "smoke does not touch the user's session"
    );
    assert_eq!(files(&app.logs(), "urfin.").len(), 1);
    assert!(!stdout(&o).contains("banner:"));
}

#[cfg(debug_assertions)]
#[test]
fn crash_test_writes_crash_file_and_next_start_shows_banner() {
    let app = App::new();
    // Текст баннера сверяется по-английски; `auto` взял бы язык ОС стенда.
    app.write_config("[general]\nlanguage = \"en\"\n");
    let o = app.run(&["--crash-test"], &[]);
    assert!(!o.status.success());
    let crashes = files(&app.logs(), "crash-");
    assert_eq!(crashes.len(), 1, "{o:?}");
    let report = fs::read_to_string(&crashes[0]).unwrap();
    assert!(report.contains("crash test"), "{report}");
    assert!(report.contains("backtrace:"));
    assert!(report.contains(env!("CARGO_PKG_VERSION")));
    let marker = app.data.join("state").join("crashed");
    assert!(marker.is_file());
    assert!(app.log_text().contains("panic"), "panic is logged too");

    let o = app.run(&["--smoke"], &[]);
    assert!(o.status.success(), "{o:?}");
    let out = stdout(&o);
    assert!(out.contains("banner: Urfin closed unexpectedly"), "{out}");
    assert!(out.contains("crash-"), "{out}");
    assert!(!marker.exists(), "marker is removed after it is shown");

    let o = app.run(&["--smoke"], &[]);
    assert!(!stdout(&o).contains("banner:"), "banner only once");
}

#[cfg(debug_assertions)]
#[test]
fn panic_outside_ui_thread_is_not_a_crash() {
    let app = App::new();
    let o = app.run(&["--thread-panic-test"], &[]);
    assert!(o.status.success(), "{o:?}");
    assert!(files(&app.logs(), "crash-").is_empty(), "no crash file");
    assert!(
        !app.data.join("state").join("crashed").exists(),
        "no marker"
    );
    assert!(
        app.log_text().contains("thread panic test"),
        "logged as an error"
    );
    let o = app.run(&["--smoke"], &[]);
    assert!(!stdout(&o).contains("banner:"), "no crash banner");
}

#[test]
fn bad_env_filter_does_not_block_start() {
    let app = App::new();
    let o = app.run(&["--smoke"], &[("URFIN_LOG", "urfin=loud")]);
    assert!(o.status.success(), "{o:?}");
    let log = app.log_text();
    assert!(log.contains("URFIN_LOG ignored"), "{log}");
}

#[test]
fn env_filter_beats_config_level() {
    let app = App::new();
    app.write_config("[log]\nlevel = \"error\"\n");
    assert!(app.run(&["--smoke"], &[]).status.success());
    assert!(!app.log_text().contains("DEBUG"), "config level error");

    let app = App::new();
    app.write_config("[log]\nlevel = \"error\"\n");
    assert!(
        app.run(&["--smoke"], &[("URFIN_LOG", "debug")])
            .status
            .success()
    );
    let log = app.log_text();
    assert!(log.contains("DEBUG"), "{log}");
    assert!(log.contains("smoke done"), "{log}");
}

#[test]
fn log_files_are_pruned_to_keep_files() {
    let app = App::new();
    app.write_config("[log]\nkeep_files = 2\n");
    fs::create_dir_all(app.logs()).unwrap();
    for d in 1..=6 {
        fs::write(app.logs().join(format!("urfin.2026-01-0{d}.log")), "old").unwrap();
    }
    fs::write(app.logs().join("crash-1.txt"), "keep me").unwrap();
    assert!(app.run(&["--smoke"], &[]).status.success());
    let logs = files(&app.logs(), "urfin.");
    assert!(logs.len() <= 2, "{logs:?}");
    assert!(
        app.logs().join("crash-1.txt").is_file(),
        "only log files are pruned"
    );
}

/// Строки событий Urfin из журнала «Приложение» (новые первыми).
#[cfg(windows)]
fn event_log_texts() -> String {
    let o = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-EventLog -LogName Application -Source Urfin -Newest 30 \
             -ErrorAction SilentlyContinue | ForEach-Object { \
             \"$($_.InstanceId) $(($_.ReplacementStrings -join ' ') -replace '\\r?\\n', ' ')\" }",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[cfg(all(windows, debug_assertions))]
#[test]
fn crash_and_errors_go_to_windows_event_log() {
    let app = App::new();
    assert!(app.run(&["--error-test"], &[]).status.success());
    let o = app.run(&["--crash-test"], &[]);
    assert!(!o.status.success());
    let crash = files(&app.logs(), "crash-");
    let name = crash[0].file_name().unwrap().to_string_lossy().into_owned();
    let events = event_log_texts();
    let crash_line = events
        .lines()
        .find(|l| l.contains(&name))
        .unwrap_or_else(|| panic!("no crash event for {name} in:\n{events}"));
    assert!(crash_line.starts_with("1000 "), "{crash_line}");
    assert!(crash_line.contains("crash test"), "{crash_line}");
    assert!(
        events
            .lines()
            .any(|l| l.starts_with("1001 ") && l.contains("error test")),
        "{events}"
    );
    // Паника не дублируется событием 1001.
    assert!(
        !events
            .lines()
            .any(|l| l.starts_with("1001 ") && l.contains(&name)),
        "{events}"
    );
}

#[cfg(debug_assertions)]
#[test]
fn second_start_hands_paths_to_the_first() {
    use std::io::{BufRead, BufReader, Lines};
    use std::process::{Child, ChildStdout, Stdio};
    use std::time::{Duration, Instant};

    let app = App::new();
    // Первый экземпляр без окна: печатает `ready`, затем один принятый запрос.
    let server = || -> (Child, Lines<BufReader<ChildStdout>>) {
        let mut child = Command::new(&app.exe)
            .arg("--ipc-test-server")
            .env_remove("URFIN_LOG")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        assert_eq!(lines.next().unwrap().unwrap(), "ready");
        (child, lines)
    };

    let (mut first, mut lines) = server();
    let work = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let o = Command::new(&app.exe)
        .arg("sub")
        .current_dir(work.path())
        .env_remove("URFIN_LOG")
        .output()
        .unwrap();
    let took = t.elapsed();
    assert!(o.status.success(), "{o:?}");
    let req: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(req["argv"], serde_json::json!(["sub"]));
    // macOS: `/var` — ссылка на `/private/var`, `current_dir` даёт канонический путь.
    assert_eq!(
        fs::canonicalize(req["cwd"].as_str().unwrap()).unwrap(),
        fs::canonicalize(work.path()).unwrap()
    );
    assert!(first.wait().unwrap().success());
    assert!(took < Duration::from_secs(2), "second start took {took:?}");
    assert!(
        app.log_text()
            .contains("handed over to the running instance")
    );

    // Завершённый и убитый экземпляры не оставляют lock: следующий снова первый.
    let (mut again, _) = server();
    again.kill().unwrap();
    again.wait().unwrap();
    let (mut after_kill, _) = server();
    after_kill.kill().unwrap();
    after_kill.wait().unwrap();
}

/// `--du-elevated` (F01-12) на временном каталоге без повышения: тот же бинарь, что
/// запускается через UAC, считает итоги, пишет только файл результата и выходит до
/// каталогов, логов и окна. Неверные аргументы — код 2, файла нет.
#[test]
fn du_elevated_counts_without_uac() {
    use urfin_vfs::du::elevated::{self, Request};
    let app = App::new();
    let tmp = tempfile::tempdir().unwrap();
    let canon = fs::canonicalize(tmp.path()).unwrap();
    let canon = canon.to_string_lossy();
    let base = PathBuf::from(canon.strip_prefix(r"\\?\").unwrap_or(&canon));
    let data = base.join("data");
    fs::create_dir_all(data.join("sub")).unwrap();
    fs::write(data.join("a.bin"), vec![0u8; 1234]).unwrap();
    fs::write(data.join("sub").join("b.bin"), vec![0u8; 766]).unwrap();
    let dir = base.join("du").join(elevated::SUBDIR);
    fs::create_dir_all(&dir).unwrap();
    let req = Request {
        out: elevated::out_path(&dir, &elevated::new_nonce()),
        roots: vec![data],
    };
    let o = Command::new(&app.exe)
        .args(req.args())
        .env_remove("URFIN_LOG")
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let a = elevated::decode(&fs::read(&req.out).unwrap(), &req).unwrap();
    assert_eq!(a.roots.len(), 1);
    assert!(a.roots[0].ok);
    assert_eq!((a.roots[0].files, a.roots[0].logical), (2, 2000));
    assert!(!app.data.exists(), "no app dirs, config or logs");
    // Корень — UNC: отказ до обхода, файла нет.
    let bad = Request {
        out: elevated::out_path(&dir, &elevated::new_nonce()),
        roots: vec![PathBuf::from(r"\\srv\share")],
    };
    let o = Command::new(&app.exe).args(bad.args()).output().unwrap();
    assert_eq!(o.status.code(), Some(i32::from(elevated::exit::BAD_ARGS)));
    assert!(!bad.out.exists());
}
