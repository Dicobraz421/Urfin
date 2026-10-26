//! Выделение, буфер, прокрутка (S6-07) на сетке без процесса (`TermState.preview`);
//! OSC 52 и bracketed paste — на эмуляторе.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::unbounded;
use urfin_core::{
    Boot, CmdError, Command, CommandMeta, Core, Cx, Handler, Location, Services, UiEvent, Waker,
};

use crate::TermState;
use crate::session::{SessionEvent, WriterCmd};
use crate::sys::conpty::PtySize;
use crate::vt::shared::{TermShared, VtListener};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _: Duration) {}
}

fn grid(
    osc52: &str,
) -> (
    Arc<TermShared>,
    crossbeam_channel::Receiver<WriterCmd>,
    crossbeam_channel::Receiver<SessionEvent>,
) {
    let (tx, rx) = unbounded();
    let (ev_tx, ev_rx) = unbounded();
    let sh = Arc::new(TermShared::new(
        PtySize { cols: 40, rows: 6 },
        100,
        osc52,
        VtListener::new(tx, ev_tx),
    ));
    (sh, rx, ev_rx)
}

/// Ставит сетку вместо сеанса (`test.preview`), как сцены `--screens`.
struct Preview(Arc<TermShared>);

static PREVIEW: [CommandMeta; 1] = [CommandMeta {
    id: "test.preview",
    title: "preview",
    category: "test",
    args: &[],
    repeatable: false,
}];

impl Handler for Preview {
    fn namespace(&self) -> &'static str {
        "test"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &PREVIEW
    }

    fn handle(&mut self, _: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        if let Some(t) = cx.state.ext.get_mut::<TermState>() {
            t.preview = Some(self.0.clone());
        }
        Ok(())
    }
}

fn core_with(sh: &Arc<TermShared>) -> Core {
    let root = Location::local(std::env::temp_dir());
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    crate::register(&mut core, crate::TermConfig::default());
    core.register(Box::new(Preview(sh.clone())));
    core.pump(Duration::from_millis(1));
    core.dispatch(Command::new("test.preview"));
    core
}

fn clips(core: &mut Core) -> Vec<String> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Ext { id, args } if id.as_str() == "clip.write" => {
                args.str("text").map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

fn mouse(core: &mut Core, kind: &str, row: i64, col: i64, clicks: i64, mods: &str) {
    let mut c = Command::new("term.mouse")
        .arg("kind", kind)
        .arg("button", "left")
        .arg("row", row)
        .arg("col", col)
        .arg("clicks", clicks);
    if !mods.is_empty() {
        c = c.arg("mods", mods);
    }
    core.dispatch(c);
}

/// Приёмка: semantic, line и block работают; Ctrl+C с выделением копирует и снимает
/// его.
#[test]
fn semantic_line_block_and_ctrl_c() {
    let (sh, _w, _e) = grid("copy");
    sh.inject(b"hello world-wide\r\nsecond line here\r\nabcdef\r\nghijkl");
    let mut core = core_with(&sh);

    mouse(&mut core, "down", 0, 2, 2, "");
    core.dispatch(Command::new("term.copy"));
    assert_eq!(clips(&mut core), ["hello"]);
    assert!(!sh.has_selection(), "copy clears the selection");

    mouse(&mut core, "down", 1, 3, 3, "");
    core.dispatch(Command::new("term.copy"));
    assert_eq!(
        clips(&mut core),
        ["second line here\n"],
        "line with its break"
    );

    // Alt+протяжка — блок: колонки 1..=3 строк 2 и 3.
    mouse(&mut core, "down", 2, 1, 1, "alt");
    core.dispatch(
        Command::new("term.mouse")
            .arg("kind", "move")
            .arg("button", "left")
            .arg("row", 3)
            .arg("col", 3)
            .arg("right", true)
            .arg("mods", "alt"),
    );
    assert_eq!(
        sh.snapshot(&crate::vt::grid::Defaults {
            fg: [0; 3],
            bg: [0; 3],
            cursor: [0; 3],
        })
        .selection,
        [(2, 1, 3), (3, 1, 3)]
    );
    core.dispatch(Command::new("term.key").arg("key", "c").arg("mods", "ctrl"));
    assert_eq!(clips(&mut core), ["bcd\nhij"]);
    assert!(!sh.has_selection());

    // Простая протяжка через строки.
    mouse(&mut core, "down", 0, 6, 1, "");
    core.dispatch(
        Command::new("term.mouse")
            .arg("kind", "move")
            .arg("button", "left")
            .arg("row", 1)
            .arg("col", 5)
            .arg("right", true),
    );
    core.dispatch(Command::new("term.copy"));
    assert_eq!(clips(&mut core), ["world-wide\nsecond"]);

    // Без выделения copy ничего не пишет.
    core.dispatch(Command::new("term.copy"));
    assert_eq!(clips(&mut core), Vec::<String>::new());

    core.dispatch(Command::new("term.select_all"));
    core.dispatch(Command::new("term.copy"));
    let all = clips(&mut core);
    assert!(
        all[0].starts_with("hello world-wide\nsecond line here"),
        "{all:?}"
    );
}

/// Mouse mode приложения: клик — его, Shift+клик — выделение.
#[test]
fn mouse_mode_shift_selects() {
    let (sh, _w, _e) = grid("copy");
    sh.inject(b"\x1b[?1000h\x1b[?1006hword");
    let mut core = core_with(&sh);
    mouse(&mut core, "down", 0, 1, 2, "");
    assert!(!sh.has_selection(), "click goes to the app");
    mouse(&mut core, "down", 0, 1, 2, "shift");
    assert!(sh.has_selection());
}

/// Колесо и Shift+PgUp/PgDn листают скроллбэк.
#[test]
fn scrollback_by_wheel_and_pages() {
    let (sh, _w, _e) = grid("copy");
    for i in 0..40 {
        sh.inject(format!("line {i}\r\n").as_bytes());
    }
    let mut core = core_with(&sh);
    core.dispatch(
        Command::new("term.mouse")
            .arg("kind", "wheel")
            .arg("lines", 3),
    );
    assert_eq!(sh.display_offset(), 3);
    core.dispatch(Command::new("term.scroll").arg("pages", -1));
    assert_eq!(sh.display_offset(), 3 + 6);
    core.dispatch(Command::new("term.scroll").arg("lines", 4));
    assert_eq!(sh.display_offset(), 5);
    core.dispatch(Command::new("term.scroll"));
    assert_eq!(sh.display_offset(), 0);
}

/// Приёмка: OSC 52 только пишет — запрос буфера (`?`) без ответа, запись — событием;
/// `osc52 = "off"` — ничего.
#[test]
fn osc52_write_only() {
    let (sh, writer, events) = grid("copy");
    sh.inject(b"\x1b]52;c;?\x07");
    assert!(writer.try_recv().is_err(), "no clipboard read reply");
    assert!(events.try_recv().is_err());
    sh.inject(b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(
        events.try_recv(),
        Ok(SessionEvent::Clipboard("hello".into()))
    );

    let (off, _w, ev) = grid("off");
    off.inject(b"\x1b]52;c;aGVsbG8=\x07");
    assert!(ev.try_recv().is_err());
}

/// Bracketed paste: приложение включило DEC 2004 — вставка в скобках, `ESC` внутри
/// вырезан; выключило — `\n` → `\r`.
#[test]
fn bracketed_paste_follows_mode() {
    let (sh, _w, _e) = grid("copy");
    sh.inject(b"\x1b[?2004h");
    let m = sh.input_modes();
    assert!(m.bracketed_paste);
    let mut out = Vec::new();
    crate::vt::input::paste("a\nb\x1b[201~c", m, &mut out);
    assert_eq!(out, b"\x1b[200~a\nb[201~c\x1b[201~");
    sh.inject(b"\x1b[?2004l");
    out.clear();
    crate::vt::input::paste("a\r\nb", sh.input_modes(), &mut out);
    assert_eq!(out, b"a\rb");
}
