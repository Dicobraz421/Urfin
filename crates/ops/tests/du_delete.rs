//! F8 во вкладке анализа места (F01-06): `ops.delete` удаляет настоящие файлы
//! (вкладка `du:` — те же папки на диске), после задачи вкладка перечитывается и размеры
//! предков уменьшаются без пересканирования.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, ListingStatus, Location, Row, Services, Waker};
use urfin_vfs::du::{DuCfg, DuRegistry, DuVfs, SizeMode};
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

fn names(core: &Core) -> Vec<String> {
    let t = core.state().active_tab();
    (0..t.rows())
        .filter_map(|r| match t.row(r) {
            Some(Row::Entry { entry, .. }) => Some(entry.name.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn delete_in_du_tab_shrinks_ancestors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("w");
    fs::create_dir_all(root.join("a").join("old")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::write(root.join("a").join("old").join("x.bin"), vec![0u8; 5000]).unwrap();
    fs::write(root.join("a").join("keep.bin"), vec![0u8; 700]).unwrap();
    fs::write(root.join("b").join("y.bin"), vec![0u8; 40]).unwrap();

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
        ..DuCfg::default()
    };
    urfin_vfs::du::register_with(&mut core, cfg, Arc::clone(&du), None);
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());

    core.dispatch(Command::new("du.open"));
    let ready = |c: &Core| {
        let t = c.state().active_tab();
        t.loc.backend == "du"
            && matches!(t.listing.status, ListingStatus::Complete)
            && c.state().jobs.iter().all(|j| !j.state.is_active())
    };
    pump_until(&mut core, "scan", |c| ready(c) && names(c).len() == 2);
    let top = core.state().active_tab().loc.clone();
    assert_eq!(du.info(&top).unwrap().bytes, 5740);
    // Внутрь «a» (листинг — база сравнения), отметить «old», F8 насовсем.
    core.dispatch(Command::new("panel.cursor.to").arg("name", "a"));
    core.dispatch(Command::new("panel.enter"));
    pump_until(&mut core, "inside a", |c| {
        ready(c) && names(c).iter().any(|n| n == "old")
    });
    core.dispatch(Command::new("panel.cursor.to").arg("name", "old"));
    core.dispatch(Command::new("sel.toggle"));
    core.dispatch(
        Command::new("ops.delete")
            .arg("permanent", true)
            .arg("confirmed", true),
    );
    pump_until(&mut core, "deleted and re-listed", |c| {
        ready(c) && !names(c).iter().any(|n| n == "old")
    });
    assert!(!root.join("a").join("old").exists(), "real folder deleted");
    // Предки уменьшились без нового скана.
    pump_until(&mut core, "ancestors shrink", |_| {
        du.info(&top).is_some_and(|i| i.bytes == 740)
    });
    assert_eq!(
        core.state().jobs.iter().filter(|j| j.kind == "du").count(),
        1,
        "no rescan"
    );
}
