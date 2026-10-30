#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    reason = "тесты: короткие имена, доли длины, таблицы случаев"
)]

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use proptest::prelude::*;
use urfin_core::{Attrs, CancelToken, Entry, Location};
use urfin_vfs::volume::{FsKind, Media};
use urfin_vfs::{
    Caps, ListOpts, ListStream, Metadata, Space, Vfs, VfsError, VfsPath, VfsRead, VfsRegistry,
    WatchGuard, WatchTx,
};

use super::*;

fn facts() -> Facts {
    Facts {
        local: true,
        seek: true,
        size: Some(MAP_MIN),
        age: Some(MAP_AGE),
        follow: false,
        mmap: MmapPolicy::Auto,
        volume: Some((Media::Unknown, FsKind::Ntfs)),
    }
}

#[test]
fn choose_by_volume_size_age_follow() {
    assert_eq!(choose(&facts()), SourceKind::Mapped);
    let cases: [(&str, fn(&mut Facts)); 8] = [
        ("сеть", |f| f.volume = Some((Media::Net, FsKind::Ntfs))),
        ("USB", |f| f.volume = Some((Media::Usb, FsKind::ExFat))),
        ("SMB", |f| f.volume = Some((Media::Unknown, FsKind::Smb))),
        ("том неизвестен", |f| f.volume = None),
        ("mtime < 60 с", |f| {
            f.age = MAP_AGE.checked_sub(Duration::from_secs(1));
        }),
        ("mtime в будущем", |f| f.age = None),
        ("follow", |f| f.follow = true),
        ("mmap = false", |f| f.mmap = MmapPolicy::Off),
    ];
    for (name, tweak) in cases {
        let mut f = facts();
        tweak(&mut f);
        assert_eq!(choose(&f), SourceKind::File, "{name}");
    }
    let mut f = facts();
    f.size = Some(MAP_MIN - 1);
    assert_eq!(choose(&f), SourceKind::File, "меньше 64 МиБ");
    f.size = Some(OWNED_MAX);
    assert_eq!(choose(&f), SourceKind::Owned);
    f.follow = true;
    assert_eq!(choose(&f), SourceKind::File, "follow не Owned: файл растёт");
    let remote = Facts {
        local: false,
        size: None,
        ..facts()
    };
    assert_eq!(choose(&remote), SourceKind::Seek);
    assert_eq!(
        choose(&Facts {
            seek: false,
            ..remote.clone()
        }),
        SourceKind::Spool
    );
    assert_eq!(
        choose(&Facts {
            size: Some(10),
            seek: false,
            ..remote
        }),
        SourceKind::Owned,
        "маленький поток читается целиком"
    );
}

/// Байты файла по смещению: шаблон без периода в пределах теста.
fn byte_at(i: u64) -> u8 {
    let x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (x >> 56) as u8 ^ (i as u8)
}

fn content(len: usize) -> Vec<u8> {
    (0..len as u64).map(byte_at).collect()
}

/// `VfsRead` поверх файла; `seek: false` — поток без seek.
struct Reader {
    file: File,
    seek: bool,
    len: Option<u64>,
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for Reader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if self.seek {
            self.file.seek(pos)
        } else {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

impl VfsRead for Reader {
    fn len(&self) -> Option<u64> {
        self.len
    }
}

/// Все пять источников над одним файлом.
struct Fixture {
    _dir: tempfile::TempDir,
    data: Vec<u8>,
    sources: Vec<Arc<dyn ByteSource>>,
}

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let data = content(300_000);
        std::fs::write(&path, &data).unwrap();
        let open = || File::open(&path).unwrap();
        let (spool, pump) = SpoolSource::start(
            Box::new(Reader {
                file: open(),
                seek: false,
                len: None,
            }),
            &dir.path().join("spool"),
            CancelToken::new(),
        )
        .unwrap();
        pump.run();
        let sources: Vec<Arc<dyn ByteSource>> = vec![
            Arc::new(OwnedSource::read(open(), 0).unwrap()),
            Arc::new(FileSource::new(open(), path.clone(), false)),
            Arc::new(MappedSource::new(&open(), path.clone()).unwrap()),
            Arc::new(
                SeekSource::new(
                    Box::new(Reader {
                        file: open(),
                        seek: true,
                        len: None,
                    }),
                    None,
                )
                .unwrap(),
            ),
            Arc::new(spool),
        ];
        Fixture {
            _dir: dir,
            data,
            sources,
        }
    })
}

#[test]
fn every_kind_is_present() {
    let kinds: Vec<_> = fixture().sources.iter().map(|s| s.kind()).collect();
    assert_eq!(
        kinds,
        [
            SourceKind::Owned,
            SourceKind::File,
            SourceKind::Mapped,
            SourceKind::Seek,
            SourceKind::Spool
        ]
    );
    for s in &fixture().sources {
        assert_eq!(s.len(), 300_000, "{:?}", s.kind());
        assert!(s.is_final(), "{:?}", s.kind());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Приёмка S7-02: `read_at` всех источников побайтно совпадает на случайных диапазонах.
    #[test]
    fn read_at_matches_everywhere(off in 0u64..310_000, len in 0usize..140_000) {
        let f = fixture();
        let want_end = (off as usize + len).min(f.data.len());
        let want = f.data.get(off as usize..want_end).unwrap_or(&[]);
        for s in &f.sources {
            let mut buf = vec![0u8; len];
            let n = read_full(&**s, off, &mut buf).unwrap();
            prop_assert_eq!(&buf[..n], want, "{:?}", s.kind());
        }
    }
}

/// Поток: первая часть сразу, вторая — по сигналу теста.
struct Gate {
    parts: Vec<Vec<u8>>,
    at: usize,
    pos: usize,
    wait: Mutex<mpsc::Receiver<()>>,
}

impl Read for Gate {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at < self.parts.len() && self.pos == self.parts[self.at].len() {
            self.at += 1;
            self.pos = 0;
            if self.at < self.parts.len() {
                let _ = self.wait.lock().unwrap().recv();
            }
        }
        let Some(part) = self.parts.get(self.at) else {
            return Ok(0);
        };
        let n = buf.len().min(part.len() - self.pos);
        buf[..n].copy_from_slice(&part[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Seek for Gate {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

impl VfsRead for Gate {
    fn len(&self) -> Option<u64> {
        None
    }
}

/// VFS без `Caps::SEEK` и без размера: отдаёт `Gate`.
struct MockVfs {
    reader: Mutex<Option<Gate>>,
}

impl Vfs for MockVfs {
    fn scheme(&self) -> &'static str {
        "mock"
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        Caps(0)
    }
    fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn stat(&self, _: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn stat_entry(&self, _: &VfsPath, _: &str, _: Attrs) -> Result<Entry, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn open_read(&self, _: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        let r = self.reader.lock().unwrap().take();
        r.map(|g| Box::new(g) as Box<dyn VfsRead>)
            .ok_or(VfsError::Unsupported("mock: once"))
    }
    fn space(&self, _: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
    fn local_path(&self, _: &VfsPath) -> Option<PathBuf> {
        None
    }
    fn watch(&self, _: &VfsPath, _: WatchTx) -> Result<WatchGuard, VfsError> {
        Err(VfsError::Unsupported("mock"))
    }
}

fn spool_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.flatten().count())
}

/// Приёмка S7-02: `Spool` отдаёт данные до конца копирования (мок-VFS без SEEK); файл
/// удаляется при закрытии.
#[test]
fn spool_serves_before_copy_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let spool = spool_dir(tmp.path());
    let first = content(200_000);
    let second: Vec<u8> = content(350_000)[200_000..].to_vec();
    let (tx, rx) = mpsc::channel();
    let mut reg = VfsRegistry::default();
    reg.register(Arc::new(MockVfs {
        reader: Mutex::new(Some(Gate {
            parts: vec![first.clone(), second.clone()],
            at: 0,
            pos: 0,
            wait: Mutex::new(rx),
        })),
    }));
    let loc = Location {
        backend: "mock".into(),
        path: PathBuf::from("a/b.txt"),
        outer: None,
    };
    let opts = SourceOpts {
        spool_dir: Some(spool.clone()),
        ..SourceOpts::default()
    };
    let opened = open_source(&reg, &loc, &opts, &CancelToken::new()).unwrap();
    assert_eq!(opened.src.kind(), SourceKind::Spool);
    let pump = opened.pump.expect("Spool даёт насос");
    let src = opened.src;
    let worker = std::thread::spawn(move || pump.run());

    let deadline = Instant::now() + Duration::from_secs(10);
    while src.len() < first.len() as u64 {
        assert!(Instant::now() < deadline, "первая часть не скопирована");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!src.is_final(), "копирование не закончено");
    let mut buf = vec![0u8; first.len() + 1000];
    let n = read_full(&*src, 0, &mut buf).unwrap();
    assert_eq!(
        &buf[..n],
        &first[..],
        "скопированное читается, дальше — ничего"
    );
    assert_eq!(spool_files(&spool), 1);

    tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(src.is_final());
    assert_eq!(src.len(), 350_000);
    let mut all = vec![0u8; 350_000];
    assert_eq!(read_full(&*src, 0, &mut all).unwrap(), 350_000);
    assert_eq!(all, content(350_000));
    drop(src);
    assert_eq!(spool_files(&spool), 0, "временный файл удалён при закрытии");
}

#[test]
fn sweep_removes_stale_spool_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("1-1.spool");
    let fresh = tmp.path().join("1-2.spool");
    let other = tmp.path().join("keep.txt");
    for p in [&old, &fresh, &other] {
        std::fs::write(p, b"x").unwrap();
    }
    let f = File::options().write(true).open(&old).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();
    drop(f);
    assert_eq!(sweep_spool(tmp.path(), Duration::from_secs(3600)), 1);
    assert!(!old.exists() && fresh.exists() && other.exists());
}

#[test]
fn open_local_picks_owned_file_mapped() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = VfsRegistry::with_local();
    let cancel = CancelToken::new();
    let opts = SourceOpts::default();

    let small = tmp.path().join("small.txt");
    std::fs::write(&small, b"hello").unwrap();
    let o = open_source(&reg, &Location::local(&small), &opts, &cancel).unwrap();
    assert_eq!(o.src.kind(), SourceKind::Owned);
    assert_eq!(o.src.mapped(), Some(&b"hello"[..]));

    // 64 МиБ: свежий — File; mtime старше минуты — Mapped (локальный фиксированный том).
    let big = tmp.path().join("big.bin");
    let f = File::create(&big).unwrap();
    f.set_len(MAP_MIN).unwrap();
    (&f).write_all(b"head").unwrap();
    drop(f);
    let o = open_source(&reg, &Location::local(&big), &opts, &cancel).unwrap();
    assert_eq!(o.src.kind(), SourceKind::File);
    let f = File::options().write(true).open(&big).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(120))
        .unwrap();
    drop(f);
    let o = open_source(&reg, &Location::local(&big), &opts, &cancel).unwrap();
    assert_eq!(o.src.kind(), SourceKind::Mapped);
    assert_eq!(&o.src.mapped().unwrap()[..4], b"head");
    let follow = SourceOpts {
        follow: true,
        ..SourceOpts::default()
    };
    let o = open_source(&reg, &Location::local(&big), &follow, &cancel).unwrap();
    assert_eq!(o.src.kind(), SourceKind::File, "follow — без mmap");
    assert!(!o.src.is_final());
}

/// Приёмка S7-03: UI-поток не вызывает `read_at` (`debug_assert!` по потоку).
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "UI thread")]
fn read_at_from_ui_thread_panics() {
    let src = OwnedSource::new(vec![1u8, 2, 3]);
    mark_ui_thread();
    let mut buf = [0u8; 3];
    let _ = src.read_at(0, &mut buf);
}
