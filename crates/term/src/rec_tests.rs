//! Запись сеанса (F03-12): формат asciicast v2 по эталону и валидатору, ввод не
//! пишется (пароль после «Password:»), ошибка диска, настройки и имена файлов.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, bounded, unbounded};

use super::*;
use crate::session::{
    PtyPort, SessionEvent, SessionOpts, SessionSink, Tap, TapEvent, WriterCmd, start_pipeline,
};

/// Файл в памяти.
#[derive(Clone, Default)]
struct Mem(Arc<Mutex<Vec<u8>>>);

impl Write for Mem {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl CastFile for Mem {
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Mem {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// Диск, на котором место кончилось после `left` байт.
struct Full {
    left: usize,
}

impl Write for Full {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        let n = b.len().min(self.left);
        self.left -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl CastFile for Full {
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn header() -> Header {
    Header {
        width: 80,
        height: 24,
        timestamp: 1_792_764_151,
        term: "xterm-256color".into(),
        shell: "ssh.exe".into(),
        title: "srv1".into(),
    }
}

/// Поток записи над `rx` в файл `file`; `stop` — остановка.
fn job(
    rx: TapRx,
    stop: Receiver<()>,
    file: Box<dyn CastFile>,
    start: Instant,
    shared: Arc<RecShared>,
) -> Job {
    Job {
        rx,
        stop,
        open: Box::new(move || Ok((file, PathBuf::from("x.cast"), "x.cast".into()))),
        header: header(),
        start,
        shared,
        cancel: Box::new(|| false),
        poke: Box::new(|| {}),
    }
}

/// Валидатор asciicast v2: заголовок — объект `version: 2` с размером; события —
/// `[t, "o" | "r", строка]`, время не убывает, `"r"` — `WxH`; `"i"` нет.
fn validate(text: &str) -> Result<usize, String> {
    let mut lines = text.lines();
    let head: serde_json::Value =
        serde_json::from_str(lines.next().ok_or("empty")?).map_err(|e| e.to_string())?;
    if head["version"] != 2 {
        return Err("version".into());
    }
    for k in ["width", "height", "timestamp"] {
        if !head[k].as_u64().is_some_and(|v| v > 0) {
            return Err(format!("header {k}"));
        }
    }
    if !head["env"]["TERM"].is_string() || !head["env"]["SHELL"].is_string() {
        return Err("env".into());
    }
    let mut last = 0.0f64;
    let mut n = 0;
    for l in lines {
        let ev: serde_json::Value = serde_json::from_str(l).map_err(|e| format!("{l}: {e}"))?;
        let a = ev.as_array().ok_or("event is not array")?;
        if a.len() != 3 {
            return Err("event arity".into());
        }
        let t = a[0].as_f64().ok_or("time")?;
        if t < last {
            return Err("time goes back".into());
        }
        last = t;
        let data = a[2].as_str().ok_or("data")?;
        match a[1].as_str() {
            Some("o") => {}
            Some("r") => {
                let (w, h) = data.split_once('x').ok_or("resize")?;
                w.parse::<u16>().map_err(|e| e.to_string())?;
                h.parse::<u16>().map_err(|e| e.to_string())?;
            }
            other => return Err(format!("event type {other:?}")),
        }
        n += 1;
    }
    Ok(n)
}

/// Эталон (`rec_golden.cast`, формат asciinema 2): заголовок, вывод с ESC, кавычками,
/// кириллицей и неверным UTF-8, resize.
#[test]
fn golden_cast() {
    let (tx, rx) = unbounded();
    let (_stop_tx, stop) = bounded(1);
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    tx.send(TapEvent::Output(
        at(100),
        b"\x1b[1;32madmin@srv1\x1b[0m:~$ ".to_vec(),
    ))
    .unwrap();
    tx.send(TapEvent::Output(at(500), b"Password: ".to_vec()))
        .unwrap();
    tx.send(TapEvent::Resize(
        at(1000),
        PtySize {
            cols: 100,
            rows: 30,
        },
    ))
    .unwrap();
    tx.send(TapEvent::Output(
        at(1250),
        "Привет \"мир\" \\ \t\r\n".as_bytes().to_vec(),
    ))
    .unwrap();
    tx.send(TapEvent::Output(at(2000), vec![0xff, b'o', b'k']))
        .unwrap();
    drop(tx); // сеанс закрыт
    let mem = Mem::default();
    let shared = Arc::new(RecShared::default());
    run(job(rx, stop, Box::new(mem.clone()), start, shared.clone()));
    let text = mem.text();
    assert_eq!(text, include_str!("rec_golden.cast"));
    assert_eq!(validate(&text), Ok(5));
    assert_eq!(shared.bytes(), text.len() as u64);
    assert!(shared.done() && !shared.failed());
}

/// Источник PTY по каналу: читает, пока отправитель жив.
struct ChanRead(Receiver<Vec<u8>>, Vec<u8>);

impl Read for ChanRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(b) => self.1 = b,
                Err(_) => return Ok(0),
            }
        }
        let n = self.1.len().min(buf.len());
        buf[..n].copy_from_slice(&self.1[..n]);
        self.1.drain(..n);
        Ok(n)
    }
}

/// PTY-порт: копит ввод.
struct Port(Arc<Mutex<Vec<u8>>>);

impl PtyPort for Port {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }

    fn resize(&mut self, _size: PtySize) {}

    fn close(&mut self) {}
}

#[derive(Default)]
struct Sink(AtomicBool);

impl SessionSink for Sink {
    fn poke(&self) {}

    fn event(&self, ev: SessionEvent) {
        if matches!(ev, SessionEvent::Exited(_)) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

fn wait(pred: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !pred() {
        assert!(t0.elapsed() < Duration::from_secs(10), "timeout");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Приёмка: ввод (`"i"`) не пишется — пароль, набранный после «Password:» (эхо
/// выключено), в файл не попадает; вывод — с первого байта.
#[test]
fn input_is_not_recorded() {
    let tap = Arc::new(Tap::default());
    let rx = tap.subscribe();
    let (src_tx, src_rx) = unbounded();
    let typed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(Sink::default());
    let spawn = |name: String, f: Box<dyn FnOnce() + Send>| {
        thread::Builder::new().name(name).spawn(f).unwrap();
    };
    let opts = SessionOpts {
        size: PtySize { cols: 80, rows: 24 },
        scrollback: 100,
        osc52: "copy".into(),
        nonce: None,
    };
    let p = start_pipeline(
        &spawn,
        "rec",
        ChanRead(src_rx, Vec::new()),
        Port(typed.clone()),
        || Some(0),
        &opts,
        sink.clone(),
        tap.clone(),
    );
    let mem = Mem::default();
    let shared = Arc::new(RecShared::default());
    let (stop_tx, stop) = bounded(1);
    let rec = {
        let (mem, shared) = (mem.clone(), shared.clone());
        thread::spawn(move || run(job(rx, stop, Box::new(mem), Instant::now(), shared)))
    };
    src_tx
        .send(b"srv1 login ok\r\nPassword: ".to_vec())
        .unwrap();
    p.tx.send(WriterCmd::Bytes(b"hunter2\r".to_vec())).unwrap();
    wait(|| typed.lock().unwrap().ends_with(b"hunter2\r"));
    src_tx.send(b"\r\nWelcome\r\n".to_vec()).unwrap();
    drop(src_tx);
    wait(|| sink.0.load(Ordering::SeqCst));
    stop_tx.send(()).unwrap();
    rec.join().unwrap();
    let text = mem.text();
    validate(&text).unwrap();
    assert!(text.contains("srv1 login ok"), "first byte: {text}");
    assert!(text.contains("Welcome"), "{text}");
    assert!(!text.contains("hunter2"), "input leaked: {text}");
    assert!(!text.contains(",\"i\","), "{text}");
}

/// Приёмка: диск без места — флаг ошибки; обязательная запись блокирует ввод.
#[test]
fn disk_full_fails_and_blocks_required() {
    let (tx, rx) = unbounded();
    let (_stop_tx, stop) = bounded(1);
    let start = Instant::now();
    // Больше буфера: BufWriter пишет в файл, не дожидаясь сброса.
    for _ in 0..(BUF / 1024 + 4) {
        tx.send(TapEvent::Output(start, vec![b'x'; 1024])).unwrap();
    }
    drop(tx);
    let shared = Arc::new(RecShared::default());
    run(job(
        rx,
        stop,
        Box::new(Full { left: 200 }),
        start,
        shared.clone(),
    ));
    assert!(shared.failed() && shared.done());
    assert!(shared.error().is_some());
    let mk = |required| Recording {
        shared: shared.clone(),
        started: start,
        required,
        reported: false,
        frozen: None,
        _stop: None,
    };
    assert!(mk(Some(PolicySource::Gpo)).blocks_input());
    assert!(!mk(None).blocks_input(), "manual recording: input stays");
}

/// Файл не открылся (нет прав) — ошибка сразу, поток выходит.
#[test]
fn open_error_fails() {
    let (_tx, rx) = unbounded();
    let (_stop_tx, stop) = bounded(1);
    let shared = Arc::new(RecShared::default());
    let mut j = job(
        rx,
        stop,
        Box::new(Mem::default()),
        Instant::now(),
        shared.clone(),
    );
    j.open = Box::new(|| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    run(j);
    assert!(shared.failed() && shared.done());
}

/// Остановка: прочитанное до неё — в файле, затем файл закрыт.
#[test]
fn stop_drains_and_closes() {
    let (tx, rx) = unbounded();
    let (stop_tx, stop) = bounded(1);
    let start = Instant::now();
    tx.send(TapEvent::Output(start, b"before stop".to_vec()))
        .unwrap();
    stop_tx.send(()).unwrap();
    let mem = Mem::default();
    let shared = Arc::new(RecShared::default());
    run(job(rx, stop, Box::new(mem.clone()), start, shared.clone()));
    assert!(mem.text().contains("before stop"));
    assert!(shared.done());
    // Отписка: отвод больше не шлёт.
    assert!(tx.send(TapEvent::Output(start, Vec::new())).is_err());
}

#[test]
fn settings_policy_wins() {
    use urfin_core::{GpoValue, PolicyKey};
    let mut cfg = Config::default();
    let s = Settings::read(&cfg, None);
    assert_eq!(s, Settings::default());
    assert_eq!(s.ssh(), SshRule::User(UserMode::Manual));
    cfg.ext = r#"
[ssh]
recording = "always"
recordings_dir = 'D:\rec'
"#
    .parse()
    .unwrap();
    let s = Settings::read(&cfg, None);
    assert_eq!(s.ssh(), SshRule::User(UserMode::Always));
    assert_eq!(s.dir, r"D:\rec");
    assert!(s.changed(&cfg, None).is_none(), "same — no change");
    let (mut p, _) = PolicySet::from_toml("[ssh]\nrecording = \"forbidden\"\n");
    assert_eq!(
        s.changed(&cfg, Some(&p)).map(|n| n.ssh()),
        Some(SshRule::Forbidden(PolicySource::Toml))
    );
    p.merge_gpo([(PolicyKey::SshRecording, GpoValue::Sz("required".into()))]);
    assert_eq!(
        Settings::read(&cfg, Some(&p)).ssh(),
        SshRule::Required(PolicySource::Gpo)
    );
    let (p, _) = PolicySet::from_toml("[ssh]\nrecording = \"user\"\n");
    assert_eq!(
        Settings::read(&cfg, Some(&p)).ssh(),
        SshRule::User(UserMode::Always)
    );
}

#[test]
fn names_and_env() {
    let env = |v: &str| (v == "LOCALAPPDATA").then(|| r"C:\Users\me\AppData\Local".to_owned());
    assert_eq!(
        expand_env(DEFAULT_DIR, env),
        r"C:\Users\me\AppData\Local\Urfin\recordings"
    );
    assert_eq!(expand_env(r"%NOPE%\x", env), r"%NOPE%\x");
    assert_eq!(expand_env("100% sure", env), "100% sure");
    assert_eq!(
        file_name("srv1", "2026-10-23-1402", 1),
        "srv1-2026-10-23-1402.cast"
    );
    assert_eq!(file_name("a:b/c", "s", 2), "a_b_c-s-2.cast");
    assert_eq!(file_name("..", "s", 1), "session-s.cast");
}

/// Занятое имя — `-2`; папка создаётся; показ — с папкой как в конфиге.
#[test]
fn open_in_creates_unique() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("rec");
    let (_f, p1, shown) = open_in(&dir, r"%X%\rec\", "srv1", "2026-10-23-1402").unwrap();
    assert!(p1.ends_with("srv1-2026-10-23-1402.cast"));
    assert_eq!(shown, r"%X%\rec\srv1-2026-10-23-1402.cast");
    let (_f, p2, _) = open_in(&dir, "d", "srv1", "2026-10-23-1402").unwrap();
    assert!(p2.ends_with("srv1-2026-10-23-1402-2.cast"));
}

/// Настоящий поток Runtime: файл на диске проходит валидатор.
#[test]
fn file_on_disk_is_valid() {
    let tmp = tempfile::tempdir().unwrap();
    let tap = Tap::default();
    let (rx, stop) = (tap.subscribe(), bounded::<()>(1));
    let shared = Arc::new(RecShared::default());
    let dir = tmp.path().to_path_buf();
    let mut j = job(
        rx,
        stop.1,
        Box::new(Mem::default()),
        Instant::now(),
        shared.clone(),
    );
    j.open = Box::new(move || {
        let (f, p, s) = open_in(&dir, "d", "pwsh", "2026-01-01-0000")?;
        Ok((Box::new(f) as Box<dyn CastFile>, p, s))
    });
    let h = thread::spawn(move || run(j));
    tap.output(b"PS C:\\> dir\r\n");
    tap.resize(PtySize { cols: 90, rows: 20 });
    wait(|| shared.bytes() > 150);
    stop.0.send(()).unwrap();
    h.join().unwrap();
    let path = shared.path().unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(validate(&text), Ok(2), "{text}");
}
