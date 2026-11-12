//! S7-11 (выделение, копирование) и S7-13 (быстрый просмотр, dock) через core.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{
    Boot, CmdError, Command, Core, Focus, Location, OverlayKind, PanelMode, Services, Side,
    UiEvent, Waker,
};

use crate::layout::Mode;
use crate::{
    EditorCfg, InboxMsg, Placement, PreviewHit, Prio, QuickNoteKind, ViewState, ViewerCfg,
    ViewerDoc, register_with,
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
    // Чтение на сторону — одно: первое (его отменил следующий курсор) и последнее. Журнал
    // общий на процесс: считаем только свою папку.
    let root = Location::local(dir.path()).path;
    let reads = crate::viewer::QUICK_READ_LOG
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(&root))
        .count();
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
    // S7-46: без фокуса страницы быстрого просмотра просятся фоновым приоритетом.
    let prio = |c: &Core| view(c).viewer(qid).unwrap().prio;
    assert_eq!(prio(&core), Prio::Background);

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
    assert_eq!(prio(&core), Prio::Visible, "с фокусом — видимый");
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
    assert_eq!(d.prio, Prio::Visible, "окно — видимый");

    // `viewer.dock`: окно → пассивная панель, фокус — ему; Tab (`panel.focus`) — к панели.
    core.dispatch(Command::new("viewer.dock"));
    core.pump(Duration::from_millis(5));
    let v = view(&core);
    assert_eq!(v.screens.len(), 0);
    assert_eq!(v.quick[1], Some(qid));
    assert_eq!(v.panel_focus(), Some(Side::Right));
    assert_eq!(core.state().focus, Focus::Viewer);
    assert_eq!(prio(&core), Prio::Visible);
    core.dispatch(Command::new("panel.focus"));
    core.pump(Duration::from_millis(5));
    core.pump(Duration::from_millis(5));
    assert_eq!(core.state().focus, Focus::Panel);
    assert!(view(&core).qfocus.is_none());
    assert_eq!(
        prio(&core),
        Prio::Background,
        "фокус ушёл к панели — снова фоновый"
    );
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

/// Поиск в фокусе быстрого просмотра считает совпадения — «N из M» (S7-13).
#[test]
fn quick_focus_search_counts() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    std::fs::write(&a, "слово\nдругое\n".repeat(50)).unwrap();
    let mut core = core(dir.path());
    qview(&mut core);
    quick(&mut core, "right", &a);
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .is_some_and(ViewerDoc::is_ready)
    });
    core.dispatch(
        Command::new("viewer.quick")
            .arg("side", "right")
            .arg("path", Location::local(&a))
            .arg("focus", true),
    );
    core.pump(Duration::from_millis(5));
    core.dispatch(Command::new("viewer.search").arg("text", "слово"));
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .and_then(|d| d.search.as_ref())
            .is_some_and(|s| s.count.done)
    });
    let d = view(&core).in_panel(Side::Right).unwrap();
    assert_eq!(d.search.as_ref().unwrap().count.n, 50);
}

/// Экран поиска держит фокус `Overlay(Search)`; в тестах его ставит эта команда (окна нет).
struct FocusProbe;

static FOCUS_COMMANDS: [urfin_core::CommandMeta; 2] = [
    urfin_core::CommandMeta {
        id: "qtest.search",
        title: "",
        category: "",
        args: &[],
        repeatable: false,
    },
    urfin_core::CommandMeta {
        id: "qtest.panel",
        title: "",
        category: "",
        args: &[],
        repeatable: false,
    },
];

impl urfin_core::Handler for FocusProbe {
    fn namespace(&self) -> &'static str {
        "qtest"
    }

    fn commands(&self) -> &'static [urfin_core::CommandMeta] {
        &FOCUS_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut urfin_core::Cx<'_>) -> Result<(), CmdError> {
        cx.state.focus = if cmd.id.as_str() == "qtest.search" {
            Focus::Overlay(OverlayKind::Search)
        } else {
            Focus::Panel
        };
        Ok(())
    }
}

/// Core с открытым экраном поиска.
fn on_screen(dir: &Path) -> Core {
    let mut c = core(dir);
    c.register(Box::new(FocusProbe));
    c.dispatch(Command::new("qtest.search"));
    c
}

fn leave_screen(core: &mut Core) {
    core.dispatch(Command::new("qtest.panel"));
    core.pump(Duration::from_millis(5));
    core.pump(Duration::from_millis(5));
}

fn preview_cmd(path: &Path, line: i64, hit: Option<&str>) -> Command {
    let c = Command::new("viewer.quick")
        .arg("side", "right")
        .arg("path", Location::local(path))
        .arg("line", line);
    match hit {
        Some(h) => c.arg("hit", h),
        None => c,
    }
}

fn mark(core: &Core) -> Option<PreviewHit> {
    view(core).in_panel(Side::Right)?.preview.clone()
}

/// Ждёт, пока строка `line` (с 0) встанет и совпадение будет найдено (или не найдено).
fn settled(core: &mut Core, line: u64) -> PreviewHit {
    pump_until(core, |c| {
        view(c).in_panel(Side::Right).is_some_and(|d| {
            d.goto_wait.is_none()
                && d.reveal.is_none()
                && d.preview
                    .as_ref()
                    .is_some_and(|p| p.line == line && p.start.is_some() && !p.searching())
        })
    });
    mark(core).unwrap()
}

fn reads(dir: &Path) -> usize {
    let root = Location::local(dir).path;
    crate::viewer::QUICK_READ_LOG
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(&root))
        .count()
}

/// Файл из 300 строк: строка 200 (с 1) — с отступом, как у кода; `Hit` экрана знает только
/// фрагмент без него.
fn numbered(dir: &Path) -> (std::path::PathBuf, u64) {
    let p = dir.join("code.rs");
    let mut s = String::new();
    let mut start = 0;
    for i in 1..=300 {
        if i == 200 {
            start = s.len() as u64;
            s.push_str("        let игла = spawn_blocking(move || copy());\n");
        } else {
            let _ = writeln!(s, "строка {i}");
        }
    }
    std::fs::write(&p, s).unwrap();
    (p, start)
}

/// S8 §2.5: подсветка — данные документа; поиск документа и строка «Найти» ни при чём.
#[test]
fn preview_marks_line_and_hit_without_find() {
    let dir = tempfile::tempdir().unwrap();
    let (f, start) = numbered(dir.path());
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 200, Some("spawn_blocking")));
    let p = settled(&mut core, 199);
    let off = start + "        let игла = ".len() as u64;
    assert_eq!(p.start, Some(start));
    assert_eq!(p.hit, Some(off..off + "spawn_blocking".len() as u64));
    let d = view(&core).in_panel(Side::Right).unwrap();
    assert!(d.search.is_none(), "строка «Найти» не открыта");
    assert!(d.back.is_empty(), "следование за списком — не переход");
    assert!(view(&core).active_viewer().is_none(), "фокус у экрана");
    // Строка на экране: 10 строк выше неё при окне 80×25 по умолчанию.
    let (first, last) = crate::visible_lines(d).unwrap();
    assert_eq!(first, 199 - 10);
    assert!(last >= 199);
}

/// Новая высота области (первый `Viewport` приходит после открытия) — строка на месте;
/// при 30 строках выше неё 12, как на макете `screen-search`.
#[test]
fn preview_keeps_place_on_viewport() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = numbered(dir.path());
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 200, None));
    settled(&mut core, 199);
    let doc = view(&core).quick[1].unwrap();
    view(&core).inbox.sender().send(InboxMsg::Viewport {
        doc,
        cols: 100,
        rows: 30,
    });
    pump_until(&mut core, |c| {
        view(c).in_panel(Side::Right).is_some_and(|d| {
            d.reveal.is_none() && crate::visible_lines(d).is_some_and(|l| l.0 == 199 - 12)
        })
    });
}

/// Без `qview` область живёт, пока открыт экран; закрыт — документа нет.
#[test]
fn preview_lives_with_screen() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = numbered(dir.path());
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 5, None));
    settled(&mut core, 4);
    let id = view(&core).quick[1];
    for _ in 0..5 {
        core.pump(Duration::from_millis(5));
    }
    assert_eq!(view(&core).quick[1], id);
    assert!(crate::panel_shows(core.state(), Side::Right));
    leave_screen(&mut core);
    let v = view(&core);
    assert!(v.quick[1].is_none() && v.is_empty() && !v.qpreview[1]);
    assert!(!crate::panel_shows(core.state(), Side::Right));
}

/// В обычном быстром просмотре подсветка экрана не остаётся.
#[test]
fn preview_mark_leaves_quick_view() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = numbered(dir.path());
    let mut core = on_screen(dir.path());
    qview(&mut core);
    core.dispatch(preview_cmd(&f, 200, Some("spawn_blocking")));
    settled(&mut core, 199);
    leave_screen(&mut core);
    assert!(view(&core).in_panel(Side::Right).is_some(), "режим qview");
    assert!(mark(&core).is_none());
}

/// Ни `viewer.open {line}`, ни следование за курсором панели метку не ставят.
#[test]
fn plain_views_have_no_preview() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = numbered(dir.path());
    let mut core = core(dir.path());
    core.dispatch(
        Command::new("viewer.open")
            .arg("path", Location::local(&f))
            .arg("line", 200),
    );
    pump_until(&mut core, |c| {
        view(c).active_viewer().is_some() && active(c).is_ready() && active(c).goto_wait.is_none()
    });
    assert!(active(&core).preview.is_none());
    assert!(
        !active(&core).back.is_empty(),
        "обычный переход — с «назад»"
    );
    core.dispatch(Command::new("viewer.close"));
    qview(&mut core);
    quick(&mut core, "right", &f);
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .is_some_and(ViewerDoc::is_ready)
    });
    assert!(mark(&core).is_none());
    assert!(!view(&core).qpreview[1]);
}

/// Приёмка: 1000 строк одного файла — одно чтение и один документ.
#[test]
fn fast_list_same_file_reads_once() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = numbered(dir.path());
    let mut core = on_screen(dir.path());
    for i in 0..1000 {
        core.dispatch(preview_cmd(&f, 1 + i % 300, Some("строка")));
        if i % 100 == 0 {
            core.pump(Duration::from_millis(1));
        }
    }
    settled(&mut core, 999 % 300);
    assert_eq!(view(&core).len(), 1);
    assert_eq!(reads(dir.path()), 1);
}

/// Приёмка: 1000 строк разных файлов — запрет двух чтений на сторону как у курсора панели.
#[test]
fn fast_list_many_files_reads_at_most_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    for i in 0..1000 {
        let p = dir.path().join(format!("f{i:04}.txt"));
        std::fs::write(&p, format!("начало\nфайл {i}\n")).unwrap();
        files.push(p);
    }
    let mut core = on_screen(dir.path());
    for p in &files {
        core.dispatch(preview_cmd(p, 2, Some("файл")));
    }
    let p = settled(&mut core, 1);
    assert_eq!(p.hit, Some(13..21));
    assert_eq!(view(&core).len(), 1);
    assert_eq!(
        view(&core).in_panel(Side::Right).unwrap().loc.path,
        files[999]
    );
    assert!(reads(dir.path()) <= 2, "reads = {}", reads(dir.path()));
}

/// Приёмка: облачный файл не читается, хотя он не под курсором панели.
#[test]
fn preview_of_offline_file_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let f = sub.join("cloud.txt");
    std::fs::write(&f, "в облаке\n").unwrap();
    let ok = std::process::Command::new("attrib")
        .arg("+O")
        .arg(&f)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "attrib +O");
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 1, Some("облаке")));
    pump_until(&mut core, |c| view(c).qnote[1].is_some());
    let v = view(&core);
    assert!(v.quick[1].is_none());
    assert_eq!(
        v.qnote[1].as_ref().map(|n| &n.kind),
        Some(&QuickNoteKind::Cloud)
    );
    assert_eq!(reads(dir.path()), 0);
    // Другая строка того же файла: заметка на месте, чтений нет.
    core.dispatch(preview_cmd(&f, 2, None));
    core.pump(Duration::from_millis(5));
    assert!(view(&core).qnote[1].is_some());
    assert_eq!(reads(dir.path()), 0);
    let _ = std::process::Command::new("attrib")
        .arg("-O")
        .arg(&f)
        .status();
}

/// Файл больше 4 МБ: индекс ради строки, а путь от метки длиннее кэша области (1 МБ) —
/// счёт продолжается, а не начинается заново.
#[test]
fn preview_reaches_line_past_quick_cache() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("long.log");
    let line = "x".repeat(3000);
    let mut s = String::new();
    for i in 0..1700 {
        let _ = writeln!(s, "{i:05} {line}");
    }
    std::fs::write(&f, s).unwrap();
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 1001, Some("01000")));
    let p = settled(&mut core, 1000);
    assert_ne!(
        view(&core).in_panel(Side::Right).unwrap().kind,
        Some(crate::source::SourceKind::Owned)
    );
    let at = 1000 * 3007;
    assert_eq!(p.start, Some(at));
    assert_eq!(p.hit, Some(at..at + 5));
}

#[test]
fn preview_hit_in_document_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("cp1251.txt");
    let first = "первая строка текста на русском языке\n";
    let text = format!("{first}\tтретья: привет, мир\n").repeat(20);
    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(&text);
    std::fs::write(&cp, &bytes).unwrap();
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&cp, 2, Some("привет")));
    let p = settled(&mut core, 1);
    let off = (first.chars().count() + "\tтретья: ".chars().count()) as u64;
    assert_eq!(p.hit, Some(off..off + 6));

    let u16 = dir.path().join("utf16.txt");
    let mut b = vec![0xFF, 0xFE];
    b.extend("один\nдва игла\n".encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&u16, &b).unwrap();
    core.dispatch(preview_cmd(&u16, 2, Some("игла")));
    let p = settled(&mut core, 1);
    assert_eq!(p.start, Some(2 + 2 * 5));
    assert_eq!(p.hit, Some(2 + 2 * 9..2 + 2 * 13));
}

/// Текста в строке нет (файл изменился): остаётся строка.
#[test]
fn preview_without_match_keeps_line() {
    let dir = tempfile::tempdir().unwrap();
    let (f, start) = numbered(dir.path());
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 200, Some("нет такого")));
    let p = settled(&mut core, 199);
    assert_eq!((p.start, p.hit), (Some(start), None));
}

/// Hex: `viewer.goto` понял бы номер строки как смещение — перехода нет.
#[test]
fn preview_in_hex_does_not_jump() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.bin");
    std::fs::write(&f, vec![0u8; 8192]).unwrap();
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 100, Some("x")));
    pump_until(&mut core, |c| {
        view(c)
            .in_panel(Side::Right)
            .is_some_and(ViewerDoc::is_ready)
    });
    core.dispatch(preview_cmd(&f, 200, None));
    core.pump(Duration::from_millis(5));
    let d = view(&core).in_panel(Side::Right).unwrap();
    assert_eq!((d.mode, d.top), (Mode::Hex, 0));
    assert_eq!(d.preview.as_ref().map(|p| p.line), Some(199));
}

/// Markdown в области — документ; строка совпадения — в Тексте.
#[test]
fn preview_of_markdown_is_text() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.md");
    std::fs::write(&f, "# Заголовок\n\nтекст\n\nигла тут\n").unwrap();
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 5, Some("игла")));
    let p = settled(&mut core, 4);
    assert!(!view(&core).in_panel(Side::Right).unwrap().md_mode());
    assert!(p.hit.is_some());
}

/// Строка без текста в запросе (`line` нет) — начало файла, без подсветки и без чтения;
/// Tab и повтор той же цели позицию не трогают.
#[test]
fn preview_without_line_goes_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = numbered(dir.path());
    let mut core = on_screen(dir.path());
    core.dispatch(preview_cmd(&f, 200, Some("spawn_blocking")));
    settled(&mut core, 199);
    let top = view(&core).in_panel(Side::Right).unwrap().top;

    core.dispatch(preview_cmd(&f, 200, Some("spawn_blocking")));
    core.dispatch(
        Command::new("viewer.quick")
            .arg("side", "right")
            .arg("path", Location::local(&f))
            .arg("focus", true),
    );
    core.pump(Duration::from_millis(5));
    assert_eq!(view(&core).in_panel(Side::Right).unwrap().top, top);
    assert!(mark(&core).is_some());

    core.dispatch(
        Command::new("viewer.quick")
            .arg("side", "right")
            .arg("path", Location::local(&f)),
    );
    core.pump(Duration::from_millis(5));
    let d = view(&core).in_panel(Side::Right).unwrap();
    assert_eq!(d.top, 0);
    assert!(d.preview.is_none());
    assert_eq!(reads(dir.path()), 1);
}

/// Перенесённый вьювер в области не вытесняется предпросмотром.
#[test]
fn preview_leaves_docked_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = numbered(dir.path());
    let other = dir.path().join("other.txt");
    std::fs::write(&other, "другой\n").unwrap();
    let mut core = core(dir.path());
    core.register(Box::new(FocusProbe));
    open(&mut core, &other);
    core.dispatch(Command::new("viewer.dock"));
    core.pump(Duration::from_millis(5));
    let id = view(&core).quick[1];
    assert!(id.is_some());
    core.dispatch(Command::new("qtest.search"));
    core.dispatch(preview_cmd(&f, 200, None));
    core.pump(Duration::from_millis(5));
    let v = view(&core);
    assert_eq!(v.quick[1], id);
    assert!(!v.qpreview[1]);
    assert!(mark(&core).is_none());
}
