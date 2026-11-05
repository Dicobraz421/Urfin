//! Процессы на подставном менеджере `fake-pkgmgr` (urfin-testdata, приёмка F02-02).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use urfin_core::CancelToken;

use super::*;
use crate::testutil::fake;

fn argv(args: &[&str]) -> Vec<OsString> {
    std::iter::once(fake().into_os_string())
        .chain(args.iter().map(OsString::from))
        .collect()
}

fn cx(timeout_ms: Option<u64>) -> RunCx {
    RunCx {
        cancel: CancelToken::new(),
        timeout: timeout_ms.map(Duration::from_millis),
        lines: None,
        tape: None,
    }
}

/// Приёмка: CP866 и UTF-8 в одном потоке; `\r`-перезаписи и VT сняты; stderr — там же.
#[test]
fn decodes_mixed_stream() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("mixed.txt");
    let mut bytes = "Найдено: Mozilla Firefox\r\n".as_bytes().to_vec();
    bytes.extend_from_slice(&encoding_rs::IBM866.encode("Установка пакета").0);
    bytes.extend_from_slice(b"\r\n  - \r  \\ \r10%\r100%\r\n\x1b[32mOK\x1b[0m\n");
    std::fs::write(&fixture, bytes).unwrap();
    let args = argv(&[
        "--fixture",
        fixture.to_str().unwrap(),
        "--delay-ms",
        "10",
        "--stderr",
        "--line",
        "warning",
    ]);
    let (code, lines) = run_with(&args, &cx(Some(10_000)), Decoder::new(866), Mode::Query).unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        lines,
        [
            "Найдено: Mozilla Firefox",
            "Установка пакета",
            "100%",
            "OK",
            "warning"
        ]
    );
}

#[test]
fn exit_code_and_tail() {
    let out = run_query(
        &argv(&["--line", "a", "--line", "", "--line", "b", "--exit", "3"]),
        &cx(Some(10_000)),
    )
    .unwrap();
    assert_eq!(out.code, 3);
    assert_eq!(
        out.ok(),
        Err(PkgError::Exit {
            code: 3,
            tail: "a\nb".into()
        })
    );
}

/// Приёмка: отмена дерева из 3 процессов ≤100 мс. Возврат — после EOF вывода, а EOF
/// приходит, только когда все три процесса (у всех — наш пайп) мертвы.
#[test]
fn cancel_tree_of_three() {
    let mut c = cx(None);
    let token = c.cancel.clone();
    let ready = Arc::new(AtomicUsize::new(0));
    let at = Arc::new(Mutex::new(None::<Instant>));
    let (r, a) = (ready.clone(), at.clone());
    c.lines = Some(Arc::new(move |l: &str| {
        if l == "ready" && r.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
            *a.lock().unwrap() = Some(Instant::now());
            token.cancel();
        }
    }));
    let args = argv(&["--line", "ready", "--spawn", "2", "--sleep-ms", "60000"]);
    let r = run_query(&args, &c);
    let elapsed = at.lock().unwrap().expect("3 processes ready").elapsed();
    assert_eq!(r, Err(PkgError::Cancelled));
    assert_eq!(ready.load(Ordering::SeqCst), 3);
    assert!(elapsed <= Duration::from_millis(100), "{elapsed:?}");
}

/// Приёмка: таймаут — `PkgError::Timeout`, читатель вывода завершён (его копия
/// приёмника отпущена). Дерево (процесс и его потомок) спит 60 с: без убийства дерева
/// читатель ждал бы его. Граница — с запасом на запуск процессов под нагрузкой CI
/// (было 2 с вместе со сборкой `fake-pkgmgr` — раз не хватило), но вдвое меньше сна.
#[test]
fn timeout_kills_and_joins_reader() {
    const SLEEP: Duration = Duration::from_secs(60);
    let mut c = cx(Some(300));
    let sink: LineSink = Arc::new(|_: &str| {});
    c.lines = Some(sink.clone());
    // Путь к `fake-pkgmgr` (под `-p urfin-pkg` — его сборка) — до отсчёта.
    let sleep = SLEEP.as_millis().to_string();
    let args = argv(&["--line", "x", "--spawn", "1", "--sleep-ms", &sleep]);
    let t = Instant::now();
    let r = run_query(&args, &c);
    assert_eq!(r, Err(PkgError::Timeout));
    assert!(t.elapsed() < SLEEP / 2, "{:?}", t.elapsed());
    drop(c);
    assert_eq!(Arc::strong_count(&sink), 1);
}

/// Приёмка: UI-поток не блокируется — запрос идёт в выделенном потоке Runtime,
/// постановка мгновенна, ответ приходит позже.
#[test]
fn query_off_ui_thread() {
    struct NoWake;
    impl urfin_core::Waker for NoWake {
        fn wake(&self) {}
        fn wake_after(&self, _d: Duration) {}
    }
    let (rt, _inbox) =
        urfin_core::Runtime::new(&urfin_core::RuntimeConfig::default(), Arc::new(NoWake));
    let (tx, rx) = mpsc::channel();
    let args = argv(&["--sleep-ms", "500", "--line", "done"]);
    let t = Instant::now();
    rt.spawn_dedicated("pkg.query".into(), rt.token(), move |_| {
        let _ = tx.send(run_query(&args, &cx(Some(10_000))));
    });
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    let out = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert_eq!(out.lines, ["done"]);
}

#[test]
fn missing_program() {
    let args = [OsString::from(r"C:\nonexistent\urfin-fake-missing.exe")];
    assert_eq!(run_query(&args, &cx(None)), Err(PkgError::NotFound));
}

#[test]
fn action_exit_codes() {
    let plan = |code: &str| ActionPlan {
        argv: argv(&["--line", "installing", "--exit", code]),
        elevate: false,
        success_codes: vec![0],
        reboot_codes: vec![3010],
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let mut c = cx(None);
    c.lines = Some(Arc::new(move |l: &str| {
        s.lock().unwrap().push(l.to_owned());
    }));
    assert_eq!(run_action(&plan("0"), &c), Ok(ActionDone::Ok));
    assert_eq!(run_action(&plan("3010"), &c), Ok(ActionDone::Reboot));
    assert_eq!(
        run_action(&plan("5"), &c),
        Err(PkgError::Exit {
            code: 5,
            tail: "installing".into()
        })
    );
    assert_eq!(seen.lock().unwrap().len(), 3);
    let elevated = ActionPlan {
        elevate: true,
        ..plan("0")
    };
    assert!(matches!(run_action(&elevated, &c), Err(PkgError::Spawn(_))));
}
