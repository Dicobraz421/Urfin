//! S7-11 (выделение, копирование) и S7-13 (быстрый просмотр, dock) через core.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, Focus, Location, PanelMode, Services, Side, UiEvent, Waker};

use crate::layout::Mode;
use crate::{
    EditorCfg, InboxMsg, Placement, QUICK_READS, QuickNoteKind, ViewState, ViewerCfg, ViewerDoc,
    register_with,
};

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

fn pump_until(core: &mut Core, mut f: impl FnMut(&Core) -> bool) {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(5));
        if f(core) {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn view(core: &Core) -> &ViewState {
    core.state().ext.get::<ViewState>().unwrap()
}

fn active(core: &Core) -> &ViewerDoc {
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
}

/// Текст `viewer.copy` из `UiEvent::Ext`.
fn copied(core: &mut Core) -> String {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(5));
        for e in core.take_ui_events() {
            if let UiEvent::Ext { id, args } = e
                && id.as_str() == "viewer.copy"
                && let Some(text) = args.str("text")
            {
                return text.to_owned();
            }
        }
        assert!(t.elapsed() < Duration::from_secs(20), "timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn select(core: &mut Core, sel: std::ops::Range<u64>) {
    let v = view(core);
    let doc = v.active_viewer().unwrap();
    v.inbox.sender().send(InboxMsg::Select {
        doc,
        sel: Some(sel),
    });
    core.pump(Duration::from_millis(5));
}

#[test]
fn copy_decodes_selection_in_document_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("cp1251.txt");
    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode("Привет, мир\nвторая строка\n");
    std::fs::write(&f, &bytes).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &f);
    core.dispatch(Command::new("viewer.codepage").arg("cp", "windows-1251"));
    core.pump(Duration::from_millis(5));
    select(&mut core, 0..11);
    assert_eq!(active(&core).sel, Some(0..11));
    core.take_ui_events();
    core.dispatch(Command::new("viewer.copy"));
    assert_eq!(copied(&mut core), "Привет, мир");

    // Ctrl+A — весь файл; Hex — байтами.
    core.dispatch(Command::new("viewer.select_all"));
    core.pump(Duration::from_millis(5));
    assert_eq!(active(&core).sel, Some(0..bytes.len() as u64));
    core.dispatch(Command::new("viewer.mode").arg("mode", "hex"));
    core.pump(Duration::from_millis(5));
    select(&mut core, 0..2);
    core.dispatch(Command::new("viewer.copy"));
    assert_eq!(copied(&mut core), "CF F0");
}

#[test]
fn copy_over_limit_toasts() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    std::fs::write(&f, "x".repeat(4096)).unwrap();
    let root = Location::local(dir.path());
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    let cfg = ViewerCfg {
        copy_limit_mb: 0,
        ..ViewerCfg::default()
    };
    register_with(&mut core, cfg, EditorCfg::default());
    core.pump(Duration::from_millis(5));
    open(&mut core, &f);
    select(&mut core, 0..10);
    core.take_ui_events();
    core.dispatch(Command::new("viewer.copy"));
    core.pump(Duration::from_millis(5));
    let ev = core.take_ui_events();
    assert!(ev.iter().any(|e| matches!(e, UiEvent::Toast { .. })));
    assert!(
        !ev.iter()
            .any(|e| matches!(e, UiEvent::Ext { id, .. } if id.as_str() == "viewer.copy"))
    );
}

#[test]
fn scroll_to_aligns_to_line_start() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    std::fs::write(&f, "0123456789\n".repeat(100)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &f);
    let doc = view(&core).active_viewer().unwrap();
    view(&core)
        .inbox
        .sender()
        .send(InboxMsg::ScrollTo { doc, off: 555 });
    core.pump(Duration::from_millis(5));
    assert_eq!(active(&core).top, 550);
}

fn quick(core: &mut Core, side: &str, path: &Path) {
    core.dispatch(
        Command::new("viewer.quick")
            .arg("side", side)
            .arg("path", Location::local(path)),
    );
    core.pump(Duration::from_millis(5));
}

fn qview(core: &mut Core) {
    core.dispatch(Command::new("panel.mode").arg("mode", "qview"));
    core.pump(Duration::from_millis(5));
    assert_eq!(core.state().panel(Side::Right).tab().mode, PanelMode::QView);
}

#[test]
fn quick_reopens_slot_and_follows_mode() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.toml");
    let b = dir.path().join("b.md");
    std::fs::write(&a, "[workspace]\nresolver = \"3\"\n").unwrap();
    std::fs::write(&b, "# Заголовок\n\nтекст\n").unwrap();
    let mut core = core(dir.path());
    qview(&mut core);
    quick(&mut core, "right", &a);
    let id = view(&core).quick[1].unwrap();
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .is_some_and(ViewerDoc::is_ready)
    });
    let d = view(&core).in_panel(Side::Right).unwrap();
    assert!(d.quick && d.placement == Placement::Panel(Side::Right));
    assert_eq!(d.pages.cap(), 16, "кэш 1 МиБ — 16 страниц");
    // Файл целиком в памяти — индекс есть («из N» в статусе).
    pump_until(&mut core, |c| {
        view(c).in_panel(Side::Right).is_some_and(|d| d.index.done)
    });
    assert_eq!(view(&core).screens.len(), 0);
    assert_eq!(core.state().focus, Focus::Panel);

    quick(&mut core, "right", &b);
    assert_ne!(
        view(&core).quick[1],
        Some(id),
        "новый DocId: ответы прошлого файла не попадут"
    );
    assert_eq!(view(&core).len(), 1, "прошлый документ закрыт");
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .is_some_and(ViewerDoc::is_ready)
    });
    let d = view(&core).in_panel(Side::Right).unwrap();
    assert!(d.md_mode(), "Markdown ≤ 4 МиБ — документом");

    // Ctrl+Q ещё раз — режим выключен, документ закрыт.
    core.dispatch(Command::new("panel.mode").arg("mode", "qview"));
    core.pump(Duration::from_millis(5));
    core.pump(Duration::from_millis(5));
    assert!(view(&core).quick[1].is_none());
    assert!(view(&core).is_empty());
}

#[test]
fn fast_cursor_reads_at_most_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    for i in 0..1000 {
        let p = dir.path().join(format!("f{i:04}.txt"));
        std::fs::write(&p, format!("файл {i}\n")).unwrap();
        files.push(p);
    }
    let mut core = core(dir.path());
    qview(&mut core);
    let before = QUICK_READS.load(Ordering::Relaxed);
    // Все команды — в одном проходе: каждая отменяет прошлое открытие.
    for p in &files {
        core.dispatch(
            Command::new("viewer.quick")
                .arg("side", "right")
                .arg("path", Location::local(p)),
        );
    }
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .is_some_and(ViewerDoc::is_ready)
    });
    assert_eq!(view(&core).len(), 1);
    let d = view(&core).in_panel(Side::Right).unwrap();
    assert_eq!(d.loc.path, files[999]);
    let reads = QUICK_READS.load(Ordering::Relaxed) - before;
    // Чтение на сторону — одно: первое (его отменил следующий курсор) и последнее.
    // Счётчик общий для процесса: nextest гоняет тест в своём.
    assert!(reads <= 2, "reads = {reads}");
}

#[test]
fn offline_file_is_not_opened() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("cloud.txt");
    std::fs::write(&f, "в облаке\n").unwrap();
    // `FILE_ATTRIBUTE_OFFLINE` ставится и без облака (`attrib +O`).
    let ok = std::process::Command::new("attrib")
        .arg("+O")
        .arg(&f)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "attrib +O");
    let mut core = core(dir.path());
    qview(&mut core);
    quick(&mut core, "right", &f);
    pump_until(&mut core, |c| view(c).qnote[1].is_some());
    let v = view(&core);
    assert!(v.quick[1].is_none());
    assert_eq!(
        v.qnote[1].as_ref().map(|n| &n.kind),
        Some(&QuickNoteKind::Cloud)
    );
    let _ = std::process::Command::new("attrib")
        .arg("-O")
        .arg(&f)
        .status();
}

#[test]
fn focus_dock_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    std::fs::write(&a, "одна\nдве\nтри\n".repeat(100)).unwrap();
    let mut core = core(dir.path());
    qview(&mut core);
    quick(&mut core, "right", &a);
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .is_some_and(ViewerDoc::is_ready)
    });
    let qid = view(&core).quick[1].unwrap();

    // Tab: фокус — быстрому просмотру, команды вьювера — ему.
    core.dispatch(
        Command::new("viewer.quick")
            .arg("side", "right")
            .arg("path", Location::local(&a))
            .arg("focus", true),
    );
    core.pump(Duration::from_millis(5));
    assert_eq!(core.state().focus, Focus::Viewer);
    assert_eq!(view(&core).panel_focus(), Some(Side::Right));
    assert_eq!(view(&core).active_viewer(), Some(qid));
    core.dispatch(Command::new("viewer.mode").arg("mode", "hex"));
    core.pump(Duration::from_millis(5));
    assert_eq!(view(&core).in_panel(Side::Right).unwrap().mode, Mode::Hex);

    // F3 из фокуса: тот же документ — в окно.
    core.dispatch(Command::new("viewer.dock"));
    core.pump(Duration::from_millis(5));
    let v = view(&core);
    assert_eq!(v.screens, vec![qid]);
    assert!(v.quick[1].is_none() && v.panel_focus().is_none());
    let d = v.viewer(qid).unwrap();
    assert!(!d.quick && d.placement == Placement::Window);

    // `viewer.dock`: окно → пассивная панель, фокус — ему; Tab (`panel.focus`) — к панели.
    core.dispatch(Command::new("viewer.dock"));
    core.pump(Duration::from_millis(5));
    let v = view(&core);
    assert_eq!(v.screens.len(), 0);
    assert_eq!(v.quick[1], Some(qid));
    assert_eq!(v.panel_focus(), Some(Side::Right));
    assert_eq!(core.state().focus, Focus::Viewer);
    core.dispatch(Command::new("panel.focus"));
    core.pump(Duration::from_millis(5));
    core.pump(Duration::from_millis(5));
    assert_eq!(core.state().focus, Focus::Panel);
    assert!(view(&core).qfocus.is_none());
    // Перенесённый вьювер за курсором не следует.
    let other = dir.path().join("b.txt");
    std::fs::write(&other, "b\n").unwrap();
    quick(&mut core, "right", &other);
    assert_eq!(view(&core).quick[1], Some(qid));

    // Новый экран забирает фокус у области.
    core.dispatch(
        Command::new("viewer.quick")
            .arg("side", "right")
            .arg("path", Location::local(&a))
            .arg("focus", true),
    );
    core.pump(Duration::from_millis(5));
    open(&mut core, &other);
    assert!(view(&core).panel_focus().is_none());
    assert_eq!(active(&core).loc.path, other);
}
