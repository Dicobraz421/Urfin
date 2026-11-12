//! Backend `sftp` против `testsrv` (F03-04): листинг батчами (10k файлов), ссылки,
//! владельцы из `longname`, чтение и запись, `mkdir`/`remove`/`rename`/`setstat`, RTT;
//! слежение `viewer.follow` за файлом и его цена в запросах (F03-21).
//! Вызовы `Vfs` — из обычного потока, как у Runtime; tokio — свой поток, как у крейта.

mod testsrv;

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc as smpsc;
use std::time::{Duration, Instant};

use russh::keys::ssh_key::LineEnding;
use tokio::sync::{mpsc, oneshot};
use urfin_core::CancelToken;
use urfin_net::conn::{self, ConnCfg, Event, Pool};
use urfin_net::sftp::{Session, SetStat};
use urfin_net::{Auth, Profile, SftpFs, SftpUri, SshCfg, hostkey};
use urfin_vfs::{Entry, EntryKind, ListOpts, Vfs, VfsError};

use testsrv::{Opts, READDIR_BATCH, Shared, TestSrv, key};

/// Подключение и SFTP-канал на своём потоке tokio; `Drop` закрывает.
struct Remote {
    fs: Arc<SftpFs>,
    /// Журнал запросов сервера (F03-21).
    shared: Shared,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _home: tempfile::TempDir,
    _srv: TestSrv,
}

impl Remote {
    fn start(
        root: &Path,
        owner: Option<(&'static str, &'static str)>,
        links: &[(&str, &str)],
    ) -> Remote {
        let shared = Shared::default();
        let srv = TestSrv::start(Opts {
            keys: vec![key(21).public_key().clone()],
            sftp_root: Some(root.to_path_buf()),
            shared: shared.clone(),
            owner,
            links: links
                .iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect(),
            ..Opts::default()
        });
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        hostkey::append(
            &ssh.join("known_hosts"),
            &hostkey::host_name("127.0.0.1", srv.port),
            &srv.host_key,
        )
        .unwrap();
        let id = ssh.join("id_21");
        std::fs::write(&id, key(21).to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
        let mut cfg = ConnCfg::new(
            &SshCfg::default(),
            Some(home.path()),
            Some(&home.path().join("state")),
        );
        cfg.default_keys.clear();
        let mut p = Profile::new("srv1", "127.0.0.1");
        p.port = srv.port;
        p.user = "tester".into();
        p.auth = Auth::Key;
        p.key = id.display().to_string();

        let (ready_tx, ready_rx) = smpsc::channel::<Arc<Session>>();
        let (stop, stopped) = oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let (ctl_tx, ctl) = mpsc::unbounded_channel();
                let task =
                    tokio::spawn(conn::session(vec![p], cfg, Arc::new(Pool::new()), tx, ctl));
                let wait = async {
                    while let Some(ev) = rx.recv().await {
                        match ev {
                            Event::Connected(c) => {
                                let s = Session::open(c).await.unwrap();
                                ready_tx.send(s).unwrap();
                            }
                            Event::Failed(e) => panic!("connect: {e}"),
                            _ => {}
                        }
                    }
                };
                tokio::select! {
                    () = wait => {}
                    _ = stopped => {}
                }
                let _ = ctl_tx.send(conn::Control::Disconnect);
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            });
        });
        let s = ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        let fs = SftpFs::new();
        fs.attach(s);
        Remote {
            fs: Arc::new(fs),
            shared,
            stop: Some(stop),
            thread: Some(thread),
            _home: home,
            _srv: srv,
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.fs.detach("srv1");
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

fn loc(path: &str) -> urfin_core::Location {
    SftpUri::new("srv1", path).to_location()
}

fn list_all(fs: &SftpFs, path: &str) -> Vec<Entry> {
    let mut s = fs.list(&loc(path), &ListOpts::default()).unwrap();
    let mut out = Vec::new();
    let cancel = CancelToken::new();
    while s.next_batch(&mut out, &cancel).unwrap() {}
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[test]
fn listing_links_and_owners() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), b"hello").unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join(".hidden"), b"").unwrap();
    let r = Remote::start(
        root.path(),
        Some(("администратор", "пользователи")),
        &[("/ln", "sub"), ("/dead", "/nope")],
    );
    let list = list_all(&r.fs, "/");
    let names: Vec<_> = list.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        names,
        [
            (".hidden", EntryKind::File),
            ("a.txt", EntryKind::File),
            ("dead", EntryKind::LinkBroken),
            ("ln", EntryKind::LinkDir),
            ("sub", EntryKind::Dir),
        ]
    );
    assert_eq!(list[1].size, 5);
    assert!(list[1].mtime > 0);

    // Цель ссылки — в подсказке.
    let md = r.fs.stat(&loc("/ln"), false).unwrap();
    assert_eq!(md.kind, EntryKind::LinkDir);
    assert_eq!(md.link_target.as_deref(), Some(Path::new("sub")));
    assert_eq!(r.fs.stat(&loc("/ln"), true).unwrap().kind, EntryKind::Dir);
    let ent =
        r.fs.stat_entry(&loc("/"), "dead", urfin_vfs::Attrs::default())
            .unwrap();
    assert_eq!(ent.kind, EntryKind::LinkBroken);

    // Колонки «Права» и «Владелец».
    let rows = r.fs.details(&loc("/")).unwrap();
    let a = rows.iter().find(|(n, ..)| n == "a.txt").unwrap();
    assert_eq!(a.1, "-rw-r--r--");
    assert_eq!(
        (a.2.user.as_str(), a.2.group.as_str()),
        ("администратор", "пользователи")
    );
    assert!(r.fs.rtt("srv1").is_some());

    // Те же колонки схемы (v6 №1): `Entry.ext` — из памяти листинга, без запросов.
    let ids: Vec<_> = r.fs.columns(&loc("/")).iter().map(|c| c.id).collect();
    assert_eq!(ids, ["size", "mtime", "perms", "owner"]);
    let cell = |name: &str, col: &str| {
        let e = list.iter().find(|e| e.name == name).unwrap();
        r.fs.ext(&loc("/"), e, col)
    };
    assert_eq!(
        cell("a.txt", "perms"),
        Some(urfin_vfs::ExtCell::Text("-rw-r--r--".into()))
    );
    assert_eq!(
        cell("a.txt", "owner"),
        Some(urfin_vfs::ExtCell::Text("администратор".into()))
    );
    assert!(
        matches!(cell("sub", "perms"), Some(urfin_vfs::ExtCell::Text(p)) if p.starts_with('d'))
    );
    assert_eq!(cell("a.txt", "size"), None);
    assert!(matches!(
        r.fs.list(&loc("/nope"), &ListOpts::default()),
        Err(VfsError::NotFound(_))
    ));
}

#[test]
fn garbage_longname_falls_back_to_ids() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("x"), b"1").unwrap();
    let r = Remote::start(root.path(), None, &[]);
    let d = r.fs.details(&loc("/")).unwrap();
    let x = d.iter().find(|(n, ..)| n == "x").unwrap();
    assert_eq!((x.2.user.as_str(), x.2.group.as_str()), ("1000", "1000"));
}

/// Приёмка: первый батч 10k файлов — первый кадр ≤50 мс после ответа сервера.
#[test]
fn ten_thousand_files_first_batch() {
    const N: usize = 10_000;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("big");
    std::fs::create_dir(&dir).unwrap();
    for i in 0..N {
        std::fs::File::create(dir.join(format!("f{i:05}"))).unwrap();
    }
    let r = Remote::start(root.path(), Some(("u", "g")), &[]);
    let mut s = r.fs.list(&loc("/big"), &ListOpts::default()).unwrap();
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    let t = Instant::now();
    assert!(s.next_batch(&mut out, &cancel).unwrap());
    let first = t.elapsed();
    // Первый ответ сервера несёт и «.».
    assert_eq!(out.len(), READDIR_BATCH - 1);
    assert!(first <= Duration::from_millis(50), "{first:?}");
    let mut batches = 1;
    while s.next_batch(&mut out, &cancel).unwrap() {
        batches += 1;
    }
    assert_eq!(out.len(), N);
    assert!(batches > N / READDIR_BATCH);
    // Отмена — до запроса.
    let mut s = r.fs.list(&loc("/big"), &ListOpts::default()).unwrap();
    cancel.cancel();
    assert!(matches!(
        s.next_batch(&mut Vec::new(), &cancel),
        Err(VfsError::Cancelled)
    ));
}

#[test]
fn read_write_and_file_ops() {
    let root = tempfile::tempdir().unwrap();
    let r = Remote::start(root.path(), Some(("u", "g")), &[]);
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();

    let mut w = r.fs.create_file(&loc("/new.bin"), true).unwrap();
    w.write_all(&data).unwrap();
    w.finish().unwrap();
    assert!(r.fs.create_file(&loc("/new.bin"), true).is_err());
    assert_eq!(std::fs::read(root.path().join("new.bin")).unwrap(), data);

    let mut rd = r.fs.open_read(&loc("/new.bin")).unwrap();
    assert_eq!(rd.len(), Some(data.len() as u64));
    let mut back = Vec::new();
    rd.read_to_end(&mut back).unwrap();
    assert_eq!(back, data);
    rd.seek(SeekFrom::End(-10)).unwrap();
    let mut tail = Vec::new();
    rd.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, data[data.len() - 10..]);
    drop(rd);

    r.fs.mkdir(&loc("/d")).unwrap();
    r.fs.rename(&loc("/new.bin"), &loc("/d/n.bin"), false)
        .unwrap();
    assert!(root.path().join("d").join("n.bin").exists());
    let other = SftpUri::new("srv2", "/x").to_location();
    assert!(matches!(
        r.fs.rename(&loc("/d/n.bin"), &other, false),
        Err(VfsError::Unsupported(_))
    ));

    let t = 1_600_000_000u32;
    r.fs.setstat(
        &loc("/d/n.bin"),
        SetStat {
            mode: None,
            times: Some((t, t)),
        },
    )
    .unwrap();
    let m = r.fs.stat(&loc("/d/n.bin"), true).unwrap();
    assert_eq!(m.mtime, i64::from(t) * 1_000_000_000);
    assert_eq!(m.size, data.len() as u64);

    // Непустой каталог не удаляется, файл и пустой — да.
    assert!(r.fs.remove(&loc("/d")).is_err());
    r.fs.remove(&loc("/d/n.bin")).unwrap();
    r.fs.remove(&loc("/d")).unwrap();
    assert!(matches!(
        r.fs.stat(&loc("/d"), true),
        Err(VfsError::NotFound(_))
    ));
    // Места сервер не сообщает (нет `statvfs@openssh.com`).
    assert!(matches!(
        r.fs.space(&loc("/")),
        Err(VfsError::Unsupported(_))
    ));
}

/// Таб `sftp://` из сессии сам не подключается: без канала — «нет связи», сервер не
/// трогается.
#[test]
fn tab_from_session_does_not_connect() {
    let fs = SftpFs::new();
    let err = fs.list(&loc("/etc"), &ListOpts::default()).err().unwrap();
    let VfsError::Io { source, .. } = err else {
        panic!("{err:?}");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::NotConnected);
}

/// Запросы сервера по видам, по алфавиту. `CLOSE` старого ридера уходит без ожидания —
/// ждём, пока их не станет `close`.
fn ops_until(shared: &Shared, close: usize) -> Vec<&'static str> {
    let t0 = Instant::now();
    let mut ops = Vec::new();
    loop {
        ops.extend(shared.take_ops());
        if ops.iter().filter(|o| **o == "close").count() >= close
            || t0.elapsed() > Duration::from_secs(5)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    ops.sort_unstable();
    ops
}

fn append(path: &Path, data: &[u8]) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(data).unwrap();
}

/// F03-21: слежение вьювера (`SeekSource` из `open_source` с `follow`) за файлом на
/// `testsrv` видит дозапись и усечение. Цена одного роста — запросы сервера от шага
/// опроса до чтения прироста: сейчас `STAT` + переоткрытие (`OPEN`, `FSTAT`, `CLOSE`
/// старого) + `READ` — 5; шаг без роста — один `STAT`.
#[test]
fn follow_sees_append_and_truncate() {
    use urfin_view::{SourceKind, SourceOpts, open_source};
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("app.log");
    std::fs::write(&log, b"one\n").unwrap();
    let r = Remote::start(root.path(), None, &[]);
    let mut reg = urfin_vfs::VfsRegistry::with_local();
    reg.register(Arc::clone(&r.fs) as Arc<dyn Vfs>);
    let opts = SourceOpts {
        follow: true,
        ..SourceOpts::default()
    };
    let src = open_source(&reg, &loc("/app.log"), &opts, &CancelToken::new())
        .unwrap()
        .src;
    assert_eq!(src.kind(), SourceKind::Seek);
    assert!(!src.is_final(), "слежение включено");
    assert_eq!(src.len(), 4);
    let read = |off: u64, n: usize| {
        let mut buf = vec![0; n];
        let k = urfin_view::source::read_full(src.as_ref(), off, &mut buf).unwrap();
        buf.truncate(k);
        buf
    };
    assert_eq!(read(0, 4), b"one\n");

    // Шаг без роста — один `STAT`.
    r.shared.take_ops();
    assert_eq!(src.refresh_len().unwrap(), 4);
    assert_eq!(ops_until(&r.shared, 0), ["stat"]);

    // Дозапись: длина растёт, новые байты читаются.
    append(&log, b"two\n");
    assert_eq!(src.refresh_len().unwrap(), 8);
    assert_eq!(src.len(), 8);
    assert_eq!(read(4, 4), b"two\n");
    let grow = ops_until(&r.shared, 1);
    assert_eq!(
        grow,
        ["close", "fstat", "open", "read", "stat"],
        "цена роста"
    );

    // Ещё раз — и длинный хвост (больше блока `READ`).
    let tail = vec![b'x'; 100 * 1024];
    append(&log, &tail);
    assert_eq!(src.refresh_len().unwrap(), 8 + tail.len() as u64);
    assert_eq!(read(8, tail.len()), tail);
    r.shared.take_ops();

    // Усечение (logrotate copytruncate): длина меньше, байты — новые.
    let f = std::fs::OpenOptions::new().write(true).open(&log).unwrap();
    f.set_len(0).unwrap();
    drop(f);
    append(&log, b"new\n");
    assert_eq!(src.refresh_len().unwrap(), 4);
    assert_eq!(src.len(), 4);
    assert_eq!(read(0, 16), b"new\n");
}

/// F03-21: рост без переоткрытия — [`urfin_net::sftp::SftpReader::refresh_len`]
/// (`FSTAT` по открытому handle): на рост — `FSTAT` + `READ`, 2 запроса вместо 5;
/// дозапись и усечение видны тем же handle.
#[test]
fn reader_refresh_len_without_reopen() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("app.log");
    std::fs::write(&log, b"one\n").unwrap();
    let r = Remote::start(root.path(), None, &[]);
    let mut rd = r.fs.open_reader(&loc("/app.log")).unwrap();
    let mut buf = Vec::new();
    rd.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"one\n");

    r.shared.take_ops();
    assert_eq!(rd.refresh_len().unwrap(), 4);
    assert_eq!(ops_until(&r.shared, 0), ["fstat"]);

    append(&log, b"two\n");
    assert_eq!(rd.refresh_len().unwrap(), 8);
    assert_eq!(urfin_vfs::VfsRead::len(&rd), Some(8));
    rd.seek(SeekFrom::Start(4)).unwrap();
    let mut got = [0u8; 4];
    rd.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"two\n");
    assert_eq!(ops_until(&r.shared, 0), ["fstat", "read"], "цена роста");

    // Тот же `pos` после роста: упреждение сброшено, хвост читается без `seek`.
    append(&log, b"three\n");
    assert_eq!(rd.refresh_len().unwrap(), 14);
    let mut rest = Vec::new();
    rd.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"three\n");

    // Усечение: длина меньше, старые байты не отдаются.
    let f = std::fs::OpenOptions::new().write(true).open(&log).unwrap();
    f.set_len(0).unwrap();
    drop(f);
    append(&log, b"new\n");
    assert_eq!(rd.refresh_len().unwrap(), 4);
    rd.seek(SeekFrom::Start(0)).unwrap();
    let mut buf = Vec::new();
    rd.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"new\n");
    drop(rd);
    assert!(ops_until(&r.shared, 1).contains(&"close"));
}
