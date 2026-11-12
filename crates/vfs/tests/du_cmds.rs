//! F01-15: команды вида «Дубликаты» и фильтров — хэш только группы под курсором,
//! «Отметить группу», «К папке файла».

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, ListingStatus, Location, Row, Services, Waker};
use urfin_vfs::du::dvfs::{self, DupHead};
use urfin_vfs::du::{DuCfg, DuRegistry, DuVfs, DupLevel, SizeMode};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn idle(c: &Core) -> bool {
    matches!(
        c.state().active_tab().listing.status,
        ListingStatus::Complete
    ) && c.state().jobs.iter().all(|j| !j.state.is_active())
}

fn names(core: &Core) -> Vec<String> {
    let t = core.state().active_tab();
    (0..t.rows())
        .filter_map(|r| match t.row(r) {
            Some(Row::Entry { entry, .. }) => Some(entry.name.to_string()),
            _ => None,
        })
        .collect()
}

fn selected(core: &Core) -> Vec<String> {
    let t = core.state().active_tab();
    let mut out: Vec<String> = (0..t.rows())
        .filter_map(|r| match t.row(r) {
            Some(Row::Entry { idx, entry }) if t.selection.contains(idx) => {
                Some(entry.name.to_string())
            }
            _ => None,
        })
        .collect();
    out.sort();
    out
}

fn put(root: &Path, dir: &str, name: &str, byte: u8, len: usize) {
    fs::create_dir_all(root.join(dir)).unwrap();
    fs::write(root.join(dir).join(name), vec![byte; len]).unwrap();
}

/// Две группы дубликатов больше 64 KiB (частичного хэша мало): `x.bin` в `a`, `b`
/// и `y.bin` в `c`, `d`; открыт вид «Дубликаты», частичный хэш прошёл.
fn rig(tmp: &Path) -> (Core, Arc<DuRegistry>, Location) {
    let root = tmp.join("w");
    put(&root, "a", "x.bin", 1, 100 << 10);
    put(&root, "b", "x.bin", 1, 100 << 10);
    put(&root, "c", "y.bin", 2, 120 << 10);
    put(&root, "d", "y.bin", 2, 120 << 10);

    let du = DuRegistry::new();
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(DuVfs::new(Arc::clone(&du))));
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(&root), Location::local(&root)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    let cfg = DuCfg {
        size: SizeMode::Logical,
        track_min: "1 KiB".into(),
        dup_min: "1 KiB".into(),
        ..DuCfg::default()
    };
    urfin_vfs::du::register_with(&mut core, cfg, Arc::clone(&du), None);
    core.dispatch(Command::new("du.open"));
    pump_until(&mut core, "scan", |c| {
        c.state().active_tab().loc.backend == "du" && idle(c)
    });
    core.dispatch(Command::new("du.filter").arg("kind", "dups"));
    let reg = Arc::clone(&du);
    pump_until(&mut core, "partial hash", |c| {
        idle(c)
            && names(c).len() == 6
            && reg
                .dups_info(&c.state().active_tab().loc)
                .is_some_and(|i| i.groups == 2 && !i.hashing)
    });
    let loc = core.state().active_tab().loc.clone();
    (core, du, loc)
}

fn levels(du: &DuRegistry, loc: &Location) -> Vec<DupLevel> {
    let mut l: Vec<DupLevel> = du
        .dup_heads(loc)
        .iter()
        .map(|h: &DupHead| h.level)
        .collect();
    l.sort_by_key(|l| *l as u8);
    l
}

#[test]
fn du_select_group_marks_whole_group() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _du, _) = rig(tmp.path());
    core.dispatch(Command::new("panel.cursor.to").arg("name", r"c\y.bin"));
    core.dispatch(Command::new("du.select_group"));
    assert_eq!(selected(&core), [r"c\y.bin", r"d\y.bin"]);
    // Ещё раз — отметка группы снимается.
    core.dispatch(Command::new("du.select_group"));
    assert_eq!(selected(&core), Vec::<String>::new());
    // С заголовка группы — так же.
    core.dispatch(Command::new("panel.cursor.to").arg("name", r"a\x.bin"));
    core.dispatch(Command::new("panel.cursor.move").arg("by", -1));
    core.dispatch(Command::new("du.select_group"));
    assert_eq!(selected(&core), [r"a\x.bin", r"b\x.bin"]);
}

#[test]
fn du_verify_hash_only_cursor_group() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, du, loc) = rig(tmp.path());
    assert_eq!(levels(&du, &loc), [DupLevel::Partial, DupLevel::Partial]);
    core.dispatch(Command::new("panel.cursor.to").arg("name", r"c\y.bin"));
    core.dispatch(Command::new("du.verify_hash").arg("group", true));
    pump_until(&mut core, "group hash", |c| {
        idle(c)
            && du
                .dups_info(&loc)
                .is_some_and(|i| i.checked == 1 && !i.hashing)
    });
    // Хэширована только группа `y`: `x` осталась «частичной», без отметок.
    assert_eq!(levels(&du, &loc), [DupLevel::Partial, DupLevel::Full]);
    pump_until(&mut core, "marks", |c| selected(c).len() == 1);
    assert!(
        selected(&core)[0].ends_with("y.bin"),
        "{:?}",
        selected(&core)
    );
    let job = core
        .state()
        .jobs
        .iter()
        .filter(|j| j.kind == "du.dups")
        .last()
        .map(|j| j.progress.snapshot().items_total);
    assert_eq!(job, Some(2), "hashed files");
}

#[test]
fn du_goto_file_opens_its_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut core, _du, loc) = rig(tmp.path());
    let (id, _) = dvfs::parse(&loc).unwrap();
    core.dispatch(Command::new("du.filter").arg("kind", "large"));
    pump_until(&mut core, "large", |c| idle(c) && names(c).len() == 4);
    core.dispatch(Command::new("panel.cursor.to").arg("name", r"d\y.bin"));
    core.dispatch(Command::new("du.goto_file"));
    let want = dvfs::location(id, Path::new("d"));
    pump_until(&mut core, "folder", |c| {
        let t = c.state().active_tab();
        t.loc == want
            && idle(c)
            && matches!(t.current(), Some(Row::Entry { entry, .. }) if entry.name == "y.bin")
    });
}
