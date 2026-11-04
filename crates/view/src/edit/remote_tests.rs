//! Шаги правки удалённого файла на схеме в памяти (F03-08): скачивание, атомарная
//! закачка с правами, конфликт, ссылка, имена, «ждёт связи», уборка папок.

use std::collections::HashMap;
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use urfin_core::{CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{
    Attrs, Caps, Link, ListOpts, ListStream, Metadata, Space, Vfs, VfsError, VfsPath, VfsRead,
    VfsRegistry, VfsWrite, WatchGuard, WatchTx,
};

use super::*;

#[derive(Clone, Debug)]
struct Node {
    data: Vec<u8>,
    mtime: i64,
    mode: u32,
    link: Option<PathBuf>,
}

/// Схема `mem://srv` в памяти: файлы, ссылки, права, журнал шагов записи.
#[derive(Default)]
struct MemFs {
    files: Mutex<HashMap<String, Node>>,
    log: Mutex<Vec<String>>,
    /// 0 — Up, 1 — Waiting, 2 — Down.
    link: Arc<AtomicU8>,
    /// Ошибка ввода-вывода на ближайшей записи.
    fail_write: Mutex<bool>,
    clock: Mutex<i64>,
}

fn key(p: &VfsPath) -> String {
    p.path.to_string_lossy().replace('\\', "/")
}

fn mem(path: &str) -> Location {
    Location {
        backend: "mem".into(),
        authority: Some("srv".into()),
        path: PathBuf::from(path),
        outer: None,
    }
}

impl MemFs {
    fn put(&self, path: &str, data: &[u8], mode: u32) {
        let mut c = self.clock.lock().unwrap();
        *c += 1_000_000_000;
        self.files.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: data.to_vec(),
                mtime: *c,
                mode,
                link: None,
            },
        );
    }

    fn symlink(&self, path: &str, to: &str) {
        self.files.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: Vec::new(),
                mtime: 0,
                mode: 0o777,
                link: Some(PathBuf::from(to)),
            },
        );
    }

    fn get(&self, path: &str) -> Option<Node> {
        self.files.lock().unwrap().get(path).cloned()
    }

    fn steps(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

struct MemRead(Cursor<Vec<u8>>);

impl io::Read for MemRead {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}

impl io::Seek for MemRead {
    fn seek(&mut self, p: io::SeekFrom) -> io::Result<u64> {
        self.0.seek(p)
    }
}

impl VfsRead for MemRead {
    fn len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

struct MemWrite<'a> {
    fs: &'a MemFs,
    path: String,
    buf: Vec<u8>,
}

impl Write for MemWrite<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if std::mem::take(&mut *self.fs.fail_write.lock().unwrap()) {
            self.fs.link.store(1, Ordering::SeqCst);
            return Err(io::Error::new(io::ErrorKind::ConnectionReset, "link lost"));
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// Писатель живёт не дольше вызова: `'static` даёт копия ссылки на схему в тесте.
struct Owned(MemWrite<'static>);

impl Write for Owned {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl VfsWrite for Owned {
    fn finish(self: Box<Self>) -> Result<(), VfsError> {
        let w = self.0;
        w.fs.log.lock().unwrap().push(format!("finish {}", w.path));
        w.fs.put(&w.path, &w.buf, 0o644);
        Ok(())
    }
}

struct NoList;

impl ListStream for NoList {
    fn next_batch(&mut self, _: &mut Vec<Entry>, _: &CancelToken) -> Result<bool, VfsError> {
        Ok(false)
    }
}

fn meta(n: &Node, kind: EntryKind) -> Metadata {
    Metadata {
        kind,
        attrs: Attrs(0),
        size: n.data.len() as u64,
        alloc_size: None,
        mtime: n.mtime,
        atime: i64::MIN,
        ctime: None,
        btime: None,
        mode: Some(n.mode),
        uid: None,
        gid: None,
        nlink: None,
        file_id: None,
        reparse_tag: None,
        link_target: n.link.clone(),
    }
}

impl Vfs for &'static MemFs {
    fn scheme(&self) -> &'static str {
        "mem"
    }

    fn caps(&self, _: &VfsPath) -> Caps {
        Caps(Caps::SEEK)
    }

    fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Ok(Box::new(NoList))
    }

    fn stat(&self, p: &VfsPath, follow: bool) -> Result<Metadata, VfsError> {
        let mut k = key(p);
        for _ in 0..8 {
            let n = self
                .get(&k)
                .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
            match (&n.link, follow) {
                (Some(t), true) => {
                    let base = Path::new(&k).parent().unwrap().to_path_buf();
                    k = super::normalize(&base.join(t))
                        .to_string_lossy()
                        .replace('\\', "/");
                }
                (Some(_), false) => return Ok(meta(&n, EntryKind::LinkFile)),
                (None, _) => return Ok(meta(&n, EntryKind::File)),
            }
        }
        Err(VfsError::Unsupported("loop"))
    }

    fn stat_entry(&self, _: &VfsPath, n: &str, _: Attrs) -> Result<Entry, VfsError> {
        Err(VfsError::NotFound(n.into()))
    }

    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        let n = self
            .get(&key(p))
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        Ok(Box::new(MemRead(Cursor::new(n.data))))
    }

    fn space(&self, _: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("mem"))
    }

    fn local_path(&self, _: &VfsPath) -> Option<PathBuf> {
        None
    }

    fn watch(&self, _: &VfsPath, _: WatchTx) -> Result<WatchGuard, VfsError> {
        Err(VfsError::Unsupported("mem"))
    }

    fn open_write(&self, p: &VfsPath, from: Option<u64>) -> Result<Box<dyn VfsWrite>, VfsError> {
        assert_eq!(from, None, "копия пишется целиком");
        if self.link.load(Ordering::SeqCst) != 0 {
            return Err(VfsError::Io {
                path: p.path.clone(),
                source: io::Error::new(io::ErrorKind::NotConnected, "no link"),
            });
        }
        self.log.lock().unwrap().push(format!("open {}", key(p)));
        let fs: &'static MemFs = self;
        Ok(Box::new(Owned(MemWrite {
            fs,
            path: key(p),
            buf: Vec::new(),
        })))
    }

    fn remove(&self, p: &VfsPath) -> Result<(), VfsError> {
        self.log.lock().unwrap().push(format!("remove {}", key(p)));
        self.files.lock().unwrap().remove(&key(p));
        Ok(())
    }

    fn rename(&self, from: &VfsPath, to: &VfsPath, replace: bool) -> Result<(), VfsError> {
        assert!(replace);
        self.log
            .lock()
            .unwrap()
            .push(format!("rename {} {}", key(from), key(to)));
        let mut f = self.files.lock().unwrap();
        let n = f.remove(&key(from)).unwrap();
        f.insert(key(to), n);
        Ok(())
    }

    fn set_meta(&self, p: &VfsPath, mode: Option<u32>, _: Option<i64>) -> Result<(), VfsError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("mode {} {:o}", key(p), mode.unwrap_or(0)));
        if let (Some(m), Some(n)) = (mode, self.files.lock().unwrap().get_mut(&key(p))) {
            n.mode = m;
        }
        Ok(())
    }

    fn link(&self, _: &VfsPath) -> Link {
        match self.link.load(Ordering::SeqCst) {
            0 => Link::Up,
            1 => Link::Waiting,
            _ => Link::Down,
        }
    }
}

fn fs() -> &'static MemFs {
    Box::leak(Box::default())
}

fn none(_: u64, _: u64) {}

#[test]
fn fetch_then_upload_is_atomic_and_keeps_mode() {
    let fs = fs();
    fs.put("/etc/nginx/app.conf", b"listen 80;\n", 0o640);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("copy").join("app.conf");
    let loc = mem("/etc/nginx/app.conf");
    let t = CancelToken::new();
    let f = fetch(&fs, &loc, &file, &t, &mut none).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"listen 80;\n");
    assert_eq!(f.mode, Some(0o640));
    std::fs::write(&file, b"listen 443;\n").unwrap();
    let req = UpReq {
        local: file.clone(),
        target: f.target.clone(),
        expect: Some(Some(f.stamp)),
        mode: f.mode,
    };
    let out = upload(&fs, &req, &t, &mut |_| {}).unwrap();
    let n = fs.get("/etc/nginx/app.conf").unwrap();
    assert_eq!(n.data, b"listen 443;\n");
    assert_eq!(n.mode, 0o640, "права файла сохранены");
    assert!(fs.get("/etc/nginx/app.conf.urfin-part").is_none());
    assert_eq!(
        fs.steps(),
        [
            "open /etc/nginx/app.conf.urfin-part",
            "finish /etc/nginx/app.conf.urfin-part",
            "mode /etc/nginx/app.conf.urfin-part 640",
            "rename /etc/nginx/app.conf.urfin-part /etc/nginx/app.conf",
        ],
        "part, finish, права, rename поверх"
    );
    let UpOutcome::Done { stamp, bytes } = out else {
        panic!("{out:?}");
    };
    assert_eq!(bytes, 12);
    assert_eq!(stamp.size, 12);
}

#[test]
fn server_change_is_a_conflict_and_writes_nothing() {
    let fs = fs();
    fs.put("/a.txt", b"one", 0o644);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let t = CancelToken::new();
    let f = fetch(&fs, &mem("/a.txt"), &file, &t, &mut none).unwrap();
    // Кто-то правит файл на сервере, пока копия открыта.
    fs.put("/a.txt", b"two!", 0o644);
    let req = UpReq {
        local: file.clone(),
        target: f.target.clone(),
        expect: Some(Some(f.stamp)),
        mode: f.mode,
    };
    assert_eq!(
        upload(&fs, &req, &t, &mut |_| {}).unwrap(),
        UpOutcome::Changed
    );
    assert!(fs.steps().is_empty(), "ничего не записано");
    // Удалён на сервере — тоже конфликт.
    fs.files.lock().unwrap().remove("/a.txt");
    assert_eq!(
        upload(&fs, &req, &t, &mut |_| {}).unwrap(),
        UpOutcome::Changed
    );
    // «Перезаписать» — без сверки.
    let force = UpReq {
        expect: None,
        ..req.clone()
    };
    assert!(matches!(
        upload(&fs, &force, &t, &mut |_| {}).unwrap(),
        UpOutcome::Done { .. }
    ));
    assert_eq!(fs.get("/a.txt").unwrap().data, b"one");
}

#[test]
fn new_file_expects_nothing_on_server() {
    let fs = fs();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("n.txt");
    std::fs::write(&file, b"new").unwrap();
    let req = UpReq {
        local: file,
        target: mem("/n.txt"),
        expect: Some(None),
        mode: None,
    };
    let t = CancelToken::new();
    assert!(matches!(
        upload(&fs, &req, &t, &mut |_| {}).unwrap(),
        UpOutcome::Done { .. }
    ));
    // Теперь файл есть — второй «новый» уже конфликт.
    assert_eq!(
        upload(&fs, &req, &t, &mut |_| {}).unwrap(),
        UpOutcome::Changed
    );
}

#[test]
fn link_is_kept_target_is_written() {
    let fs = fs();
    fs.put("/etc/nginx/sites-available/app.conf", b"x", 0o600);
    fs.symlink(
        "/etc/nginx/sites-enabled/app.conf",
        "../sites-available/app.conf",
    );
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("app.conf");
    let t = CancelToken::new();
    let f = fetch(
        &fs,
        &mem("/etc/nginx/sites-enabled/app.conf"),
        &file,
        &t,
        &mut none,
    )
    .unwrap();
    assert_eq!(
        f.target.path,
        PathBuf::from("/etc/nginx/sites-available/app.conf")
    );
    assert_eq!(f.mode, Some(0o600));
    std::fs::write(&file, b"y").unwrap();
    let req = UpReq {
        local: file,
        target: f.target.clone(),
        expect: Some(Some(f.stamp)),
        mode: f.mode,
    };
    upload(&fs, &req, &t, &mut |_| {}).unwrap();
    assert!(
        fs.get("/etc/nginx/sites-enabled/app.conf")
            .unwrap()
            .link
            .is_some(),
        "ссылка осталась ссылкой"
    );
    assert_eq!(
        fs.get("/etc/nginx/sites-available/app.conf").unwrap().data,
        b"y"
    );
}

#[test]
fn broken_link_mid_write_removes_nothing_and_waits() {
    let fs = fs();
    fs.put("/a.txt", b"one", 0o644);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let t = CancelToken::new();
    let f = fetch(&fs, &mem("/a.txt"), &file, &t, &mut none).unwrap();
    std::fs::write(&file, b"two").unwrap();
    *fs.fail_write.lock().unwrap() = true;
    let req = UpReq {
        local: file,
        target: f.target.clone(),
        expect: Some(Some(f.stamp)),
        mode: f.mode,
    };
    let e = upload(&fs, &req, &t, &mut |_| {}).unwrap_err();
    assert!(maybe_net(&e), "{e:?}");
    assert!(
        !fs.steps().iter().any(|s| s.starts_with("remove")),
        "без связи part не трогаем"
    );
    // Сеанс переподключается и возвращается через 300 мс.
    let flag = Arc::clone(&fs.link);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(0, Ordering::SeqCst);
    });
    let mut waited = false;
    assert!(wait_link(&fs, &req.target, &t, &mut || waited = true));
    assert!(waited, "строка «ждёт связи»");
    assert!(matches!(
        upload(&fs, &req, &t, &mut |_| {}).unwrap(),
        UpOutcome::Done { .. }
    ));
    assert_eq!(fs.get("/a.txt").unwrap().data, b"two");
    // Отключено совсем — ждать нечего.
    fs.link.store(2, Ordering::SeqCst);
    assert!(!wait_link(&fs, &req.target, &t, &mut || {}));
}

#[test]
fn cyrillic_and_windows_unsafe_names() {
    assert_eq!(local_name("отчёт 2026.txt"), "отчёт 2026.txt");
    assert_eq!(local_name("a:b*c?.conf"), "a_b_c_.conf");
    assert_eq!(local_name("con.txt"), "_con.txt");
    assert_eq!(local_name("COM1"), "_COM1");
    assert_eq!(local_name("COMX"), "COMX");
    assert_eq!(local_name("trail. "), "trail");
    let fs = fs();
    fs.put("/home/иван/заметки.md", "Привет".as_bytes(), 0o600);
    let root = tempfile::tempdir().unwrap();
    let loc = mem("/home/иван/заметки.md");
    let d = temp_dir(root.path(), &loc);
    assert!(d.starts_with(root.path().join("mem").join("srv")));
    assert_eq!(d, temp_dir(root.path(), &loc), "папка — от адреса");
    assert_ne!(d, temp_dir(root.path(), &mem("/home/иван/другое.md")));
    let file = d.join(local_name("заметки.md"));
    let t = CancelToken::new();
    fetch(&fs, &loc, &file, &t, &mut none).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "Привет");
}

#[test]
fn editable_needs_a_writable_scheme() {
    let mut reg = VfsRegistry::with_local();
    assert!(!editable(&reg, &mem("/a")), "схемы нет");
    reg.register(Arc::new(fs()));
    assert!(editable(&reg, &mem("/a")));
    assert!(!editable(&reg, &Location::local(std::env::temp_dir())));
    let inner = Location {
        outer: Some(Arc::new(mem("/a.zip"))),
        ..mem("/x")
    };
    assert!(!editable(&reg, &inner), "внутри архива");
    assert_eq!(label_of(&mem("/a")).as_deref(), Some("srv"));
    assert_eq!(label_of(&Location::local(std::env::temp_dir())), None);
}

#[test]
fn cancel_stops_fetch() {
    let fs = fs();
    fs.put("/big", &vec![7u8; 4 << 20], 0o644);
    let dir = tempfile::tempdir().unwrap();
    let t = CancelToken::new();
    t.cancel();
    let e = fetch(&fs, &mem("/big"), &dir.path().join("big"), &t, &mut none).unwrap_err();
    assert!(matches!(e, VfsError::Cancelled), "{e:?}");
}

#[test]
fn sweep_removes_only_stale_copies() {
    let root = tempfile::tempdir().unwrap();
    let old = root
        .path()
        .join("sftp")
        .join("srv1")
        .join("0000000000000001");
    let fresh = root
        .path()
        .join("sftp")
        .join("srv1")
        .join("0000000000000002");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&fresh).unwrap();
    std::fs::write(old.join("a.conf"), b"a").unwrap();
    std::fs::write(fresh.join("b.conf"), b"b").unwrap();
    let past = std::time::SystemTime::now() - Duration::from_hours(8 * 24);
    for p in [old.join("a.conf"), old.clone()] {
        let f = std::fs::OpenOptions::new()
            .write(p.is_file())
            .read(true)
            .custom_flags_for_dirs()
            .open(&p)
            .unwrap();
        f.set_modified(past).unwrap();
    }
    assert_eq!(sweep(root.path(), KEEP), 1);
    assert!(!old.exists());
    assert!(fresh.join("b.conf").exists());
    remove_dir(&fresh);
    assert!(!fresh.exists());
}

/// Каталог открывается с `FILE_FLAG_BACKUP_SEMANTICS` (иначе Windows не даст handle).
trait DirOpen {
    fn custom_flags_for_dirs(&mut self) -> &mut Self;
}

impl DirOpen for std::fs::OpenOptions {
    fn custom_flags_for_dirs(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            // FILE_FLAG_BACKUP_SEMANTICS; FILE_WRITE_ATTRIBUTES — для времени.
            self.custom_flags(0x0200_0000).access_mode(0x100 | 0x80);
        }
        self
    }
}
