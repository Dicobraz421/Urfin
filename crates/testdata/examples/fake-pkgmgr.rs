//! Подставной менеджер пакетов для тестов `urfin-pkg` (F02-02): повторяет фикстуру
//! с паузами и выходит с заданным кодом.
//!
//! ```text
//! fake-pkgmgr [--fixture <файл>] [--line <текст>]… [--delay-ms N] [--spawn N]
//!             [--sleep-ms N] [--exit N] [--stderr]
//! ```
//! - `--fixture` — байты файла построчно, как есть (`\r\n`, `\r`-спиннеры, OEM);
//! - `--line` — строка после фикстуры (повторяется);
//! - `--delay-ms` — пауза перед каждой строкой;
//! - `--spawn N` — сначала запустить себя же с `--spawn N-1` и теми же прочими
//!   аргументами (дерево из N+1 процессов, вывод общий);
//! - `--sleep-ms` — пауза после вывода;
//! - `--exit` — код выхода;
//! - `--stderr` — строки `--line` — в stderr.

use std::io::Write;
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Default)]
struct Args {
    fixture: Option<String>,
    lines: Vec<String>,
    delay: Duration,
    spawn: u32,
    sleep: Duration,
    exit: i32,
    stderr: bool,
    /// Аргументы без `--spawn` — для ребёнка.
    rest: Vec<String>,
}

fn parse() -> Args {
    let mut a = Args::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    let ms = |v: &str| Duration::from_millis(v.parse().expect("ms"));
    while let Some(k) = it.next() {
        if k == "--stderr" {
            a.stderr = true;
            a.rest.push(k.clone());
            continue;
        }
        let v = it.next().unwrap_or_else(|| panic!("{k}: no value")).clone();
        match k.as_str() {
            "--spawn" => {
                a.spawn = v.parse().expect("--spawn N");
                continue;
            }
            "--fixture" => a.fixture = Some(v.clone()),
            "--line" => a.lines.push(v.clone()),
            "--delay-ms" => a.delay = ms(&v),
            "--sleep-ms" => a.sleep = ms(&v),
            "--exit" => a.exit = v.parse().expect("--exit N"),
            _ => panic!("unknown argument {k}"),
        }
        a.rest.extend([k.clone(), v]);
    }
    a
}

fn main() {
    let a = parse();
    // Ребёнок наследует stdout и stderr: держит пайп, как настоящий инсталлятор.
    let _child = (a.spawn > 0).then(|| {
        Command::new(std::env::current_exe().expect("current exe"))
            .args(&a.rest)
            .args(["--spawn", &(a.spawn - 1).to_string()])
            .spawn()
            .expect("spawn child")
    });
    let mut out = std::io::stdout().lock();
    let fixture = a
        .fixture
        .map(|f| std::fs::read(&f).unwrap_or_else(|e| panic!("{f}: {e}")))
        .unwrap_or_default();
    for line in fixture.split_inclusive(|&b| b == b'\n') {
        std::thread::sleep(a.delay);
        out.write_all(line).expect("stdout");
        out.flush().expect("stdout");
    }
    for line in &a.lines {
        std::thread::sleep(a.delay);
        if a.stderr {
            eprintln!("{line}");
        } else {
            writeln!(out, "{line}").expect("stdout");
            out.flush().expect("stdout");
        }
    }
    drop(out);
    std::thread::sleep(a.sleep);
    std::process::exit(a.exit);
}
