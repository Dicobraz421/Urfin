//! Менеджер сеансов: выбор shell по умолчанию, cwd, клавиши; пул на настоящих
//! cmd и Windows PowerShell (приёмка S6-10).

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use urfin_core::{Boot, Command, Core, Services, Waker};

use super::*;
use crate::shells::{PathStyle, ShellKind};

fn spec(id: &str) -> ShellSpec {
    ShellSpec {
        id: id.into(),
        title: id.into(),
        kind: ShellKind::Other,
        program: PathBuf::from(format!("{id}.exe")),
        args: Vec::new(),
        env: Vec::new(),
        integration: false,
        path_style: PathStyle::Windows,
    }
}

fn with_shells(ids: &[&str]) -> Manager {
    let mut m = Manager::new(None);
    m.shells = Some(ids.iter().map(|i| spec(i)).collect());
    m
}

/// №52: настройка → статистика → pwsh → cmd → первый.
#[test]
fn default_shell_order() {
    let cfg = TermConfig::default();
    let id = |m: &Manager, c: &TermConfig| m.default_shell(c).map(|s| s.id.to_string());
    let mut m = with_shells(&["cmd", "powershell", "pwsh", "wsl:Ubuntu"]);
    assert_eq!(id(&m, &cfg).as_deref(), Some("pwsh"), "first run: pwsh 7");
    m.usage.insert("wsl:Ubuntu".into(), 2);
    m.usage.insert("cmd".into(), 2);
    assert_eq!(
        id(&m, &cfg).as_deref(),
        Some("cmd"),
        "most used, tie → list order"
    );
    m.usage.insert("wsl:Ubuntu".into(), 3);
    assert_eq!(id(&m, &cfg).as_deref(), Some("wsl:Ubuntu"));
    let set = TermConfig {
        default_shell: "powershell".into(),
        ..TermConfig::default()
    };
    assert_eq!(id(&m, &set).as_deref(), Some("powershell"), "setting wins");
    let gone = TermConfig {
        default_shell: "fish".into(),
        ..TermConfig::default()
    };
    assert_eq!(
        id(&m, &gone).as_deref(),
        Some("wsl:Ubuntu"),
        "missing → stats"
    );
    let m = with_shells(&["powershell", "cmd"]);
    assert_eq!(id(&m, &cfg).as_deref(), Some("cmd"), "no pwsh → cmd");
    let m = with_shells(&["gitbash"]);
    assert_eq!(id(&m, &cfg).as_deref(), Some("gitbash"));
}

#[test]
fn cwd_is_nearest_local() {
    let local = Location::local(r"C:\work\src");
    assert_eq!(session_cwd(&local), Some(PathBuf::from(r"C:\work\src")));
    let zip = Location {
        backend: "zip".into(),
        path: PathBuf::from("inner/dir"),
        outer: Some(Arc::new(Location::local(r"C:\work\a.zip"))),
    };
    assert_eq!(session_cwd(&zip), Some(PathBuf::from(r"C:\work")));
    let sftp = Location {
        backend: "sftp".into(),
        path: PathBuf::from("/home"),
        outer: None,
    };
    assert_eq!(session_cwd(&sftp), None);
}

#[test]
fn key_names_and_mods() {
    let m = input::Modes::default();
    assert_eq!(key_bytes("Enter", None, m), Some(b"\r".to_vec()));
    assert_eq!(key_bytes("c", Some("ctrl"), m), Some(vec![3]));
    assert_eq!(
        key_bytes("up", Some("Ctrl+Shift"), m),
        Some(b"\x1b[1;6A".to_vec())
    );
    assert_eq!(key_bytes("up", Some("hyper"), m), None);
}

#[test]
fn usage_roundtrip() {
    let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
    let p = d.path().join("term").join(USAGE_FILE);
    let mut u = BTreeMap::new();
    u.insert(CompactString::from("wsl:Ubuntu"), 3);
    u.insert(CompactString::from("cmd"), 1);
    save_usage(&p, &u).unwrap();
    assert_eq!(load_usage(&p), u);
    assert!(load_usage(&d.path().join("none.toml")).is_empty());
}

#[test]
fn nonce_is_128_bit_hex_and_differs() {
    let (a, b) = (nonce(), nonce());
    assert_eq!(a.len(), 32);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _: Duration) {}
}

struct H {
    core: Core,
    /// Каталог панели: полное имя (`TEMP` бывает с короткими 8.3, shell — с длинными).
    home: PathBuf,
    _tmp: tempfile::TempDir,
}

impl H {
    fn new() -> H {
        H::with(TermConfig::default())
    }

    fn with(cfg: TermConfig) -> H {
        let dir = tempfile::Builder::new()
            .prefix("urfin-term-")
            .tempdir()
            .unwrap();
        let full = std::fs::canonicalize(dir.path()).unwrap();
        let home = full
            .to_str()
            .and_then(|s| s.strip_prefix(r"\\?\"))
            .map_or(full.clone(), PathBuf::from);
        let root = Location::local(&home);
        let mut core = Core::new(
            Boot::new(root.clone(), root),
            Services::null(),
            Arc::new(NoWake),
        );
        crate::register_with(&mut core, cfg, Some(home.join("term")));
        H {
            core,
            home,
            _tmp: dir,
        }
    }

    fn dir(&self) -> PathBuf {
        self.home.clone()
    }

    fn slot(&self) -> &Slot {
        self.mgr().active().unwrap()
    }

    /// Набрать в cmdline и выполнить (submit S6-15 с ленивым cd).
    fn submit(&mut self, text: &str) {
        self.run(Command::new("cmdline.insert").arg("text", text));
        self.run(Command::new("cmdline.exec"));
    }

    fn mgr(&self) -> &Manager {
        &self.core.state().ext.get::<TermState>().unwrap().sessions
    }

    fn until(&mut self, what: &str, done: impl Fn(&Manager) -> bool) {
        let t0 = Instant::now();
        loop {
            self.core.pump(Duration::from_millis(1));
            if self.core.state().ext.get::<TermState>().is_some() && done(self.mgr()) {
                return;
            }
            if t0.elapsed() > Duration::from_secs(30) {
                let a = self.mgr().active();
                panic!(
                    "timeout: {what}; cwd {:?}; screen:\n{}",
                    a.and_then(|s| s.cwd.clone()),
                    a.map(screen).unwrap_or_default().trim_end()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn run(&mut self, cmd: Command) {
        self.core.dispatch(cmd);
        self.core.pump(Duration::from_millis(1));
    }

    fn input(&mut self, text: &str) {
        self.run(Command::new("term.input").arg("text", text));
    }
}

fn screen(slot: &Slot) -> String {
    let Some(sh) = slot.shared() else {
        return String::new();
    };
    let vt = sh.vt.lock();
    let g = vt.term.grid();
    let top = -i32::try_from(g.history_size()).unwrap();
    let rows = i32::try_from(g.screen_lines()).unwrap();
    (top..rows)
        .map(|l| {
            let row = &g[Line(l)];
            (0..g.columns())
                .map(|c| row[Column(c)].c)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn active_has(m: &Manager, s: &str) -> bool {
    m.active().is_some_and(|a| screen(a).contains(s))
}

fn running(m: &Manager) -> bool {
    m.active().is_some_and(|a| a.state == SlotState::Running)
}

/// Приёмка S6-10 (без WSL: старт ВМ в тесте — десятки секунд): прогрев, переключение
/// cmd ↔ powershell с сохранением вывода, `exit` → строка и перезапуск по Enter,
/// выход при `ping -t` → вопрос.
#[test]
fn pool_switch_exit_restart_and_quit_veto() {
    let mut h = H::new();
    // Прогрев: shell по умолчанию стартует сам; без pwsh 7 на стенде — cmd.
    h.until("warm-up session", running);
    let warm = h.mgr().active().unwrap().shell.clone();
    let has_pwsh = h.mgr().shells().iter().any(|s| s.id == "pwsh");
    assert_eq!(warm, if has_pwsh { "pwsh" } else { "cmd" });
    let first = h.mgr().active().unwrap().id;
    h.run(Command::new("term.shell").arg("id", "cmd"));
    h.until("cmd active", running);
    let cmd_id = h.mgr().active().unwrap().id;
    if warm == "cmd" {
        assert_eq!(cmd_id, first, "reuse the warm cmd session");
    }
    h.input("echo URFIN_ONE\r");
    h.until("cmd output", |m| active_has(m, "URFIN_ONE"));

    h.run(Command::new("term.shell").arg("id", "powershell"));
    h.until("powershell running", |m| {
        running(m) && m.active().is_some_and(|a| a.shell == "powershell")
    });
    h.input("echo URFIN_TWO\r");
    h.until("powershell output", |m| active_has(m, "URFIN_TWO"));

    h.run(Command::new("term.shell").arg("id", "cmd"));
    assert_eq!(h.mgr().active().unwrap().id, cmd_id);
    assert!(active_has(h.mgr(), "URFIN_ONE"), "cmd output kept");
    let ps = h
        .mgr()
        .slots()
        .iter()
        .find(|s| s.shell == "powershell")
        .unwrap();
    assert!(screen(ps).contains("URFIN_TWO"), "background output kept");
    let ps_id = ps.id;
    h.run(Command::new("term.next"));
    assert_ne!(h.mgr().active().unwrap().id, cmd_id);

    // exit → строка «процесс завершён», Enter — новый сеанс того же shell.
    h.run(Command::new("term.shell").arg("id", "cmd"));
    h.input("exit 3\r");
    h.until("cmd exited", |m| {
        m.active()
            .is_some_and(|a| a.state == SlotState::Exited(Some(3)))
    });
    let line = tr("term-exited", &[("code", "3")]);
    assert!(
        active_has(h.mgr(), &line),
        "{}",
        screen(h.mgr().active().unwrap())
    );
    h.run(Command::new("term.key").arg("key", "enter"));
    h.until("cmd restarted", running);
    let restarted = h.mgr().active().unwrap();
    assert_eq!(restarted.shell, "cmd");
    assert_ne!(restarted.id, cmd_id);

    // ping -t → выход спрашивает; kill — вопроса нет.
    h.until("prompt after restart", |m| active_has(m, ">"));
    h.input("ping -t 127.0.0.1\r");
    h.until("busy", |m| m.active().is_some_and(Slot::busy));
    assert!(matches!(h.core.quit_decision(), QuitDecision::Veto(_)));
    h.run(Command::new("term.kill"));
    h.until("killed", |m| {
        m.active()
            .is_some_and(|a| matches!(a.state, SlotState::Exited(_)))
    });
    assert!(h.mgr().slots().iter().any(|s| s.id == ps_id));
    assert!(matches!(h.core.quit_decision(), QuitDecision::Allow));
    assert!(h.mgr().slots().is_empty(), "allowed quit closes the pool");
}

fn panels_events(core: &mut Core) -> Vec<Option<bool>> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            // Переоценка фокуса (`refocus`) — не просьба спрятать или показать.
            urfin_core::UiEvent::Ext { id, args }
                if id.as_str() == "term.panels" && args.bool("refocus").is_none() =>
            {
                Some(args.bool("show"))
            }
            _ => None,
        })
        .collect()
}

/// Приёмка S6-15/S6-08 на настоящем cmd: размер области — у PTY и сетки; строка
/// набирается командами `cmdline.*` и уходит в shell по submit; alt-screen прячет
/// панели, выход из него — возвращает.
#[test]
fn submit_area_and_alt_screen_autoshow() {
    let mut h = H::new();
    h.until("warm-up session", running);
    h.run(Command::new("term.shell").arg("id", "cmd"));
    h.until("cmd active", running);
    h.run(Command::new("term.resize").arg("cols", 100).arg("rows", 40));
    h.until("area applied", |m| {
        m.area()
            == Some(PtySize {
                cols: 100,
                rows: 40,
            })
            && m.active().and_then(Slot::shared).is_some_and(|s| {
                s.size()
                    == PtySize {
                        cols: 100,
                        rows: 40,
                    }
            })
    });

    assert!(crate::cmdline_empty(h.core.state()));
    h.run(Command::new("cmdline.insert").arg("text", "echo URFIN_SUBMIT"));
    assert!(!crate::cmdline_empty(h.core.state()));
    h.run(Command::new("cmdline.exec"));
    assert!(crate::cmdline_empty(h.core.state()));
    h.until("submitted output", |m| active_has(m, "URFIN_SUBMIT"));
    // История (S6-16): строка — в историю cmd и в файл; с ведущим пробелом — нет.
    h.submit(" echo URFIN_SECRET");
    h.until("secret ran", |m| active_has(m, "URFIN_SECRET"));
    let file = h.home.join("term").join("history").join("cmd.jsonl");
    let t0 = Instant::now();
    while !std::fs::read_to_string(&file).is_ok_and(|t| t.contains("URFIN_SUBMIT")) {
        assert!(t0.elapsed() < Duration::from_secs(10), "history file");
        h.core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!std::fs::read_to_string(&file).unwrap().contains("SECRET"));
    // Alt+F8: фильтр по набору, Enter — строка в cmdline.
    h.run(Command::new("cmdline.history"));
    assert!(crate::cmdline_menu(h.core.state()));
    h.run(Command::new("cmdline.insert").arg("text", "subm"));
    h.run(Command::new("cmdline.menu.accept"));
    assert!(!crate::cmdline_menu(h.core.state()));
    assert_eq!(
        h.core.state().ext.get::<TermState>().unwrap().line.text(),
        "echo URFIN_SUBMIT"
    );
    // Ghost: набранное — начало строки истории; → принимает.
    h.run(Command::new("cmdline.clear"));
    h.run(Command::new("cmdline.insert").arg("text", "echo URF"));
    assert!(crate::cmdline_ghost(h.core.state()));
    h.run(Command::new("cmdline.cursor.right"));
    assert_eq!(
        h.core.state().ext.get::<TermState>().unwrap().line.text(),
        "echo URFIN_SUBMIT"
    );
    h.run(Command::new("cmdline.clear"));

    let _ = panels_events(&mut h.core);
    let shared = h.mgr().active().and_then(Slot::shared).unwrap().clone();
    shared.inject(b"\x1b[?1049h");
    let t0 = Instant::now();
    let mut seen = Vec::new();
    while seen.is_empty() && t0.elapsed() < Duration::from_secs(5) {
        h.core.pump(Duration::from_millis(1));
        seen = panels_events(&mut h.core);
    }
    assert_eq!(seen, [Some(false)], "alt-screen hides panels");
    shared.inject(b"\x1b[?1049l");
    let t0 = Instant::now();
    seen.clear();
    while seen.is_empty() && t0.elapsed() < Duration::from_secs(5) {
        h.core.pump(Duration::from_millis(1));
        seen = panels_events(&mut h.core);
    }
    assert_eq!(seen, [Some(true)], "leaving alt-screen returns panels");

    // Многострочная вставка в cmd (без bracketed paste): сначала вопрос; «Вставить» —
    // строки в shell, панели гаснут. Одна строка с переводом в конце — в cmdline.
    h.run(Command::new("cmdline.paste").arg("text", "echo URFIN_L1\r\necho URFIN_L2\r\n"));
    assert!(
        crate::cmdline_empty(h.core.state()),
        "multi-line skips cmdline"
    );
    let id = h
        .core
        .state()
        .prompts
        .active()
        .map(|p| p.id)
        .expect("question");
    let _ = panels_events(&mut h.core);
    h.run(
        Command::new("prompt.reply")
            .arg("id", i64::try_from(id.0).unwrap())
            .arg("choice", "paste"),
    );
    h.until("pasted lines ran", |m| {
        active_has(m, "URFIN_L1") && active_has(m, "URFIN_L2")
    });
    assert_eq!(panels_events(&mut h.core), [Some(false)], "terminal shown");
    h.run(Command::new("cmdline.paste").arg("text", "dir\r\n"));
    assert_eq!(
        h.core.state().ext.get::<TermState>().unwrap().line.text(),
        "dir"
    );
}

fn is_dir(m: &Manager, p: &Path) -> bool {
    m.active()
        .and_then(|s| s.cwd.as_deref())
        .is_some_and(|c| same_path(c, p))
}

fn prompt_ready(m: &Manager) -> bool {
    m.active()
        .is_some_and(|s| s.integrated && !s.running && s.prompt.is_some_and(|(_, b)| b.is_some()))
}

fn exit_is(m: &Manager, code: Option<i32>) -> bool {
    m.active().is_some_and(|s| s.last_exit == Some(code))
}

/// Приёмка S6-11 на Windows PowerShell 5.1 (pwsh 7 — тот же скрипт): маркеры и cwd,
/// коды выхода в `D`, поддельный `FnCwd` без nonce не действует, промпт для cmdline,
/// тихий cd из панели без следа в выводе; с `-ExecutionPolicy AllSigned` интеграция
/// тоже работает.
#[test]
fn powershell_integration() {
    let mut h = H::new();
    h.until("warm-up session", running);
    h.run(Command::new("term.shell").arg("id", "powershell"));
    h.until("powershell prompt", prompt_ready);
    let home = h.dir();
    h.until("start cwd", |m| is_dir(m, &home));
    assert!(h.slot().hooks, "PSReadLine hooks available");

    h.input("cd C:\\Windows\r");
    h.until("cwd C:\\Windows", |m| is_dir(m, Path::new(r"C:\Windows")));
    h.input("cmd /c exit 3\r");
    h.until("exit code 3", |m| exit_is(m, Some(3)));
    h.input("Write-Output ok\r");
    h.until("exit code 0", |m| exit_is(m, Some(0)));
    h.input("Write-Host \"$([char]27)]633;P;FnCwd=bad;D:\\$([char]7)\"; cmd /c exit 5\r");
    h.until("prompt after forged marker", |m| {
        prompt_ready(m) && exit_is(m, Some(5))
    });
    assert!(
        is_dir(h.mgr(), Path::new(r"C:\Windows")),
        "forged FnCwd ignored"
    );

    let d = crate::vt::grid::Defaults {
        fg: [200; 3],
        bg: [0; 3],
        cursor: [255; 3],
    };
    let st = h.core.state().ext.get::<TermState>().unwrap();
    let prompt: String = st.prompt_cells(&d).unwrap().iter().map(|c| c.ch).collect();
    assert!(prompt.starts_with(r"PS C:\Windows>"), "{prompt:?}");
    assert!(st.prompt_row().is_some());

    // Панель пошла за shell (S6-14); вернуть её — submit сначала тихо переходит туда.
    h.panel_at(Path::new(r"C:\Windows"));
    h.go(&home);
    h.submit("Write-Output \"URFIN_Q=$PWD\"");
    let want = format!("URFIN_Q={}", home.display());
    h.until("quiet cd before command", |m| active_has(m, &want));
    h.until("cwd follows the panel", |m| is_dir(m, &home));
    assert!(
        !active_has(h.mgr(), "Set-Location"),
        "quiet cd leaves no trace"
    );

    // AllSigned: -EncodedCommand политике не подчиняется.
    let ps = h
        .mgr()
        .shells()
        .iter()
        .find(|s| s.id == "powershell")
        .unwrap()
        .program
        .display()
        .to_string();
    let mut cfg = TermConfig::default();
    cfg.shells.push(crate::config::ShellConfig {
        id: "ps-signed".into(),
        title: "ps-signed".into(),
        program: ps.as_str().into(),
        args: vec![
            "-NoLogo".into(),
            "-ExecutionPolicy".into(),
            "AllSigned".into(),
        ],
        kind: "powershell".into(),
        integration: true,
    });
    let mut h2 = H::with(cfg);
    h2.until("warm-up session", running);
    h2.run(Command::new("term.shell").arg("id", "ps-signed"));
    h2.until("AllSigned prompt", prompt_ready);
}

/// Приёмка S6-12: cmd с `PROMPT`-инъекцией — cwd из `$P`, видимый `cd /d` перед
/// командой, когда панель в другом каталоге; перетёртый `PROMPT` (как Clink) —
/// через 5 с без интеграции, с тостом.
#[test]
fn cmd_integration_and_silent_fallback() {
    let comspec =
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let mut cfg = TermConfig::default();
    cfg.shells.push(crate::config::ShellConfig {
        id: "cmd-clink".into(),
        title: "cmd-clink".into(),
        program: comspec.as_str().into(),
        args: vec!["/k".into(), "prompt $P$G".into()],
        kind: "cmd".into(),
        integration: true,
    });
    let mut h = H::with(cfg);
    h.until("warm-up session", running);
    h.run(Command::new("term.shell").arg("id", "cmd"));
    h.until("cmd prompt", prompt_ready);
    let home = h.dir();
    // Сеанс прогрева — в каталоге процесса; cwd приходит с первым промптом.
    h.until("cwd reported", |m| {
        m.active().is_some_and(|s| s.cwd.is_some())
    });
    h.input("cd /d C:\\Windows\r");
    h.until("cwd C:\\Windows", |m| is_dir(m, Path::new(r"C:\Windows")));
    h.panel_at(Path::new(r"C:\Windows"));
    h.go(&home);
    h.submit("echo URFIN_C=%CD%");
    let want = format!("URFIN_C={}", home.display());
    h.until("visible cd before command", |m| active_has(m, &want));
    assert!(active_has(h.mgr(), "cd /d \""), "visible cd");
    assert_eq!(h.slot().last_exit, Some(None), "cmd reports no exit code");

    let _ = h.core.take_ui_events();
    h.run(Command::new("term.shell").arg("id", "cmd-clink"));
    h.until("custom prompt cmd running", running);
    let t0 = Instant::now();
    let mut toast = false;
    while !toast && t0.elapsed() < Duration::from_secs(15) {
        h.core.pump(Duration::from_millis(1));
        toast = h.core.take_ui_events().iter().any(
            |e| matches!(e, urfin_core::UiEvent::Toast { text, .. } if text.contains("cmd-clink")),
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(toast, "no-integration toast");
    assert!(!h.slot().integrated);
}

/// Приёмка S6-13 (Git Bash; WSL — ручная проверка): rcfile подключил
/// login-окружение (`/etc/profile` задаёт `MSYSTEM`), cwd `/c/Windows` →
/// `C:\Windows`, коды выхода, тихий cd из панели через `bind -x`. Без Git Bash на
/// стенде — пропуск.
#[test]
fn git_bash_integration() {
    let mut h = H::new();
    h.until("shells found", |m| !m.shells().is_empty());
    if !h.mgr().shells().iter().any(|s| s.id == "gitbash") {
        eprintln!("no Git Bash: skipped");
        return;
    }
    h.run(Command::new("term.shell").arg("id", "gitbash"));
    h.until("bash prompt", prompt_ready);
    h.input("echo URFIN_ENV=$MSYSTEM\r");
    // MSYSTEM задаёт /etc/profile: MINGW64 у установщика, UCRT64 у раннеров CI.
    h.until("login environment", |m| {
        ["MINGW", "UCRT", "CLANG", "MSYS"]
            .iter()
            .any(|v| active_has(m, &format!("URFIN_ENV={v}")))
    });
    h.input("cd /c/Windows\r");
    h.until("cwd C:\\Windows", |m| is_dir(m, Path::new(r"C:\Windows")));
    h.input("(exit 4)\r");
    h.until("exit code 4", |m| exit_is(m, Some(4)));
    let home = h.dir();
    h.panel_at(Path::new(r"C:\Windows"));
    h.go(&home);
    h.submit("echo URFIN_B=done");
    h.until("command after quiet cd", |m| active_has(m, "URFIN_B=done"));
    h.until("cwd back to the panel", |m| is_dir(m, &home));
    // Bracketed paste (S6-07): readline включил DEC 2004 — строки вставки не
    // выполняются по одной и без вопроса; Enter выполняет обе.
    h.until("bracketed paste on", |m| m.input_modes().bracketed_paste);
    h.run(Command::new("term.paste").arg("text", "echo URFIN_P1\necho URFIN_P2"));
    assert!(h.core.state().prompts.active().is_none(), "no question");
    h.input("\r");
    h.until("both pasted lines ran", |m| {
        active_has(m, "URFIN_P1") && active_has(m, "URFIN_P2")
    });
}

/// Приёмка S6-19: Enter по `.bat` — в cmd-сеансе из каталога файла, с выводом.
#[test]
fn enter_on_batch_runs_in_session() {
    let mut h = H::new();
    h.until("warm-up session", running);
    h.run(Command::new("term.shell").arg("id", "cmd"));
    h.until("cmd prompt", prompt_ready);
    let dir = h.dir().join("bin dir");
    std::fs::create_dir_all(&dir).unwrap();
    let bat = dir.join("urfin t.bat");
    std::fs::write(&bat, "@echo URFIN_BAT %CD%\r\n").unwrap();
    h.run(Command::new("term.open").arg("path", bat.display().to_string()));
    h.until("batch output", |m| active_has(m, "URFIN_BAT"));
    h.until("ran from its folder", |m| is_dir(m, &dir));
}

impl H {
    fn go(&mut self, p: &Path) {
        self.run(Command::new("panel.go").arg("path", Location::local(p)));
    }

    fn panel_at(&mut self, p: &Path) {
        let t0 = Instant::now();
        while !same_path(&self.core.state().active_tab().loc.path, p) {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "panel at {:?}, want {}",
                self.core.state().active_tab().loc.path,
                p.display()
            );
            self.core.pump(Duration::from_millis(1));
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Приёмка S6-14 на настоящем Windows PowerShell: 50 быстрых навигаций панели — 0
/// записей в PTY, затем submit — ровно один тихий cd; `cd ..` в shell сдвигает
/// панель, обратного cd нет.
#[test]
fn cwd_sync_both_ways() {
    let mut h = H::new();
    h.until("warm-up session", running);
    h.run(Command::new("term.shell").arg("id", "powershell"));
    h.until("powershell prompt", prompt_ready);
    let home = h.dir();
    h.until("start cwd", |m| is_dir(m, &home));
    let win = PathBuf::from(r"C:\Windows");
    let before = h.mgr().syncs;
    for i in 0..50 {
        h.go(if i % 2 == 0 { &home } else { &win });
    }
    h.panel_at(&win);
    assert_eq!(h.mgr().syncs, before, "no PTY writes while the panel moves");
    assert!(is_dir(h.mgr(), &home));

    h.submit("Write-Output URFIN_S1");
    h.until("command ran", |m| active_has(m, "URFIN_S1"));
    h.until("shell followed the panel", |m| is_dir(m, &win));
    assert_eq!(h.mgr().syncs, before + 1, "exactly one quiet cd");

    h.input("cd ..\r");
    h.until("shell went up", |m| is_dir(m, Path::new(r"C:\")));
    h.panel_at(Path::new(r"C:\"));
    h.submit("Write-Output URFIN_S2");
    h.until("second command", |m| active_has(m, "URFIN_S2"));
    assert_eq!(h.mgr().syncs, before + 1, "no reverse cd");
}
