//! S7-12 через core: слежение за растущим, усекаемым, удаляемым и ротируемым файлом.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, Location, Services, UiEvent, Waker};

use crate::layout::rows_down;
use crate::source::SourceKind;
use crate::{EditorCfg, InboxMsg, ViewState, ViewerCfg, ViewerDoc, register_with};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn core(dir: &Path) -> Core {
    let root = Location::local(dir);
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    register_with(&mut core, ViewerCfg::default(), EditorCfg::default());
    core.pump(Duration::from_millis(5));
    core
}

fn pump_until(core: &mut Core, mut f: impl FnMut(&Core) -> bool) -> Duration {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(2));
        if f(core) {
            return t.elapsed();
        }
        assert!(t.elapsed() < Duration::from_secs(5), "timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn view(core: &Core) -> &ViewState {
    core.state().ext.get::<ViewState>().unwrap()
}

fn doc(core: &Core) -> &ViewerDoc {
    let v = view(core);
    v.viewer(v.active_viewer().unwrap()).unwrap()
}

fn open(core: &mut Core, path: &Path) {
    core.dispatch(Command::new("viewer.open").arg("path", Location::local(path)));
    pump_until(core, |c| {
        let v = view(c);
        v.active_viewer()
            .and_then(|id| v.viewer(id))
            .is_some_and(ViewerDoc::is_ready)
    });
    let v = view(core);
    let doc = v.active_viewer().unwrap();
    v.inbox.sender().send(InboxMsg::Viewport {
        doc,
        cols: 100,
        rows: 20,
    });
    core.pump(Duration::from_millis(5));
}

/// Байты `[at, at + n)` видны на экране: строки от `top` вниз по кэшу их покрывают.
fn visible(d: &ViewerDoc, at: u64) -> bool {
    let spec = d.view_layout();
    let (_, rows) = d.viewport();
    rows_down(&d.pages, d.pos(&spec), rows as usize, &spec).is_ok_and(|r| {
        r.iter()
            .any(|row| row.start <= at && at < row.end.max(row.start + 1))
    })
}

fn toasts(core: &mut Core) -> Vec<String> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn lines(from: usize, n: usize) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for i in from..from + n {
        let _ = writeln!(s, "{i:08} запись журнала: всё в порядке");
    }
    s
}

/// Приёмка V6: писатель дописывает файл, новые строки видны на экране ≤ 300 мс; источник
/// Owned заменяется на `File`, прилипание к концу.
#[test]
fn grew_lines_visible_within_300ms() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v6.log");
    std::fs::write(&path, lines(0, 2000)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &path);
    assert_eq!(doc(&core).kind, Some(SourceKind::Owned));
    core.dispatch(Command::new("viewer.follow"));
    pump_until(&mut core, |c| doc(c).kind == Some(SourceKind::File));
    let d = doc(&core);
    assert!(d.follow && d.follow_stick && !d.len_final);

    let mut w = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    let mut worst = Duration::ZERO;
    let mut next = 2000;
    for _ in 0..8 {
        // Порция «10 МБ/с»: 100 строк, затем ожидание её последней строки на экране.
        let chunk = lines(next, 100);
        next += 100;
        let at = std::fs::metadata(&path).unwrap().len() + chunk.len() as u64 - 2;
        w.write_all(chunk.as_bytes()).unwrap();
        w.flush().unwrap();
        let t = pump_until(&mut core, |c| visible(doc(c), at));
        println!("{t:?}");
        worst = worst.max(t);
    }
    println!("V6: худшая задержка {worst:?}");
    assert!(worst <= Duration::from_millis(300), "{worst:?}");
    pump_until(&mut core, |c| doc(c).index.lines == next as u64);

    // Выключение: источник финальный, индексатор завершается.
    core.dispatch(Command::new("viewer.follow"));
    pump_until(&mut core, |c| doc(c).index.done);
    let d = doc(&core);
    assert!(!d.follow && d.len_final);
}

/// Прокрутка вверх снимает прилипание, End возвращает.
#[test]
fn scroll_up_pauses_end_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    // Больше 4 МБ: источник `File`, рост включается на месте.
    std::fs::write(&path, lines(0, 120_000)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &path);
    assert_eq!(doc(&core).kind, Some(SourceKind::File));
    core.dispatch(Command::new("viewer.follow"));
    let end = doc(&core).len;
    pump_until(&mut core, |c| visible(doc(c), end - 2));
    core.dispatch(Command::new("viewer.scroll").arg("dir", "page_up"));
    core.pump(Duration::from_millis(5));
    let top = doc(&core).top;
    assert!(!doc(&core).follow_stick);

    let mut w = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    w.write_all(lines(120_000, 50).as_bytes()).unwrap();
    w.flush().unwrap();
    let grown = std::fs::metadata(&path).unwrap().len();
    pump_until(&mut core, |c| doc(c).len == grown);
    assert_eq!(doc(&core).top, top, "без прилипания позиция стоит");

    core.dispatch(Command::new("viewer.scroll").arg("dir", "end"));
    pump_until(&mut core, |c| visible(doc(c), grown - 2));
    assert!(doc(&core).follow_stick);
}

/// Писатель усекает файл и пишет заново, потом удаляет и создаёт другой с тем же именем:
/// вьювер показывает новое содержимое, тосты «усечён» и «заменён».
#[test]
fn truncate_delete_and_rotate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rot.log");
    std::fs::write(&path, lines(0, 3000)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &path);
    core.dispatch(Command::new("viewer.follow"));
    pump_until(&mut core, |c| doc(c).kind == Some(SourceKind::File));
    toasts(&mut core);

    // Усечение и новая запись (std открывает со всеми режимами совместного доступа).
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(0).unwrap();
    drop(f);
    let short = lines(9000, 10);
    pump_until(&mut core, |c| doc(c).len == 0);
    std::fs::write(&path, &short).unwrap();
    pump_until(&mut core, |c| {
        let d = doc(c);
        d.len == short.len() as u64 && visible(d, 0) && d.index.lines == 10
    });
    let t = toasts(&mut core);
    assert_eq!(t.len(), 1, "{t:?}");
    let first = doc(&core).pages.get(0..8).map(|b| b.to_vec()).unwrap();
    assert_eq!(first, b"00009000");

    // Удаление при открытом handle: вьювер живёт дальше, без тостов.
    std::fs::remove_file(&path).unwrap();
    for _ in 0..3 {
        core.pump(Duration::from_millis(100));
    }
    assert!(doc(&core).follow);
    assert_eq!(toasts(&mut core), Vec::<String>::new());

    // Новый файл по тому же пути — ротация, переоткрытие.
    let fresh = lines(5, 3);
    let mut f = File::create(&path).unwrap();
    f.write_all(fresh.as_bytes()).unwrap();
    drop(f);
    pump_until(&mut core, |c| {
        let d = doc(c);
        d.len == fresh.len() as u64 && visible(d, 0)
    });
    let first = doc(&core).pages.get(0..8).map(|b| b.to_vec()).unwrap();
    assert_eq!(first, b"00000005");
    assert_eq!(toasts(&mut core).len(), 1);
    // Новый файл растёт — видно.
    let mut w = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    w.write_all(lines(6, 1).as_bytes()).unwrap();
    drop(w);
    let len = std::fs::metadata(&path).unwrap().len();
    pump_until(&mut core, |c| visible(doc(c), len - 2));
}

/// Закрытие документа гасит поток опроса и отпускает файл.
#[test]
fn close_stops_follow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.log");
    std::fs::write(&path, lines(0, 10)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &path);
    core.dispatch(Command::new("viewer.follow"));
    pump_until(&mut core, |c| doc(c).kind == Some(SourceKind::File));
    core.dispatch(Command::new("viewer.close"));
    core.pump(Duration::from_millis(5));
    assert!(view(&core).is_empty());
    // Поток закрытого документа не мешает удалить файл.
    std::thread::sleep(Duration::from_millis(400));
    std::fs::remove_file(&path).unwrap();
}

/// Простой ≤ 0,1 % CPU: шаг опроса (stat по handle и id файла по пути) — время одного
/// шага × 4 шага в секунду.
#[test]
#[ignore = "замер: cargo nextest run -p urfin-view --release --run-ignored only follow_idle"]
fn follow_idle_cpu() {
    use crate::source::{ByteSource, FileSource};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("idle.log");
    std::fs::write(&path, lines(0, 100)).unwrap();
    let src = FileSource::new(File::open(&path).unwrap(), path.clone(), true);
    let mut w = crate::viewer::Watch {
        len: src.len(),
        id: src.file_id(),
        ignore_path: false,
    };
    let n = 2000;
    let t = Instant::now();
    for _ in 0..n {
        let len = src.refresh_len().unwrap();
        let _ = w.step(len, || {
            let f = File::open(&path).ok()?;
            crate::sys::file_id(&f).ok()
        });
    }
    let per = t.elapsed() / n;
    let cpu = per.as_secs_f64() * 4.0 * 100.0;
    println!("follow: шаг {per:?}, простой ≈ {cpu:.4} % CPU (4 шага/с)");
    assert!(cpu <= 0.1, "{cpu}");
}
