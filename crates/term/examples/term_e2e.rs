//! S6-22: e2e-замеры терминала на стенде, release.
//!
//! - память Urfin на сеанс до вывода и CPU в простое с 3 сеансами `cmd.exe`;
//! - поток: `type` файла 100 МБ и `dir /s C:\Windows` через `ConPTY` и сетку — время
//!   до `Exited`; рядом тот же `dir /s` в `NUL` без терминала.
//!
//! `cargo run --release -p urfin-term --example term_e2e [-- --skip-dir]`.
//! Итоги — `docs/perf/term.md`.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use urfin_term::session::{Session, SessionEvent, SessionOpts, SessionSink};
use urfin_term::sys::conpty::{PtySize, SpawnSpec, cpu_time, private_bytes};

const CMD: &str = r"C:\Windows\System32\cmd.exe";
const MB: u64 = 1 << 20;

struct Sink(std::sync::Mutex<Sender<SessionEvent>>);

impl SessionSink for Sink {
    fn poke(&self) {}
    fn event(&self, ev: SessionEvent) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(ev);
        }
    }
}

fn sink() -> (Arc<Sink>, Receiver<SessionEvent>) {
    let (tx, rx) = channel();
    (Arc::new(Sink(std::sync::Mutex::new(tx))), rx)
}

fn opts() -> SessionOpts {
    SessionOpts {
        size: PtySize {
            cols: 200,
            rows: 60,
        },
        scrollback: 10_000,
        osc52: "off".into(),
        nonce: None,
    }
}

fn spec(args: &[&str]) -> SpawnSpec {
    SpawnSpec {
        program: CMD.into(),
        args: args.iter().map(Into::into).collect(),
        cwd: None,
        env: Vec::new(),
    }
}

fn mib(b: u64) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "отчёт")]
    let v = b as f64 / MB as f64;
    v
}

/// 3 сеанса до вывода: прирост private bytes и CPU за 10 с простоя.
fn idle() {
    // Прогрев: первый сеанс тянет DLL ConPTY и пулы аллокатора — их не считаем.
    {
        let (s, rx) = sink();
        let warm = Session::start(0, &spec(&["/q", "/c", "exit"]), &opts(), s).unwrap();
        while !matches!(
            rx.recv_timeout(Duration::from_secs(10)),
            Ok(SessionEvent::Exited(_)) | Err(_)
        ) {}
        drop(warm);
        std::thread::sleep(Duration::from_millis(500));
    }
    let before = private_bytes();
    let mut keep = Vec::new();
    for id in 1..=3 {
        let (s, rx) = sink();
        let sess = Session::start(id, &spec(&["/q", "/k", "prompt URFIN$G"]), &opts(), s).unwrap();
        let t0 = Instant::now();
        while !sess.shared().rows_text(0, 100).contains("URFIN>")
            && t0.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        keep.push((sess, rx));
    }
    std::thread::sleep(Duration::from_millis(500));
    let after = private_bytes();
    let per = after.saturating_sub(before) / 3;
    println!(
        "idle: private {:.1} -> {:.1} MiB, per session {:.2} MiB",
        mib(before),
        mib(after),
        mib(per)
    );
    let c0 = cpu_time();
    let w = Duration::from_secs(10);
    std::thread::sleep(w);
    let spent = cpu_time().saturating_sub(c0);
    println!(
        "idle: cpu {:?} over {:?} with 3 sessions ({:.3}% of one core)",
        spent,
        w,
        spent.as_secs_f64() / w.as_secs_f64() * 100.0
    );
    for (s, _) in &keep {
        s.close();
    }
}

/// Команда в сеансе до выхода: время и прирост CPU нашего процесса.
fn run(name: &str, args: &[&str], bytes: Option<u64>) {
    let (s, rx) = sink();
    let c0 = cpu_time();
    let t0 = Instant::now();
    let sess = Session::start(10, &spec(args), &opts(), s).unwrap();
    loop {
        match rx.recv_timeout(Duration::from_secs(600)) {
            Ok(SessionEvent::Exited(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let t = t0.elapsed();
    let cpu = cpu_time().saturating_sub(c0);
    let rate = bytes
        .map(|b| format!(", {:.0} MiB/s", mib(b) / t.as_secs_f64()))
        .unwrap_or_default();
    println!("{name}: {t:.2?} wall, {cpu:.2?} cpu in Urfin{rate}");
    drop(sess);
}

fn main() {
    let skip_dir = std::env::args().any(|a| a == "--skip-dir");
    idle();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cat100.txt");
    {
        let line: Vec<u8> = (0..98u8).map(|i| b'!' + i % 90).chain(*b"\r\n").collect();
        let mut f = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
        for _ in 0..(100 * MB) / 100 {
            f.write_all(&line).unwrap();
        }
    }
    let path = file.to_string_lossy().into_owned();
    run("type 100MB", &["/q", "/c", "type", &path], Some(100 * MB));
    if !skip_dir {
        let t0 = Instant::now();
        let st = std::process::Command::new(CMD)
            .args(["/q", "/c", r"dir /s C:\Windows > NUL"])
            .status()
            .unwrap();
        println!(
            "dir /s C:\\Windows > NUL (no terminal): {:.2?} {st}",
            t0.elapsed()
        );
        run(
            "dir /s C:\\Windows",
            &["/q", "/c", "dir", "/s", r"C:\Windows"],
            None,
        );
    }
}
