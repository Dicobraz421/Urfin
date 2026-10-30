//! Режим документа Markdown через core (S7-22): разбор на Interactive lane, позиция
//! документ ↔ Текст, прокрутка блоками, Alt+F8 и Hex → Текст, F7 по проекции, история.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, Location, Services, Waker};

use crate::layout::Mode;
use crate::md::{BlockKind, IMAGE_MAX_MB, MdDoc};
use crate::{EditorCfg, InboxMsg, MdView, ViewState, ViewerCfg, ViewerDoc, register_with};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn core(dir: &Path, cfg: ViewerCfg) -> Core {
    let root = Location::local(dir);
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    register_with(&mut core, cfg, EditorCfg::default());
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
        assert!(t.elapsed() < Duration::from_secs(10), "timeout");
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

fn md(core: &Core) -> &MdDoc {
    doc(core).md_doc().expect("документ разобран")
}

fn parsed(core: &Core) -> bool {
    let v = view(core);
    v.active_viewer()
        .and_then(|id| v.viewer(id))
        .and_then(ViewerDoc::md_doc)
        .is_some_and(|d| d.complete)
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

const SRC: &str = "# Заголовок\n\nПервый абзац.\n\n## Второй\n\nТекст второго раздела.\n\n<details>\n<summary>Скрыто</summary>\n\nИголка в стоге.\n\n</details>\n\n- пункт\n- ещё\n";

fn setup(src: &str, cfg: ViewerCfg) -> (tempfile::TempDir, Core) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("README.md");
    std::fs::write(&p, src).unwrap();
    let mut c = core(dir.path(), cfg);
    open(&mut c, &p);
    (dir, c)
}

#[test]
fn markdown_opens_as_document() {
    let (_d, mut c) = setup(SRC, ViewerCfg::default());
    assert_eq!(doc(&c).mode, Mode::Markup);
    assert!(doc(&c).md_mode());
    pump_until(&mut c, parsed);
    let m = md(&c);
    assert_eq!(m.blocks[0].kind, BlockKind::Heading(1));
    assert!(
        m.blocks
            .iter()
            .any(|b| b.kind == BlockKind::Details { open: false })
    );
}

#[test]
fn source_setting_keeps_markup_as_source() {
    let cfg = ViewerCfg {
        markdown: MdView::Source,
        ..ViewerCfg::default()
    };
    let (_d, c) = setup(SRC, cfg);
    assert_eq!(doc(&c).mode, Mode::Markup);
    assert!(!doc(&c).md_mode());
    assert!(doc(&c).md.is_none());
    assert_eq!(ViewerCfg::default().md_image_max_mb, IMAGE_MAX_MB);
}

#[test]
fn position_survives_document_text_switch() {
    let (_d, mut c) = setup(SRC, ViewerCfg::default());
    pump_until(&mut c, parsed);
    // Вниз на три блока: «Текст второго раздела.».
    c.dispatch(
        Command::new("viewer.scroll")
            .arg("dir", "down")
            .arg("n", 3i64),
    );
    c.pump(Duration::from_millis(5));
    let d = doc(&c);
    let top = d.md.as_ref().unwrap().top;
    assert_eq!(top, 3);
    let start = md(&c).blocks[3].src.start;
    assert_eq!(d.top, start);
    assert_eq!(&SRC[start as usize..][..10], "Текст");
    // В Текст — со строки начала блока; обратно — тот же блок.
    c.dispatch(Command::new("viewer.mode").arg("mode", "text"));
    pump_until(&mut c, |c| doc(c).mode == Mode::Text);
    assert_eq!(doc(&c).top, start);
    assert!(!doc(&c).md_mode());
    c.dispatch(Command::new("viewer.mode").arg("mode", "markup"));
    pump_until(&mut c, |c| doc(c).md_mode());
    assert_eq!(doc(&c).md.as_ref().unwrap().top, 3);
}

#[test]
fn goto_and_hex_search_switch_to_text() {
    let (_d, mut c) = setup(SRC, ViewerCfg::default());
    pump_until(&mut c, parsed);
    c.dispatch(Command::new("viewer.goto"));
    pump_until(&mut c, |c| doc(c).mode == Mode::Text);
    c.dispatch(Command::new("viewer.mode").arg("mode", "markup"));
    pump_until(&mut c, |c| doc(c).md_mode());
    c.dispatch(
        Command::new("viewer.search")
            .arg("text", "D0 98")
            .arg("kind", "hex"),
    );
    pump_until(&mut c, |c| {
        doc(c).mode == Mode::Text && doc(c).search.as_ref().is_some_and(|s| s.hit.is_some())
    });
    assert!(!doc(&c).search.as_ref().unwrap().proj);
}

#[test]
fn f7_searches_projection_and_opens_details() {
    let (_d, mut c) = setup(SRC, ViewerCfg::default());
    pump_until(&mut c, parsed);
    c.dispatch(Command::new("viewer.search").arg("text", "иголка"));
    pump_until(&mut c, |c| {
        doc(c).md.as_ref().is_some_and(|m| m.hit.is_some())
    });
    let d = doc(&c);
    assert!(d.md_mode(), "F7 не уходит из документа");
    let m = d.md.as_ref().unwrap();
    let hit = m.hit.clone().unwrap();
    let doc_ = m.doc.as_ref().unwrap();
    assert_eq!(&doc_.text[hit.start as usize..hit.end as usize], "Иголка");
    let b = doc_.block_at_text(hit.start).unwrap();
    assert_eq!(m.reveal, Some(b));
    let det = doc_.ancestors(b).next().unwrap();
    assert!(m.is_open(doc_, det), "свёрнутый <details> раскрыт");
    assert!(m.visible(doc_, b));
    assert!(d.search.as_ref().unwrap().proj);
    // Счётчик — по проекции.
    pump_until(&mut c, |c| {
        doc(c).search.as_ref().is_some_and(|s| s.count.done)
    });
    assert_eq!(doc(&c).search.as_ref().unwrap().count.n, 1);
}

#[test]
fn inbox_top_toggle_select_and_history() {
    let (dir, mut c) = setup(SRC, ViewerCfg::default());
    pump_until(&mut c, parsed);
    let id = view(&c).active_viewer().unwrap();
    let det = md(&c)
        .blocks
        .iter()
        .position(|b| matches!(b.kind, BlockKind::Details { .. }))
        .unwrap() as u32;
    let tx = view(&c).inbox.sender();
    tx.send(InboxMsg::MdTop {
        doc: id,
        block: det,
        frac: 0.5,
    });
    tx.send(InboxMsg::MdToggle {
        doc: id,
        block: det,
    });
    tx.send(InboxMsg::MdSelect {
        doc: id,
        sel: Some(0..10),
    });
    c.pump(Duration::from_millis(5));
    let d = doc(&c);
    let m = d.md.as_ref().unwrap();
    assert_eq!((m.top, m.top_frac), (det, 0.5));
    assert!(m.is_open(m.doc.as_ref().unwrap(), det));
    assert_eq!(d.md_copy_text(false), Some("Загол"));
    let start = md(&c).blocks[det as usize].src.start;
    assert_eq!(d.top, start);
    // История: позиция — начало верхнего блока и доля.
    let memo = d.memo();
    assert_eq!(
        (memo.top, memo.frac, memo.mode),
        (start, 5000, Mode::Markup)
    );
    c.dispatch(Command::new("viewer.close"));
    c.pump(Duration::from_millis(5));
    let p = dir.path().join("README.md");
    open(&mut c, &p);
    pump_until(&mut c, parsed);
    let m = doc(&c).md.as_ref().unwrap();
    assert_eq!(m.top, det);
    assert!((m.top_frac - 0.5).abs() < 1e-3);
}

#[test]
fn big_file_parses_head_first() {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < (1 << 20) + 4096 {
        let _ = write!(s, "## H{i}\n\nабзац {i}\n\n");
        i += 1;
    }
    let (_d, mut c) = setup(&s, ViewerCfg::default());
    pump_until(&mut c, parsed);
    let m = md(&c);
    assert!(m.complete);
    assert_eq!(m.src_len, s.len() as u64);
    assert_eq!(m.anchors.last().unwrap().slug, format!("h{}", i - 1));
}
