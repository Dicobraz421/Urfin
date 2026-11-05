//! Мультивыполнение на фейковых PTY (F03-11): настоящие reader и writer сеанса, порт
//! копит ввод; команды — через `Core`, как от клавиатуры.

use std::io;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use urfin_core::net::{NetModel, TermLink, TermLogin};
use urfin_core::{
    Boot, CmdError, Command, CommandMeta, Core, Cx, Focus, Handler, Location, PolicySet, Services,
    Waker,
};

use super::*;
use crate::manager::{External, SessionKind};
use crate::session::{PtyPort, Session, SessionEvent, SessionOpts, SessionSink, start_pipeline};
use crate::sys::conpty::PtySize;

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _: Duration) {}
}

struct Sink;

impl SessionSink for Sink {
    fn poke(&self) {}
    fn event(&self, _: SessionEvent) {}
}

/// PTY-порт: копит ввод.
struct Port(Arc<Mutex<Vec<u8>>>);

impl PtyPort for Port {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }

    fn resize(&mut self, _size: PtySize) {}

    fn close(&mut self) {}
}

/// Сеанс с фейковым PTY; `w` держит вывод открытым — reader не выходит.
struct Fake {
    typed: Arc<Mutex<Vec<u8>>>,
    _w: io::PipeWriter,
}

fn fake_session(id: u32) -> (Session, Fake) {
    let (r, w) = io::pipe().unwrap();
    let typed = Arc::new(Mutex::new(Vec::new()));
    let spawn = |name: String, f: Box<dyn FnOnce() + Send>| {
        thread::Builder::new().name(name).spawn(f).unwrap();
    };
    let opts = SessionOpts {
        size: PtySize { cols: 40, rows: 8 },
        scrollback: 100,
        osc52: "off".into(),
        nonce: None,
    };
    let p = start_pipeline(
        &spawn,
        &id.to_string(),
        r,
        Port(typed.clone()),
        || None,
        &opts,
        Arc::new(Sink),
        Arc::default(),
    );
    (Session::fake(id, p), Fake { typed, _w: w })
}

/// Команды `ssh.group.*` — как их зовёт `urfin-net`.
struct Ssh(bool);

static SSH: [CommandMeta; 3] = [
    CommandMeta {
        id: "ssh.group.edit",
        title: "edit",
        category: "ssh",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "ssh.group.toggle",
        title: "toggle",
        category: "ssh",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "ssh.group.leave",
        title: "leave",
        category: "ssh",
        args: &[],
        repeatable: false,
    },
];

impl Handler for Ssh {
    fn namespace(&self) -> &'static str {
        "ssh"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &SSH
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "ssh.group.edit" => edit(cx, self.0),
            "ssh.group.toggle" => toggle(cx, None),
            _ => leave(cx),
        }
    }
}

/// Сцена теста: сеансы и модель ставит команда — у `Core` нет доступа к состоянию снаружи.
struct Setup(Mutex<Vec<(u32, Session)>>);

static SETUP: [CommandMeta; 4] = [
    CommandMeta {
        id: "test.setup",
        title: "setup",
        category: "test",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "test.screen",
        title: "screen",
        category: "test",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "test.forbid",
        title: "forbid",
        category: "test",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "test.unlink",
        title: "unlink",
        category: "test",
        args: &[],
        repeatable: false,
    },
];

impl Handler for Setup {
    fn namespace(&self) -> &'static str {
        "test"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &SETUP
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "test.setup" => {
                let mut m = NetModel::default();
                let st = cx.state.ext.get_mut::<TermState>().unwrap();
                let cfg = st.config.clone();
                for (id, s) in self.0.lock().unwrap().drain(..) {
                    let ext = Arc::new(External {
                        argv: vec!["ssh.exe".into()],
                        env: Vec::new(),
                        title: format!("ssh srv{id}"),
                    });
                    st.sessions.push_fake(SessionKind::External(ext), s);
                    m.terms.push(TermLink {
                        id,
                        profile: format!("srv{id}"),
                        n: 1,
                        login: TermLogin::Password,
                    });
                }
                st.sessions.set_active(&cfg, 1);
                cx.state.ext.insert(m);
                cx.state.focus = Focus::Terminal;
            }
            // Окно спрятало панели (`ui.panels.toggle_all`).
            "test.screen" => {
                if let Some(g) = cx
                    .state
                    .ext
                    .get_mut::<NetModel>()
                    .and_then(|m| m.group.as_mut())
                {
                    g.on_screen = true;
                }
            }
            "test.forbid" => {
                let (p, _) = PolicySet::from_toml("[ssh]\nmulti_exec = false\n");
                cx.state.ext.insert(p);
            }
            // `urfin-net` убрал закрытый сеанс из `terms`.
            _ => {
                if let Some(m) = cx.state.ext.get_mut::<NetModel>() {
                    m.terms.pop();
                }
            }
        }
        Ok(())
    }
}

/// Настройки «SSH» (F03-14): `[term.highlight]` нового конфига — на лету; Ctrl+Alt+H пишет
/// тот же ключ, и перечитывание его не сбрасывает.
#[test]
fn highlight_follows_config_and_toggle_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = urfin_core::Paths::in_dir(tmp.path());
    let file = paths.config_file();
    let root = Location::local(std::env::temp_dir());
    let mut boot = Boot::new(root.clone(), root);
    boot.paths = paths.clone();
    let mut core = Core::new(boot, Services::null(), Arc::new(NoWake));
    crate::register(&mut core, crate::TermConfig::default());
    let mut pump_until = |core: &mut Core, what: &dyn Fn(&crate::hl::HighlightConfig) -> bool| {
        let t = Instant::now();
        loop {
            core.pump(Duration::from_millis(1));
            let st = core.state().ext.get::<TermState>();
            if st.is_some_and(|s| what(&s.config.highlight)) {
                return;
            }
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "highlight not applied"
            );
            thread::sleep(Duration::from_millis(2));
        }
    };
    pump_until(&mut core, &|h| h.enabled && !h.rules.hashes);
    std::fs::write(
        &file,
        "[term.highlight]\nenabled = false\nrules = { hashes = true }\n",
    )
    .unwrap();
    core.dispatch(Command::new("app.reload_config"));
    pump_until(&mut core, &|h| !h.enabled && h.rules.hashes);

    core.dispatch(Command::new("term.highlight.toggle"));
    pump_until(&mut core, &|h| h.enabled);
    core.dispatch(Command::new("app.reload_config"));
    let t = Instant::now();
    while std::fs::read_to_string(&file).is_ok_and(|s| !s.contains("enabled = true")) {
        assert!(t.elapsed() < Duration::from_secs(10), "key not written");
        core.pump(Duration::from_millis(1));
        thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..50 {
        core.pump(Duration::from_millis(1));
        thread::sleep(Duration::from_millis(1));
    }
    let st = core.state().ext.get::<TermState>().unwrap();
    assert!(st.config.highlight.enabled, "reload keeps the toggle");
    let _ = core.shutdown(Duration::from_millis(500));
}

struct H {
    core: Core,
    ptys: Vec<Fake>,
}

impl H {
    fn with(n: u32, ask: bool) -> H {
        let root = Location::local(std::env::temp_dir());
        let mut core = Core::new(
            Boot::new(root.clone(), root),
            Services::null(),
            Arc::new(NoWake),
        );
        crate::register(&mut core, crate::TermConfig::default());
        let mut sessions = Vec::new();
        let mut ptys = Vec::new();
        for id in 1..=n {
            let (s, f) = fake_session(id);
            sessions.push((id, s));
            ptys.push(f);
        }
        core.register(Box::new(Ssh(ask)));
        core.register(Box::new(Setup(Mutex::new(sessions))));
        core.pump(Duration::from_millis(1));
        let mut h = H { core, ptys };
        h.run(Command::new("test.setup"));
        h
    }

    fn run(&mut self, c: Command) {
        self.core.dispatch(c);
        self.core.pump(Duration::from_millis(1));
    }

    fn key(&mut self, key: &str) {
        self.run(Command::new("term.key").arg("key", key));
    }

    fn input(&mut self, text: &str) {
        self.run(Command::new("term.input").arg("text", text));
    }

    /// Группа из первых двух-трёх сеансов, клавиши — вводу.
    fn group(&mut self, members: usize) {
        self.run(Command::new("ssh.group.edit"));
        self.run(Command::new("test.screen"));
        for _ in 0..members {
            self.input(" ");
            self.key("down");
        }
        self.key("enter");
    }

    fn model(&self) -> &NetModel {
        self.core.state().ext.get::<NetModel>().unwrap()
    }

    fn g(&self) -> &Group {
        self.model().group.as_ref().unwrap()
    }

    fn typed(&self, i: usize) -> Vec<u8> {
        self.ptys[i].typed.lock().unwrap().clone()
    }

    /// Writer асинхронный: ждать, пока в PTY `i` дойдёт `want`.
    fn wait(&self, i: usize, want: &[u8]) {
        let t0 = Instant::now();
        while self.typed(i) != want {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "pty {i}: {:?}, want {want:?}",
                String::from_utf8_lossy(&self.typed(i))
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Приёмка: ввод попадает во все PTY группы в одном порядке; вне группы — ни байта.
#[test]
fn input_reaches_every_pty_in_order() {
    let mut h = H::with(3, false);
    h.group(2);
    assert_eq!(h.g().members, [1, 2]);
    let mut want = Vec::new();
    for t in ["u", "p", "t", "и", "m", "e"] {
        h.input(t);
        want.extend_from_slice(t.as_bytes());
    }
    h.key("backspace");
    want.push(0x7f);
    h.key("enter");
    want.push(b'\r');
    h.wait(0, &want);
    h.wait(1, &want);
    assert!(h.typed(2).is_empty(), "session outside the group");
}

/// Приёмка: первый Enter задержан до подтверждения, дальше идёт без вопросов.
#[test]
fn first_enter_is_held_until_yes() {
    let mut h = H::with(2, true);
    h.group(2);
    h.input("ls");
    h.key("enter");
    assert!(h.g().asking());
    assert_eq!(h.g().typed, "ls ");
    h.wait(0, b"ls");
    h.input("x");
    h.wait(1, b"ls");
    h.key("enter");
    assert!(!h.g().asking() && h.g().confirmed.is_some());
    h.wait(0, b"ls\r");
    h.wait(1, b"ls\r");
    h.key("enter");
    h.wait(0, b"ls\r\r");
}

/// Приёмка: Esc в строке вопроса шлёт `Ctrl+U` во все сеансы; группа остаётся. Esc вне
/// вопроса — выход, в PTY не идёт ничего.
#[test]
fn esc_kills_the_line_then_leaves() {
    let mut h = H::with(2, true);
    h.group(2);
    h.input("rm");
    h.key("enter");
    h.run(Command::new("ssh.group.leave"));
    h.wait(0, b"rm\x15");
    h.wait(1, b"rm\x15");
    assert!(!h.g().asking());
    h.run(Command::new("ssh.group.leave"));
    assert!(h.model().group.is_none());
    h.wait(0, b"rm\x15");
    h.input("z");
    h.wait(0, b"rm\x15z");
    h.wait(1, b"rm\x15");
}

/// Смена состава снова спрашивает; набранное у колонки в PTY не идёт.
#[test]
fn list_owns_the_keys_and_change_asks_again() {
    let mut h = H::with(3, true);
    h.group(2);
    h.key("enter");
    h.key("enter");
    assert!(h.g().confirmed.is_some());
    h.run(Command::new("ssh.group.edit"));
    assert!(h.g().list);
    h.input("q");
    h.input(" ");
    assert_eq!(h.g().members, [1, 2, 3]);
    assert_eq!(h.g().confirmed, None, "the group changed");
    h.key("enter");
    assert!(!h.g().list);
    h.wait(0, b"\r");
    h.wait(2, b"");
    h.key("enter");
    assert!(h.g().asking());
}

/// `multi_exec_confirm = false`: без вопроса.
#[test]
fn confirm_off() {
    let mut h = H::with(2, false);
    h.group(2);
    h.key("enter");
    assert!(!h.g().asking());
    h.wait(1, b"\r");
}

/// Приёмка: при `ssh.multi_exec=false` команды выключены; запрет на лету снимает группу,
/// ввод снова идёт в один сеанс.
#[test]
fn policy_turns_commands_off() {
    let mut h = H::with(2, true);
    h.group(2);
    h.run(Command::new("test.forbid"));
    h.core.pump(Duration::from_millis(1));
    assert!(h.model().group.is_none());
    h.run(Command::new("ssh.group.edit"));
    assert!(h.model().group.is_none(), "edit rejected by the policy");
    h.input("a");
    h.wait(0, b"a");
    h.wait(1, b"");
}

/// Закрытый сеанс уходит из группы; один оставшийся — снова колонка.
#[test]
fn closed_session_leaves_the_group() {
    let mut h = H::with(2, true);
    h.group(2);
    h.run(Command::new("test.unlink"));
    assert_eq!(h.g().members, [1]);
    assert!(h.g().list);
    h.input("a");
    h.wait(0, b"");
}
