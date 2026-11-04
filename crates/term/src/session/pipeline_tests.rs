//! Pipeline сеанса: фейковый PTY (pipe), разбиение на чанки, poke, DEC 2026,
//! настоящий `cmd.exe` в простое (приёмка S6-04).

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use proptest::prelude::*;

use super::*;

const SIZE: PtySize = PtySize { cols: 40, rows: 8 };

fn opts(nonce: Option<&str>) -> SessionOpts {
    SessionOpts {
        size: SIZE,
        scrollback: 100,
        osc52: "copy".into(),
        nonce: nonce.map(str::to_owned),
    }
}

#[derive(Default)]
struct TestSink {
    pokes: AtomicUsize,
    events: Mutex<Vec<SessionEvent>>,
    wake: Mutex<Option<crossbeam_channel::Sender<()>>>,
}

impl SessionSink for TestSink {
    fn poke(&self) {
        self.pokes.fetch_add(1, Ordering::SeqCst);
        if let Some(w) = &*self.wake.lock().unwrap() {
            let _ = w.send(());
        }
    }

    fn event(&self, ev: SessionEvent) {
        self.events.lock().unwrap().push(ev);
    }
}

impl TestSink {
    fn events(&self) -> Vec<SessionEvent> {
        self.events.lock().unwrap().clone()
    }

    fn wait_event(&self, pred: impl Fn(&SessionEvent) -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if self.events().iter().any(&pred) {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

/// Источник, отдающий заданные чанки по одному за `read`.
struct Chunks(VecDeque<Vec<u8>>);

impl Read for Chunks {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(mut c) = self.0.pop_front() else {
            return Ok(0);
        };
        if c.is_empty() {
            return self.read(buf);
        }
        let n = c.len().min(buf.len());
        if n < c.len() {
            self.0.push_front(c.split_off(n));
        }
        buf[..n].copy_from_slice(&c);
        Ok(n)
    }
}

/// Сетка целиком (история + экран): символ и цвет каждой ячейки, курсор.
fn dump(shared: &TermShared) -> Vec<String> {
    let vt = shared.vt.lock();
    let g = vt.term.grid();
    let top = -i32::try_from(g.history_size()).unwrap();
    let rows = i32::try_from(g.screen_lines()).unwrap();
    let mut out: Vec<String> = (top..rows)
        .map(|l| {
            let row = &g[Line(l)];
            let mut s = String::new();
            for c in 0..g.columns() {
                let cell = &row[Column(c)];
                let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{}{:?}", cell.c, cell.fg));
            }
            s
        })
        .collect();
    out.push(format!("{:?}", g.cursor.point));
    out
}

fn text(shared: &TermShared) -> String {
    let vt = shared.vt.lock();
    let g = vt.term.grid();
    let rows = i32::try_from(g.screen_lines()).unwrap();
    (0..rows)
        .map(|l| {
            let row = &g[Line(l)];
            (0..g.columns())
                .map(|c| row[Column(c)].c)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn piece() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        "[a-zа-я0-9 ]{0,20}".prop_map(String::into_bytes),
        Just(b"\r\n".to_vec()),
        Just(b"\x1b[31m".to_vec()),
        Just(b"\x1b[38;2;10;20;30m".to_vec()),
        Just(b"\x1b[0m".to_vec()),
        Just(b"\x1b[2;5H".to_vec()),
        Just(b"\x1b[K".to_vec()),
        Just("日本".as_bytes().to_vec()),
        Just(b"\x1b]0;t\x07".to_vec()),
        Just(b"\x1b]633;A\x07".to_vec()),
        Just(b"\x1b]633;B\x07".to_vec()),
        Just(b"\x1b]633;D;1\x07".to_vec()),
        Just(b"\x1b]633;P;FnCwd=k;C:\\w\x07".to_vec()),
    ]
}

fn parse(chunks: Vec<Vec<u8>>) -> (Vec<String>, Vec<SessionEvent>) {
    let sink = TestSink::default();
    let shared = parse_inline(Chunks(chunks.into()), &opts(Some("k")), &sink);
    (dump(&shared), sink.events())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Приёмка S6-04: любое разбиение потока на чанки даёт те же события и сетку.
    #[test]
    fn any_split_same_grid_and_events(
        parts in proptest::collection::vec(piece(), 0..40),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..16),
    ) {
        let stream: Vec<u8> = parts.concat();
        let whole = parse(vec![stream.clone()]);
        let mut at: Vec<usize> = cuts.iter().map(|c| c.index(stream.len() + 1)).collect();
        at.sort_unstable();
        let mut chunks = Vec::new();
        let mut prev = 0;
        for a in at {
            chunks.push(stream[prev..a].to_vec());
            prev = a;
        }
        chunks.push(stream[prev..].to_vec());
        prop_assert_eq!(parse(chunks), whole);
    }
}

/// Порт для тестов: запись — в общий буфер, закрытие отмечается.
#[derive(Clone, Default)]
struct FakePort {
    written: Arc<Mutex<Vec<u8>>>,
    closed: Arc<AtomicUsize>,
}

impl PtyPort for FakePort {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.written.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }

    fn resize(&mut self, _size: PtySize) {}

    fn close(&mut self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

fn threads(name: String, f: Box<dyn FnOnce() + Send>) {
    thread::Builder::new().name(name).spawn(f).unwrap();
}

#[test]
fn replies_events_and_exit_over_pipe() {
    let (r, mut w) = io::pipe().unwrap();
    let port = FakePort::default();
    let sink = Arc::new(TestSink::default());
    let p = start_pipeline(
        &threads,
        "t",
        r,
        port.clone(),
        || Some(7),
        &opts(Some("k")),
        sink.clone(),
        Arc::default(),
    );
    w.write_all(b"\x1b]633;A\x07hi\x1b[6n\x1b]0;T\x07\x1b]633;P;FnCwd=k;C:\\w\x07")
        .unwrap();
    assert!(sink.wait_event(|e| matches!(e, SessionEvent::Title(Some(t)) if t == "T")));
    let t0 = Instant::now();
    while port.written.lock().unwrap().is_empty() && t0.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(&*port.written.lock().unwrap(), b"\x1b[1;3R", "DSR reply");
    drop(w);
    assert!(sink.wait_event(|e| *e == SessionEvent::Exited(Some(7))));
    let t0 = Instant::now();
    while port.closed.load(Ordering::SeqCst) == 0 && t0.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(port.closed.load(Ordering::SeqCst), 1, "writer closes once");
    let ev = sink.events();
    let zero = Mark {
        line: 0,
        history: 0,
        col: 0,
    };
    assert_eq!(
        ev[0],
        SessionEvent::Osc {
            event: OscEvent::PromptStart,
            at: zero
        }
    );
    assert!(ev.contains(&SessionEvent::Osc {
        event: OscEvent::Cwd {
            path: "C:\\w".into(),
            source: CwdSource::FnCwd,
            host: String::new(),
        },
        // После «hi»: колонка курсора — 2.
        at: Mark { col: 2, ..zero }
    }));
    assert!(text(&p.shared).starts_with("hi "));
}

/// Приёмка: poke не чаще кадров — пока UI не забрал изменения, будить нечего.
#[test]
fn pokes_coalesce_to_frames() {
    let (r, mut w) = io::pipe().unwrap();
    let sink = Arc::new(TestSink::default());
    let (wake_tx, wake_rx) = crossbeam_channel::unbounded();
    *sink.wake.lock().unwrap() = Some(wake_tx);
    let p = start_pipeline(
        &threads,
        "p",
        r,
        FakePort::default(),
        || None,
        &opts(None),
        sink.clone(),
        Arc::default(),
    );
    let shared = p.shared.clone();
    let ui = thread::spawn(move || {
        let mut frames = 0;
        while wake_rx.recv_timeout(Duration::from_secs(2)).is_ok() {
            // Кадр: 4 мс работы, затем снапшот сбрасывает dirty.
            thread::sleep(Duration::from_millis(4));
            if shared.take_dirty() {
                frames += 1;
            }
        }
        frames
    });
    p.shared.take_dirty();
    let writes = 2000;
    for i in 0..writes {
        w.write_all(format!("line {i}\r\n").as_bytes()).unwrap();
        if i % 50 == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    drop(w);
    assert!(sink.wait_event(|e| matches!(e, SessionEvent::Exited(_))));
    let frames = ui.join().unwrap();
    let pokes = sink.pokes.load(Ordering::SeqCst);
    assert!(pokes <= frames, "pokes {pokes} > frames {frames}");
    assert!(pokes < writes / 4, "pokes {pokes} for {writes} writes");
}

/// Кадры UI, пока в сетке не появится `seen`: на каждое пробуждение — снапшот
/// (сброс dirty), как в боевом UI, поэтому пробуждения не теряются при любой
/// задержке потоков. Возвращает (кадров без `seen`, время от `t0` до кадра с ним).
fn frames_until(
    wake: &crossbeam_channel::Receiver<()>,
    shared: &TermShared,
    seen: &str,
    t0: Instant,
) -> (usize, Duration) {
    let mut frames = 0;
    loop {
        assert!(
            wake.recv_deadline(t0 + Duration::from_secs(10)).is_ok(),
            "no frame with {seen:?} after {frames} frames: {:?}",
            text(shared)
        );
        shared.take_dirty();
        if text(shared).contains(seen) {
            return (frames, t0.elapsed());
        }
        frames += 1;
    }
}

/// DEC 2026: пока синхронизация открыта, UI не будят; по таймауту — показ.
///
/// Без фиксированных пауз: тест — это UI, который рисует кадр на каждое
/// пробуждение. Под чужой нагрузкой reader разбирает вывод с опозданием, и
/// снапшот «через 60 мс» ловил его на полпути.
#[test]
fn sync_update_times_out() {
    let (r, mut w) = io::pipe().unwrap();
    let sink = Arc::new(TestSink::default());
    let (wake_tx, wake_rx) = crossbeam_channel::unbounded();
    *sink.wake.lock().unwrap() = Some(wake_tx);
    let p = start_pipeline(
        &threads,
        "s",
        r,
        FakePort::default(),
        || None,
        &opts(None),
        sink.clone(),
        Arc::default(),
    );
    p.shared.take_dirty();
    let sent = Instant::now();
    w.write_all(b"\x1b[?2026hHELLO").unwrap();
    let (before, at) = frames_until(&wake_rx, &p.shared, "HELLO", sent);
    // Начало синхронизации могло разбудить один раз, вывод внутри — нет.
    assert!(before <= 1, "frames before HELLO: {before}");
    // Показ — по таймауту (150 мс от разбора, разбор — после записи).
    assert!(at >= Duration::from_millis(100), "{at:?}");
    // Закрытая вовремя синхронизация показывается сразу. Таймаут не показал бы
    // её раньше 150 мс от записи, так что один показ быстрее 100 мс это
    // доказывает; под чужой нагрузкой reader просыпается позже — попыток 10.
    let mut slow = Vec::new();
    for d in b'0'..=b'9' {
        // Сначала забрать пробуждения, потом dirty: иначе запоздалый poke
        // таймаута оставит dirty поднятым, и показ цифры UI не разбудит.
        while wake_rx.try_recv().is_ok() {}
        p.shared.take_dirty();
        let t0 = Instant::now();
        w.write_all(&[&b"\x1b[?2026h"[..], &[d], b"\x1b[?2026l"].concat())
            .unwrap();
        let (_, at) = frames_until(&wake_rx, &p.shared, &char::from(d).to_string(), t0);
        if at < Duration::from_millis(100) {
            slow.clear();
            break;
        }
        slow.push(at);
    }
    assert!(slow.is_empty(), "closed sync shown only after {slow:?}");
    drop(w);
}

/// Настоящий `cmd.exe`: вывод в сетке, 0 % CPU в простое, выход с кодом.
#[test]
fn real_cmd_session_idle_and_exit() {
    let sink = Arc::new(TestSink::default());
    let spec = SpawnSpec {
        program: r"C:\Windows\System32\cmd.exe".into(),
        args: vec!["/q".into(), "/k".into(), "prompt URFIN$G".into()],
        cwd: None,
        env: Vec::new(),
    };
    let s = Session::start(1, &spec, &opts(None), sink.clone()).unwrap();
    let t0 = Instant::now();
    while !text(s.shared()).contains("URFIN>") && t0.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(text(s.shared()).contains("URFIN>"), "{}", text(s.shared()));
    // Дать ConPTY дописать хвост, затем мерить простой.
    thread::sleep(Duration::from_millis(300));
    let cpu0 = crate::sys::conpty::cpu_time();
    thread::sleep(Duration::from_secs(1));
    let idle = crate::sys::conpty::cpu_time().saturating_sub(cpu0);
    assert!(
        idle < Duration::from_millis(30),
        "idle CPU {idle:?} per 1 s"
    );
    s.write(b"echo ok\r".to_vec());
    let t0 = Instant::now();
    while !text(s.shared()).contains("ok") && t0.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(20));
    }
    s.write(b"exit 5\r".to_vec());
    assert!(
        sink.wait_event(|e| *e == SessionEvent::Exited(Some(5))),
        "{:?}",
        sink.events()
    );
}

/// vte 0.15.0 теряет `0` в `а0а`, если чанки режут оба символа; reader
/// переносит незавершённый UTF-8 в следующее чтение.
#[test]
fn utf8_split_keeps_ascii_between() {
    let chunks = vec![vec![b'x', 0xD0], vec![0xB0, b'0', 0xD0], vec![0xB0, b'y']];
    let shared = parse_inline(Chunks(chunks.into()), &opts(None), &TestSink::default());
    assert!(text(&shared).starts_with("xа0аy"), "{:?}", text(&shared));
    assert_eq!(reader::utf8_complete(b"ab"), 2);
    assert_eq!(reader::utf8_complete(&[b'a', 0xD0]), 1);
    assert_eq!(reader::utf8_complete(&[0xD0, 0xB0]), 2);
    assert_eq!(reader::utf8_complete(&[b'a', 0xE6, 0x97]), 1);
    assert_eq!(reader::utf8_complete(&[0xE6, 0x97, 0xA5]), 3);
    assert_eq!(reader::utf8_complete(&[0xF0, 0x9F, 0x98]), 0);
    assert_eq!(reader::utf8_complete(&[0x80, 0x80, 0x80]), 3);
}
