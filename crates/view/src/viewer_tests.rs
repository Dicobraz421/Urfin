//! S7-08 через core: открытие, режимы, кодировки, история, поиск командой.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{
    ArgSpec, Boot, CmdError, Command, CommandMeta, Core, Cx, Focus, Handler, Location, Services,
    UiEvent, ValueKind, Waker,
};

use crate::cache::PAGE;
use crate::layout::Mode;
use crate::{EditorCfg, InboxMsg, OpenState, ViewState, ViewerCfg, ViewerDoc, register_with};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Тестовый handler: `vtest.top n=…` ставит `top` активного вьювера.
struct SetTop;

static SET_TOP: [CommandMeta; 1] = [CommandMeta {
    id: "vtest.top",
    title: "top",
    category: "test",
    args: &[ArgSpec {
        name: "n",
        kind: ValueKind::Int,
        required: true,
    }],
    repeatable: false,
}];

impl Handler for SetTop {
    fn namespace(&self) -> &'static str {
        "vtest"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &SET_TOP
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let view = cx.state.ext.get_mut::<ViewState>().unwrap();
        let id = view.active_viewer().unwrap();
        let d = view.viewer_mut(id).unwrap();
        d.top = cmd.args.int("n").unwrap().cast_unsigned();
        Ok(())
    }
}

fn core(dir: &Path) -> Core {
    let root = Location::local(dir);
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    register_with(&mut core, ViewerCfg::default(), EditorCfg::default());
    core.register(Box::new(SetTop));
    core.pump(Duration::from_millis(5));
    core
}

/// Прокачивает core, пока `f` не вернёт `true` (не дольше 5 с).
fn pump_until(core: &mut Core, mut f: impl FnMut(&Core) -> bool) {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(5));
        if f(core) {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "timeout");
        std::thread::sleep(Duration::from_millis(2));
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
}

fn index_done(core: &mut Core) {
    pump_until(core, |c| doc(c).index.done);
}

/// Начало строки `n` (с 0).
fn line_start(text: &str, n: usize) -> u64 {
    if n == 0 {
        return 0;
    }
    text.match_indices('\n').nth(n - 1).unwrap().0 as u64 + 1
}

/// `InboxMsg::Viewport` активному вьюверу и проход core.
fn viewport(core: &mut Core, cols: u32, rows: u32) {
    let v = view(core);
    let doc = v.active_viewer().unwrap();
    v.inbox
        .sender()
        .send(InboxMsg::Viewport { doc, cols, rows });
    core.pump(Duration::from_millis(5));
}

fn scroll(core: &mut Core, dir: &str, n: Option<i64>) {
    let mut c = Command::new("viewer.scroll").arg("dir", dir);
    if let Some(n) = n {
        c = c.arg("n", n);
    }
    core.dispatch(c);
    core.pump(Duration::from_millis(5));
}

fn log_text(lines: usize) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for i in 0..lines {
        let _ = writeln!(s, "строка {i} — проверка вьювера");
    }
    s
}

#[test]
fn open_detects_mode_and_seeds_first_page() {
    let dir = tempfile::tempdir().unwrap();
    let txt = dir.path().join("a.log");
    std::fs::write(&txt, log_text(5000)).unwrap();
    let bin = dir.path().join("b.bin");
    std::fs::write(&bin, [0u8, 1, 2, 3, 0, 0, 7, 8].repeat(100)).unwrap();
    let md = dir.path().join("c.md");
    std::fs::write(&md, "# Заголовок\n\n- пункт\n").unwrap();

    let mut core = core(dir.path());
    core.dispatch(Command::new("viewer.open").arg("path", Location::local(&txt)));
    core.pump(Duration::from_millis(1));
    assert_eq!(core.state().focus, Focus::Viewer);
    let first = view(&core).active_viewer().unwrap();
    pump_until(&mut core, |c| doc(c).is_ready());
    let d = doc(&core);
    assert_eq!(d.state, OpenState::Ready);
    assert!(d.enc.is_utf8() && d.enc.auto);
    assert_eq!((d.mode, d.lang), (Mode::Text, None));
    assert!(
        d.pages.get(0..PAGE).is_ok(),
        "первая страница — из выборки детекта"
    );
    index_done(&mut core);
    assert_eq!(doc(&core).index.lines, 5000);

    open(&mut core, &bin);
    assert_eq!(doc(&core).mode, Mode::Hex);
    open(&mut core, &md);
    let d = doc(&core);
    assert_eq!((d.mode, d.lang), (Mode::Markup, Some("markdown")));
    assert_eq!(view(&core).screens.len(), 3);

    for _ in 0..3 {
        core.dispatch(Command::new("viewer.close"));
        core.pump(Duration::from_millis(5));
    }
    assert!(view(&core).is_empty());
    assert_eq!(core.state().focus, Focus::Panel);
    assert!(view(&core).history.len() == 3 && first.0 == 1);
}

#[test]
fn open_failure_toasts_and_closes() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    core.take_ui_events();
    core.dispatch(
        Command::new("viewer.open").arg("path", Location::local(dir.path().join("nope.txt"))),
    );
    pump_until(&mut core, |c| {
        view(c).is_empty() && c.state().focus == Focus::Panel
    });
    let toasts = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(toasts, 1);
}

/// Text → Hex → Text сохраняет позицию; из Hex с середины строки — к её началу.
#[test]
fn text_hex_keeps_position() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.txt");
    let text = log_text(200);
    std::fs::write(&p, &text).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    let line = text.match_indices('\n').nth(40).unwrap().0 as u64 + 1;
    core.dispatch(Command::new("vtest.top").arg("n", line.cast_signed()));
    core.dispatch(Command::new("viewer.hex"));
    core.pump(Duration::from_millis(5));
    let d = doc(&core);
    assert_eq!((d.mode, d.top), (Mode::Hex, line));
    assert_eq!(
        d.pos(&d.layout(80)).0,
        line / 16 * 16,
        "Hex рисуется с top вниз до hex_cols"
    );
    core.dispatch(Command::new("viewer.hex"));
    core.pump(Duration::from_millis(5));
    assert_eq!((doc(&core).mode, doc(&core).top), (Mode::Text, line));

    core.dispatch(Command::new("viewer.mode").arg("mode", "hex"));
    core.dispatch(Command::new("vtest.top").arg("n", (line + 7).cast_signed()));
    core.dispatch(Command::new("viewer.mode").arg("mode", "text"));
    core.pump(Duration::from_millis(5));
    let d = doc(&core);
    assert_eq!((d.mode, d.top, d.realign), (Mode::Text, line, false));
}

/// 1251 ↔ 866 индекс не сбрасывают, UTF-8 ↔ UTF-16 — сбрасывают.
#[test]
fn codepage_change_resets_index_only_for_utf16() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.txt");
    let text = log_text(3000);
    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(&text);
    std::fs::write(&p, &bytes).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    index_done(&mut core);
    assert_eq!(doc(&core).enc.enc, encoding_rs::WINDOWS_1251);
    let lines = doc(&core).index.lines;

    core.dispatch(Command::new("viewer.codepage").arg("cp", "ibm866"));
    core.pump(Duration::from_millis(5));
    let d = doc(&core);
    assert_eq!((d.enc.enc, d.enc.auto), (encoding_rs::IBM866, false));
    assert_eq!(
        (d.index_epoch(), d.index.lines, d.index.done),
        (0, lines, true)
    );

    core.dispatch(Command::new("viewer.codepage").arg("cp", "utf-16le"));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).index_epoch(), 1);
    index_done(&mut core);
    core.dispatch(Command::new("viewer.codepage").arg("cp", "utf-8"));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).index_epoch(), 2);
    index_done(&mut core);
    assert_eq!(doc(&core).index.lines, lines);

    // F8: кольцо auto → utf-8 → windows-1251 → ibm866, одинаковые кодировки пропускаются.
    core.dispatch(Command::new("viewer.codepage"));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).enc.enc, encoding_rs::WINDOWS_1251);
    core.dispatch(Command::new("viewer.codepage"));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).enc.enc, encoding_rs::IBM866);
    core.dispatch(Command::new("viewer.codepage").arg("cp", "auto"));
    core.pump(Duration::from_millis(5));
    assert!(doc(&core).enc.auto);
    core.dispatch(Command::new("viewer.codepage").arg("cp", "klingon"));
    core.pump(Duration::from_millis(5));
    assert!(doc(&core).enc.auto);
}

/// Позиция, кодировка, режим и перенос восстанавливаются из истории.
#[test]
fn history_restores_position_and_modes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.txt");
    let text = log_text(300);
    std::fs::write(&p, &text).unwrap();
    let line = text.match_indices('\n').nth(100).unwrap().0 as u64 + 1;
    let mut core = core(dir.path());
    open(&mut core, &p);
    core.dispatch(Command::new("vtest.top").arg("n", line.cast_signed()));
    core.dispatch(Command::new("viewer.codepage").arg("cp", "windows-1251"));
    core.dispatch(Command::new("viewer.wrap"));
    core.dispatch(Command::new("viewer.wrap_mode"));
    core.dispatch(Command::new("viewer.close"));
    core.pump(Duration::from_millis(5));
    open(&mut core, &p);
    let d = doc(&core);
    assert_eq!(d.top, line);
    assert_eq!((d.enc.enc, d.enc.auto), (encoding_rs::WINDOWS_1251, false));
    assert_eq!(d.wrap, Some(crate::WrapKind::Chars));
}

#[test]
fn search_command_finds_counts_and_wraps() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.txt");
    let text = log_text(1000);
    std::fs::write(&p, &text).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    core.dispatch(
        Command::new("viewer.search")
            .arg("text", "СТРОКА 99 ")
            .arg("case", false),
    );
    pump_until(&mut core, |c| {
        doc(c)
            .search
            .as_ref()
            .is_some_and(|s| !s.running && s.count.done)
    });
    let at = text.find("строка 99 ").unwrap() as u64;
    let d = doc(&core);
    let s = d.search.as_ref().unwrap();
    assert_eq!(s.hit, Some(at..at + "строка 99 ".len() as u64));
    assert_eq!(d.sel, s.hit);
    assert_eq!((s.count.n, s.ordinal()), (1, Some(1)));
    assert_eq!(
        d.top,
        line_start(&text, 99 - 25 / 3),
        "совпадение — на трети экрана (до Viewport окно 25 строк)"
    );

    core.dispatch(Command::new("viewer.search_next"));
    pump_until(&mut core, |c| !doc(c).search.as_ref().unwrap().running);
    let s = doc(&core).search.as_ref().unwrap();
    assert!(
        s.wrapped,
        "единственное совпадение — после переноса через конец"
    );
    assert_eq!(s.hit.as_ref().map(|r| r.start), Some(at));

    core.dispatch(
        Command::new("viewer.search")
            .arg("text", "(")
            .arg("kind", "regex"),
    );
    core.pump(Duration::from_millis(5));
    let s = doc(&core).search.as_ref().unwrap();
    assert!(matches!(
        s.error,
        Some(crate::SearchError::Regex { pos: 0, .. })
    ));
}

/// Viewport и `viewer.scroll`: строки, страницы, конец через непрочитанное, горизонталь.
#[test]
fn viewport_and_scroll() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.log");
    let text = log_text(5000);
    std::fs::write(&p, &text).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    viewport(&mut core, 100, 20);
    assert_eq!((doc(&core).view_cols, doc(&core).view_rows), (100, 20));

    scroll(&mut core, "down", Some(3));
    assert_eq!(doc(&core).top, line_start(&text, 3));
    scroll(&mut core, "page_down", None);
    assert_eq!(doc(&core).top, line_start(&text, 23));
    scroll(&mut core, "up", None);
    assert_eq!(doc(&core).top, line_start(&text, 22));
    scroll(&mut core, "page_up", Some(5));
    assert_eq!(doc(&core).top, 0);

    // Конец файла — не в первой странице: позиция встаёт после чтения.
    scroll(&mut core, "end", None);
    pump_until(&mut core, |c| doc(c).top == line_start(&text, 5000 - 20));
    scroll(&mut core, "down", Some(5));
    assert_eq!(doc(&core).top, line_start(&text, 4980), "ниже конца не уходит");
    scroll(&mut core, "home", None);
    assert_eq!(doc(&core).top, 0);

    scroll(&mut core, "right", Some(4));
    assert_eq!(doc(&core).hscroll, 4);
    scroll(&mut core, "left", Some(10));
    assert_eq!(doc(&core).hscroll, 0);

    core.dispatch(Command::new("viewer.scroll").arg("dir", "sideways"));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).top, 0);
}

/// Hex: строка — `hex_cols` байт (auto по ширине из Viewport), конец без индекса.
#[test]
fn hex_scroll_uses_auto_columns() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("b.bin");
    std::fs::write(&p, [0u8, 1, 2, 3, 0, 0, 7, 8].repeat(1000)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    assert_eq!(doc(&core).mode, Mode::Hex);
    viewport(&mut core, 160, 10);
    assert_eq!(doc(&core).view_layout().hex_cols, 32);
    scroll(&mut core, "down", Some(2));
    assert_eq!(doc(&core).top, 64);
    scroll(&mut core, "end", None);
    assert_eq!(doc(&core).top, (8000 / 32 - 10) * 32);
    scroll(&mut core, "page_down", None);
    assert_eq!(doc(&core).top, (8000 / 32 - 10) * 32);
    viewport(&mut core, 80, 10);
    assert_eq!(doc(&core).view_layout().hex_cols, 16);
    scroll(&mut core, "home", None);
    assert_eq!(doc(&core).top, 0);
}

/// Совпадение — на трети экрана по строкам из Viewport.
#[test]
fn search_reveals_at_third_of_viewport() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.txt");
    let text = log_text(1000);
    std::fs::write(&p, &text).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    viewport(&mut core, 100, 30);
    core.dispatch(Command::new("viewer.search").arg("text", "строка 500 "));
    pump_until(&mut core, |c| {
        doc(c).search.as_ref().is_some_and(|s| !s.running) && doc(c).reveal.is_none()
    });
    assert_eq!(doc(&core).top, line_start(&text, 490));
}
