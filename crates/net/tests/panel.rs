//! Панель `sftp://` через core (F03-06): подключение командами `ssh.*`, листинг из
//! `testsrv`, обрыв (сервер упал) — листинг на месте, модель «обрыв»; сервер поднялся на
//! том же порту — переподключение, папка перечитана на месте: курсор и отметка те же.
//! Ключ и стартовая папка из строки «Новое подключение» (F03-15).

mod testsrv;

use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::net::{AskKind, LiveState, NetModel, sftp_profile};
use urfin_core::{Boot, Command, Core, ListingStatus, Location, Services, Waker};
use urfin_net::{SftpFs, SshCfg};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

use testsrv::{Opts, TestSrv, key};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn opts(root: &std::path::Path) -> Opts {
    Opts {
        host_seed: 7,
        password: Some("secret"),
        sftp_root: Some(root.to_path_buf()),
        owner: Some(("deploy", "deploy")),
        ..Opts::default()
    }
}

fn setup(start: &std::path::Path) -> Core {
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
        Boot::new(Location::local(start), Location::local(start)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, cfg);
    let mut ssh = SshCfg::default();
    ssh.reconnect.interval_s = 1;
    // Без диска: ни `connections.toml`, ни истории, ни кэша паролей.
    urfin_net::register_shared(&mut core, ssh, None, fs, None);
    core
}

fn model(core: &Core) -> &NetModel {
    core.state().ext.get::<NetModel>().expect("net model")
}

/// Сеанс профиля панели.
fn state(core: &Core) -> Option<LiveState> {
    let p = sftp_profile(&core.state().active_tab().loc)?;
    model(core).live(&p).map(|l| l.state.clone())
}

/// Крутит core, отвечая на вопросы подключения: ключ сервера — «только в этот раз»
/// (файлы `known_hosts` не трогаются), пароль — `secret`.
fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
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
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn names(core: &Core) -> Vec<String> {
    let t = core.state().active_tab();
    t.listing
        .entries
        .iter()
        .map(|e| e.name.to_string())
        .collect()
}

fn cursor(core: &Core) -> Option<String> {
    match core.state().active_tab().current()? {
        urfin_core::Row::Entry { entry, .. } => Some(entry.name.to_string()),
        urfin_core::Row::Parent => None,
    }
}

fn selected(core: &Core) -> Vec<String> {
    let t = core.state().active_tab();
    let mut v: Vec<String> = t
        .selection
        .names(&t.listing)
        .into_iter()
        .map(|n| n.to_string())
        .collect();
    v.sort();
    v
}

fn loaded(core: &Core) -> bool {
    let t = core.state().active_tab();
    sftp_profile(&t.loc).is_some()
        && matches!(t.listing.status, ListingStatus::Complete)
        && !t.listing.entries.is_empty()
}

#[test]
fn lost_keeps_listing_and_reconnect_keeps_cursor_and_marks() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("app");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..8 {
        std::fs::write(dir.join(format!("f{i}.txt")), vec![b'x'; i * 10]).unwrap();
    }
    let srv = TestSrv::start(opts(root.path()));
    let port = srv.port;
    let mut core = setup(local.path());
    core.pump(Duration::from_millis(5));
    core.dispatch(
        Command::new("ssh.connect")
            .arg("host", "127.0.0.1")
            .arg("port", port.to_string())
            .arg("user", "tester")
            .arg("auth", "password")
            .arg("save", false)
            .arg("side", "left"),
    );
    pump_until(&mut core, "connected", |c| {
        state(c) == Some(LiveState::Connected) && loaded(c)
    });
    let p = sftp_profile(&core.state().active_tab().loc).unwrap();
    let live = model(&core).live(&p).unwrap();
    assert_eq!(live.algo, "ssh-ed25519");
    assert_eq!(live.login, [urfin_core::net::Login::Password]);
    // Папка `app`: курсор на f3, отмечены f2 и f5.
    let app = urfin_net::SftpUri::new(&p, "/app").to_location();
    core.dispatch(Command::new("panel.go").arg("path", app));
    pump_until(&mut core, "app listed", |c| {
        loaded(c) && names(c).iter().any(|n| n == "f7.txt")
    });
    for n in ["f2.txt", "f5.txt"] {
        core.dispatch(Command::new("panel.cursor.to").arg("name", n));
        core.dispatch(Command::new("sel.toggle"));
    }
    core.dispatch(Command::new("panel.cursor.to").arg("name", "f3.txt"));
    core.pump(Duration::from_millis(5));
    assert_eq!(cursor(&core).as_deref(), Some("f3.txt"));
    assert_eq!(selected(&core), ["f2.txt", "f5.txt"]);
    let before = names(&core);

    // Сервер упал: обрыв, листинг и отметка на месте.
    drop(srv);
    pump_until(&mut core, "lost", |c| {
        matches!(state(c), Some(LiveState::Lost { .. }))
    });
    let Some(LiveState::Lost {
        attempt,
        tries,
        retry_at,
    }) = state(&core)
    else {
        unreachable!()
    };
    assert_eq!((attempt, tries), (1, 5));
    assert!(retry_at.is_some());
    assert_eq!(names(&core), before, "обрыв не стирает листинг");
    assert_eq!(selected(&core), ["f2.txt", "f5.txt"]);

    // Поднялся на том же порту: переподключение и перечитывание на месте.
    let rev = core.state().active_tab().rev;
    std::fs::write(dir.join("f8.txt"), b"new").unwrap();
    let _srv = TestSrv::start_on(port, opts(root.path()));
    pump_until(&mut core, "reconnected", |c| {
        state(c) == Some(LiveState::Connected)
            && loaded(c)
            && c.state().active_tab().rev != rev
            && names(c).iter().any(|n| n == "f8.txt")
    });
    assert_eq!(cursor(&core).as_deref(), Some("f3.txt"));
    assert_eq!(selected(&core), ["f2.txt", "f5.txt"]);
    assert!(
        core.state().active_tab().loc.path.ends_with("app"),
        "папка та же"
    );
}

/// F03-15: `key` и `start_dir` из `ssh.connect` — в профиле; вход этим ключом, панель
/// открывается в стартовой папке.
#[test]
fn form_key_and_start_dir_reach_profile_and_panel() {
    use russh::keys::ssh_key::LineEnding;
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("app").join("logs")).unwrap();
    std::fs::write(root.path().join("app").join("logs").join("a.log"), b"x").unwrap();
    let id = local.path().join("id_17");
    std::fs::write(&id, key(17).to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    let srv = TestSrv::start(Opts {
        password: None,
        keys: vec![key(17).public_key().clone()],
        ..opts(root.path())
    });
    let mut core = setup(local.path());
    core.pump(Duration::from_millis(5));
    core.dispatch(
        Command::new("ssh.connect")
            .arg("host", "127.0.0.1")
            .arg("port", srv.port.to_string())
            .arg("user", "tester")
            .arg("auth", "key")
            .arg("key", id.display().to_string())
            .arg("start_dir", "/app/logs")
            .arg("save", false)
            .arg("side", "left"),
    );
    pump_until(&mut core, "connected in start_dir", |c| {
        state(c) == Some(LiveState::Connected) && loaded(c) && names(c).iter().any(|n| n == "a.log")
    });
    let p = sftp_profile(&core.state().active_tab().loc).unwrap();
    let prof = model(&core).profile(&p).unwrap();
    assert_eq!(prof.key, id.display().to_string());
    assert_eq!(prof.start_dir, "/app/logs");
    assert_eq!(
        model(&core).live(&p).unwrap().login,
        [urfin_core::net::Login::Key(id.clone())]
    );
    assert!(
        core.state().active_tab().loc.path.ends_with("logs"),
        "{:?}",
        core.state().active_tab().loc
    );
}
