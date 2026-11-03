//! Операции SFTP через движок ops (F03-07) на `testsrv`: закачка с обрывом на 60 % и
//! докачкой, чужой `.urfin-part`, права и даты, сравнение с точностью 1 с, «удалять
//! лишнее» выключено по умолчанию, отмена закрывает каналы ≤200 мс.

mod testsrv;

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use urfin_core::net::{AskKind, LiveState, NetModel, sftp_profile};
use urfin_core::{Boot, Command, Core, JobState, ListingStatus, Location, Services, Side, Waker};
use urfin_net::{SftpFs, SftpUri, SshCfg};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

use testsrv::{Opts, Shared, TestSrv};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn opts(root: &Path, shared: &Shared) -> Opts {
    Opts {
        host_seed: 7,
        password: Some("secret"),
        sftp_root: Some(root.to_path_buf()),
        owner: Some(("deploy", "deploy")),
        shared: shared.clone(),
        ..Opts::default()
    }
}

fn setup(left: &Path) -> Core {
    let fs = Arc::new(SftpFs::new());
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::clone(&fs) as Arc<dyn urfin_vfs::Vfs>);
    let reg = Arc::new(reg);
    let cfg = VfsCfg::default();
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg.clone())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(left), Location::local(left)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, cfg);
    let mut ssh = SshCfg::default();
    ssh.reconnect.interval_s = 1;
    urfin_net::register_shared(&mut core, ssh, None, fs, None);
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
    core
}

fn model(core: &Core) -> &NetModel {
    core.state().ext.get::<NetModel>().expect("net model")
}

/// Крутит core, отвечая на вопросы подключения (ключ — «только в этот раз», пароль).
fn pump_until(core: &mut Core, what: &str, secs: u64, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        let answer = model(core).ask().map(|a| match a.kind {
            AskKind::HostKey { .. } => Command::new("ssh.answer").arg("choice", "once"),
            AskKind::HostChanged { .. } => Command::new("ssh.answer").arg("choice", "cancel"),
            AskKind::Secret { .. } => Command::new("ssh.answer")
                .arg("choice", "ok")
                .arg("text", "secret"),
        });
        if let Some(c) = answer {
            core.dispatch(c);
        }
        core.pump(Duration::from_millis(2));
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Подключение справа; профиль сеанса.
fn connect(core: &mut Core, port: u16) -> String {
    core.pump(Duration::from_millis(5));
    core.dispatch(
        Command::new("ssh.connect")
            .arg("host", "127.0.0.1")
            .arg("port", port.to_string())
            .arg("user", "tester")
            .arg("auth", "password")
            .arg("save", false)
            .arg("side", "right"),
    );
    pump_until(core, "connected", 30, |c| {
        let t = c.state().panel(Side::Right).tab();
        sftp_profile(&t.loc).is_some_and(|p| {
            model(c).live(&p).map(|l| l.state.clone()) == Some(LiveState::Connected)
        }) && matches!(t.listing.status, ListingStatus::Complete)
    });
    sftp_profile(&core.state().panel(Side::Right).tab().loc).unwrap()
}

fn remote(p: &str, path: &str) -> String {
    SftpUri::new(p, path).to_location().to_uri()
}

/// Активная — левая панель на `dir`, курсор на `name`.
fn cursor_left(core: &mut Core, dir: &Path, name: &str) {
    core.dispatch(Command::new("panel.focus").arg("side", "left"));
    core.dispatch(
        Command::new("panel.go")
            .arg("side", "left")
            .arg("path", Location::local(dir).to_uri()),
    );
    pump_until(core, "left listed", 10, |c| {
        let t = c.state().panel(Side::Left).tab();
        matches!(t.listing.status, ListingStatus::Complete)
            && t.listing.entries.iter().any(|e| e.name == name)
    });
    core.dispatch(Command::new("panel.cursor.to").arg("name", name));
    core.pump(Duration::from_millis(2));
}

/// Последняя задача вида `kind`.
fn job(core: &Core, kind: &str) -> Option<(urfin_core::JobId, JobState, u64, u64)> {
    core.state()
        .jobs
        .iter()
        .filter(|j| j.kind == kind)
        .last()
        .map(|j| {
            let p = j.progress.snapshot();
            (j.id, j.state.clone(), p.bytes_done, p.bytes_total)
        })
}

fn done(core: &Core, kind: &str) -> bool {
    matches!(job(core, kind), Some((_, JobState::Done(_), ..)))
}

fn assert_ok(core: &Core, kind: &str) {
    match job(core, kind) {
        Some((_, JobState::Done(s), ..)) => assert_eq!(s.errors, 0, "{s:?}"),
        other => panic!("job {kind}: {other:?}"),
    }
}

/// Данные без диска в памяти: xorshift, блоками.
fn write_data(path: &Path, size: u64, seed: u64) {
    use std::io::Write as _;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut x = seed | 1;
    let mut left = size;
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        for c in buf.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let b = x.to_le_bytes();
            c.copy_from_slice(&b[..c.len()]);
        }
        let n = usize::try_from(left.min(buf.len() as u64)).unwrap();
        f.write_all(&buf[..n]).unwrap();
        left -= n as u64;
    }
}

fn xxh3(path: &Path) -> u128 {
    let mut f = std::fs::File::open(path).unwrap();
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    h.digest128()
}

fn mtime_secs(p: &Path) -> u64 {
    std::fs::metadata(p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn set_mtime(p: &Path, secs: u64, nanos: u32) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::new(secs, nanos))
        .unwrap();
}

/// Закачка `size` байт с обрывом на 60 %: сервер падает и поднимается на том же порту,
/// job ждёт связи и дописывает с конца `part`; итог совпадает по xxh3.
fn upload_with_drop(size: u64) {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let src = local.path().join("big.bin");
    write_data(&src, size, 42);
    let shared = Shared::default();
    let srv = TestSrv::start(opts(root.path(), &shared));
    let port = srv.port;
    let mut core = setup(local.path());
    let p = connect(&mut core, port);
    cursor_left(&mut core, local.path(), "big.bin");
    core.dispatch(Command::new("ops.copy").arg("dest", format!("{}/", remote(&p, "/up"))));
    pump_until(&mut core, "60 %", 120, |c| {
        job(c, "copy").is_some_and(|(_, _, d, t)| t > 0 && d * 10 >= t * 6)
    });
    drop(srv);
    let cut = shared.written.load(Ordering::SeqCst);
    assert!(cut < size, "сервер упал до конца файла");
    std::thread::sleep(Duration::from_millis(300));
    let _srv = TestSrv::start_on(port, opts(root.path(), &shared));
    pump_until(&mut core, "copy done", 300, |c| done(c, "copy"));
    assert_ok(&core, "copy");
    let dst = root.path().join("up").join("big.bin");
    assert_eq!(xxh3(&dst), xxh3(&src), "итог совпадает по xxh3");
    assert!(!root.path().join("up").join("big.bin.urfin-part").exists());
    let writes = shared.writes.lock().unwrap().clone();
    let resumed = writes
        .iter()
        .filter(|(path, _)| path == "/up/big.bin.urfin-part")
        .map(|(_, at)| *at)
        .collect::<Vec<_>>();
    assert_eq!(resumed.first(), Some(&0), "первая запись — с начала");
    let again = *resumed.last().unwrap();
    assert!(
        again * 2 >= size && again <= cut,
        "докачка с ~60 %: с {again} из {size} (обрыв на {cut})"
    );
    // Записано на сервер не больше, чем файл и дописанный хвост после обрыва.
    assert!(shared.written.load(Ordering::SeqCst) < size + size / 2);
}

#[test]
fn upload_resumes_after_drop_at_60_percent() {
    upload_with_drop(24 << 20);
}

/// Приёмка F03-07 целиком: 1 ГиБ (долго; `cargo nextest run --run-ignored all`).
#[test]
#[ignore = "1 ГиБ: долго для обычного прогона"]
fn upload_1gib_resumes_after_drop_at_60_percent() {
    upload_with_drop(1 << 30);
}

/// Чужой `.urfin-part` (другой хвост) — запись заново с нуля.
#[test]
fn foreign_part_is_rewritten() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let src = local.path().join("f.bin");
    write_data(&src, 3 << 20, 7);
    std::fs::create_dir_all(root.path().join("up")).unwrap();
    let part = root.path().join("up").join("f.bin.urfin-part");
    write_data(&part, 2 << 20, 99);
    let shared = Shared::default();
    let _srv = TestSrv::start(opts(root.path(), &shared));
    let mut core = setup(local.path());
    let p = connect(&mut core, _srv.port);
    cursor_left(&mut core, local.path(), "f.bin");
    core.dispatch(Command::new("ops.copy").arg("dest", format!("{}/", remote(&p, "/up"))));
    pump_until(&mut core, "copy done", 60, |c| done(c, "copy"));
    assert_ok(&core, "copy");
    let dst = root.path().join("up").join("f.bin");
    assert_eq!(xxh3(&dst), xxh3(&src));
    assert!(!part.exists());
    let w = shared.writes.lock().unwrap().clone();
    assert_eq!(w, [("/up/f.bin.urfin-part".to_owned(), 0)], "запись заново");
}

/// «Сохранить права и даты»: закачка переносит время, копия на сервере — права и время;
/// без флажка — время не переносится.
#[test]
fn preserve_mode_and_mtime() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let t = 1_600_000_000;
    let src = local.path().join("a.txt");
    std::fs::write(&src, b"hello").unwrap();
    set_mtime(&src, t, 400_000_000);
    let shared = Shared::default();
    let _srv = TestSrv::start(opts(root.path(), &shared));
    let mut core = setup(local.path());
    let p = connect(&mut core, _srv.port);

    // Локальный → сервер: время (секунды SFTP).
    cursor_left(&mut core, local.path(), "a.txt");
    core.dispatch(Command::new("ops.copy").arg("dest", format!("{}/", remote(&p, "/up"))));
    pump_until(&mut core, "upload", 60, |c| done(c, "copy"));
    assert_ok(&core, "copy");
    assert_eq!(mtime_secs(&root.path().join("up").join("a.txt")), t);

    // Сервер → сервер: права 0750 и время.
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("x.sh"), b"#!/bin/sh\n").unwrap();
    set_mtime(&root.path().join("src").join("x.sh"), t + 100, 0);
    shared
        .modes
        .lock()
        .unwrap()
        .insert("/src/x.sh".into(), 0o100_750);
    let n = core.state().jobs.iter().count();
    core.dispatch(Command::new("panel.focus").arg("side", "right"));
    core.dispatch(
        Command::new("panel.go")
            .arg("side", "right")
            .arg("path", remote(&p, "/src")),
    );
    pump_until(&mut core, "src listed", 10, |c| {
        let t = c.state().panel(Side::Right).tab();
        matches!(t.listing.status, ListingStatus::Complete)
            && t.listing.entries.iter().any(|e| e.name == "x.sh")
    });
    core.dispatch(Command::new("panel.cursor.to").arg("name", "x.sh"));
    core.dispatch(Command::new("ops.copy").arg("dest", format!("{}/", remote(&p, "/dst"))));
    pump_until(&mut core, "remote copy", 60, |c| {
        c.state().jobs.iter().count() > n && done(c, "copy")
    });
    assert_ok(&core, "copy");
    let got = shared.modes.lock().unwrap().get("/dst/x.sh").copied();
    assert_eq!(got.map(|m| m & 0o7777), Some(0o750), "права перенесены");
    assert_eq!(mtime_secs(&root.path().join("dst").join("x.sh")), t + 100);

    // Без флажка время не переносится.
    let n = core.state().jobs.iter().count();
    cursor_left(&mut core, local.path(), "a.txt");
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", format!("{}/", remote(&p, "/plain")))
            .arg("preserve", false),
    );
    pump_until(&mut core, "plain", 60, |c| {
        c.state().jobs.iter().count() > n && done(c, "copy")
    });
    assert_ne!(mtime_secs(&root.path().join("plain").join("a.txt")), t);
}

/// Сравнение «локальная ↔ sftp» — точность 1 с; синхронизация не удаляет лишнее на
/// сервере, пока не попросили (`delete_extra`).
#[test]
fn compare_one_second_and_keep_extra_by_default() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let t = 1_700_000_000;
    let srv_dir = root.path().join("site");
    std::fs::create_dir_all(&srv_dir).unwrap();
    for (name, lt, rt) in [("same.txt", (t, 700_000_000), t), ("new.txt", (t + 5, 0), t)] {
        std::fs::write(local.path().join(name), b"12345").unwrap();
        set_mtime(&local.path().join(name), lt.0, lt.1);
        std::fs::write(srv_dir.join(name), b"12345").unwrap();
        set_mtime(&srv_dir.join(name), rt, 0);
    }
    std::fs::write(srv_dir.join("extra.txt"), b"x").unwrap();
    let shared = Shared::default();
    let _srv = TestSrv::start(opts(root.path(), &shared));
    let mut core = setup(local.path());
    let p = connect(&mut core, _srv.port);
    core.dispatch(
        Command::new("panel.go")
            .arg("side", "right")
            .arg("path", remote(&p, "/site")),
    );
    pump_until(&mut core, "site listed", 10, |c| {
        let t = c.state().panel(Side::Right).tab();
        matches!(t.listing.status, ListingStatus::Complete) && t.listing.entries.len() == 3
    });
    cursor_left(&mut core, local.path(), "new.txt");
    core.dispatch(Command::new("ops.compare"));
    let selected = |c: &Core, side: Side| {
        let t = c.state().panel(side).tab();
        let mut v: Vec<String> = t
            .selection
            .names(&t.listing)
            .into_iter()
            .map(|n| n.to_string())
            .collect();
        v.sort();
        v
    };
    pump_until(&mut core, "compared", 10, |c| !selected(c, Side::Left).is_empty());
    assert_eq!(selected(&core, Side::Left), ["new.txt"], "0,7 с — равны, 5 с — новее");
    assert_eq!(selected(&core, Side::Right), ["extra.txt"]);

    // Синхронизация → без `delete_extra` лишнее остаётся.
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", remote(&p, "/site"))
            .arg("conflict", "newer"),
    );
    pump_until(&mut core, "sync", 60, |c| done(c, "copy"));
    assert_ok(&core, "copy");
    assert!(srv_dir.join("extra.txt").exists(), "лишнее не удалено по умолчанию");
    assert_eq!(mtime_secs(&srv_dir.join("new.txt")), t + 5);

    // С `delete_extra` — удалено.
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", remote(&p, "/site"))
            .arg("conflict", "newer")
            .arg("delete_extra", true),
    );
    pump_until(&mut core, "mirror", 60, |_| !srv_dir.join("extra.txt").exists());
}

/// Отмена посреди закачки закрывает файлы на сервере за ≤200 мс.
#[test]
fn cancel_closes_channels_within_200ms() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let src = local.path().join("slow.bin");
    write_data(&src, 64 << 20, 3);
    let shared = Shared::default();
    let _srv = TestSrv::start(Opts {
        write_delay: Some(Duration::from_millis(1)),
        ..opts(root.path(), &shared)
    });
    let mut core = setup(local.path());
    let p = connect(&mut core, _srv.port);
    cursor_left(&mut core, local.path(), "slow.bin");
    core.dispatch(Command::new("ops.copy").arg("dest", format!("{}/", remote(&p, "/up"))));
    pump_until(&mut core, "started", 60, |c| {
        job(c, "copy").is_some_and(|(_, _, d, _)| d > 1 << 20)
    });
    assert!(shared.open.load(Ordering::SeqCst) > 0);
    let (id, ..) = job(&core, "copy").unwrap();
    let t0 = Instant::now();
    core.dispatch(Command::new("job.cancel").arg("id", i64::try_from(id.0).unwrap()));
    pump_until(&mut core, "cancelled", 5, |c| {
        job(c, "copy").is_some_and(|(_, st, ..)| !st.is_active())
            && shared.open.load(Ordering::SeqCst) == 0
    });
    let dt = t0.elapsed();
    assert!(dt <= Duration::from_millis(200), "отмена за {dt:?}");
    assert!(!root.path().join("up").join("slow.bin").exists());
}
