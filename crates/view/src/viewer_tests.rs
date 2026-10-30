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
    assert_eq!(
        doc(&core).top,
        line_start(&text, 4980),
        "ниже конца не уходит"
    );
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

/// Путь относительно каталога таба; поиск, заданный до открытия, идёт после детекта.
#[test]
fn relative_open_and_queued_search() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let text = log_text(300);
    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(&text);
    std::fs::write(dir.path().join("sub").join("a.txt"), &bytes).unwrap();
    let mut core = core(dir.path());
    core.dispatch(Command::new("viewer.open").arg("path", "sub/a.txt"));
    core.dispatch(Command::new("viewer.search").arg("text", "строка 200 "));
    pump_until(&mut core, |c| {
        let v = view(c);
        v.active_viewer()
            .and_then(|id| v.viewer(id))
            .and_then(|d| d.search.as_ref())
            .is_some_and(|s| s.hit.is_some())
    });
    let d = doc(&core);
    assert_eq!(d.enc.enc, encoding_rs::WINDOWS_1251);
    let at = encoding_rs::WINDOWS_1251
        .encode(&text[..text.find("строка 200 ").unwrap()])
        .0
        .len() as u64;
    assert_eq!(d.sel.as_ref().map(|r| r.start), Some(at));
}

// ------------------------------------------------------------ S7-10: переход, назад, закладки

/// Тестовый handler: `vtest.cut n=…` — индекс «дошёл» только до строки `n` (остальное
/// прячется), `vtest.uncut` — остаток приходит дельтой, как от индексатора.
#[derive(Default)]
struct CutIndex {
    full: Option<crate::LineIndex>,
}

static CUT: [CommandMeta; 2] = [
    CommandMeta {
        id: "vtest2.cut",
        title: "cut",
        category: "test",
        args: &[ArgSpec {
            name: "n",
            kind: ValueKind::Int,
            required: true,
        }],
        repeatable: false,
    },
    CommandMeta {
        id: "vtest2.uncut",
        title: "uncut",
        category: "test",
        args: &[],
        repeatable: false,
    },
];

impl Handler for CutIndex {
    fn namespace(&self) -> &'static str {
        "vtest2"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &CUT
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let rt = cx.rt;
        let view = cx.state.ext.get_mut::<ViewState>().unwrap();
        let id = view.active_viewer().unwrap();
        let d = view.viewer_mut(id).unwrap();
        if cmd.id.as_str() == "vtest2.cut" {
            let n = cmd.args.int("n").unwrap().cast_unsigned();
            let full = d.index.clone();
            let marks: Vec<_> = full.marks.iter().copied().filter(|m| m.1 <= n).collect();
            let last = *marks.last().unwrap();
            d.index = crate::LineIndex {
                marks,
                lines: last.1,
                upto: last.0,
                done: false,
            };
            self.full = Some(full);
        } else {
            let full = self.full.take().unwrap();
            let from = d.index.marks.len();
            let delta = crate::index::IndexDelta {
                marks: full.marks[from..].to_vec(),
                lines: full.lines,
                upto: full.upto,
                done: true,
            };
            let epoch = d.index_epoch();
            d.on_index(epoch, delta, rt);
            crate::handler::after_goto(cx, id);
        }
        Ok(())
    }
}

fn goto(core: &mut Core, to: &str) {
    core.dispatch(Command::new("viewer.goto").arg("to", to));
    core.pump(Duration::from_millis(5));
}

fn settled(core: &mut Core) {
    pump_until(core, |c| {
        let d = doc(c);
        !d.realign && d.goto_wait.is_none() && d.pages.inflight() == 0
    });
}

/// Строка, смещение, процент, `+N`/`-N` и `back` по стеку.
#[test]
fn goto_line_offset_percent_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.log");
    let text = log_text(5000);
    std::fs::write(&p, &text).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    index_done(&mut core);
    viewport(&mut core, 100, 20);

    goto(&mut core, "100");
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 99));
    assert_eq!(core.state().focus, Focus::Viewer);
    goto(&mut core, "+10");
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 109));
    goto(&mut core, "-5");
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 104));
    goto(&mut core, "4 900");
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 4899));
    goto(&mut core, "999999");
    settled(&mut core);
    assert_eq!(
        doc(&core).top,
        line_start(&text, 4999),
        "за концом — последняя строка"
    );

    let half = text.len() / 2;
    goto(&mut core, "50%");
    settled(&mut core);
    let mid = text[..half].rfind('\n').unwrap() as u64 + 1;
    assert_eq!(doc(&core).top, mid, "процент — к началу строки");
    goto(&mut core, "0x10");
    settled(&mut core);
    assert_eq!(doc(&core).top, 0);

    for want in [mid, line_start(&text, 4999), line_start(&text, 4899)] {
        core.dispatch(Command::new("viewer.back"));
        settled(&mut core);
        assert_eq!(doc(&core).top, want);
    }
    for want in [104, 109, 99] {
        core.dispatch(Command::new("viewer.back"));
        settled(&mut core);
        assert_eq!(doc(&core).top, line_start(&text, want));
    }
    core.dispatch(Command::new("viewer.back"));
    settled(&mut core);
    assert_eq!(doc(&core).top, 0);
    assert!(doc(&core).back.is_empty());

    goto(&mut core, "12q");
    assert_eq!(doc(&core).top, 0, "ошибка разбора — позиция прежняя");
}

/// Строка дальше индекса: ждём его (фокус — строке перехода), Esc отменяет; индекс
/// дошёл — переход и строка закрывается.
#[test]
fn goto_waits_for_index_and_esc_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.log");
    let text = log_text(5000);
    std::fs::write(&p, &text).unwrap();
    let mut core = core(dir.path());
    core.register(Box::new(CutIndex::default()));
    core.pump(Duration::from_millis(5));
    open(&mut core, &p);
    index_done(&mut core);
    core.dispatch(Command::new("vtest2.cut").arg("n", 1100));
    core.pump(Duration::from_millis(5));
    assert!(!doc(&core).index.done);

    core.take_ui_events();
    goto(&mut core, "3000");
    assert!(doc(&core).goto_wait.is_some());
    assert_eq!(
        core.state().focus,
        Focus::Overlay(urfin_core::OverlayKind::Find)
    );
    let opened = core
        .take_ui_events()
        .into_iter()
        .any(|e| matches!(e, UiEvent::Ext { id, .. } if id.as_str() == "viewer.goto"));
    assert!(opened, "строка перехода показывает ожидание");
    core.dispatch(Command::new("viewer.search_cancel"));
    core.pump(Duration::from_millis(5));
    assert!(doc(&core).goto_wait.is_none());
    assert_eq!(core.state().focus, Focus::Viewer);
    assert_eq!(doc(&core).top, 0);

    // Строка в пределах индекса — сразу.
    goto(&mut core, "1000");
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 999));

    goto(&mut core, "3000");
    assert!(doc(&core).goto_wait.is_some());
    core.dispatch(Command::new("vtest2.uncut"));
    pump_until(&mut core, |c| doc(c).goto_wait.is_none());
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 2999));
    assert_eq!(core.state().focus, Focus::Viewer);
    core.dispatch(Command::new("viewer.back"));
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 999));
}

/// Закладки 0–9: поставить, перейти, нет закладки — тост; номер вне 0–9 — ошибка.
#[test]
fn bookmarks_set_and_go() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.log");
    let text = log_text(3000);
    std::fs::write(&p, &text).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    index_done(&mut core);
    viewport(&mut core, 100, 20);
    goto(&mut core, "700");
    settled(&mut core);
    core.dispatch(Command::new("viewer.bookmark.set").arg("n", 3));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).bookmarks[3], Some(line_start(&text, 699)));
    scroll(&mut core, "home", None);
    assert_eq!(doc(&core).top, 0);
    core.dispatch(Command::new("viewer.bookmark.go").arg("n", 3));
    settled(&mut core);
    assert_eq!(doc(&core).top, line_start(&text, 699));
    core.dispatch(Command::new("viewer.back"));
    settled(&mut core);
    assert_eq!(doc(&core).top, 0, "переход к закладке — тоже в back");

    core.take_ui_events();
    core.dispatch(Command::new("viewer.bookmark.go").arg("n", 5));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).top, 0);
    let toasts = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(toasts, 1);
    core.dispatch(Command::new("viewer.bookmark.set").arg("n", 12));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).bookmarks.iter().flatten().count(), 1);
}

/// Hex: число — смещение (десятичное), `0x…` — смещение, курсор — на нём.
#[test]
fn hex_goto_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("b.bin");
    std::fs::write(&p, [0u8, 1, 2, 3, 0, 0, 7, 8].repeat(1000)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    viewport(&mut core, 80, 10);
    assert_eq!(doc(&core).mode, Mode::Hex);
    goto(&mut core, "0x123");
    let d = doc(&core);
    assert_eq!((d.top, d.sel.clone()), (0x120, Some(0x123..0x123)));
    goto(&mut core, "100");
    assert_eq!(doc(&core).sel, Some(100..100));
    goto(&mut core, "+16");
    assert_eq!(doc(&core).sel, Some(116..116));
    goto(&mut core, "100%");
    let d = doc(&core);
    assert_eq!(d.sel, Some(7999..7999));
    assert_eq!(d.top, (8000 / 16 - 10) * 16, "конец — внизу экрана");
    assert_eq!(core.state().focus, Focus::Viewer);
}

/// F7 без текста и Alt+F8 без цели — строки UI: фокус `Find`, событие; Esc закрывает.
#[test]
fn find_and_goto_bars_take_focus() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.log");
    std::fs::write(&p, log_text(100)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &p);
    let find = Focus::Overlay(urfin_core::OverlayKind::Find);
    for id in ["viewer.search", "viewer.goto", "viewer.search_next"] {
        core.take_ui_events();
        core.dispatch(Command::new(id));
        core.pump(Duration::from_millis(5));
        assert_eq!(core.state().focus, find, "{id}");
        let want = if id == "viewer.goto" {
            "viewer.goto"
        } else {
            "viewer.search"
        };
        assert!(
            core.take_ui_events()
                .into_iter()
                .any(|e| matches!(e, UiEvent::Ext { id, .. } if id.as_str() == want))
        );
        core.dispatch(Command::new("viewer.search_cancel"));
        core.pump(Duration::from_millis(5));
        assert_eq!(core.state().focus, Focus::Viewer);
    }
    core.dispatch(Command::new("viewer.search"));
    core.pump(Duration::from_millis(5));
    core.dispatch(Command::new("viewer.close"));
    core.pump(Duration::from_millis(5));
    assert_eq!(
        core.state().focus,
        Focus::Panel,
        "закрытие из строки поиска"
    );
}
