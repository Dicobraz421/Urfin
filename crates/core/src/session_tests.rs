//! Интеграционные тесты сессии: tempfile + `FakeListing`.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::bus::{Boot, Core};
use crate::cmd::Command;
use crate::config::Config;
use crate::entry::Entry;
use crate::fake::FakeListing;
use crate::ids::Side;
use crate::loc::Location;
use crate::msg::Waker;
use crate::paths::Paths;
use crate::services::Services;
use crate::session::{Session, WindowGeom};
use crate::tab::{ListingStatus, PanelMode, Row, SortKey, Tab, ViewMode};

#[derive(Default)]
struct TestWaker {
    pending: Mutex<bool>,
    cv: Condvar,
}

impl Waker for TestWaker {
    fn wake(&self) {
        *self.pending.lock().unwrap() = true;
        self.cv.notify_all();
    }
    fn wake_after(&self, _d: Duration) {}
}

impl TestWaker {
    fn wait(&self, timeout: Duration) -> bool {
        let g = self.pending.lock().unwrap();
        let (mut g, _) = self.cv.wait_timeout_while(g, timeout, |p| !*p).unwrap();
        std::mem::take(&mut *g)
    }
}

fn dir(name: &str) -> Location {
    let root = if cfg!(windows) { r"C:\t" } else { "/t" };
    Location::local(format!("{root}/{name}"))
}

fn files(names: &[&str]) -> Vec<Entry> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| match n.strip_suffix('/') {
            Some(d) => FakeListing::dir(d),
            None => FakeListing::file(n, (i as u64 + 1) * 10),
        })
        .collect()
}

fn fake() -> Arc<FakeListing> {
    let f = Arc::new(FakeListing::new(8));
    f.set_dir(dir("a"), files(&["x/", "f1"]));
    f.set_dir(dir("a/x"), files(&["x1"]));
    f.set_dir(dir("b"), files(&["b1", "b2", "b3"]));
    f.set_dir(dir("c"), files(&["c1.rs", "c2.txt"]));
    f
}

struct H {
    core: Core,
    w: Arc<TestWaker>,
    paths: Paths,
}

impl H {
    fn new(tmp: &tempfile::TempDir, session: Option<Session>, config: Config) -> H {
        let paths = Paths::in_dir(tmp.path());
        let w = Arc::new(TestWaker::default());
        let mut boot = Boot::new(dir("a"), dir("a"));
        boot.paths = paths.clone();
        boot.config = config;
        boot.session = session;
        let services = Services {
            listing: fake(),
            open_resolvers: Vec::new(),
        };
        let mut h = H {
            core: Core::new(boot, services, w.clone()),
            w,
            paths,
        };
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

    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "did not settle");
            if self.core.pump(Duration::from_secs(1)).more {
                continue;
            }
            let woke = self
                .w
                .wait(Duration::from_millis(if self.busy() { 100 } else { 20 }));
            if !woke && !self.busy() {
                return;
            }
        }
    }

    fn cmd(&mut self, text: &str) {
        self.core.dispatch(Command::parse(text).unwrap());
        self.settle();
    }

    fn go(&mut self, name: &str) {
        self.core
            .dispatch(Command::new("panel.go").arg("path", dir(name)));
        self.settle();
    }

    fn file(&self) -> PathBuf {
        self.paths.session_file()
    }
}

fn cur(t: &Tab) -> Option<String> {
    match t.current()? {
        Row::Entry { entry, .. } => Some(entry.name.to_string()),
        Row::Parent => Some("..".into()),
    }
}

fn back(t: &Tab) -> Vec<Location> {
    t.history
        .back_entries()
        .iter()
        .map(|h| h.loc.clone())
        .collect()
}

#[test]
fn roundtrip_capture_save_load_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = H::new(&tmp, None, Config::default());
    // Левая панель: два таба, второй активный.
    h.go("b");
    h.cmd("panel.cursor.to name=b2");
    h.cmd("panel.sort key=size");
    h.cmd("panel.view_mode mode=brief");
    h.cmd("panel.mode mode=info side=left");
    h.cmd("tab.new");
    h.go("c");
    h.cmd("panel.bookmark.set n=3");
    // Правая панель: фильтр и скрытые.
    h.cmd("panel.switch");
    h.go("c");
    h.cmd("panel.filter mask=*.rs");
    h.cmd("panel.hidden.toggle");
    h.cmd("panel.switch");
    h.core.set_window(WindowGeom {
        x: 10.0,
        y: 20.0,
        w: 800.0,
        h: 600.0,
        maximized: false,
        split: 0.4,
    });

    let saved = h.core.shutdown(Duration::from_secs(5));
    let loaded = Session::load(&Paths::in_dir(tmp.path())).expect("session file");
    assert_eq!(loaded, saved);
    assert_eq!(loaded.window.map(|w| w.split), Some(0.4));

    let tmp2 = tempfile::tempdir().unwrap();
    let h2 = H::new(&tmp2, Some(loaded), Config::default());
    let s = h2.core.state();
    assert_eq!(s.active, Side::Left);
    let left = s.panel(Side::Left);
    assert_eq!(left.tabs.len(), 2);
    assert_eq!(left.active, 1);
    let t0 = &left.tabs[0];
    assert_eq!(t0.loc, dir("b"));
    assert_eq!(cur(t0).as_deref(), Some("b2"));
    assert_eq!(t0.sort.key, SortKey::Size);
    assert_eq!(t0.view_mode, ViewMode::Brief);
    assert_eq!(t0.mode, PanelMode::Info);
    assert_eq!(back(t0), [dir("a")]);
    let t1 = &left.tabs[1];
    assert_eq!(t1.loc, dir("c"));
    assert_eq!(back(t1), [dir("b")]);
    let right = s.panel(Side::Right).tab();
    assert_eq!(right.loc, dir("c"));
    assert_eq!(
        right.filter.as_ref().map(crate::mask::Mask::as_str),
        Some("*.rs")
    );
    assert!(right.show_hidden);
    assert_eq!(s.bookmarks[3], Some(dir("c")));
    let visited: Vec<Location> = s.visited.iter().cloned().collect();
    assert_eq!(visited[..3], [dir("c"), dir("b"), dir("a")]);
    // Время посещения переживает сессию.
    assert!(s.visited.iter_at().all(|(_, t)| t > 0));
    // Уникальные id после восстановления.
    let mut ids: Vec<u32> = s
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter().map(|t| t.id.0))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3);
}

#[test]
fn hundred_navigations_in_a_second_write_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = H::new(&tmp, None, Config::default());
    assert_eq!(h.core.session_writes, 0, "initial loads are not a change");
    // Шаги по расписанию «старт + i·10 мс»: грубый таймер (macOS) не растягивает
    // серию — отставшие шаги идут без сна.
    let started = Instant::now();
    let mut last = started;
    let mut max_gap = Duration::ZERO;
    for i in 0..100u32 {
        let due = started + Duration::from_millis(10) * i;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let d = if i % 2 == 0 { "b" } else { "c" };
        h.core
            .dispatch(Command::new("panel.go").arg("path", dir(d)));
        h.core.pump(Duration::from_millis(5));
        let now = Instant::now();
        max_gap = max_gap.max(now - last);
        last = now;
    }
    // Одна запись требует, чтобы пауза между изменениями не доходила до 2 с.
    assert!(max_gap < Duration::from_secs(1), "gap {max_gap:?}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "series took {:?}",
        started.elapsed()
    );
    assert_eq!(
        h.core.session_writes, 0,
        "no write while changes keep coming"
    );
    thread::sleep(Duration::from_millis(2100));
    h.core.pump(Duration::from_millis(5));
    assert_eq!(h.core.session_writes, 1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !h.file().exists() {
        assert!(Instant::now() < deadline, "session not written");
        thread::sleep(Duration::from_millis(10));
    }
    let s = Session::load(&h.paths).unwrap();
    assert_eq!(s.panels[0].tabs[0].loc, dir("c"));
    h.core.pump(Duration::from_millis(5));
    assert_eq!(h.core.session_writes, 1, "nothing changed since");
}

#[test]
fn save_session_command_writes_now() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = H::new(&tmp, None, Config::default());
    h.cmd("app.save_session");
    assert_eq!(h.core.session_writes, 1);
}

#[test]
fn interrupted_write_keeps_previous_file() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    let s1 = Session {
        active: Side::Right,
        ..Session::default()
    };
    s1.save(&paths).unwrap();
    // Как после kill посреди записи: недописанный временный файл, rename не было.
    let stray = paths.state_dir.join(".session.toml.999-0.tmp");
    fs::write(&stray, "version = 1\nactive = \"Le").unwrap();
    assert_eq!(Session::load(&paths), Some(s1.clone()));
    // Запись идёт во временный файл, поэтому оригинал целый до rename.
    let before = fs::read(paths.session_file()).unwrap();
    let mut s2 = s1.clone();
    s2.active = Side::Left;
    s2.save(&paths).unwrap();
    assert_ne!(fs::read(paths.session_file()).unwrap(), before);
    assert_eq!(Session::load(&paths), Some(s2));
}

#[test]
fn corrupt_file_gives_none_and_corrupt_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(tmp.path());
    fs::create_dir_all(&paths.state_dir).unwrap();
    fs::write(paths.session_file(), "active = [[[").unwrap();
    assert_eq!(Session::load(&paths), None);
    let corrupt = paths.state_dir.join("session.toml.corrupt");
    assert_eq!(fs::read_to_string(corrupt).unwrap(), "active = [[[");
    assert_eq!(Session::load(&Paths::in_dir(tmp.path().join("none"))), None);
}

#[test]
fn missing_dir_falls_back_to_existing_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::default();
    s.panels[0].tabs.push(crate::session::TabSession {
        loc: dir("a/x/gone/deeper"),
        focus: None,
        top: 0,
        sort: crate::tab::SortSpec::default(),
        view_mode: ViewMode::Full,
        mode: PanelMode::Files,
        filter: None,
        show_hidden: false,
        back: Vec::new(),
    });
    let h = H::new(&tmp, Some(s), Config::default());
    let t = h.core.state().panel(Side::Left).tab();
    assert_eq!(t.loc, dir("a/x"));
    assert_eq!(t.listing.status, ListingStatus::Complete);
    assert_eq!(t.restore_up, 0);
}

#[test]
fn virtual_tabs_are_saved_as_last_real_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = H::new(&tmp, None, Config::default());
    h.go("b");
    let search = Location {
        backend: "search".into(),
        authority: None,
        path: "/q".into(),
        outer: None,
    };
    h.core
        .dispatch(Command::new("panel.go").arg("path", search));
    h.settle();
    let s = Session::capture(h.core.state(), None);
    assert_eq!(s.panels[0].tabs[0].loc, dir("b"));
    assert!(s.visited.iter().all(|l| l.backend != "search"));
}

#[test]
fn restore_disabled_ignores_session() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session {
        active: Side::Right,
        ..Session::default()
    };
    let mut config = Config::default();
    config.general.restore_session = false;
    let h = H::new(&tmp, Some(s), config);
    assert_eq!(h.core.state().active, Side::Left);
    assert_eq!(h.core.state().active_tab().loc, dir("a"));
}

#[test]
fn session_migrations_run_in_order() {
    fn v0_to_v1(t: &mut toml::Table) -> Result<(), String> {
        // Пример: в v0 активная сторона называлась `side`.
        let side = t.remove("side").ok_or("no side")?;
        t.insert("active".into(), side);
        Ok(())
    }
    let migrations: &[crate::config::Migration] = &[(0, v0_to_v1)];
    let s = crate::session::parse("version = 0\nside = \"Right\"\n", migrations, 1).unwrap();
    assert_eq!(s.version, 1);
    assert_eq!(s.active, crate::ids::Side::Right);
    // Без шага — как есть: неизвестное поле игнорируется, active по умолчанию.
    let s = crate::session::parse("version = 0\nside = \"Right\"\n", &[], 1).unwrap();
    assert_eq!(s.active, crate::ids::Side::Left);
    assert!(crate::session::parse("version = [", &[], 1).is_err());
}
