//! F3/F4 удалённых файлов (F03-08) на `testsrv`: правка и F2 меняют файл на сервере
//! атомарно (`.urfin-part` + `posix-rename`) с сохранением прав; изменение на сервере
//! между скачиванием и F2 — строка конфликта; обрыв связи посреди закачки — «ждёт
//! связи» и повтор; закрытие таба удаляет временную папку; F3 — без копии, страницы по
//! запросу; кириллица в имени и cp1251 в содержимом — байт в байт.

mod testsrv;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use urfin_core::net::{AskKind, LiveState, NetModel, sftp_profile};
use urfin_core::{Boot, Command, Core, ListingStatus, Location, Services, Side, Waker};
use urfin_net::{SftpFs, SftpUri, SshCfg};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};
use urfin_view::edit::remote::UpState;
use urfin_view::{
    EdAsk, EditorCfg, EditorDoc, InboxMsg, SourceKind, ViewEnv, ViewState, ViewerCfg,
};

use testsrv::{Opts, Shared, TestSrv};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn opts(root: &Path, shared: &Shared) -> Opts {
    Opts {
        host_seed: 9,
        password: Some("secret"),
        sftp_root: Some(root.to_path_buf()),
        owner: Some(("deploy", "deploy")),
        shared: shared.clone(),
        ..Opts::default()
    }
}

fn setup(left: &Path, temp: &Path) -> Core {
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
    urfin_vfs::register(&mut core, Arc::clone(&reg), cfg);
    let mut ssh = SshCfg::default();
    ssh.reconnect.interval_s = 1;
    urfin_net::register_shared(&mut core, ssh, None, fs, None);
    urfin_view::register_env(
        &mut core,
        ViewerCfg::default(),
        EditorCfg::default(),
        ViewEnv {
            vfs: reg,
            cache_dir: None,
            temp: Some(temp.to_path_buf()),
        },
    );
    core
}

fn model(core: &Core) -> &NetModel {
    core.state().ext.get::<NetModel>().expect("net model")
}

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

fn remote(p: &str, path: &str) -> Location {
    SftpUri::new(p, path).to_location()
}

fn view(core: &Core) -> &ViewState {
    core.state().ext.get::<ViewState>().expect("view state")
}

fn editor(core: &Core) -> Option<&EditorDoc> {
    let v = view(core);
    v.editor(v.active_editor()?)
}

/// F4 на файле сервера: документ на временной копии готов.
fn open_copy(core: &mut Core, loc: &Location) -> PathBuf {
    core.dispatch(Command::new("editor.open").arg("path", loc.clone()));
    pump_until(core, "copy opened", 30, |c| {
        editor(c)
            .is_some_and(|d| d.is_ready() && d.remote.as_deref().is_some_and(|r| r.stamp.is_some()))
    });
    let d = editor(core).unwrap();
    assert_eq!(d.shown_loc(), loc, "шапка и история — адрес сервера");
    d.loc.path.clone()
}

/// Весь текст документа — `text` (как ввод).
fn replace_all(core: &mut Core, text: &str) {
    core.dispatch(Command::new("editor.select_all"));
    let v = view(core);
    let doc = v.active_editor().unwrap();
    v.inbox.sender().send(InboxMsg::Input {
        doc,
        text: text.into(),
    });
    pump_until(core, "edited", 10, |c| editor(c).is_some_and(|d| d.dirty));
}

fn up_state(core: &Core) -> Option<UpState> {
    editor(core)?
        .remote
        .as_deref()?
        .up
        .as_ref()
        .map(|u| u.state.clone())
}

fn uploaded(core: &Core) -> bool {
    up_state(core) == Some(UpState::Done)
}

#[test]
fn edit_and_f2_upload_atomically_keep_mode_and_close_removes_temp() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("etc")).unwrap();
    std::fs::write(root.path().join("etc").join("app.conf"), b"listen 80;\n").unwrap();
    let shared = Shared::default();
    shared
        .modes
        .lock()
        .unwrap()
        .insert("/etc/app.conf".into(), 0o100_640);
    let srv = TestSrv::start(opts(root.path(), &shared));
    let mut core = setup(local.path(), temp.path());
    let p = connect(&mut core, srv.port);
    let loc = remote(&p, "/etc/app.conf");
    let copy = open_copy(&mut core, &loc);
    assert!(copy.starts_with(temp.path().join("sftp")), "{copy:?}");
    assert_eq!(std::fs::read(&copy).unwrap(), b"listen 80;\n");
    assert_eq!(
        view(&core)
            .server_chip(view(&core).active().unwrap())
            .as_deref(),
        Some(p.as_str())
    );

    replace_all(&mut core, "listen 443;\n");
    core.dispatch(Command::new("editor.save"));
    pump_until(&mut core, "uploaded", 30, uploaded);
    let dst = root.path().join("etc").join("app.conf");
    assert_eq!(std::fs::read(&dst).unwrap(), b"listen 443;\n");
    assert!(!root.path().join("etc").join("app.conf.urfin-part").exists());
    let mode = shared.modes.lock().unwrap().get("/etc/app.conf").copied();
    assert_eq!(mode.map(|m| m & 0o7777), Some(0o640), "права сохранены");
    let writes = shared.writes.lock().unwrap().clone();
    assert!(
        writes
            .iter()
            .all(|(path, _)| path == "/etc/app.conf.urfin-part"),
        "данные — только в part: {writes:?}"
    );
    let d = editor(&core).unwrap();
    assert!(!d.dirty && !d.unsynced());

    // Второй F2 без правок на сервере — снова без конфликта (отметка обновлена).
    replace_all(&mut core, "listen 8443;\n");
    core.dispatch(Command::new("editor.save"));
    pump_until(&mut core, "uploaded again", 30, |c| {
        uploaded(c)
            && std::fs::read(root.path().join("etc").join("app.conf")).unwrap() == b"listen 8443;\n"
    });

    let dir = copy.parent().unwrap().to_path_buf();
    core.dispatch(Command::new("editor.close"));
    pump_until(&mut core, "closed", 10, |c| editor(c).is_none());
    pump_until(&mut core, "temp removed", 10, |_| !dir.exists());
    let hist = view(&core)
        .history
        .get_kind(&loc, urfin_view::HistKind::Edit);
    assert!(hist.is_some(), "в истории правки — адрес сервера");
}

#[test]
fn server_change_between_download_and_f2_asks() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let dst = root.path().join("a.txt");
    std::fs::write(&dst, b"one\n").unwrap();
    let shared = Shared::default();
    let srv = TestSrv::start(opts(root.path(), &shared));
    let mut core = setup(local.path(), temp.path());
    let p = connect(&mut core, srv.port);
    open_copy(&mut core, &remote(&p, "/a.txt"));
    replace_all(&mut core, "mine\n");
    // Другой человек правит файл на сервере, пока копия открыта.
    std::fs::write(&dst, b"theirs, longer\n").unwrap();
    core.dispatch(Command::new("editor.save"));
    pump_until(&mut core, "conflict", 30, |c| {
        editor(c).is_some_and(|d| d.ask == Some(EdAsk::RemoteChanged))
    });
    assert_eq!(
        std::fs::read(&dst).unwrap(),
        b"theirs, longer\n",
        "сервер не тронут"
    );
    let d = editor(&core).unwrap();
    assert!(!d.dirty && d.unsynced(), "копия сохранена, не закачана");
    // Закрыть нельзя молча: копия не на сервере.
    core.dispatch(Command::new("editor.answer").arg("choice", "cancel"));
    core.dispatch(Command::new("editor.close"));
    core.pump(Duration::from_millis(5));
    assert_eq!(editor(&core).unwrap().ask, Some(EdAsk::Close));
    core.dispatch(Command::new("editor.answer").arg("choice", "cancel"));
    // F2 ещё раз — снова конфликт; «Перезаписать» — закачка без сверки.
    core.dispatch(Command::new("editor.save"));
    pump_until(&mut core, "conflict again", 30, |c| {
        editor(c).is_some_and(|d| d.ask == Some(EdAsk::RemoteChanged))
    });
    core.dispatch(Command::new("editor.answer").arg("choice", "overwrite"));
    pump_until(&mut core, "overwritten", 30, uploaded);
    assert_eq!(std::fs::read(&dst).unwrap(), b"mine\n");
}

#[test]
fn link_drop_mid_upload_waits_and_retries() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let dst = root.path().join("big.log");
    std::fs::write(&dst, vec![b'a'; 64 << 10]).unwrap();
    let shared = Shared::default();
    let slow = Opts {
        write_delay: Some(Duration::from_millis(20)),
        ..opts(root.path(), &shared)
    };
    let srv = TestSrv::start(slow);
    let port = srv.port;
    let mut core = setup(local.path(), temp.path());
    let p = connect(&mut core, port);
    open_copy(&mut core, &remote(&p, "/big.log"));
    let body = "b".repeat(3 << 20);
    replace_all(&mut core, &body);
    core.dispatch(Command::new("editor.save"));
    pump_until(&mut core, "upload started", 30, |_| {
        shared.written.load(Ordering::SeqCst) > 0
    });
    drop(srv);
    pump_until(&mut core, "waiting for link", 30, |c| {
        up_state(c) == Some(UpState::Waiting)
    });
    assert_eq!(std::fs::read(&dst).unwrap().len(), 64 << 10, "файл цел");
    let _srv = TestSrv::start_on(
        port,
        Opts {
            write_delay: None,
            ..opts(root.path(), &shared)
        },
    );
    pump_until(&mut core, "uploaded after reconnect", 120, uploaded);
    assert_eq!(std::fs::read(&dst).unwrap(), body.as_bytes());
    assert!(!root.path().join("big.log.urfin-part").exists());
}

#[test]
fn cyrillic_name_and_cp1251_bytes_round_trip() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("заметки")).unwrap();
    let dst = root.path().join("заметки").join("отчёт.txt");
    // «Привет, мир» в cp1251 и строка ASCII.
    let mut cp1251 = vec![
        0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, 0x2C, 0x20, 0xEC, 0xE8, 0xF0, b'\n',
    ];
    cp1251.extend_from_slice(b"end\n");
    std::fs::write(&dst, &cp1251).unwrap();
    let shared = Shared::default();
    let srv = TestSrv::start(opts(root.path(), &shared));
    let mut core = setup(local.path(), temp.path());
    let p = connect(&mut core, srv.port);
    let copy = open_copy(&mut core, &remote(&p, "/заметки/отчёт.txt"));
    assert_eq!(copy.file_name().unwrap(), "отчёт.txt");
    // Правка — в конце (`end` → `END`); байты cp1251 до неё не меняются.
    let v = view(&core);
    let doc = v.active_editor().unwrap();
    let n = cp1251.len() as u64;
    let tx = v.inbox.sender();
    tx.send(InboxMsg::Caret {
        doc,
        off: n - 4,
        extend: false,
        unit: urfin_view::edit::input::Unit::Char,
        block: None,
    });
    tx.send(InboxMsg::Caret {
        doc,
        off: n - 1,
        extend: true,
        unit: urfin_view::edit::input::Unit::Char,
        block: None,
    });
    tx.send(InboxMsg::Input {
        doc,
        text: "END".into(),
    });
    pump_until(&mut core, "edited", 10, |c| {
        editor(c).is_some_and(|d| d.dirty)
    });
    core.dispatch(Command::new("editor.save"));
    pump_until(&mut core, "uploaded", 30, uploaded);
    let mut want = cp1251[..cp1251.len() - 4].to_vec();
    want.extend_from_slice(b"END\n");
    assert_eq!(std::fs::read(&dst).unwrap(), want);
}

#[test]
fn f3_reads_pages_without_a_copy() {
    let local = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("huge.log"), vec![b'x'; 12 << 20]).unwrap();
    let shared = Shared::default();
    let srv = TestSrv::start(opts(root.path(), &shared));
    let mut core = setup(local.path(), temp.path());
    let p = connect(&mut core, srv.port);
    core.dispatch(
        Command::new("viewer.open")
            .arg("path", remote(&p, "/huge.log"))
            .arg("internal", true),
    );
    pump_until(&mut core, "viewer opened", 30, |c| {
        let v = view(c);
        v.active_viewer()
            .and_then(|id| v.viewer(id))
            .is_some_and(|d| d.kind.is_some())
    });
    let v = view(&core);
    let id = v.active_viewer().unwrap();
    assert_eq!(v.viewer(id).unwrap().kind, Some(SourceKind::Seek));
    assert_eq!(v.server_chip(id).as_deref(), Some(p.as_str()));
    assert!(
        std::fs::read_dir(temp.path()).map_or(true, |mut d| d.next().is_none()),
        "вьювер копии не делает"
    );
}
