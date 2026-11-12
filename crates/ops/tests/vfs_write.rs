//! Операции через схему с записью (F03-16) на подставной схеме `mem:` (файлы в памяти):
//! `ops.retry` копирует только упавшее, `ops.attrs` и `ops.touch` меняют права и время
//! через `Vfs::set_meta`, конфликт имени решается «новым именем» и «старое — в .bak».

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_core::{
    Attrs, Boot, CancelToken, Command, Core, Entry, EntryKind, JobId, JobState, Location, Services,
    Waker,
};
use urfin_ops::{ErrorPolicy, IssueState, JobIssues, OpOptions, OpsConfig};
use urfin_vfs::{
    Caps, ListOpts, ListStream, Metadata, Space, Vfs, VfsCfg, VfsError, VfsListing, VfsPath,
    VfsRead, VfsRegistry, VfsWrite, WatchGuard, WatchTx,
};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

#[derive(Clone, Debug, PartialEq)]
struct Node {
    dir: bool,
    data: Vec<u8>,
    mtime: i64,
    mode: u32,
}

/// Файлы схемы `mem:` по пути (`/dst/a.txt`), запреты записи и журнал записанного.
#[derive(Default)]
struct Store {
    nodes: Mutex<BTreeMap<String, Node>>,
    /// Имена файлов, запись которых кончается «нет доступа».
    deny: Mutex<HashSet<String>>,
    /// Пути файлов, записанных до конца (`finish`).
    written: Mutex<Vec<String>>,
}

impl Store {
    fn dir(&self, path: &str) {
        let node = Node {
            dir: true,
            data: Vec::new(),
            mtime: 0,
            mode: 0o755,
        };
        self.nodes.lock().unwrap().insert(path.to_owned(), node);
    }

    fn file(&self, path: &str, data: &[u8]) {
        let node = Node {
            dir: false,
            data: data.to_vec(),
            mtime: 0,
            mode: 0o644,
        };
        self.nodes.lock().unwrap().insert(path.to_owned(), node);
    }

    fn get(&self, path: &str) -> Option<Node> {
        self.nodes.lock().unwrap().get(path).cloned()
    }

    fn text(&self, path: &str) -> Option<String> {
        self.get(path)
            .map(|n| String::from_utf8_lossy(&n.data).into_owned())
    }

    fn mode(&self, path: &str) -> u32 {
        self.get(path).map_or(0, |n| n.mode)
    }

    fn names(&self) -> Vec<String> {
        self.nodes.lock().unwrap().keys().cloned().collect()
    }
}

struct Mem(Arc<Store>);

fn key(p: &VfsPath) -> String {
    let s = p.path.to_string_lossy().replace('\\', "/");
    s.trim_end_matches('/').to_owned()
}

fn name_of(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

fn exists() -> VfsError {
    VfsError::Io {
        path: PathBuf::new(),
        source: io::ErrorKind::AlreadyExists.into(),
    }
}

fn entry(name: &str, n: &Node) -> Entry {
    Entry {
        name: name.into(),
        size: n.data.len() as u64,
        mtime: n.mtime,
        ctime: i64::MIN,
        atime: i64::MIN,
        attrs: Attrs::default(),
        kind: if n.dir {
            EntryKind::Dir
        } else {
            EntryKind::File
        },
    }
}

struct Ready(Option<Vec<Entry>>);
impl ListStream for Ready {
    fn next_batch(&mut self, out: &mut Vec<Entry>, _: &CancelToken) -> Result<bool, VfsError> {
        out.extend(self.0.take().unwrap_or_default());
        Ok(false)
    }
}

struct MemRead(Cursor<Vec<u8>>);
impl Read for MemRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}
impl Seek for MemRead {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}
impl VfsRead for MemRead {
    fn len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

struct MemWrite {
    store: Arc<Store>,
    key: String,
    buf: Vec<u8>,
}
impl Write for MemWrite {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl VfsWrite for MemWrite {
    fn finish(self: Box<Self>) -> Result<(), VfsError> {
        self.store.file(&self.key, &self.buf);
        self.store.written.lock().unwrap().push(self.key.clone());
        Ok(())
    }
}

impl Vfs for Mem {
    fn scheme(&self) -> &'static str {
        "mem"
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        Caps::default()
    }
    fn list(&self, dir: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        let prefix = format!("{}/", key(dir));
        let nodes = self.0.nodes.lock().unwrap();
        let list = nodes
            .iter()
            .filter_map(|(k, n)| {
                let rest = k.strip_prefix(&prefix)?;
                (!rest.is_empty() && !rest.contains('/')).then(|| entry(rest, n))
            })
            .collect();
        Ok(Box::new(Ready(Some(list))))
    }
    fn stat(&self, p: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        let n = self
            .0
            .get(&key(p))
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        Ok(Metadata {
            kind: if n.dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            attrs: Attrs::default(),
            size: n.data.len() as u64,
            alloc_size: None,
            mtime: n.mtime,
            atime: 0,
            ctime: None,
            btime: None,
            mode: Some(n.mode),
            uid: None,
            gid: None,
            nlink: None,
            file_id: None,
            reparse_tag: None,
            link_target: None,
        })
    }
    fn stat_entry(&self, d: &VfsPath, name: &str, _: Attrs) -> Result<Entry, VfsError> {
        self.0
            .get(&format!("{}/{name}", key(d)))
            .map(|n| entry(name, &n))
            .ok_or_else(|| VfsError::NotFound(d.path.clone()))
    }
    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        let n = self
            .0
            .get(&key(p))
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        Ok(Box::new(MemRead(Cursor::new(n.data))))
    }
    fn space(&self, _: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("space"))
    }
    fn local_path(&self, _: &VfsPath) -> Option<PathBuf> {
        None
    }
    fn watch(&self, _: &VfsPath, _: WatchTx) -> Result<WatchGuard, VfsError> {
        Ok(WatchGuard::new(()))
    }

    fn open_write(&self, p: &VfsPath, from: Option<u64>) -> Result<Box<dyn VfsWrite>, VfsError> {
        let k = key(p);
        let name = name_of(&k);
        let name = name.strip_suffix(".urfin-part").unwrap_or(name);
        if self.0.deny.lock().unwrap().contains(name) {
            return Err(VfsError::Denied(p.path.clone()));
        }
        let mut buf = match from {
            Some(_) => self.0.get(&k).map(|n| n.data).unwrap_or_default(),
            None => Vec::new(),
        };
        if let Some(n) = from {
            buf.truncate(usize::try_from(n).unwrap());
        }
        Ok(Box::new(MemWrite {
            store: Arc::clone(&self.0),
            key: k,
            buf,
        }))
    }
    fn mkdir(&self, p: &VfsPath) -> Result<(), VfsError> {
        let k = key(p);
        if self.0.get(&k).is_some() {
            return Err(exists());
        }
        self.0.dir(&k);
        Ok(())
    }
    fn remove(&self, p: &VfsPath) -> Result<(), VfsError> {
        let gone = self.0.nodes.lock().unwrap().remove(&key(p));
        gone.map(|_| ())
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))
    }
    fn rename(&self, from: &VfsPath, to: &VfsPath, replace: bool) -> Result<(), VfsError> {
        let mut nodes = self.0.nodes.lock().unwrap();
        let (a, b) = (key(from), key(to));
        if !replace && nodes.contains_key(&b) {
            return Err(exists());
        }
        let n = nodes
            .remove(&a)
            .ok_or_else(|| VfsError::NotFound(from.path.clone()))?;
        nodes.insert(b, n);
        Ok(())
    }
    fn set_meta(&self, p: &VfsPath, mode: Option<u32>, mtime: Option<i64>) -> Result<(), VfsError> {
        let mut nodes = self.0.nodes.lock().unwrap();
        let n = nodes
            .get_mut(&key(p))
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        if let Some(m) = mode {
            n.mode = m;
        }
        if let Some(t) = mtime {
            n.mtime = t;
        }
        Ok(())
    }
}

/// Ядро с панелью на `left` и схемой `mem:`: папка `/dst` и файлы `files` в ней (до
/// первого листинга панели).
fn setup(left: Location, cfg: OpsConfig, files: &[(&str, &[u8])]) -> (Core, Arc<Store>) {
    let store = Arc::new(Store::default());
    store.dir("/dst");
    for (name, data) in files {
        store.file(&format!("/dst/{name}"), data);
    }
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(Mem(Arc::clone(&store))));
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(Boot::new(left.clone(), left), services, Arc::new(NoWake));
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    urfin_ops::register(&mut core, cfg);
    (core, store)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn cursor(core: &mut Core, name: &str) {
    pump_until(core, "listed", |c| {
        let tab = c.state().active_tab();
        tab.listing.entries.iter().any(|e| e.name == name)
    });
    core.dispatch(Command::new("panel.cursor.to").arg("name", name));
    core.pump(Duration::from_millis(2));
}

/// Ждёт `n` завершённых задач копирования; номер и итог последней.
fn copied(core: &mut Core, n: usize) -> (JobId, JobState) {
    let done = |c: &Core| {
        let jobs = c.state().jobs.iter();
        jobs.filter(|j| j.kind == "copy" && !j.state.is_active())
            .count()
    };
    pump_until(core, "copy job ends", |c| done(c) == n);
    let jobs = core.state().jobs.iter();
    let last = jobs.filter(|j| j.kind == "copy").last().unwrap();
    (last.id, last.state.clone())
}

const DEST: &str = "mem://h/dst/";
/// Наносекунд в секунде.
const S: i64 = 1_000_000_000;

/// Приёмка F03-16: повтор копирует только упавшие.
#[test]
fn retry_copies_only_failed_through_scheme() {
    let tmp = tempfile::tempdir().unwrap();
    for n in ["a.txt", "b.txt", "c.txt"] {
        fs::create_dir_all(tmp.path().join("d")).unwrap();
        fs::write(tmp.path().join("d").join(n), n).unwrap();
    }
    let cfg = OpsConfig {
        defaults: OpOptions {
            errors: ErrorPolicy::Skip,
            ..OpOptions::default()
        },
        ..OpsConfig::default()
    };
    let (mut core, store) = setup(Location::local(tmp.path()), cfg, &[]);
    store.deny.lock().unwrap().insert("b.txt".to_owned());
    cursor(&mut core, "d");
    core.dispatch(Command::new("ops.copy").arg("dest", DEST));
    let (first, st) = copied(&mut core, 1);
    assert!(
        matches!(&st, JobState::Done(s) if s.errors == 1 && s.items >= 2),
        "{st:?}"
    );
    assert_eq!(store.text("/dst/d/a.txt").as_deref(), Some("a.txt"));
    assert_eq!(store.text("/dst/d/c.txt").as_deref(), Some("c.txt"));
    assert_eq!(store.get("/dst/d/b.txt"), None);
    // Ошибка — в списке проблем, с пометкой «можно повторить».
    let job = i64::try_from(first.0).unwrap();
    core.dispatch(Command::new("ops.issues").arg("job", job));
    core.pump(Duration::from_millis(2));
    let list = core.state().ext.get::<JobIssues>().expect("issues").clone();
    assert_eq!(list.job, first);
    let rows: Vec<_> = list
        .rows
        .iter()
        .map(|r| {
            (
                r.state,
                r.path.to_string_lossy().replace('\\', "/"),
                r.retry,
            )
        })
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].0, rows[0].2), (IssueState::Failed, true));
    assert!(rows[0].1.ends_with("d/b.txt"), "{rows:?}");

    // Причина устранена: повтор — новая задача только из упавшего.
    store.deny.lock().unwrap().clear();
    store.written.lock().unwrap().clear();
    core.dispatch(Command::new("ops.retry").arg("id", job));
    let (second, st) = copied(&mut core, 2);
    assert_ne!(second, first);
    assert!(
        matches!(&st, JobState::Done(s) if s.errors == 0 && s.items == 1),
        "{st:?}"
    );
    assert_eq!(
        *store.written.lock().unwrap(),
        ["/dst/d/b.txt.urfin-part"],
        "only the failed file is written again"
    );
    for n in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(store.text(&format!("/dst/d/{n}")).as_deref(), Some(n));
    }
    assert_eq!(
        store.names(),
        [
            "/dst",
            "/dst/d",
            "/dst/d/a.txt",
            "/dst/d/b.txt",
            "/dst/d/c.txt"
        ],
        "no part files left"
    );
    // У задачи без ошибок повторять нечего: новой задачи нет.
    core.dispatch(Command::new("ops.retry").arg("id", i64::try_from(second.0).unwrap()));
    core.pump(Duration::from_millis(5));
    let copies = core.state().jobs.iter().filter(|j| j.kind == "copy");
    assert_eq!(copies.count(), 2);
}

/// Панель на папке схемы: `left` — `mem://h/dst`.
fn on_scheme(files: &[(&str, &[u8])]) -> (Core, Arc<Store>) {
    let left = Location::parse_uri("mem://h/dst").unwrap();
    setup(left, OpsConfig::default(), files)
}

/// Команда над целью `name` активной панели (курсор — на неё: листинг мог обновиться).
fn on(core: &mut Core, name: &str, cmd: Command) {
    cursor(core, name);
    core.dispatch(cmd);
}

fn wait(core: &mut Core, what: &str, done: impl Fn() -> bool) {
    pump_until(core, what, |_| done());
}

/// Приёмка F03-16: права и время на сервере меняются (`Vfs::set_meta`).
#[test]
fn attrs_and_touch_change_mode_and_time_on_scheme() {
    let (mut core, store) = on_scheme(&[("a.txt", b"x"), ("b.txt", b"x")]);
    // Права целиком.
    on(
        &mut core,
        "a.txt",
        Command::new("ops.attrs").arg("set", "600"),
    );
    wait(&mut core, "chmod 600", || store.mode("/dst/a.txt") == 0o600);
    // Правки, как у chmod: текущие права — из `stat`.
    on(
        &mut core,
        "a.txt",
        Command::new("ops.attrs").arg("set", "u+x,go+r"),
    );
    wait(&mut core, "chmod u+x,go+r", || {
        store.mode("/dst/a.txt") == 0o744
    });
    on(
        &mut core,
        "a.txt",
        Command::new("ops.attrs").arg("clear", "x"),
    );
    wait(&mut core, "chmod a-x", || store.mode("/dst/a.txt") == 0o644);
    assert_eq!(store.mode("/dst/b.txt"), 0o644, "only the target changes");
    assert_eq!(store.get("/dst/a.txt").unwrap().mtime, 0, "time is kept");

    // Время изменения: местное «2026-10-21 18:05» — в пределах суток от UTC, ровно по минуте.
    on(
        &mut core,
        "a.txt",
        Command::new("ops.touch").arg("mtime", "2026-10-21 18:05"),
    );
    wait(&mut core, "touch", || {
        store.get("/dst/a.txt").unwrap().mtime != 0
    });
    let t = store.get("/dst/a.txt").unwrap().mtime;
    assert!(
        (1_792_454_400 * S..1_792_713_600 * S).contains(&t),
        "mtime {t}"
    );
    assert_eq!(t % (60 * S), 0, "mtime {t}");
    assert_eq!(store.mode("/dst/a.txt"), 0o644, "mode is kept");
    assert_eq!(store.get("/dst/b.txt").unwrap().mtime, 0);

    // Выделение: обе цели.
    on(&mut core, "a.txt", Command::new("sel.all"));
    core.pump(Duration::from_millis(2));
    core.dispatch(Command::new("ops.attrs").arg("set", "0640"));
    wait(&mut core, "chmod both", || {
        store.mode("/dst/a.txt") == 0o640 && store.mode("/dst/b.txt") == 0o640
    });
    // Непонятные права — отказ, ничего не меняется.
    core.dispatch(Command::new("ops.attrs").arg("set", "u+q"));
    for _ in 0..20 {
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(store.mode("/dst/a.txt"), 0o640);
}

/// Приёмка F03-16: «новое имя» и «старое — в .bak» на сервере — политикой и ответом на
/// вопрос.
#[test]
fn rename_new_and_backup_on_scheme_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.txt"), b"new").unwrap();
    let files: [(&str, &[u8]); 1] = [("a.txt", b"old")];
    let (mut core, store) = setup(Location::local(tmp.path()), OpsConfig::default(), &files);
    cursor(&mut core, "a.txt");
    let done = |st: &JobState| matches!(st, JobState::Done(s) if s.errors == 0 && s.items == 1);

    // Политика «новое имя»: существующий не тронут, копия — `a (2).txt`.
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", DEST)
            .arg("conflict", "rename"),
    );
    let (_, st) = copied(&mut core, 1);
    assert!(done(&st), "{st:?}");
    assert_eq!(store.text("/dst/a.txt").as_deref(), Some("old"));
    assert_eq!(store.text("/dst/a (2).txt").as_deref(), Some("new"));

    // Политика «старое — в .bak»: существующий — `a.txt.bak~1`, новый — на его месте.
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", DEST)
            .arg("conflict", "rename_existing"),
    );
    let (_, st) = copied(&mut core, 2);
    assert!(done(&st), "{st:?}");
    assert_eq!(store.text("/dst/a.txt").as_deref(), Some("new"));
    assert_eq!(store.text("/dst/a.txt.bak~1").as_deref(), Some("old"));

    // Вопрос (политика по умолчанию) и ответ «Переименовать»: следующее свободное имя.
    fs::write(tmp.path().join("a.txt"), b"third").unwrap();
    core.dispatch(Command::new("ops.copy").arg("dest", DEST));
    pump_until(&mut core, "question", |c| {
        c.state().prompts.active().is_some()
    });
    let prompt = core.state().prompts.active().unwrap().id;
    core.dispatch(
        Command::new("prompt.reply")
            .arg("id", i64::try_from(prompt.0).unwrap())
            .arg("choice", "rename")
            .arg("all", false),
    );
    let (_, st) = copied(&mut core, 3);
    assert!(done(&st), "{st:?}");
    assert_eq!(store.text("/dst/a (3).txt").as_deref(), Some("third"));
    assert_eq!(store.text("/dst/a.txt").as_deref(), Some("new"));
    assert_eq!(
        store.names(),
        [
            "/dst",
            "/dst/a (2).txt",
            "/dst/a (3).txt",
            "/dst/a.txt",
            "/dst/a.txt.bak~1"
        ],
        "no part files left"
    );
}

/// Перенос в пределах сервера (rename) с политикой «новое имя»: без копии данных.
#[test]
fn move_within_scheme_takes_new_name() {
    let (mut core, store) = on_scheme(&[("a.txt", b"x")]);
    store.dir("/dst/sub");
    store.file("/dst/sub/a.txt", b"old");
    cursor(&mut core, "a.txt");
    core.dispatch(
        Command::new("ops.move")
            .arg("dest", "mem://h/dst/sub/")
            .arg("conflict", "rename"),
    );
    pump_until(&mut core, "move job ends", |c| {
        let mut jobs = c.state().jobs.iter();
        jobs.any(|j| j.kind == "move" && !j.state.is_active())
    });
    assert_eq!(store.text("/dst/sub/a.txt").as_deref(), Some("old"));
    assert_eq!(store.text("/dst/sub/a (2).txt").as_deref(), Some("x"));
    assert_eq!(store.get("/dst/a.txt"), None);
    assert!(store.written.lock().unwrap().is_empty(), "rename, no data");
}
