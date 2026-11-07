//! S8-10: `SearchVfs`, схема `search:` — листинг из стора, `resolve`, вид «без доступа»,
//! перепроверка по `panel.refresh`, время жизни сторов, память.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use rustc_hash::FxHashSet;
use urfin_core::search::{Hit, SearchId, SearchModel, SearchStore};
use urfin_core::{
    Attrs, Boot, CancelToken, Command, Core, Entry, EntryKind, ListingStatus, Location, Services,
    Waker,
};
use urfin_search::svfs::{self, View};
use urfin_search::{SearchStores, SearchVfs};
use urfin_vfs::{
    Caps, ColumnSpec, ColumnView, ExtCell, ListOpts, ListStream, Metadata, Space, Vfs, VfsCfg,
    VfsError, VfsListing, VfsPath, VfsRead, VfsRegistry, WatchGuard, WatchTx,
};

fn entry(name: &str) -> Entry {
    Entry {
        name: name.into(),
        size: 1,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind: EntryKind::File,
    }
}

fn hit(name: &str, root: u16, line: u32) -> Hit {
    Hit {
        e: entry(name),
        root,
        line,
        hl: (0, 0),
        preview: None,
        status: 0,
    }
}

/// Стор поиска по `roots` в реестре и backend над ним.
fn setup(roots: Vec<Location>, text: bool) -> (Arc<SearchStore>, Arc<SearchStores>, SearchVfs) {
    let store = SearchModel::default().open(roots);
    let stores = SearchStores::new();
    stores.insert(&store, &Arc::default(), text);
    let vfs = SearchVfs::new(Arc::clone(&stores));
    (store, stores, vfs)
}

fn files(id: SearchId) -> Location {
    svfs::location(id, View::Files)
}

fn read_all(vfs: &SearchVfs, loc: &Location) -> Vec<Entry> {
    let mut s = vfs.list(loc, &ListOpts::default()).unwrap();
    let mut out = Vec::new();
    while s.next_batch(&mut out, &CancelToken::new()).unwrap() {}
    out
}

fn names(v: &[Entry]) -> Vec<String> {
    v.iter().map(|e| e.name.to_string()).collect()
}

#[test]
fn address_roundtrip() {
    let loc = svfs::location(SearchId(7), View::Errors);
    let a = svfs::parse(&loc).unwrap();
    assert_eq!((a.id, a.view, a.name), (SearchId(7), View::Errors, None));
    let mut row = files(SearchId(3));
    row.path = r"/3/src\a.rs".into();
    let a = svfs::parse(&row).unwrap();
    assert_eq!(
        (a.id, a.view, a.name.as_deref()),
        (SearchId(3), View::Files, Some(r"src\a.rs"))
    );
    assert!(svfs::parse(&Location::local(r"C:\x")).is_none());
}

#[test]
fn slow_producer_rows_come_fast_and_return_loses_nothing() {
    const N: usize = 40;
    let (store, _stores, vfs) = setup(vec![Location::local(r"C:\r")], false);
    let loc = files(store.id);
    let sent = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let producer = {
        let (store, sent) = (Arc::clone(&store), Arc::clone(&sent));
        std::thread::spawn(move || {
            for i in 0..N {
                std::thread::sleep(Duration::from_millis(15));
                sent.lock().unwrap().push(Instant::now());
                store.append(vec![hit(&format!("f{i}.rs"), 0, 0)]);
            }
            store.finish(1);
        })
    };

    // Первый листинг: строки приходят вскоре после попадания; уходим на середине.
    let mut s = vfs.list(&loc, &ListOpts::default()).unwrap();
    let mut first = Vec::new();
    let cancel = CancelToken::new();
    while first.len() < N / 2 {
        let before = first.len();
        assert!(s.next_batch(&mut first, &cancel).unwrap());
        let now = Instant::now();
        let sent = sent.lock().unwrap();
        for i in before..first.len() {
            let lag = now - sent[i];
            assert!(lag < Duration::from_millis(50), "строка {i}: {lag:?}");
        }
    }
    cancel.cancel();
    assert!(matches!(
        s.next_batch(&mut first, &cancel),
        Err(VfsError::Cancelled)
    ));
    drop(s);

    // Возврат: новый листинг с начала, до конца поиска.
    let back = read_all(&vfs, &loc);
    producer.join().unwrap();
    let want: Vec<String> = (0..N).map(|i| format!("f{i}.rs")).collect();
    assert_eq!(names(&back), want, "без потерь и дублей");
    let seen: FxHashSet<_> = first.iter().map(|e| e.name.clone()).collect();
    assert_eq!(seen.len(), first.len(), "первый листинг без дублей");
}

#[test]
fn same_path_in_two_roots_resolves_to_its_root() {
    let roots = vec![
        Location::local(r"C:\a"),
        Location::local(r"C:\ab"),
        Location::local(r"D:\"),
    ];
    let (store, _s, vfs) = setup(roots, false);
    store.append(vec![hit(r"x\y.rs", 0, 0)]);
    store.append(vec![hit(r"x\y.rs", 1, 0)]);
    store.append(vec![hit(r"x\y.rs", 2, 0)]);
    store.finish(1);
    let loc = files(store.id);
    let list = read_all(&vfs, &loc);
    assert_eq!(
        names(&list),
        [r"C:\a\x\y.rs", r"C:\ab\x\y.rs", r"D:\x\y.rs"]
    );
    let real: Vec<Location> = list.iter().map(|e| vfs.resolve(&loc, e).unwrap()).collect();
    assert_eq!(
        real,
        [
            Location::local(r"C:\a\x\y.rs"),
            Location::local(r"C:\ab\x\y.rs"),
            Location::local(r"D:\x\y.rs"),
        ]
    );
    assert!(vfs.resolve(&loc, &entry(r"E:\x\y.rs")).is_err());

    // Один корень: имя — путь от корня.
    let (one, _s, vfs) = setup(vec![Location::local(r"C:\Dev")], false);
    one.append(vec![hit(r"src\a.rs", 0, 0)]);
    let e = entry(r"src\a.rs");
    assert_eq!(
        vfs.resolve(&files(one.id), &e).unwrap(),
        Location::local(r"C:\Dev\src\a.rs")
    );
}

#[test]
fn errors_view_lists_denied_folders() {
    let store = SearchModel::default().open(vec![Location::local(r"C:\r")]);
    let denied = Arc::new(Mutex::new(vec![
        Location::local(r"C:\r\secret"),
        Location::local(r"C:\r\x\sys"),
    ]));
    let stores = SearchStores::new();
    stores.insert(&store, &denied, false);
    let vfs = SearchVfs::new(stores);
    let loc = svfs::location(store.id, View::Errors);
    let list = read_all(&vfs, &loc);
    assert_eq!(names(&list), [r"C:\r\secret", r"C:\r\x\sys"]);
    assert!(list.iter().all(|e| e.kind == EntryKind::Dir));
    assert_eq!(
        vfs.resolve(&loc, &list[1]).unwrap(),
        Location::local(r"C:\r\x\sys")
    );
    assert_eq!(vfs.columns(&loc), []);
}

#[test]
fn the_scheme_is_virtual_and_read_only() {
    let (store, _s, vfs) = setup(
        vec![Location::local(
            r"C:
",
        )],
        false,
    );
    for view in [View::Files, View::Errors] {
        let caps = vfs.caps(&svfs::location(store.id, view));
        assert!(caps.has(Caps::VIRTUAL) && caps.has(Caps::READ_ONLY));
    }
    let gone = files(SearchId(999));
    assert!(matches!(
        vfs.list(&gone, &ListOpts::default()),
        Err(VfsError::NotFound(_))
    ));
}

#[test]
fn columns_and_cells_follow_the_query() {
    let (name, _s, vfs) = setup(vec![Location::local(r"C:\r")], false);
    let ids: Vec<&str> = vfs.columns(&files(name.id)).iter().map(|c| c.id).collect();
    assert_eq!(ids, ["size", "mtime"]);

    let (text, _s, vfs) = setup(vec![Location::local(r"C:\r")], true);
    let mut first = hit("scan.rs", 0, 88);
    first.preview = Some("rt.spawn(Lane::Bulk)".into());
    first.hl = (9, 19);
    text.append(vec![first, hit("scan.rs", 0, 131)]);
    text.append(vec![hit("lib.rs", 0, 3)]);
    text.finish(1);
    let loc = files(text.id);
    let cols = vfs.columns(&loc);
    assert_eq!(
        cols.iter().map(|c| c.id).collect::<Vec<_>>(),
        ["line", "hit"]
    );
    assert!(cols[0].mono && cols[0].right);
    assert_eq!(cols[1].view, ColumnView::Snippet);
    let list = read_all(&vfs, &loc);
    assert_eq!(names(&list), ["scan.rs", "lib.rs"], "строка на файл");
    assert_eq!(
        vfs.ext(&loc, &list[0], "line"),
        Some(ExtCell::Text("88".into()))
    );
    assert_eq!(
        vfs.ext(&loc, &list[0], "hit"),
        Some(ExtCell::Snippet {
            text: "rt.spawn(Lane::Bulk)".into(),
            hl: (9, 19)
        })
    );
    assert_eq!(
        vfs.ext(&loc, &list[1], "hit"),
        None,
        "без фрагмента — пусто"
    );
    assert_eq!(vfs.ext(&loc, &entry("нет.rs"), "line"), None);
}

#[test]
fn stat_and_read_go_to_the_real_file() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("src")).unwrap();
    std::fs::write(d.path().join(r"src\a.rs"), "needle").unwrap();
    let (store, _s, vfs) = setup(vec![Location::local(d.path())], false);
    store.append(vec![hit(r"src\a.rs", 0, 0)]);
    let mut p = files(store.id);
    p.path = format!("/{}/src\\a.rs", store.id.0).into();
    assert_eq!(vfs.stat(&p, true).unwrap().size, 6);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut vfs.open_read(&p).unwrap(), &mut text).unwrap();
    assert_eq!(text, "needle");
    assert_eq!(vfs.local_path(&p), Some(d.path().join(r"src\a.rs")));
    let e = vfs
        .stat_entry(&files(store.id), r"src\a.rs", Attrs::default())
        .unwrap();
    assert_eq!((e.name.as_str(), e.size), (r"src\a.rs", 6));
    assert_eq!(
        vfs.stat(&files(store.id), true).unwrap().kind,
        EntryKind::Dir
    );
}

/// 1M попаданий одним стором: файлы по одному попаданию (худший случай для индекса имён).
fn million(text: bool) -> (Arc<SearchStore>, Arc<SearchStores>, SearchVfs) {
    const N: usize = 1_000_000;
    let (store, stores, vfs) = setup(vec![Location::local(r"C:\Dev")], text);
    let preview = CompactString::from("x".repeat(100));
    let mut batch = Vec::with_capacity(4096);
    for i in 0..N {
        // `d123\f123456.rs` — короткий путь, без кучи.
        let mut h = hit(&format!("d{}\\f{i}.rs", i / 1000), 0, u32::from(text));
        if text {
            h.preview = Some(preview.clone());
            h.hl = (0, 6);
        }
        batch.push(h);
        if batch.len() == 4096 {
            store.append(std::mem::take(&mut batch));
        }
    }
    store.append(batch);
    store.finish(1);
    // Панель прочитала всё: индекс имён построен.
    let loc = files(store.id);
    let mut s = vfs.list(&loc, &ListOpts::default()).unwrap();
    let mut out = Vec::new();
    let mut n = 0;
    while s.next_batch(&mut out, &CancelToken::new()).unwrap() {
        n += out.len();
        out.clear();
    }
    n += out.len();
    assert_eq!(n, N);
    (store, stores, vfs)
}

const MIB: u64 = 1 << 20;

#[test]
fn million_name_hits_fit_110_mib() {
    let (store, stores, _vfs) = million(false);
    assert_eq!(
        stores.extra_bytes(store.id),
        0,
        "поиску по имени индекс не нужен"
    );
    let total = stores.approx_bytes(store.id);
    assert!(total <= 110 * MIB, "{} МиБ", total / MIB);
}

#[test]
fn million_text_hits_fit_240_mib() {
    let (store, stores, vfs) = million(true);
    let extra = stores.extra_bytes(store.id);
    assert!(extra <= 8 * MIB, "индекс имён: {} МиБ", extra / MIB);
    let total = stores.approx_bytes(store.id);
    assert!(total <= 240 * MIB, "{} МиБ", total / MIB);
    // Индекс находит строку по имени.
    let e = entry(r"d999\f999999.rs");
    assert_eq!(
        vfs.ext(&files(store.id), &e, "line"),
        Some(ExtCell::Text("1".into()))
    );
}

// ---------------------------------------------------------------------------
// Через `Core`: вкладки, `panel.refresh`, время жизни.

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// `SearchVfs`, который считает листинги: опрос hub — лишние `list`.
struct Spy {
    inner: SearchVfs,
    lists: Arc<AtomicUsize>,
}

impl Vfs for Spy {
    fn scheme(&self) -> &'static str {
        self.inner.scheme()
    }
    fn caps(&self, at: &VfsPath) -> Caps {
        self.inner.caps(at)
    }
    fn list(&self, dir: &VfsPath, opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        self.inner.list(dir, opts)
    }
    fn stat(&self, p: &VfsPath, follow: bool) -> Result<Metadata, VfsError> {
        self.inner.stat(p, follow)
    }
    fn stat_entry(&self, dir: &VfsPath, name: &str, hint: Attrs) -> Result<Entry, VfsError> {
        self.inner.stat_entry(dir, name, hint)
    }
    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        self.inner.open_read(p)
    }
    fn space(&self, at: &VfsPath) -> Result<Space, VfsError> {
        self.inner.space(at)
    }
    fn local_path(&self, p: &VfsPath) -> Option<PathBuf> {
        self.inner.local_path(p)
    }
    fn watch(&self, dir: &VfsPath, tx: WatchTx) -> Result<WatchGuard, VfsError> {
        self.inner.watch(dir, tx)
    }
    fn resolve(&self, base: &VfsPath, item: &Entry) -> Result<VfsPath, VfsError> {
        self.inner.resolve(base, item)
    }
    fn columns(&self, dir: &VfsPath) -> &'static [ColumnSpec] {
        self.inner.columns(dir)
    }
    fn ext(&self, dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        self.inner.ext(dir, e, col)
    }
}

fn core(start: &Path) -> Core {
    core_spy(start).0
}

/// `Core` с `search:` в реестре листинга; опрос hub — раз в 20 мс, если бы он был.
fn core_spy(start: &Path) -> (Core, Arc<AtomicUsize>) {
    let stores = SearchStores::new();
    let lists = Arc::new(AtomicUsize::new(0));
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(Spy {
        inner: SearchVfs::new(Arc::clone(&stores)),
        lists: Arc::clone(&lists),
    }));
    let reg = Arc::new(reg);
    let cfg = VfsCfg {
        poll_interval_ms: 20,
        ..VfsCfg::default()
    };
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg.clone())),
        open_resolvers: Vec::new(),
    };
    let boot = Boot::new(Location::local(start), Location::local(start));
    let mut core = Core::new(boot, services, Arc::new(NoWake));
    urfin_vfs::register(&mut core, Arc::clone(&reg), cfg);
    urfin_search::register_with(&mut core, reg, None, stores);
    (core, lists)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        core.pump(Duration::from_millis(4));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn model(core: &Core) -> &SearchModel {
    core.state().ext.get::<SearchModel>().expect("search model")
}

/// Имена в активной вкладке, когда листинг готов.
fn tab_names(core: &Core) -> Option<Vec<String>> {
    let t = core.state().active_tab();
    matches!(t.listing.status, ListingStatus::Complete).then(|| {
        let mut v = names(&t.listing.entries);
        v.sort();
        v
    })
}

fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(r"src\deep")).unwrap();
    std::fs::write(d.path().join("readme.md"), "текст\n").unwrap();
    std::fs::write(d.path().join(r"src\scan.rs"), "needle\n").unwrap();
    std::fs::write(d.path().join(r"src\deep\lib.rs"), "// needle\n").unwrap();
    d
}

/// Поиск сразу в панель, дождаться конца поиска и листинга.
fn to_panel(core: &mut Core, name: &str) -> SearchId {
    core.pump(Duration::from_millis(4));
    core.dispatch(
        Command::new("search.run")
            .arg("name", name)
            .arg("target", "panel"),
    );
    core.pump(Duration::from_millis(4));
    let id = svfs::parse(&core.state().active_tab().loc)
        .expect("tab on search:")
        .id;
    pump_until(core, "search done", |c| {
        model(c).get(id).is_some_and(|s| s.is_done())
            && c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    pump_until(core, "listing", |c| tab_names(c).is_some());
    id
}

#[test]
fn panel_lists_files_and_refresh_drops_a_file_deleted_outside() {
    let d = fixture();
    let mut core = core(d.path());
    to_panel(&mut core, "*.rs");
    assert_eq!(
        tab_names(&core).unwrap(),
        [r"src\deep\lib.rs", r"src\scan.rs"]
    );

    std::fs::remove_file(d.path().join(r"src\deep\lib.rs")).unwrap();
    core.dispatch(Command::new("panel.refresh"));
    pump_until(&mut core, "file gone", |c| {
        tab_names(c).is_some_and(|v| v == [r"src\scan.rs"])
    });

    // Повторный refresh ничего не возвращает.
    std::thread::sleep(Duration::from_millis(300));
    core.dispatch(Command::new("panel.refresh"));
    pump_until(&mut core, "listing", |c| tab_names(c).is_some());
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(300) {
        core.pump(Duration::from_millis(4));
    }
    assert_eq!(tab_names(&core).unwrap(), [r"src\scan.rs"]);
}

#[test]
fn closed_tabs_free_stores_beyond_eight_recent() {
    let d = fixture();
    let mut core = core(d.path());
    let ids: Vec<SearchId> = (0..9).map(|_| to_panel(&mut core, "*.rs")).collect();
    // Вкладка держит стор.
    for _ in 0..30 {
        core.pump(Duration::from_millis(4));
    }
    assert!(ids.iter().all(|&id| model(&core).get(id).is_some()));

    for _ in &ids {
        core.dispatch(Command::new("tab.close"));
    }
    pump_until(&mut core, "oldest released", |c| {
        model(c).get(ids[0]).is_none()
    });
    for _ in 0..30 {
        core.pump(Duration::from_millis(4));
    }
    assert!(
        ids[1..].iter().all(|&id| model(&core).get(id).is_some()),
        "восемь недавних живут"
    );
}

#[test]
fn errors_command_opens_the_errors_view() {
    let d = fixture();
    let mut core = core(d.path());
    let id = to_panel(&mut core, "*.rs");
    core.dispatch(Command::new("search.errors").arg("id", id.0));
    core.pump(Duration::from_millis(4));
    let a = svfs::parse(&core.state().active_tab().loc).unwrap();
    assert_eq!((a.id, a.view), (id, View::Errors));
    pump_until(&mut core, "errors listing", |c| tab_names(c).is_some());
    assert_eq!(tab_names(&core).unwrap(), [] as [String; 0]);
    // Вид «без доступа» тоже держит стор.
    for _ in 0..30 {
        core.pump(Duration::from_millis(4));
    }
    assert!(model(&core).get(id).is_some());

    // Нет такого поиска — панель не трогаем.
    let before = core.state().active_tab().loc.clone();
    core.dispatch(Command::new("search.errors").arg("id", 999));
    core.pump(Duration::from_millis(4));
    assert_eq!(core.state().active_tab().loc, before);
}

#[test]
fn hub_does_not_poll_the_scheme() {
    let d = fixture();
    let (mut core, lists) = core_spy(d.path());
    to_panel(&mut core, "*.rs");
    let n = lists.load(Ordering::Relaxed);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(400) {
        core.pump(Duration::from_millis(4));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(lists.load(Ordering::Relaxed), n, "листинг не повторялся");
}

/// Заместитель вьювера: запоминает, что открыли.
struct Opener(std::sync::mpsc::Sender<Command>);

const OPEN_ARGS: [urfin_core::ArgSpec; 2] = [
    urfin_core::ArgSpec {
        name: "path",
        kind: urfin_core::ValueKind::Loc,
        required: true,
    },
    urfin_core::ArgSpec {
        name: "line",
        kind: urfin_core::ValueKind::Int,
        required: false,
    },
];
static VIEWER: [urfin_core::CommandMeta; 1] = [urfin_core::CommandMeta {
    id: "viewer.open",
    title: "",
    category: "",
    args: &OPEN_ARGS,
    repeatable: false,
}];

impl urfin_core::Handler for Opener {
    fn namespace(&self) -> &'static str {
        "viewer"
    }
    fn commands(&self) -> &'static [urfin_core::CommandMeta] {
        &VIEWER
    }
    fn handle(
        &mut self,
        cmd: &Command,
        _cx: &mut urfin_core::Cx<'_>,
    ) -> Result<(), urfin_core::CmdError> {
        self.0.send(cmd.clone()).unwrap();
        Ok(())
    }
}

/// S8-11: запрос — окну (`search.panel`), F3 и Ctrl+PgUp панели — на настоящий файл.
#[test]
fn panel_row_opens_its_line_and_goes_to_its_folder() {
    let d = fixture();
    let mut core = core(d.path());
    let (tx, opened) = std::sync::mpsc::channel();
    core.register(Box::new(Opener(tx)));
    core.pump(Duration::from_millis(4));
    core.take_ui_events();
    core.dispatch(
        Command::new("search.run")
            .arg("name", "*.rs")
            .arg("text", "needle")
            .arg("target", "panel"),
    );
    core.pump(Duration::from_millis(4));
    let id = svfs::parse(&core.state().active_tab().loc).unwrap().id;
    let label = core.take_ui_events().into_iter().find_map(|e| match e {
        urfin_core::UiEvent::Ext { id, args } if id.as_str() == "search.panel" => Some(args),
        _ => None,
    });
    let label = label.expect("запрос панели — окну");
    assert_eq!(label.int("id"), Some(i64::from(id.0)));
    assert_eq!(label.str("text"), Some("needle"));
    assert_eq!(label.str("name"), Some("*.rs"));
    assert_eq!(label.str("kinds"), Some("files"));
    pump_until(&mut core, "search done", |c| {
        model(c).get(id).is_some_and(|s| s.is_done())
    });
    pump_until(&mut core, "listing", |c| tab_names(c).is_some());
    core.dispatch(Command::new("panel.cursor.to").arg("name", r"src\scan.rs"));
    core.pump(Duration::from_millis(4));

    core.dispatch(Command::new("search.view"));
    core.pump(Duration::from_millis(4));
    let c = opened.try_recv().expect("viewer.open");
    assert_eq!(
        c.args.loc("path"),
        Some(Location::local(d.path().join(r"src\scan.rs")))
    );
    assert_eq!(c.args.int("line"), Some(1));

    core.dispatch(Command::new("search.goto"));
    let folder = Location::local(d.path().join("src"));
    pump_until(&mut core, "panel in the folder", |c| {
        let t = c.state().active_tab();
        t.loc == folder && !t.listing.entries.is_empty() && t.pending_focus.is_none()
    });
    let Some(urfin_core::Row::Entry { entry, .. }) = core.state().active_tab().current() else {
        panic!("курсор не на файле");
    };
    assert_eq!(entry.name, "scan.rs");
}
