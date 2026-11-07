//! Интеграционные тесты команд core и навигации на `FakeListing` (без ФС).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::bus::{Boot, Core};
use crate::cmd::{Args, Command, Value};
use crate::entry::Entry;
use crate::fake::FakeListing;
use crate::ids::{Side, TabId};
use crate::loc::Location;
use crate::msg::Waker;
use crate::services::{ListingUpdate, OpenResolver, Patch, Services, TabEvent, listing_msg};
use crate::state::{Level, UiEvent};
use crate::tab::{ListingStatus, PanelMode, Row, SortKey, Tab};

#[derive(Default)]
struct TestWaker {
    wakes: AtomicUsize,
    pending: Mutex<bool>,
    cv: Condvar,
}

impl Waker for TestWaker {
    fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        *self.pending.lock().unwrap() = true;
        self.cv.notify_all();
    }
    /// Отложенное пробуждение (как `request_repaint_after`): тесты качают pump сами.
    fn wake_after(&self, _d: Duration) {}
}

impl TestWaker {
    fn wait(&self, timeout: Duration) -> bool {
        let g = self.pending.lock().unwrap();
        let (mut g, _) = self.cv.wait_timeout_while(g, timeout, |p| !*p).unwrap();
        std::mem::take(&mut *g)
    }
}

/// Каталог `name` от корня тестовой ФС.
fn dir(name: &str) -> Location {
    let root = if cfg!(windows) { r"C:\t" } else { "/t" };
    Location::local(format!("{root}/{name}"))
}

fn files(names: &[&str]) -> Vec<Entry> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if let Some(d) = n.strip_suffix('/') {
                FakeListing::dir(d)
            } else {
                FakeListing::file(n, (i as u64 + 1) * 10)
            }
        })
        .collect()
}

struct H {
    core: Core,
    fake: Arc<FakeListing>,
    w: Arc<TestWaker>,
}

impl H {
    fn new(chunk: usize, setup: impl FnOnce(&FakeListing)) -> H {
        Self::with_resolvers(chunk, setup, Vec::new())
    }

    fn with_resolvers(
        chunk: usize,
        setup: impl FnOnce(&FakeListing),
        open_resolvers: Vec<Arc<dyn OpenResolver>>,
    ) -> H {
        let fake = Arc::new(FakeListing::new(chunk));
        fake.set_dir(dir("a"), files(&["x/", "y/", "z/", "f1.txt", "f2.rs"]));
        setup(&fake);
        let w = Arc::new(TestWaker::default());
        let services = Services {
            listing: fake.clone(),
            open_resolvers,
        };
        let core = Core::new(Boot::new(dir("a"), dir("a")), services, w.clone());
        let mut h = H { core, fake, w };
        h.settle();
        h
    }

    fn busy(&self) -> bool {
        self.core.state().panels.iter().any(|p| {
            p.tabs.iter().any(|t| {
                t.incoming.is_some() || matches!(t.listing.status, ListingStatus::Loading { .. })
            })
        })
    }

    /// Качать pump, пока таб(ы) грузятся и приходят сообщения.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "did not settle");
            let p = self.core.pump(Duration::from_secs(1));
            if p.more {
                continue;
            }
            let woke = self
                .w
                .wait(Duration::from_millis(if self.busy() { 200 } else { 30 }));
            if !woke && !self.busy() {
                return;
            }
        }
    }

    fn run(&mut self, c: Command) {
        self.core.dispatch(c);
        self.settle();
    }

    fn cmd(&mut self, text: &str) {
        self.run(Command::parse(text).unwrap());
    }

    fn tab(&self) -> &Tab {
        self.core.state().active_tab()
    }

    fn names(&self) -> Vec<String> {
        let t = self.tab();
        t.listing
            .order
            .iter()
            .map(|&i| t.listing.entries[i as usize].name.to_string())
            .collect()
    }

    fn cur(&self) -> Option<String> {
        match self.tab().current()? {
            Row::Entry { entry, .. } => Some(entry.name.to_string()),
            Row::Parent => Some("..".into()),
        }
    }

    fn selected(&self) -> Vec<String> {
        let t = self.tab();
        let mut v: Vec<String> = t
            .selection
            .names(&t.listing)
            .into_iter()
            .map(|n| n.to_string())
            .collect();
        v.sort();
        v
    }

    fn go(&mut self, name: &str) {
        self.run(Command::new("panel.go").arg("path", dir(name)));
    }

    fn toasts(&mut self) -> Vec<(Level, String)> {
        self.core
            .take_ui_events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Toast { level, text } => Some((level, text)),
                _ => None,
            })
            .collect()
    }

    /// Отправить `ListingUpdate` активному табу «от S4».
    fn send(&mut self, u: ListingUpdate) {
        let t = self.tab();
        let m = listing_msg(t.id, t.listing.generation, u);
        self.core.rt().sink().try_send(m).unwrap();
        self.settle();
    }
}

#[test]
fn initial_load_is_sorted_dirs_first() {
    let h = H::new(2, |_| {});
    assert_eq!(h.names(), ["x", "y", "z", "f1.txt", "f2.rs"]);
    assert_eq!(h.tab().listing.status, ListingStatus::Complete);
    assert_eq!(h.cur().as_deref(), Some(".."));
    let stats = h.tab().listing.stats;
    assert_eq!((stats.dirs, stats.files, stats.bytes), (3, 2, 90));
    assert!(h.core.state().visited.iter().any(|l| *l == dir("a")));
}

#[test]
fn fast_go_shows_only_last() {
    let mut h = H::new(64, |f| {
        f.set_generated(dir("p"), 10, 5000);
        f.set_generated(dir("q"), 10, 5000);
        f.set_dir(dir("r"), files(&["r1", "r2"]));
    });
    for d in ["p", "q", "r"] {
        h.core
            .dispatch(Command::new("panel.go").arg("path", dir(d)));
    }
    h.settle();
    assert_eq!(h.tab().loc, dir("r"));
    assert_eq!(h.names(), ["r1", "r2"]);
    let loads = h.fake.loads();
    for d in ["p", "q"] {
        let l = loads.iter().find(|l| l.loc == dir(d)).unwrap();
        assert!(l.cancel.is_cancelled(), "{d} load not cancelled");
    }
    assert!(
        !loads
            .iter()
            .find(|l| l.loc == dir("r"))
            .unwrap()
            .cancel
            .is_cancelled()
    );
    // История: a → p → q → r.
    h.cmd("panel.back");
    assert_eq!(h.tab().loc, dir("q"));
}

#[test]
fn enter_and_up_focus_left_dir() {
    let mut h = H::new(2, |f| f.set_dir(dir("a/y"), files(&["in1", "in2"])));
    h.cmd("panel.cursor.to name=y");
    h.cmd("panel.enter");
    assert_eq!(h.tab().loc, dir("a/y"));
    assert_eq!(h.names(), ["in1", "in2"]);
    h.cmd("panel.cursor.end");
    h.cmd("panel.up");
    assert_eq!(h.tab().loc, dir("a"));
    assert_eq!(h.cur().as_deref(), Some("y"));
    // Enter на `..` — тоже наверх.
    h.go("a/y");
    h.cmd("panel.cursor.home");
    h.cmd("panel.enter");
    assert_eq!(h.tab().loc, dir("a"));
    assert_eq!(h.cur().as_deref(), Some("y"));
}

#[test]
fn back_and_forward() {
    let mut h = H::new(4, |f| {
        f.set_dir(dir("b"), files(&["b1", "b2", "b3"]));
        f.set_dir(dir("c"), files(&["c1"]));
    });
    h.go("b");
    h.cmd("panel.cursor.to name=b2");
    h.go("c");
    h.cmd("panel.back");
    assert_eq!(h.tab().loc, dir("b"));
    assert_eq!(h.cur().as_deref(), Some("b2"));
    h.cmd("panel.back");
    assert_eq!(h.tab().loc, dir("a"));
    h.cmd("panel.forward");
    assert_eq!(h.tab().loc, dir("b"));
    h.cmd("panel.forward");
    assert_eq!(h.tab().loc, dir("c"));
    let rev = h.core.state().rev;
    h.cmd("panel.forward");
    assert_eq!(
        h.core.state().rev,
        rev,
        "no forward history: nothing changes"
    );
}

#[test]
fn refresh_keeps_cursor_and_selection() {
    let mut h = H::new(2, |f| {
        f.set_dir(dir("b"), files(&["f1", "f2", "f3", "f4", "f5"]));
    });
    h.go("b");
    for n in ["f2", "f4"] {
        h.cmd(&format!("panel.cursor.to name={n}"));
        h.cmd("sel.toggle");
    }
    h.cmd("panel.cursor.to name=f3");
    h.fake
        .set_dir(dir("b"), files(&["f6", "f5", "f4", "f0", "f3", "f2", "f1"]));
    h.core.dispatch(Command::new("panel.refresh"));
    // Double-buffer: пока новый листинг не готов, на экране старый.
    assert!(h.tab().incoming.is_some());
    assert_eq!(h.names().len(), 5);
    h.settle();
    assert!(h.tab().incoming.is_none());
    assert_eq!(h.names(), ["f0", "f1", "f2", "f3", "f4", "f5", "f6"]);
    assert_eq!(h.cur().as_deref(), Some("f3"));
    assert_eq!(h.selected(), ["f2", "f4"]);
    assert!(
        h.fake
            .events()
            .iter()
            .any(|e| matches!(e, TabEvent::Left { loc, .. } if *loc == dir("b")))
    );
}

#[test]
fn slow_refresh_swaps_after_150ms_even_without_chunks() {
    let mut h = H::new(8, |_| {});
    h.fake.set_silent(true);
    h.core.dispatch(Command::new("panel.refresh"));
    h.core.pump(Duration::from_millis(50));
    assert!(h.tab().incoming.is_some(), "старый листинг ещё на экране");
    assert_eq!(h.names().len(), 5);
    std::thread::sleep(Duration::from_millis(160));
    h.core.pump(Duration::from_millis(50));
    assert!(h.tab().incoming.is_none(), "через 150 мс — новый листинг");
    assert!(matches!(
        h.tab().listing.status,
        ListingStatus::Loading { .. }
    ));
    assert_eq!(h.names(), Vec::<String>::new());
}

#[test]
fn refresh_is_deduplicated_per_tab_within_pump() {
    let mut h = H::new(8, |_| {});
    let n0 = h.fake.loads().len();
    let s = h.core.sender();
    for c in [
        Command::new("panel.refresh").arg("tab", 1),
        Command::new("panel.refresh").arg("tab", 2),
        Command::new("panel.refresh").arg("tab", 1),
        Command::new("panel.refresh").arg("tab", 2),
    ] {
        s.send(c).unwrap();
    }
    h.core.pump(Duration::from_secs(1));
    assert_eq!(h.fake.loads().len(), n0 + 2);
    h.settle();
}

#[test]
fn stale_or_mismatched_order_is_ignored() {
    let mut h = H::new(8, |_| {});
    h.cmd("panel.sort key=name");
    let seq = h.tab().listing.arrange_seq;
    assert_eq!(seq, 1);
    let len = h.tab().listing.entries.len() as u32;
    let before = h.names();
    let reversed: Arc<Vec<u32>> = Arc::new((0..len).rev().collect());
    h.send(ListingUpdate::Order {
        order: reversed.clone(),
        for_len: len + 1,
        seq,
    });
    assert_eq!(h.names(), before, "wrong for_len must be ignored");
    h.send(ListingUpdate::Order {
        order: reversed.clone(),
        for_len: len,
        seq: seq - 1,
    });
    assert_eq!(h.names(), before, "stale seq must be ignored");
    h.send(ListingUpdate::Order {
        order: reversed,
        for_len: len,
        seq,
    });
    assert_ne!(h.names(), before, "matching Order applies");
}

#[test]
fn sort_repeat_inverts_direction() {
    let mut h = H::new(8, |_| {});
    assert!(!h.tab().sort.desc);
    h.cmd("panel.sort key=name");
    assert!(h.tab().sort.desc);
    assert_eq!(h.names(), ["z", "y", "x", "f2.rs", "f1.txt"]);
    h.cmd("panel.sort key=name");
    assert!(!h.tab().sort.desc);
    assert_eq!(h.names(), ["x", "y", "z", "f1.txt", "f2.rs"]);
    h.cmd("panel.sort key=size");
    assert_eq!(
        (h.tab().sort.key, h.tab().sort.desc),
        (SortKey::Size, false)
    );
    h.cmd("panel.sort key=size desc=false");
    assert!(!h.tab().sort.desc);
    assert_eq!(h.fake.arranges().len(), 4);
}

#[test]
fn filter_and_hidden_rearrange() {
    let mut h = H::new(8, |_| {});
    h.cmd("panel.filter mask=*.rs");
    assert_eq!(h.names(), ["x", "y", "z", "f2.rs"]);
    h.cmd(r#"panel.filter mask="""#);
    assert_eq!(h.names().len(), 5);
    h.core.dispatch(Command::new("panel.filter"));
    assert!(
        h.core
            .take_ui_events()
            .iter()
            .any(|e| matches!(e, UiEvent::Ext { id, .. } if id.as_str() == "panel.ask_filter"))
    );
    h.cmd("panel.hidden.toggle");
    assert!(h.tab().show_hidden);
}

#[test]
fn hundred_k_first_chunk_in_one_pump() {
    let mut h = H::new(2048, |f| f.set_generated(dir("big"), 0, 100_000));
    h.w.wait(Duration::ZERO);
    h.core
        .dispatch(Command::new("panel.go").arg("path", dir("big")));
    assert!(
        h.w.wait(Duration::from_secs(5)),
        "first chunk did not arrive"
    );
    h.core.pump(Duration::from_millis(4));
    assert!(
        h.tab().rows() > 2048,
        "rows after first pump: {}",
        h.tab().rows()
    );
    h.settle();
    assert_eq!(h.tab().listing.entries.len(), 100_000);
    assert_eq!(h.tab().rows(), 100_001);
    assert_eq!(h.tab().listing.status, ListingStatus::Complete);
}

#[test]
fn pending_focus_while_loading() {
    let mut h = H::new(512, |f| f.set_generated(dir("big"), 0, 20_000));
    h.core
        .dispatch(Command::new("panel.go").arg("path", dir("big")));
    h.core
        .dispatch(Command::parse("panel.cursor.to name=f015000").unwrap());
    h.settle();
    assert_eq!(h.cur().as_deref(), Some("f015000"));
    h.cmd("panel.cursor.to name=nope");
    assert_eq!(h.toasts(), [(Level::Warn, "Not found: nope".to_owned())]);
}

#[test]
fn patch_updates_removes_appends() {
    let names: Vec<String> = (0..10).map(|i| format!("f{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut h = H::new(64, |f| f.set_dir(dir("b"), files(&refs)));
    h.go("b");
    for n in ["f3", "f9"] {
        h.cmd(&format!("panel.cursor.to name={n}"));
        h.cmd("sel.toggle");
    }
    let (seq, len) = (
        h.tab().listing.arrange_seq,
        h.tab().listing.entries.len() as u32,
    );
    let mut changed = FakeListing::file("f2", 999);
    changed.mtime = 7;
    // Порядок после патча: f9 переезжает на индекс 3, g1 — 10.
    let order = Arc::new(vec![0, 1, 2, 4, 5, 6, 7, 8, 3, 9]);
    let patch = |base_len| Patch {
        base_len,
        seq,
        update: vec![(2, changed.clone())],
        remove: vec![3],
        append: vec![FakeListing::file("g1", 5)],
        order: order.clone(),
    };
    h.send(ListingUpdate::Patch(patch(len + 1)));
    assert_eq!(
        h.tab().listing.entries.len(),
        10,
        "mismatched base_len is dropped"
    );
    h.send(ListingUpdate::Patch(patch(len)));
    let t = h.tab();
    assert_eq!(t.listing.entries.len(), 10);
    assert_eq!(t.listing.entries[3].name, "f9");
    assert_eq!(t.listing.entries[2].size, 999);
    assert_eq!(h.selected(), ["f9"]);
    assert_eq!(h.tab().selection.bytes(), 100);
    assert_eq!(
        h.cur().as_deref(),
        Some("f9"),
        "cursor stays on the same entry"
    );
    let s = h.tab().listing.stats;
    assert_eq!(
        (s.files, s.bytes),
        (10, 10 + 20 + 999 + 50 + 60 + 70 + 80 + 90 + 100 + 5)
    );
    assert_eq!(h.names().last().map(String::as_str), Some("g1"));
}

#[test]
fn cursor_motion_and_viewport() {
    let names: Vec<String> = (0..50).map(|i| format!("f{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut h = H::new(64, |f| f.set_dir(dir("b"), files(&refs)));
    h.go("b");
    h.cmd("panel.viewport rows=10 cols=1");
    assert_eq!(h.tab().cursor.page, 10);
    let rev = h.core.state().rev;
    h.cmd("panel.viewport rows=10 cols=1");
    assert_eq!(h.core.state().rev, rev, "unchanged viewport keeps rev");
    h.cmd("panel.viewport rows=10 cols=1 top=5");
    assert_eq!(h.core.state().rev, rev + 1, "top changes rev");
    assert_eq!(h.tab().cursor.top, 5);
    h.cmd("panel.cursor.down n=3");
    assert_eq!(h.cur().as_deref(), Some("f02"));
    h.cmd("panel.cursor.page_down");
    assert_eq!(h.cur().as_deref(), Some("f12"));
    h.cmd("panel.cursor.end");
    assert_eq!(h.cur().as_deref(), Some("f49"));
    h.cmd("panel.cursor.down");
    assert_eq!(h.cur().as_deref(), Some("f49"));
    h.cmd("panel.viewport rows=10 cols=2");
    h.cmd("panel.cursor.left");
    assert_eq!(h.cur().as_deref(), Some("f39"));
    h.cmd("panel.cursor.match mask=f1? dir=first");
    assert_eq!(h.cur().as_deref(), Some("f10"));
    h.cmd("panel.cursor.match mask=f1? dir=prev");
    assert_eq!(h.cur().as_deref(), Some("f19"));
    h.cmd("panel.cursor.match mask=f1? dir=next");
    assert_eq!(h.cur().as_deref(), Some("f10"));
    h.cmd("panel.cursor.match mask=zz");
    assert_eq!(h.toasts(), [(Level::Warn, "Nothing matches zz".to_owned())]);
    // Окно стало ниже: страница короче — курсор не остаётся за нижним краем.
    h.cmd("panel.viewport rows=20 cols=1");
    h.cmd("panel.cursor.to name=f30");
    h.cmd("panel.viewport rows=5 cols=1");
    let c = h.tab().cursor;
    assert!(c.row >= c.top && c.row < c.top + 5, "{c:?}");
}

#[test]
fn selection_commands() {
    let mut h = H::new(8, |f| {
        f.set_dir(dir("b"), files(&["d/", "a.rs", "b.rs", "c.txt", "e.txt"]));
    });
    h.go("b");
    h.cmd("sel.all");
    assert_eq!(h.selected(), ["a.rs", "b.rs", "c.txt", "e.txt"]);
    h.cmd("sel.none");
    assert_eq!(h.selected(), Vec::<String>::new());
    h.cmd("sel.restore");
    assert_eq!(h.selected().len(), 4);
    h.cmd("sel.invert");
    assert_eq!(h.selected(), Vec::<String>::new());
    h.cmd("sel.invert dirs=true");
    assert_eq!(h.selected(), ["a.rs", "b.rs", "c.txt", "d", "e.txt"]);
    h.cmd("sel.none");
    h.cmd("sel.add mask=*.rs");
    assert_eq!(h.selected(), ["a.rs", "b.rs"]);
    h.cmd("sel.remove mask=a*");
    assert_eq!(h.selected(), ["b.rs"]);
    h.cmd("panel.cursor.to name=c.txt");
    h.cmd("sel.same by=ext");
    assert_eq!(h.selected(), ["b.rs", "c.txt", "e.txt"]);
    h.cmd("sel.same by=ext on=false");
    assert_eq!(h.selected(), ["b.rs"]);
    h.cmd("sel.none");
    h.cmd("panel.cursor.to name=a.rs");
    h.cmd("sel.move motion=down");
    h.cmd("sel.move motion=down");
    assert_eq!(h.selected(), ["a.rs", "b.rs"]);
    assert_eq!(h.cur().as_deref(), Some("c.txt"));
    // Как в Far: Shift+Up отмечает текущий (невыделенный) и уходит вверх…
    h.cmd("sel.move motion=up");
    assert_eq!(h.selected(), ["a.rs", "b.rs", "c.txt"]);
    assert_eq!(h.cur().as_deref(), Some("b.rs"));
    // …а на выделенном снимает отметку.
    h.cmd("sel.move motion=up");
    assert_eq!(h.selected(), ["a.rs", "c.txt"]);
    assert_eq!(h.cur().as_deref(), Some("a.rs"));
    // Shift+End: до конца списка, последний — тоже (дальше шагать некуда).
    h.cmd("sel.none");
    h.cmd("panel.cursor.to name=c.txt");
    h.cmd("sel.move motion=end");
    let all: Vec<_> = h.selected();
    assert_eq!(all.first().map(String::as_str), Some("c.txt"));
    assert_eq!(all.last().map(String::as_str), Some("e.txt"), "{all:?}");
    h.cmd("sel.none");
    h.cmd("sel.add mask=a.rs;c.txt");
    h.cmd("panel.cursor.home");
    h.cmd("sel.toggle advance=true");
    assert_eq!(h.selected(), ["a.rs", "c.txt"], "`..` cannot be selected");
    assert_eq!(h.cur().as_deref(), Some("d"));
    h.core.dispatch(Command::new("sel.add"));
    assert!(h.core.take_ui_events().iter().any(
        |e| matches!(e, UiEvent::Ext { id, args } if id.as_str() == "sel.ask_mask" && args.bool("on") == Some(true))
    ));
}

#[test]
fn tabs_lifecycle_and_events() {
    let mut h = H::new(8, |f| f.set_dir(dir("b"), files(&["b1"])));
    h.cmd("tab.new");
    let p = h.core.state().panel(Side::Left);
    assert_eq!((p.tabs.len(), p.active), (2, 1));
    let new_id = p.tab().id;
    assert_eq!(new_id, TabId(3));
    assert_eq!(h.tab().loc, dir("a"));
    h.go("b");
    h.cmd("tab.prev");
    assert_eq!(h.tab().loc, dir("a"));
    h.cmd("tab.goto n=2");
    assert_eq!(h.tab().loc, dir("b"));
    h.cmd("tab.move n=1");
    assert_eq!(h.core.state().panel(Side::Left).tabs[0].id, new_id);
    h.cmd("tab.dup");
    assert_eq!(h.core.state().panel(Side::Left).tabs.len(), 3);
    assert!(h.tab().history.can_back(), "dup copies history");
    h.cmd("tab.close");
    h.cmd("tab.close");
    h.cmd("tab.close");
    assert_eq!(h.core.state().panel(Side::Left).tabs.len(), 1);
    assert_eq!(
        h.toasts(),
        [(Level::Warn, "Cannot close the last tab".to_owned())]
    );
    let ev = h.fake.events();
    // Закрыты дубликат (4) и исходный таб (1); остался `new_id`.
    assert!(ev.iter().any(|e| matches!(e, TabEvent::Closed(TabId(4)))));
    assert!(ev.iter().any(|e| matches!(e, TabEvent::Closed(TabId(1)))));
    assert_eq!(h.tab().id, new_id);
    assert!(ev.iter().any(|e| matches!(e, TabEvent::Hidden(TabId(1)))));
    h.cmd("tab.goto n=7");
    assert_eq!(h.toasts(), [(Level::Warn, "No tab 7".to_owned())]);
}

#[test]
fn panels_switch_swap_toggle_mode() {
    let mut h = H::new(8, |f| f.set_dir(dir("b"), files(&["b1"])));
    h.cmd("panel.switch");
    assert_eq!(h.core.state().active, Side::Right);
    h.go("b");
    h.cmd("panel.swap");
    assert_eq!(h.core.state().active, Side::Left);
    assert_eq!(h.tab().loc, dir("b"));
    h.cmd("panel.toggle side=active");
    assert!(!h.core.state().panel(Side::Left).visible);
    assert_eq!(h.core.state().active, Side::Right);
    h.cmd("panel.switch");
    assert_eq!(
        h.core.state().active,
        Side::Right,
        "hidden panel is not activated"
    );
    h.cmd("panel.toggle side=left");
    assert!(h.core.state().panel(Side::Left).visible);
    h.cmd("panel.mode mode=info");
    assert_eq!(h.core.state().passive_tab().mode, PanelMode::Info);
    h.cmd("panel.mode mode=info");
    assert_eq!(h.core.state().passive_tab().mode, PanelMode::Files);
    h.cmd("panel.view_mode mode=brief");
    assert_eq!(h.tab().view_mode, crate::tab::ViewMode::Brief);
    let ev = h.fake.events();
    assert!(ev.iter().any(|e| matches!(e, TabEvent::Hidden(_))));
}

#[test]
fn bookmarks_root_and_go_side() {
    let mut h = H::new(8, |f| f.set_dir(dir("b"), files(&["b1"])));
    h.cmd("panel.bookmark.set n=3");
    h.go("b");
    h.cmd("panel.bookmark.go n=3");
    assert_eq!(h.tab().loc, dir("a"));
    h.cmd("panel.bookmark.go n=4");
    assert_eq!(
        h.toasts(),
        [(Level::Warn, "Bookmark 4 is empty".to_owned())]
    );
    h.run(
        Command::new("panel.go")
            .arg("path", dir("b"))
            .arg("side", "passive"),
    );
    assert_eq!(h.core.state().passive_tab().loc, dir("b"));
    assert_eq!(h.tab().loc, dir("a"));
    h.cmd("panel.root");
    assert!(h.tab().loc.parent().is_none());
    assert_eq!(
        h.tab().listing.status,
        ListingStatus::Failed("not found".into())
    );
}

#[test]
fn enter_file_uses_resolver_else_file_open() {
    struct ToB;
    impl OpenResolver for ToB {
        fn resolve(&self, _dir: &Location, e: &Entry, _a: &Args) -> Option<Command> {
            (e.name == "f2.rs").then(|| Command::new("panel.go").arg("path", dir("b")))
        }
    }
    let mut h = H::with_resolvers(
        8,
        |f| f.set_dir(dir("b"), files(&["b1"])),
        vec![Arc::new(ToB)],
    );
    h.cmd("panel.cursor.to name=f2.rs");
    h.cmd("panel.enter");
    assert_eq!(h.tab().loc, dir("b"));
    h.cmd("panel.back");
    h.cmd("panel.cursor.to name=f1.txt");
    h.cmd("panel.enter");
    assert_eq!(h.tab().loc, dir("a"));
    assert_eq!(
        h.toasts(),
        [(Level::Error, "Unknown command: file.open".to_owned())]
    );
}

/// Виртуальная схема перехватывает вход в каталог (`search:` → настоящий каталог); без
/// резолвера — штатный вход, у корня схемы нет `..`.
#[test]
fn enter_dir_uses_resolver_and_view_root_has_no_parent() {
    struct Real;
    impl OpenResolver for Real {
        fn resolve(&self, _dir: &Location, _e: &Entry, _a: &Args) -> Option<Command> {
            None
        }
        fn enter_dir(&self, d: &Location, e: &Entry, _a: &Args) -> Option<Command> {
            (d.backend == "search" && e.name == r"src\deep")
                .then(|| Command::new("panel.go").arg("path", dir("b")))
        }
    }
    let search = Location {
        backend: "search".into(),
        authority: None,
        path: "/3".into(),
        outer: None,
    };
    let mut h = H::with_resolvers(
        8,
        |f| {
            f.set_dir(search.clone(), files(&["src\\deep/", "src\\a.rs"]));
            f.set_dir(dir("b"), files(&["b1"]));
        },
        vec![Arc::new(Real)],
    );
    // Обычный каталог — штатный вход.
    h.cmd("panel.cursor.to name=x");
    h.cmd("panel.enter");
    assert_eq!(h.tab().loc, dir("a/x"));

    h.run(Command::new("panel.go").arg("path", search.clone()));
    assert_eq!(h.tab().loc, search);
    assert_ne!(h.tab().row(0), Some(Row::Parent), "у search:/3 нет `..`");
    h.cmd("panel.up");
    assert_eq!(h.tab().loc, search, "выше корня схемы не уходим");

    h.run(Command::new("panel.cursor.to").arg("name", r"src\deep"));
    h.cmd("panel.enter");
    assert_eq!(h.tab().loc, dir("b"));
    h.cmd("panel.back");
    assert_eq!(h.tab().loc, search);
}

#[test]
fn quit_sets_flag() {
    let mut h = H::new(8, |_| {});
    assert!(!h.core.state().quit);
    h.cmd("app.quit");
    assert!(h.core.state().quit);
}

#[test]
fn locs_list_in_go_args_validate() {
    let mut h = H::new(8, |_| {});
    h.core
        .dispatch(Command::new("panel.go").arg("path", Value::Int(1)));
    let t = h.toasts();
    assert_eq!(t.len(), 1);
    assert!(t[0].1.starts_with("panel.go: arg path"), "{t:?}");
}

#[test]
fn config_applies_to_tabs_and_reloads() {
    use crate::config::Config;
    use crate::paths::Paths;
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    std::fs::write(
        paths.config_file(),
        "[panels]\nview_mode = \"brief\"\nhistory_cap = 2\n[panels.sort]\nkey = \"size\"\n[general]\nlanguage = \"bad_tag\"\n",
    )
    .unwrap();
    let (config, warnings) = Config::load(&paths);
    let fake = Arc::new(FakeListing::new(8));
    fake.set_dir(dir("a"), files(&["x/", "f1.txt", "f2.rs"]));
    let w = Arc::new(TestWaker::default());
    let mut boot = Boot::new(dir("a"), dir("a"));
    boot.paths = paths.clone();
    boot.config = config;
    boot.warnings = warnings;
    let services = Services {
        listing: fake.clone(),
        open_resolvers: Vec::new(),
    };
    let mut h = H {
        core: Core::new(boot, services, w.clone()),
        fake,
        w,
    };
    h.settle();
    assert_eq!(h.tab().view_mode, crate::tab::ViewMode::Brief);
    assert_eq!(h.tab().sort.key, SortKey::Size);
    let banner = h.core.state().banner.clone().expect("config banner");
    assert!(banner.text.contains("general.language"), "{}", banner.text);
    h.cmd("tab.new");
    assert_eq!(h.tab().view_mode, crate::tab::ViewMode::Brief);

    std::fs::write(paths.config_file(), "[panels]\nview_mode = \"wide\"\n").unwrap();
    h.cmd("app.reload_config");
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.core.state().config.panels.view_mode != crate::tab::ViewMode::Wide {
        assert!(Instant::now() < deadline, "config not reloaded");
        h.settle();
    }
    assert!(
        h.core.state().banner.is_none(),
        "clean reload clears the config banner"
    );
    h.cmd("tab.new");
    assert_eq!(h.tab().view_mode, crate::tab::ViewMode::Wide);
}

#[test]
fn crash_banner_actions() {
    let fake = Arc::new(FakeListing::new(8));
    fake.set_dir(dir("a"), files(&["f1"]));
    fake.set_dir(dir("logs"), files(&["crash-1.txt"]));
    let w = Arc::new(TestWaker::default());
    let mut boot = Boot::new(dir("a"), dir("a"));
    let crash = dir("logs").path.join("crash-1.txt");
    boot.crashed_last_time = Some(crash.clone());
    let services = Services {
        listing: fake.clone(),
        open_resolvers: Vec::new(),
    };
    let mut h = H {
        core: Core::new(boot, services, w.clone()),
        fake,
        w,
    };
    h.settle();
    let banner = h.core.state().banner.clone().expect("crash banner");
    assert_eq!(banner.level, Level::Error);
    assert!(banner.text.contains(&crash.display().to_string()));
    let labels: Vec<&str> = banner.actions.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, ["Show logs", "Dismiss"]);
    h.run(banner.actions[0].1.clone());
    assert_eq!(h.tab().loc, dir("logs"));
    assert_eq!(h.names(), ["crash-1.txt"]);
    h.run(banner.actions[1].1.clone());
    assert!(h.core.state().banner.is_none());
}

/// ADR-0017: сервис без копии записей шлёт `DoneUnsorted` — core сам досортировывает
/// свои записи и только тогда отмечает `Complete`.
#[test]
fn done_unsorted_is_arranged_by_core() {
    let names: Vec<String> = (0..40).rev().map(|i| format!("f{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut h = H::new(8, |f| {
        f.set_dir(dir("b"), files(&refs));
        f.set_unsorted(true);
    });
    let arranges = h.fake.arranges().len();
    h.go("b");
    assert!(matches!(h.tab().listing.status, ListingStatus::Complete));
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(h.names(), sorted);
    assert!(
        h.fake.arranges().len() > arranges,
        "core asked for the final order"
    );

    // Refresh: подмена — уже с финальным порядком.
    h.cmd("panel.cursor.to name=f17");
    h.cmd("panel.refresh");
    assert!(h.tab().incoming.is_none());
    assert!(matches!(h.tab().listing.status, ListingStatus::Complete));
    assert_eq!(h.names(), sorted);
    assert_eq!(h.cur().as_deref(), Some("f17"));
}
