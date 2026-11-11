//! S8-38: панель сравнения `cmp:/<id>` (S8 §4, D-12) — листинг строки на файл, `resolve` на
//! сторону строки, ячейки статуса и второй стороны, `search.to_panel {cmp}` и Ctrl+R
//! (`panel.refresh`) — сравнить заново через `ops.compare`. Сам обход — в `urfin-ops`
//! (`ops.compare { recursive }`), здесь его заменяет handler-заглушка `ops`.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_core::search::{Hit, SearchId, SearchModel, SearchStore};
use urfin_core::{
    ArgSpec, Attrs, Boot, CancelToken, CmdError, Command, CommandMeta, Core, Cx, Entry, EntryKind,
    Handler, ListingStatus, Location, Services, ValueKind, Waker,
};
use urfin_search::svfs::{self, View};
use urfin_search::{SearchStores, SearchVfs};
use urfin_vfs::{ExtCell, ListOpts, Vfs, VfsCfg, VfsListing, VfsRegistry};

fn entry(name: &str, kind: EntryKind, size: u64, mtime: i64) -> Entry {
    Entry {
        name: name.into(),
        size,
        mtime,
        ctime: i64::MIN,
        atime: i64::MIN,
        attrs: Attrs::default(),
        kind,
    }
}

fn row(name: &str, root: u16, status: u8, size: u64, mtime: i64) -> Hit {
    Hit {
        e: entry(name, EntryKind::File, size, mtime),
        root,
        line: 0,
        hl: (0, 0),
        preview: None,
        status,
    }
}

/// Строки, как их пишет `ops.compare { recursive }`: по пути, у пары — вторая строка
/// с другой стороной.
fn rows() -> Vec<Hit> {
    let mut dir = row("build", 0, 1, 0, 5);
    dir.e.kind = EntryKind::Dir;
    vec![
        dir,
        row(r"src\a.rs", 1, 4, 20, 200),
        row(r"src\a.rs", 1, 4, 10, 100),
        row(r"src\b.rs", 0, 5, 7, 100),
        row(r"src\b.rs", 0, 5, 8, 100),
        row("z.txt", 1, 2, 3, 1),
    ]
}

fn sides() -> (Location, Location) {
    (Location::local(r"C:\L"), Location::local(r"D:\R"))
}

fn read_all(vfs: &SearchVfs, loc: &Location) -> Vec<Entry> {
    let mut s = vfs.list(loc, &ListOpts::default()).unwrap();
    let mut out = Vec::new();
    while s.next_batch(&mut out, &CancelToken::new()).unwrap() {}
    out
}

#[test]
fn cmp_lists_a_row_per_file_and_resolves_its_side() {
    let (l, r) = sides();
    let store = SearchModel::default().open(vec![l, r]);
    store.append(rows());
    store.finish(1);
    let stores = SearchStores::new();
    stores.insert_cmp(&store);
    let vfs = SearchVfs::cmp(Arc::clone(&stores));
    assert_eq!(vfs.scheme(), "cmp");
    let loc = svfs::cmp_location(store.id);
    assert_eq!(svfs::parse_cmp(&loc).map(|a| a.id), Some(store.id));
    assert!(svfs::parse(&loc).is_none(), "cmp: — не search:");

    let listed = read_all(&vfs, &loc);
    let names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["build", r"src\a.rs", r"src\b.rs", "z.txt"]);
    // Размер и дата строки — стороны строки (у `newer_right` — правой).
    assert_eq!((listed[1].size, listed[1].mtime), (20, 200));

    let real = |name: &str| {
        vfs.resolve(
            &loc,
            &listed.iter().find(|e| e.name == name).unwrap().clone(),
        )
    };
    assert_eq!(real("build").unwrap().path, Path::new(r"C:\L\build"));
    assert_eq!(real(r"src\a.rs").unwrap().path, Path::new(r"D:\R\src\a.rs"));
    assert_eq!(real(r"src\b.rs").unwrap().path, Path::new(r"C:\L\src\b.rs"));
    assert_eq!(real("z.txt").unwrap().path, Path::new(r"D:\R\z.txt"));
    let (to, _) = stores.locate(&loc, &listed[0]).unwrap();
    assert_eq!(
        to.path,
        Path::new(r"C:\L\build"),
        "Enter по папке — в настоящую"
    );

    let cell = |name: &str, col: &str| vfs.ext(&loc, &entry(name, EntryKind::File, 0, 0), col);
    assert_eq!(
        cell(r"src\a.rs", "status"),
        Some(ExtCell::Text("newer_right".into()))
    );
    assert_eq!(cell(r"src\a.rs", "size2"), Some(ExtCell::Size(10)));
    assert_eq!(cell(r"src\a.rs", "mtime2"), Some(ExtCell::Date(100)));
    assert_eq!(
        cell(r"src\b.rs", "status"),
        Some(ExtCell::Text("differs".into()))
    );
    assert_eq!(cell(r"src\b.rs", "size2"), Some(ExtCell::Size(8)));
    assert_eq!(
        cell("build", "status"),
        Some(ExtCell::Text("only_left".into()))
    );
    assert_eq!(cell("z.txt", "size2"), None, "у одной стороны второй нет");
    assert_eq!(svfs::status_name(3), Some("newer_left"));
    // Колонки `screen-cmppanel`: Статус 112 · Размер · Изменён.
    let ids: Vec<&str> = vfs.columns(&loc).iter().map(|c| c.id).collect();
    assert_eq!(ids, ["status", "size", "mtime"]);
    assert_eq!(vfs.columns(&loc)[0], svfs::STATUS);

    // Тот же номер у `search:` не открывается, и наоборот.
    let search = SearchVfs::new(Arc::clone(&stores));
    assert!(
        search
            .list(&svfs::location(store.id, View::Files), &ListOpts::default())
            .is_err()
    );
    let plain = SearchModel::default().open(Vec::new());
    stores.insert(&plain, &Arc::default(), false);
    assert!(
        vfs.list(&svfs::cmp_location(plain.id), &ListOpts::default())
            .is_err()
    );
}

// Через `Core`: `search.to_panel {cmp}`, Ctrl+R.

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

const COMPARE_ARGS: [ArgSpec; 5] = [
    ArgSpec {
        name: "mode",
        kind: ValueKind::Str,
        required: false,
    },
    ArgSpec {
        name: "recursive",
        kind: ValueKind::Bool,
        required: false,
    },
    ArgSpec {
        name: "left",
        kind: ValueKind::Loc,
        required: false,
    },
    ArgSpec {
        name: "right",
        kind: ValueKind::Loc,
        required: false,
    },
    ArgSpec {
        name: "replace",
        kind: ValueKind::Bool,
        required: false,
    },
];

static OPS: [CommandMeta; 1] = [CommandMeta {
    id: "ops.compare",
    title: "",
    category: "",
    args: &COMPARE_ARGS,
    repeatable: false,
}];

/// Заглушка `ops.compare { recursive }`: стор с готовыми строками и `search.to_panel`.
struct FakeOps {
    seen: Sender<Command>,
    left: Location,
    right: Location,
    stores: Arc<Mutex<Vec<Arc<SearchStore>>>>,
}

impl Handler for FakeOps {
    fn namespace(&self) -> &'static str {
        "ops"
    }
    fn commands(&self) -> &'static [CommandMeta] {
        &OPS
    }
    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        self.seen.send(cmd.clone()).unwrap();
        let left = cmd.args.loc("left").unwrap_or_else(|| self.left.clone());
        let right = cmd.args.loc("right").unwrap_or_else(|| self.right.clone());
        if cx.state.ext.get::<SearchModel>().is_none() {
            cx.state.ext.insert(SearchModel::default());
        }
        let store = cx
            .state
            .ext
            .get_mut::<SearchModel>()
            .unwrap()
            .open(vec![left, right]);
        store.append(rows());
        store.finish(1);
        self.stores.lock().unwrap().push(Arc::clone(&store));
        cx.dispatch(
            Command::new("search.to_panel")
                .arg("id", i64::from(store.id.0))
                .arg("job", 0)
                .arg("cmp", cmd.args.str("mode").unwrap_or("quick"))
                .arg("replace", cmd.args.bool("replace").unwrap_or(false)),
        );
        Ok(())
    }
}

fn core(start: &Path) -> (Core, Receiver<Command>) {
    let stores = SearchStores::new();
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(SearchVfs::new(Arc::clone(&stores))));
    reg.register(Arc::new(SearchVfs::cmp(Arc::clone(&stores))));
    let reg = Arc::new(reg);
    let cfg = VfsCfg::default();
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg.clone())),
        open_resolvers: Vec::new(),
    };
    let boot = Boot::new(Location::local(start), Location::local(start));
    let mut core = Core::new(boot, services, Arc::new(NoWake));
    urfin_vfs::register(&mut core, Arc::clone(&reg), cfg);
    urfin_search::register_with(&mut core, reg, None, stores);
    let (seen, rx) = channel();
    let (left, right) = sides();
    core.register(Box::new(FakeOps {
        seen,
        left,
        right,
        stores: Arc::default(),
    }));
    (core, rx)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        core.pump(Duration::from_millis(4));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn shown(core: &Core) -> Option<SearchId> {
    svfs::parse_cmp(&core.state().active_tab().loc).map(|a| a.id)
}

fn listed(core: &Core) -> bool {
    let t = core.state().active_tab();
    matches!(t.listing.status, ListingStatus::Complete) && t.listing.entries.len() == 4
}

#[test]
fn cmp_opens_a_tab_and_refresh_compares_again_in_place() {
    let d = tempfile::tempdir().unwrap();
    let (mut core, seen) = core(d.path());
    core.pump(Duration::from_millis(4));
    let tabs = core.state().active_panel().tabs.len();
    core.dispatch(
        Command::new("ops.compare")
            .arg("recursive", true)
            .arg("mode", "content"),
    );
    pump_until(&mut core, "cmp tab", |c| shown(c).is_some() && listed(c));
    let first = shown(&core).unwrap();
    assert_eq!(core.state().active_panel().tabs.len(), tabs + 1);
    assert!(seen.try_recv().is_ok());

    // Ctrl+R — `panel.refresh`: сравнить заново те же стороны и режим, в том же табе.
    core.dispatch(Command::new("panel.refresh"));
    pump_until(&mut core, "compared again", |c| {
        shown(c).is_some_and(|id| id != first) && listed(c)
    });
    let again = seen.try_recv().expect("ops.compare again");
    let (l, r) = sides();
    assert_eq!(again.args.loc("left"), Some(l));
    assert_eq!(again.args.loc("right"), Some(r));
    assert_eq!(again.args.str("mode"), Some("content"));
    assert_eq!(again.args.bool("replace"), Some(true));
    assert_eq!(
        core.state().active_panel().tabs.len(),
        tabs + 1,
        "тот же таб"
    );

    // Прежний стор никто не держит — он уходит в недавние, но пока жив.
    assert!(
        core.state()
            .ext
            .get::<SearchModel>()
            .unwrap()
            .get(first)
            .is_some()
    );
}
