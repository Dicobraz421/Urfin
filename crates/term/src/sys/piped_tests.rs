//! Пайпы и job на настоящем `cmd.exe`.

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

use super::*;

fn cmd(args: &str) -> SpawnSpec {
    SpawnSpec {
        program: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
        args: vec![OsString::from("/c"), OsString::from(args)],
        cwd: None,
        env: vec![("URFIN_TEST".into(), "1".into())],
    }
}

const QUIET: PipedFlags = PipedFlags {
    no_window: true,
    merge_stderr: false,
    kill_on_drop: true,
};

fn read_all(f: Option<File>) -> String {
    let mut s = String::new();
    f.unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn stdout_stderr_and_code() {
    let mut c = spawn(&cmd("echo out %URFIN_TEST%& echo err 1>&2& exit 3"), QUIET).unwrap();
    let err = c.stderr.take();
    let err = std::thread::spawn(move || read_all(err));
    let out = read_all(c.stdout.take());
    assert_eq!(c.wait_timeout(Some(Duration::from_secs(10))), Some(3));
    assert_eq!(out.trim(), "out 1");
    assert_eq!(err.join().unwrap().trim(), "err");
}

#[test]
fn merged_stderr() {
    let flags = PipedFlags {
        merge_stderr: true,
        ..QUIET
    };
    let mut c = spawn(&cmd("echo a& echo b 1>&2& echo c"), flags).unwrap();
    assert!(c.stderr.is_none());
    let out = read_all(c.stdout.take());
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    assert_eq!(lines, ["a", "b", "c"]);
}

/// stdin ребёнка — сразу EOF: `more` не ждёт ввода.
#[test]
fn stdin_is_eof() {
    let mut c = spawn(&cmd("more"), QUIET).unwrap();
    let _ = read_all(c.stdout.take());
    assert_eq!(c.wait_timeout(Some(Duration::from_secs(10))), Some(0));
}

/// `kill_tree` убивает и внука: EOF stdout приходит сразу.
#[test]
fn kill_tree_closes_pipe() {
    let mut c = spawn(&cmd("cmd /c ping -n 30 127.0.0.1"), QUIET).unwrap();
    let mut out = c.stdout.take().unwrap();
    let mut buf = [0u8; 64];
    // Первые байты — внук уже пишет.
    assert!(out.read(&mut buf).unwrap() > 0);
    assert!(c.active_processes() >= 2);
    let t = Instant::now();
    c.kill_tree().unwrap();
    let mut rest = Vec::new();
    out.read_to_end(&mut rest).unwrap();
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    assert_eq!(c.wait_timeout(Some(Duration::from_secs(1))), Some(1));
    // Учёт job догоняет выход процессов (и conhost) не мгновенно.
    let t = Instant::now();
    while c.active_processes() > 0 && t.elapsed() < Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(c.active_processes(), 0);
}

#[test]
fn missing_program() {
    let spec = SpawnSpec {
        program: PathBuf::from(r"C:\nonexistent\urfin-missing.exe"),
        ..SpawnSpec::default()
    };
    let e = spawn(&spec, QUIET).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}
