//! Вложенные архивы и спилл (S8-31, S8 §3.4): zip → tar.gz, stored-запись на месте,
//! спилл сжатой записи и его удаление с индексом, вопрос перед бомбой, глубина ≤ 8,
//! наблюдение за файлом внизу цепочки, архив на сервере, rar в zip.
//!
//! Фикстуры собираются здесь крейтами `zip`, `tar` и `flate2`; спилл — только во
//! временный каталог теста.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_arc::{ArcCache, ArcCfg, Ask, Over, Question, SpillQuestion, open_resolver, root_of};
use urfin_core::{Args, Attrs, CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{
    Caps, ListOpts, ListStream, LocalFs, Metadata, Space, Vfs, VfsError, VfsPath, VfsRead,
    VfsRegistry, WatchEvent, WatchGuard, WatchId, WatchTx,
};
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const MIB: u64 = 1 << 20;

/// «Сервер» для теста: схема `remote` с файлами локального диска (как `sftp://`).
struct Remote;

fn on_disk(p: &VfsPath) -> Location {
    Location::local(p.path.clone())
}

impl Vfs for Remote {
    fn scheme(&self) -> &'static str {
        "remote"
    }

    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps(Caps::SEEK)
    }

    fn list(&self, _dir: &VfsPath, _opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Err(VfsError::Unsupported("test"))
    }

    fn stat(&self, p: &VfsPath, follow: bool) -> Result<Metadata, VfsError> {
        LocalFs::new().stat(&on_disk(p), follow)
    }

    fn stat_entry(&self, _dir: &VfsPath, _name: &str, _hint: Attrs) -> Result<Entry, VfsError> {
        Err(VfsError::Unsupported("test"))
    }

    fn open_read(&self, p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        LocalFs::new().open_read(&on_disk(p))
    }

    fn space(&self, _at: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("test"))
    }

    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }

    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Err(VfsError::Unsupported("test"))
    }
}

/// Отвечает на вопрос спилла `go` и запоминает вопросы.
struct Answer {
    go: bool,
    asked: Mutex<Vec<SpillQuestion>>,
}

impl Answer {
    fn new(go: bool) -> Arc<Answer> {
        Arc::new(Answer {
            go,
            asked: Mutex::default(),
        })
    }

    fn asked(&self) -> Vec<SpillQuestion> {
        self.asked.lock().unwrap().clone()
    }
}

impl Ask for Answer {
    fn ask(&self, _q: &Question, _cancel: &CancelToken) -> Option<Zeroizing<String>> {
        None
    }

    fn spill(&self, q: &SpillQuestion, _cancel: &CancelToken) -> bool {
        self.asked.lock().unwrap().push(q.clone());
        self.go
    }
}

/// Каталог теста, кэш и реестр, как у приложения: локальная ФС, все форматы, «сервер».
struct Env {
    tmp: tempfile::TempDir,
    cache: Arc<ArcCache>,
    reg: Arc<VfsRegistry>,
}

impl Env {
    fn new(limit: u64) -> Env {
        urfin_i18n::set_language("en");
        let tmp = tempfile::Builder::new()
            .prefix("urfin-arc-nested-")
            .tempdir()
            .unwrap();
        let cache = ArcCache::new(limit);
        let mut reg = VfsRegistry::with_local();
        for b in urfin_arc::backends(&cache, &ArcCfg::default()) {
            reg.register(b);
        }
        reg.register(Arc::new(Remote));
        let reg = Arc::new(reg);
        cache.set_vfs(&reg);
        cache.set_spill_dir(tmp.path().join("spill"));
        Env { tmp, cache, reg }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    /// Файлы спилла, без недописанных.
    fn spilled(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.path("spill")) else {
            return Vec::new();
        };
        rd.flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn vfs(&self, at: &Location) -> &Arc<dyn Vfs> {
        self.reg.get(at).unwrap()
    }

    fn list(&self, dir: &Location) -> Result<Vec<String>, VfsError> {
        let mut s = self.vfs(dir).list(dir, &ListOpts::default())?;
        let mut out = Vec::new();
        let cancel = CancelToken::new();
        while s.next_batch(&mut out, &cancel)? {}
        let mut names: Vec<String> = out.into_iter().map(|e| e.name.to_string()).collect();
        names.sort();
        Ok(names)
    }

    fn read(&self, p: &Location) -> Vec<u8> {
        let mut out = Vec::new();
        self.vfs(p)
            .open_read(p)
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    }
}

/// Запись `path` архива `outer` (адрес файла внутри).
fn inside(outer: &Location, scheme: &str, path: &str) -> Location {
    Location {
        backend: scheme.into(),
        authority: None,
        path: PathBuf::from(path),
        outer: Some(Arc::new(outer.clone())),
    }
}

fn at(root: &Location, path: &str) -> Location {
    Location {
        path: PathBuf::from(path),
        ..root.clone()
    }
}

fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(1_700_000_000);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, name, *data).unwrap();
    }
    b.into_inner().unwrap()
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn zip_of(entries: &[(&str, CompressionMethod, &[u8])]) -> Vec<u8> {
    let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, method, data) in entries {
        w.start_file(
            *name,
            SimpleFileOptions::default().compression_method(*method),
        )
        .unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

const README: &[u8] = b"inside tar.gz inside zip";

fn inner_tgz(extra: bool) -> Vec<u8> {
    let mut files: Vec<(&str, &[u8])> = vec![("doc/readme.txt", README), ("data.bin", b"0123")];
    if extra {
        files.push(("new.txt", b"new"));
    }
    gz(&tar_of(&files))
}

/// `outer.zip`: `inner.tar.gz` сжатой записью, `stored.tar.gz` — stored, `nested.zip`
/// (stored, внутри — stored `inner.tar.gz`), `zeros.tar` — 4 мебибайта нулей (сжатие около 1000:1).
fn outer_zip(path: &Path, extra: bool) {
    let tgz = inner_tgz(extra);
    let nested = zip_of(&[("inner.tar.gz", CompressionMethod::Stored, &tgz)]);
    let zeros = tar_of(&[("zeros.bin", &vec![0u8; 4 << 20])]);
    let z = zip_of(&[
        ("inner.tar.gz", CompressionMethod::Deflated, &tgz),
        ("stored.tar.gz", CompressionMethod::Stored, &tgz),
        ("nested.zip", CompressionMethod::Stored, &nested),
        ("zeros.tar", CompressionMethod::Deflated, &zeros),
    ]);
    std::fs::write(path, z).unwrap();
}

fn env_with_zip(limit: u64) -> (Env, Location) {
    let env = Env::new(limit);
    let file = env.path("outer.zip");
    outer_zip(&file, false);
    (env, Location::local(file))
}

fn message(e: &VfsError) -> String {
    match e {
        VfsError::Io { source, .. } => source.to_string(),
        e => panic!("ждали ошибку с причиной: {e:?}"),
    }
}

#[test]
fn zip_then_tar_gz_navigates_through_spill() {
    let (env, zip) = env_with_zip(256 * MIB);
    let root = root_of("tar", inside(&zip, "zip", "/inner.tar.gz"));
    assert_eq!(env.list(&root).unwrap(), ["data.bin", "doc"]);
    assert_eq!(env.list(&at(&root, "/doc")).unwrap(), ["readme.txt"]);
    assert_eq!(env.read(&at(&root, "/doc/readme.txt")), README);
    let spilled = env.spilled();
    assert_eq!(spilled.len(), 1, "{spilled:?}");
    assert!(spilled[0].ends_with(".tar.gz"), "{spilled:?}");
}

#[test]
fn stored_entries_are_read_in_place() {
    let (env, zip) = env_with_zip(256 * MIB);
    let root = root_of("tar", inside(&zip, "zip", "/stored.tar.gz"));
    assert_eq!(env.list(&root).unwrap(), ["data.bin", "doc"]);
    assert_eq!(env.read(&at(&root, "/doc/readme.txt")), README);
    // zip → stored zip → stored tar.gz: три архива, ни одного спилла.
    let nested = inside(&zip, "zip", "/nested.zip");
    let deep = root_of("tar", inside(&nested, "zip", "/inner.tar.gz"));
    assert_eq!(env.list(&deep).unwrap(), ["data.bin", "doc"]);
    assert_eq!(env.read(&at(&deep, "/data.bin")), b"0123");
    assert!(env.spilled().is_empty(), "{:?}", env.spilled());
}

#[test]
fn spill_is_removed_when_the_index_leaves_the_cache() {
    // Кэш на один индекс: следующий архив вытесняет предыдущий.
    let (env, zip) = env_with_zip(1);
    let root = root_of("tar", inside(&zip, "zip", "/inner.tar.gz"));
    assert_eq!(env.list(&root).unwrap(), ["data.bin", "doc"]);
    assert_eq!(env.spilled().len(), 1);
    assert_eq!(env.read(&at(&root, "/data.bin")), b"0123");
    env.list(&root_of("zip", zip.clone())).unwrap();
    assert!(env.cache.latest(root.outer.as_deref().unwrap()).is_none());
    assert!(env.spilled().is_empty(), "{:?}", env.spilled());
}

#[test]
fn bomb_stops_at_the_question() {
    let (env, zip) = env_with_zip(256 * MIB);
    env.cache.set_spill_limits(u64::MAX, 50);
    let root = root_of("tar", inside(&zip, "zip", "/zeros.tar"));

    // Спросить некому — отказ с причиной.
    let e = env.list(&root).unwrap_err();
    assert!(message(&e).contains("zip bomb"), "{e}");

    let no = Answer::new(false);
    env.cache.set_asker(Some(no.clone()));
    let e = env.list(&root).unwrap_err();
    assert!(message(&e).contains("zip bomb"), "{e}");
    let asked = no.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].name, "zeros.tar");
    assert!(asked[0].size > 4 * MIB, "{asked:?}");
    assert!(
        matches!(asked[0].over, Over::Ratio(r) if r > 50),
        "{asked:?}"
    );
    // Отказ помнится: второй вход сразу не спрашивает.
    assert!(env.list(&root).is_err());
    assert_eq!(no.asked().len(), 1);
    assert_eq!(env.spilled(), Vec::<String>::new());

    // «Распаковать» — спилл и листинг.
    let (env, zip) = env_with_zip(256 * MIB);
    env.cache.set_spill_limits(MIB, 100_000);
    let yes = Answer::new(true);
    env.cache.set_asker(Some(yes.clone()));
    let root = root_of("tar", inside(&zip, "zip", "/zeros.tar"));
    assert_eq!(env.list(&root).unwrap(), ["zeros.bin"]);
    assert!(matches!(&yes.asked()[..], [q] if q.over == Over::Size(MIB)));
    assert_eq!(env.spilled().len(), 1);
    // Под лимитом — без вопроса.
    let small = root_of("tar", inside(&zip, "zip", "/inner.tar.gz"));
    env.list(&small).unwrap();
    assert_eq!(yes.asked().len(), 1);
}

#[test]
fn deeper_than_eight_is_an_error() {
    let env = Env::new(256 * MIB);
    let mut file = Location::local(env.path("a.zip"));
    for i in 0..8 {
        file = inside(&file, "zip", &format!("/l{i}.zip"));
    }
    // Девять звеньев: восемь архивов внутри файла — девятый уровень.
    let e = env.list(&root_of("zip", file.clone())).unwrap_err();
    assert!(message(&e).contains("deeper than 8"), "{e}");

    // Enter по такому файлу — `arc.enter` (скажет о глубине), а не приложение ОС.
    let r = open_resolver(&ArcCfg::default(), Arc::clone(&env.reg));
    let e = Entry {
        name: "x.zip".into(),
        size: 1,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind: EntryKind::File,
    };
    let dir = root_of("zip", file.outer.as_deref().unwrap().clone());
    let cmd = r.resolve(&dir, &e, &Args::default()).unwrap();
    assert_eq!(cmd.id.as_str(), "arc.enter");
    let shallow = root_of("zip", Location::local(env.path("b.zip")));
    let cmd = r.resolve(&shallow, &e, &Args::default()).unwrap();
    assert_eq!(cmd.id.as_str(), "panel.go");
}

#[test]
fn changed_archive_refreshes_and_removed_closes() {
    let (env, zip) = env_with_zip(256 * MIB);
    let root = root_of("tar", inside(&zip, "zip", "/inner.tar.gz"));
    assert_eq!(env.list(&root).unwrap(), ["data.bin", "doc"]);
    let (tx, rx) = crossbeam_channel::unbounded();
    let guard = env
        .vfs(&root)
        .watch(&root, WatchTx { id: WatchId(3), tx })
        .unwrap();
    let wait = |want: fn(&WatchEvent) -> bool| {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((_, ev)) if want(&ev) => return,
                Ok((_, ev)) => assert!(matches!(ev, WatchEvent::Overflow), "{ev:?}"),
                Err(e) => panic!("нет события: {e}"),
            }
        }
    };
    outer_zip(&zip.path, true);
    wait(|e| matches!(e, WatchEvent::Overflow));
    // Перечитанный листинг проходит цепочку заново: новый индекс, новый спилл.
    assert_eq!(env.list(&root).unwrap(), ["data.bin", "doc", "new.txt"]);
    assert_eq!(env.spilled().len(), 1, "прежний спилл удалён");
    std::fs::remove_file(&zip.path).unwrap();
    wait(|e| matches!(e, WatchEvent::Gone));
    drop(guard);
}

#[test]
fn archive_on_a_server_is_spilled() {
    let env = Env::new(256 * MIB);
    let file = env.path("srv.zip");
    std::fs::write(
        &file,
        zip_of(&[("a.txt", CompressionMethod::Deflated, b"from server")]),
    )
    .unwrap();
    let remote = Location {
        backend: "remote".into(),
        authority: Some("srv".into()),
        path: file,
        outer: None,
    };
    let root = root_of("zip", remote);
    assert_eq!(env.list(&root).unwrap(), ["a.txt"]);
    assert_eq!(env.read(&at(&root, "/a.txt")), b"from server");
    assert_eq!(env.spilled().len(), 1);
}

#[cfg(feature = "rar")]
#[test]
fn rar_inside_zip_opens_from_its_spill() {
    let env = Env::new(256 * MIB);
    let rar =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rar/version.rar"))
            .unwrap();
    let file = env.path("with-rar.zip");
    std::fs::write(&file, zip_of(&[("v.rar", CompressionMethod::Stored, &rar)])).unwrap();
    let root = root_of("rar", inside(&Location::local(file), "zip", "/v.rar"));
    assert_ne!(env.list(&root).unwrap(), Vec::<String>::new());
    let spilled = env.spilled();
    assert!(
        matches!(&spilled[..], [n] if Path::new(n).extension().is_some_and(|e| e == "rar")),
        "{spilled:?}"
    );
}
