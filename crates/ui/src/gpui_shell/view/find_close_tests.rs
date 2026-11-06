//! Правило решения 2026-11-04 во вьювере: закрыли строку «Найти» — в отрисовке нет ни
//! подсветки, ни меток, ни «N отметок»; поиск остаётся у документа для Shift+F7, F7
//! открывает строку и ищет заново — как в редакторе.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Core, Location, Services, Waker};
use urfin_view::{EditorCfg, InboxMsg, ViewerCfg, register_with};

use super::*;

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
        assert!(t.elapsed() < Duration::from_secs(5), "timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn the_doc(core: &Core) -> &ViewerDoc {
    let v = core.state().ext.get::<ViewState>().unwrap();
    v.viewer(v.active_viewer().unwrap()).unwrap()
}

/// Поиск документа дошёл до конца: совпадение и счёт.
fn search_done(core: &mut Core) {
    pump_until(core, |c| {
        the_doc(c)
            .search
            .as_ref()
            .is_some_and(|x| !x.running && x.count.done)
    });
}

/// Что поиска попадает в кадр: совпадения на экране, метки полосы, середина статуса.
fn drawn(v: &Viewer, core: &Core) -> (usize, usize, String) {
    let d = the_doc(core);
    let on = v.hits_on(core.state(), d.id);
    let hits = rows::Rows::default().hits(d, 0, d.len, on);
    (
        hits.len(),
        rows::bar_marks(d, on).len(),
        rows::search_zone(d, on),
    )
}

#[test]
fn closed_bar_draws_no_hits_and_search_stays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.log");
    let text = "один\nцель раз\nдва\nцель два\n";
    std::fs::write(&path, text).unwrap();
    let first = text.find("цель").unwrap() as u64;
    let second = text.rfind("цель").unwrap() as u64;
    let hit = |at: u64| at..at + "цель".len() as u64;

    let mut core = core(dir.path());
    core.dispatch(Command::new("viewer.open").arg("path", Location::local(&path)));
    pump_until(&mut core, |c| {
        let v = c.state().ext.get::<ViewState>().unwrap();
        v.active_viewer()
            .and_then(|id| v.viewer(id))
            .is_some_and(ViewerDoc::is_ready)
    });
    let id = the_doc(&core).id;
    let inbox = core.state().ext.get::<ViewState>().unwrap().inbox.sender();
    inbox.send(InboxMsg::Viewport {
        doc: id,
        cols: 100,
        rows: 20,
    });
    pump_until(&mut core, |c| {
        let d = the_doc(c);
        d.pages.get(0..d.len).is_ok()
    });

    // F7: документ без поиска — строка пустая, искать нечего.
    let mut v = Viewer::default();
    core.dispatch(Command::new("viewer.search"));
    core.pump(Duration::from_millis(5));
    assert_eq!(v.open_bar(BarMode::Find, core.state(), id), None);
    assert_eq!(drawn(&v, &core).0, 0, "строка без образца");

    // Enter: строка открыта — совпадения и метки в кадре.
    v.bar
        .as_mut()
        .unwrap()
        .switch(BarMode::Find, "цель".to_owned());
    core.dispatch(search_cmd("цель", Flags::default(), false));
    search_done(&mut core);
    let (hits, marks, zone) = drawn(&v, &core);
    assert_eq!((hits, marks), (2, 2));
    assert_ne!(zone, "");
    assert_eq!(the_doc(&core).sel, Some(hit(first)));

    // Esc: совпадений в отрисовке нет, поиск и выделение — у документа.
    core.dispatch(Command::new("viewer.search_cancel"));
    core.pump(Duration::from_millis(5));
    assert_eq!(drawn(&v, &core), (0, 0, String::new()));
    let d = the_doc(&core);
    assert!(d.search.as_ref().is_some_and(|x| x.marks.len() == 2));
    assert_eq!(d.sel, Some(hit(first)));

    // Кадр закрывает строку (`input`).
    v.bar = None;

    // Shift+F7: следующее совпадение, строка не открывается, отрисовка без совпадений.
    core.dispatch(Command::new("viewer.search_next"));
    pump_until(&mut core, |c| {
        !the_doc(c).search.as_ref().unwrap().running && the_doc(c).sel == Some(hit(second))
    });
    assert_eq!(core.state().focus, Focus::Viewer);
    assert_eq!(drawn(&v, &core), (0, 0, String::new()));

    // F7: образец — выделение, сразу новый поиск от его конца; совпадения снова в кадре.
    core.dispatch(Command::new("viewer.search"));
    core.pump(Duration::from_millis(5));
    let cmd = v.open_bar(BarMode::Find, core.state(), id).unwrap();
    assert_eq!(cmd.id.as_str(), "viewer.search");
    assert_eq!(cmd.args.str("text"), Some("цель"));
    core.dispatch(cmd);
    pump_until(&mut core, |c| the_doc(c).sel == Some(hit(first)));
    search_done(&mut core);
    let (hits, marks, _) = drawn(&v, &core);
    assert_eq!((hits, marks), (2, 2));
}

/// В Goto образца нет: совпадения сняты, как в редакторе.
#[test]
fn bar_shows_hits_only_with_pattern_in_find() {
    let bar = |mode, text: &str| FindBar::new(mode, 1, text.to_owned(), Flags::default());
    assert!(bar(BarMode::Find, "abc").shows_hits());
    assert!(!bar(BarMode::Find, "").shows_hits());
    assert!(!bar(BarMode::Goto, "abc").shows_hits());
}
