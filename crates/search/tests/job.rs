//! S8-06: job и команды через `Core`, как `crates/net/tests/`.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use urfin_core::search::{SearchModel, SearchStore};
use urfin_core::{
    Boot, CmdError, Command, CommandMeta, CommandRegistry, Core, Cx, Handler, JobState, Location,
    Services, UiEvent, Waker,
};
use urfin_search::COMMANDS;
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Заместитель viewer и editor: запоминает, что открыли.
struct Opener {
    ns: &'static str,
    meta: &'static [CommandMeta],
    seen: Sender<Command>,
}

impl Handler for Opener {
    fn namespace(&self) -> &'static str {
        self.ns
    }
    fn commands(&self) -> &'static [CommandMeta] {
        self.meta
    }
    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        self.seen.send(cmd.clone()).unwrap();
        Ok(())
    }
}

const fn opener_meta(id: &'static str) -> CommandMeta {
    CommandMeta {
        id,
        title: "",
        category: "",
        args: &ARGS,
        repeatable: false,
    }
}

const ARGS: [urfin_core::ArgSpec; 2] = [
    urfin_core::ArgSpec {
        name: "path",
        kind: urfin_core::ValueKind::Loc,
        required: true,
    },
    urfin_core::ArgSpec {
        name: "line",
        kind: urfin_core::ValueKind::Int,
        required: false,
    },
];
static VIEWER: [CommandMeta; 1] = [opener_meta("viewer.open")];
static EDITOR: [CommandMeta; 1] = [opener_meta("editor.open")];

fn core_with(start: &Path, config: Option<&str>, dir: Option<&Path>) -> (Core, Receiver<Command>) {
    let reg = Arc::new(VfsRegistry::with_local());
    let cfg = VfsCfg::default();
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), cfg.clone())),
        open_resolvers: Vec::new(),
    };
    let mut boot = Boot::new(Location::local(start), Location::local(start));
    if let Some(text) = config {
        boot.config = toml::from_str(text).unwrap();
    }
    let mut core = Core::new(boot, services, Arc::new(NoWake));
    urfin_vfs::register(&mut core, Arc::clone(&reg), cfg);
    urfin_search::register(&mut core, reg, dir.map(Path::to_path_buf));
    let (seen, opened) = channel();
    core.register(Box::new(Opener {
        ns: "viewer",
        meta: &VIEWER,
        seen: seen.clone(),
    }));
    core.register(Box::new(Opener {
        ns: "editor",
        meta: &EDITOR,
        seen,
    }));
    (core, opened)
}

fn setup(start: &Path) -> Core {
    core_with(start, None, None).0
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        core.pump(Duration::from_millis(4));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn model(core: &Core) -> &SearchModel {
    core.state().ext.get::<SearchModel>().expect("search model")
}

fn current(core: &Core) -> Arc<SearchStore> {
    Arc::clone(model(core).current().expect("store on screen"))
}

fn run(core: &mut Core, c: Command) -> Arc<SearchStore> {
    core.dispatch(c);
    core.pump(Duration::from_millis(4));
    current(core)
}

fn wait_done(core: &mut Core, s: &SearchStore) {
    pump_until(core, "search done", |_| s.is_done());
    pump_until(core, "job finished", |c| {
        c.state().jobs.iter().all(|j| !j.state.is_active())
    });
}

fn events(core: &mut Core) -> Vec<(String, urfin_core::Args)> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Ext { id, args } => Some((id.as_str().to_owned(), args)),
            _ => None,
        })
        .collect()
}

fn toasts(core: &mut Core) -> Vec<String> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(r"src\deep")).unwrap();
    std::fs::write(d.path().join("readme.md"), "нет кода\n").unwrap();
    let scan = "fn a() {}\nlet needle = 1;\n\nneedle();\n";
    std::fs::write(d.path().join(r"src\scan.rs"), scan).unwrap();
    std::fs::write(d.path().join(r"src\deep\lib.rs"), "// needle\n").unwrap();
    d
}

/// Файлов столько, что поиск идёт заметно дольше одного кадра.
fn crowd(root: &Path) {
    for d in 0..64 {
        let dir = root.join(format!("d{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..64 {
            std::fs::write(dir.join(format!("{f}.txt")), "текст без иглы\n".repeat(200)).unwrap();
        }
    }
}

#[test]
fn commands_are_valid_for_the_registry() {
    let mut r = CommandRegistry::new();
    r.register(&COMMANDS).unwrap();
    assert_eq!(COMMANDS.len(), 12);
    for m in &COMMANDS {
        assert!(m.id.starts_with("search."), "{}", m.id);
    }
    let c = Command::new("search.run")
        .arg("name", "*.rs")
        .arg("text", "x")
        .arg("where", vec!["C:\\".into()])
        .arg("case", true)
        .arg("regex", false)
        .arg("recursive", true)
        .arg("kinds", "files")
        .arg("target", "panel");
    assert_eq!(r.validate(&c), Ok(()));
    assert!(r.validate(&Command::new("search.cursor")).is_err());
    assert!(r.validate(&Command::new("search.errors")).is_err());
}

/// Команды `search.*` из значения привязки: строка или `{ cmd = …, аргументы }`.
fn bound(v: &toml::Value, out: &mut Vec<Command>) {
    let text = match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(t) => {
            let Some(id) = t.get("cmd").and_then(toml::Value::as_str) else {
                t.values().for_each(|x| bound(x, out));
                return;
            };
            let args = t.iter().filter(|(k, _)| *k != "cmd");
            let args = args.map(|(k, x)| format!(" {k}={x}"));
            format!("{id}{}", args.collect::<String>())
        }
        _ => return,
    };
    if text.starts_with("search.") {
        out.push(Command::parse(&text).unwrap());
    }
}

#[test]
fn preset_bindings_pass_the_real_commands() {
    let mut r = CommandRegistry::new();
    r.register(&COMMANDS).unwrap();
    let mut found = Vec::new();
    for preset in [
        include_str!("../../keymap/presets/base.toml"),
        include_str!("../../keymap/presets/dn.toml"),
        include_str!("../../keymap/presets/far.toml"),
    ] {
        let t: toml::Table = toml::from_str(preset).unwrap();
        t.values().for_each(|v| bound(v, &mut found));
    }
    assert!(found.len() > 15, "{}", found.len());
    for c in &found {
        assert_eq!(r.validate(c), Ok(()), "{c}");
    }
    let tos: Vec<_> = found.iter().filter_map(|c| c.args.str("to")).collect();
    for to in [
        "up",
        "down",
        "left",
        "right",
        "page_up",
        "page_down",
        "home",
        "end",
    ] {
        assert!(tos.contains(&to), "{to}");
    }
}

/// Те же формы, что в `crates/keymap/tests/known_commands.rs`.
#[test]
fn argument_kinds_match_the_keymap_list() {
    use urfin_core::ValueKind::{Bool, Int, List, Str};
    let args = |id: &str| COMMANDS.iter().find(|m| m.id == id).unwrap();
    let forms = |id: &str| -> Vec<_> {
        let a = args(id).args.iter();
        a.map(|s| (s.name, s.kind, s.required)).collect()
    };
    assert_eq!(forms("search.again"), [("id", Int, false)]);
    assert_eq!(forms("search.cancel"), [("id", Int, false)]);
    assert_eq!(forms("search.errors"), [("id", Int, true)]);
    assert_eq!(forms("search.cursor"), [("to", Str, true)]);
    assert!(args("search.cursor").repeatable);
    for id in ["close", "dirs", "edit", "files", "goto", "to_panel", "view"] {
        assert!(args(&format!("search.{id}")).args.is_empty(), "{id}");
    }
    let run = forms("search.run");
    assert_eq!(
        run[..8],
        [
            ("name", Str, false),
            ("text", Str, false),
            ("where", List, false),
            ("case", Bool, false),
            ("regex", Bool, false),
            ("recursive", Bool, false),
            ("kinds", Str, false),
            ("target", Str, false),
        ]
    );
    assert!(run.iter().all(|a| !a.2), "обязательных аргументов нет");
}

#[test]
fn run_fills_the_model_and_finishes() {
    let d = fixture();
    let mut core = setup(d.path());
    let s = run(
        &mut core,
        Command::new("search.run")
            .arg("name", "*.rs")
            .arg("text", "needle"),
    );
    wait_done(&mut core, &s);
    assert_eq!(s.rows_len(), 3, "две строки scan.rs и одна lib.rs");
    assert_eq!(s.files_len(), 2);
    let info = &s.info;
    let load = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Acquire);
    assert_eq!(load(&info.found), 3);
    assert_eq!(load(&info.files), 3);
    assert_eq!(load(&info.dirs), 3);
    assert_eq!(load(&info.denied), 0);
    let rows = s.rows(0..3);
    let mut lines: Vec<_> = rows
        .iter()
        .map(|h| (h.e.name.to_string(), h.line))
        .collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            (r"src\deep\lib.rs".to_owned(), 1),
            (r"src\scan.rs".to_owned(), 2),
            (r"src\scan.rs".to_owned(), 4)
        ]
    );
    let job = core.state().jobs.iter().next().unwrap();
    assert_eq!(job.kind, "search");
    assert_eq!(job.owner_ns, "search");
    // `Done`, а не `Failed`: чтение из UI-потока упало бы тут в debug.
    assert!(matches!(job.state, JobState::Done(_)), "{:?}", job.state);
    assert_eq!(model(&core).history()[0].text, "needle");
}

#[test]
fn cancel_stops_soon_and_second_run_replaces_the_first() {
    let d = tempfile::tempdir().unwrap();
    crowd(d.path());
    let mut core = setup(d.path());
    let text = Command::new("search.run").arg("text", "нет такого");
    let first = run(&mut core, text.clone());

    // Второй запуск: первый отменён, экран — на новом сторе.
    let second = run(&mut core, text);
    assert_ne!(first.id, second.id);
    pump_until(&mut core, "first done", |_| first.is_done());
    assert!(model(&core).get(first.id).is_none(), "стор ушёл с экрана");

    core.dispatch(Command::new("search.cancel"));
    let t0 = Instant::now();
    pump_until(&mut core, "second done", |_| second.is_done());
    assert!(t0.elapsed() <= Duration::from_secs(1), "{:?}", t0.elapsed());
    pump_until(&mut core, "jobs end", |c| {
        c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    assert!(
        core.state()
            .jobs
            .iter()
            .any(|j| j.state == JobState::Cancelled),
        "хотя бы первый отменён"
    );
}

#[test]
fn close_cancels_and_leaves_the_screen() {
    let d = tempfile::tempdir().unwrap();
    crowd(d.path());
    let mut core = setup(d.path());
    let s = run(
        &mut core,
        Command::new("search.run").arg("text", "нет такого"),
    );
    events(&mut core);
    core.dispatch(Command::new("search.close"));
    pump_until(&mut core, "stopped", |_| s.is_done());
    assert!(model(&core).current().is_none());
    assert!(model(&core).get(s.id).is_none());
    let ev = events(&mut core);
    assert!(
        ev.iter()
            .any(|(id, a)| id == "search.screen" && a.bool("close") == Some(true))
    );
}

#[test]
fn goto_stands_on_the_file_in_its_folder() {
    let d = fixture();
    let mut core = setup(d.path());
    let s = run(&mut core, Command::new("search.run").arg("name", "lib.rs"));
    wait_done(&mut core, &s);
    core.dispatch(Command::new("search.goto"));
    let folder = Location::local(d.path().join(r"src\deep"));
    pump_until(&mut core, "panel in the folder", |c| {
        let t = c.state().active_tab();
        t.loc == folder && !t.listing.entries.is_empty() && t.pending_focus.is_none()
    });
    let t = core.state().active_tab();
    let row = t.current();
    let Some(urfin_core::tab::Row::Entry { entry, .. }) = row else {
        panic!("курсор не на файле");
    };
    assert_eq!(entry.name, "lib.rs");
}

#[test]
fn goto_enters_a_found_folder() {
    let d = fixture();
    let mut core = setup(d.path());
    let s = run(
        &mut core,
        Command::new("search.run")
            .arg("name", "deep")
            .arg("kinds", "dirs"),
    );
    wait_done(&mut core, &s);
    core.dispatch(Command::new("search.goto"));
    let folder = Location::local(d.path().join(r"src\deep"));
    pump_until(&mut core, "panel in the folder", |c| {
        c.state().active_tab().loc == folder
    });
}

#[test]
fn view_and_edit_open_the_line() {
    let d = fixture();
    let (mut core, opened) = core_with(d.path(), None, None);
    let s = run(
        &mut core,
        Command::new("search.run")
            .arg("name", "lib.rs")
            .arg("text", "needle"),
    );
    wait_done(&mut core, &s);
    for (cmd, id) in [
        ("search.view", "viewer.open"),
        ("search.edit", "editor.open"),
    ] {
        core.dispatch(Command::new(cmd));
        core.pump(Duration::from_millis(4));
        let c = opened.try_recv().unwrap();
        assert_eq!(c.id.as_str(), id);
        assert_eq!(
            c.args.loc("path"),
            Some(Location::local(d.path().join(r"src\deep\lib.rs")))
        );
        assert_eq!(c.args.int("line"), Some(1));
    }
}

#[test]
fn cursor_stays_inside_the_list() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..30 {
        std::fs::write(d.path().join(format!("{i}.txt")), "").unwrap();
    }
    let mut core = setup(d.path());
    let s = run(&mut core, Command::new("search.run"));
    wait_done(&mut core, &s);
    let go = |core: &mut Core, to: &'static str| {
        core.dispatch(Command::new("search.cursor").arg("to", to));
        core.pump(Duration::from_millis(4));
        model(core).cursor()
    };
    assert_eq!(go(&mut core, "up"), 0);
    assert_eq!(go(&mut core, "down"), 1);
    assert_eq!(go(&mut core, "page_down"), 21);
    assert_eq!(go(&mut core, "page_down"), 29);
    assert_eq!(go(&mut core, "page_up"), 9);
    assert_eq!(go(&mut core, "end"), 29);
    assert_eq!(go(&mut core, "home"), 0);
    assert_eq!(go(&mut core, "down"), 1);
    assert_eq!(go(&mut core, "left"), 1, "список вертикальный");
    assert_eq!(go(&mut core, "right"), 1, "список вертикальный");
}

#[test]
fn form_opens_with_the_panel_folder_and_last_query() {
    let d = fixture();
    let mut core = setup(d.path());
    core.pump(Duration::from_millis(4));
    core.dispatch(Command::new("search.files"));
    core.pump(Duration::from_millis(4));
    let ev = events(&mut core);
    let (id, a) = &ev[0];
    assert_eq!(id, "search.screen");
    let want = Location::local(d.path()).path.display().to_string();
    assert_eq!(a.str("where"), Some(want.as_str()));
    assert_eq!(a.str("kinds"), Some("files"));
    assert_eq!(a.bool("recursive"), Some(true));

    let s = run(
        &mut core,
        Command::new("search.run")
            .arg("name", "*.md")
            .arg("text", "код")
            .arg("case", true),
    );
    wait_done(&mut core, &s);
    events(&mut core);
    core.dispatch(Command::new("search.dirs"));
    core.pump(Duration::from_millis(4));
    let ev = events(&mut core);
    let (_, a) = &ev[0];
    assert_eq!(a.str("name"), Some("*.md"));
    assert_eq!(a.str("text"), Some("код"));
    assert_eq!(a.bool("case"), Some(true));
    assert_eq!(a.str("kinds"), Some("dirs"));
}

#[test]
fn again_repeats_the_query() {
    let d = fixture();
    let mut core = setup(d.path());
    let s = run(&mut core, Command::new("search.run").arg("name", "*.rs"));
    wait_done(&mut core, &s);
    std::fs::write(d.path().join("ещё.rs"), "").unwrap();
    core.dispatch(Command::new("search.again"));
    pump_until(&mut core, "new store", |c| current(c).id != s.id);
    let again = current(&core);
    wait_done(&mut core, &again);
    assert_eq!(again.rows_len(), 3);
}

#[test]
fn to_panel_opens_a_tab_and_keeps_the_store() {
    let d = fixture();
    let mut core = setup(d.path());
    let s = run(&mut core, Command::new("search.run").arg("name", "*.rs"));
    wait_done(&mut core, &s);
    let tabs = core.state().active_panel().tabs.len();
    core.dispatch(Command::new("search.to_panel"));
    core.pump(Duration::from_millis(4));
    assert_eq!(core.state().active_panel().tabs.len(), tabs + 1);
    assert_eq!(core.state().active_tab().loc.backend, "search");
    assert_eq!(model(&core).get(s.id).map(|x| x.id), Some(s.id));
    assert!(model(&core).current().is_none());

    // Новый запуск из панели не трогает её стор.
    let next = run(&mut core, Command::new("search.run").arg("name", "*.md"));
    wait_done(&mut core, &next);
    assert!(model(&core).get(s.id).is_some());
    assert_eq!(next.rows_len(), 1, "корень — корень поиска панели");
}

#[test]
fn target_panel_goes_straight_to_a_tab() {
    let d = fixture();
    let mut core = setup(d.path());
    core.pump(Duration::from_millis(4));
    core.dispatch(Command::new("search.run").arg("target", "panel"));
    core.pump(Duration::from_millis(4));
    assert_eq!(core.state().active_tab().loc.backend, "search");
}

#[test]
fn max_hits_stops_with_a_banner() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..50 {
        std::fs::write(d.path().join(format!("{i}.txt")), "").unwrap();
    }
    let (mut core, _o) = core_with(d.path(), Some("[search]\nmax_hits = 7\n"), None);
    let s = run(&mut core, Command::new("search.run"));
    wait_done(&mut core, &s);
    pump_until(&mut core, "banner", |c| c.state().banner.is_some());
    assert_eq!(s.rows_len(), 7);
    assert!(core.state().banner.as_ref().unwrap().text.contains('7'));
}

#[test]
fn bad_input_is_a_toast() {
    let d = fixture();
    let mut core = setup(d.path());
    core.pump(Duration::from_millis(4));
    core.dispatch(Command::new("search.run").arg("name", "a|b|c"));
    core.pump(Duration::from_millis(4));
    let t = toasts(&mut core);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0].contains("a|b|c"), "{t:?}");
    assert!(model(&core).current().is_none());

    let s = run(
        &mut core,
        Command::new("search.run")
            .arg("text", "(")
            .arg("regex", true),
    );
    pump_until(&mut core, "failed job", |c| {
        c.state()
            .jobs
            .iter()
            .any(|j| matches!(j.state, JobState::Failed(_)))
    });
    assert!(s.is_done());
    pump_until(&mut core, "toast", |c| c.state().jobs.iter().count() == 1);
}

#[test]
fn history_survives_a_restart() {
    let d = fixture();
    let state = tempfile::tempdir().unwrap();
    {
        let (mut core, _o) = core_with(d.path(), None, Some(state.path()));
        let s = run(&mut core, Command::new("search.run").arg("name", "*.rs"));
        wait_done(&mut core, &s);
        let file = state.path().join("search_history.toml");
        pump_until(&mut core, "history file", |_| file.exists());
    }
    let (mut core, _o) = core_with(d.path(), None, Some(state.path()));
    core.pump(Duration::from_millis(4));
    pump_until(&mut core, "history read", |c| {
        !model(c).history().is_empty()
    });
    assert_eq!(model(&core).history()[0].name, "*.rs");
}

#[test]
fn denied_list_is_shared_with_the_job() {
    // `search.errors` — пока заглушка: команда принимается, список читает S8-10.
    let d = fixture();
    let mut core = setup(d.path());
    let s = run(&mut core, Command::new("search.run"));
    wait_done(&mut core, &s);
    core.dispatch(Command::new("search.errors").arg("id", s.id.0));
    core.pump(Duration::from_millis(4));
    assert_eq!(toasts(&mut core), [] as [std::string::String; 0]);
}
