//! S7-14 через core: соседние файлы, `viewer.reveal`, экраны, история в файле, внешний
//! просмотр и `[[viewer.assoc]]`, ссылки `.md` в том же экране, перечитывание `[viewer]`,
//! тост UTF-32.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use urfin_core::{
    ArgSpec, Boot, CmdError, Command, CommandMeta, Core, Cx, Focus, Handler, Location, Paths,
    Services, Side, UiEvent, ValueKind, Waker,
};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

use crate::layout::Mode;
use crate::md::LinkTarget;
use crate::{Assoc, EditorCfg, ViewEnv, ViewMemo, ViewState, ViewerCfg, ViewerDoc, register_env};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// `term.open` / `term.exec` / `file.open` без терминала: команды копятся для проверки.
struct Recorder(Arc<Mutex<Vec<Command>>>);

const fn arg(name: &'static str, kind: ValueKind) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required: false,
    }
}

static TERM: [CommandMeta; 2] = [
    CommandMeta {
        id: "term.open",
        title: "open",
        category: "term",
        args: &[arg("path", ValueKind::Str), arg("verb", ValueKind::Str)],
        repeatable: false,
    },
    CommandMeta {
        id: "term.exec",
        title: "exec",
        category: "term",
        args: &[arg("cmd", ValueKind::Str), arg("cwd", ValueKind::Str)],
        repeatable: false,
    },
];

impl Handler for Recorder {
    fn namespace(&self) -> &'static str {
        "term"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &TERM
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        self.0.lock().unwrap().push(cmd.clone());
        Ok(())
    }
}

/// Core с листингом локальной ФС (панели видят файлы), каталогами в `base` и вьювером;
/// `cache` — каталог кэша (история — в соседнем `state`).
fn core_in(start: &Path, base: &Path, viewer: ViewerCfg, cache: Option<PathBuf>) -> Core {
    let reg = Arc::new(VfsRegistry::with_local());
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut boot = Boot::new(Location::local(start), Location::local(start));
    boot.paths = Paths::in_dir(base);
    let mut core = Core::new(boot, services, Arc::new(NoWake));
    urfin_vfs::register(&mut core, Arc::clone(&reg), VfsCfg::default());
    register_env(
        &mut core,
        viewer,
        EditorCfg::default(),
        ViewEnv {
            vfs: reg,
            cache_dir: cache,
        },
    );
    core.pump(Duration::from_millis(5));
    core
}

fn pump_until(core: &mut Core, what: &str, mut f: impl FnMut(&Core) -> bool) {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(5));
        if f(core) {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(8), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn view(core: &Core) -> &ViewState {
    core.state().ext.get::<ViewState>().unwrap()
}

fn doc(core: &Core) -> &ViewerDoc {
    let v = view(core);
    v.viewer(v.active_viewer().unwrap()).unwrap()
}

fn ready_at(core: &Core, p: &Path) -> bool {
    let v = view(core);
    v.active_viewer()
        .and_then(|id| v.viewer(id))
        .is_some_and(|d| d.is_ready() && d.loc == Location::local(p))
}

fn open(core: &mut Core, p: &Path) {
    core.dispatch(Command::new("viewer.open").arg("path", Location::local(p)));
    pump_until(core, "open", |c| ready_at(c, p));
}

fn listed(core: &mut Core, n: usize) {
    pump_until(core, "listing", |c| {
        c.state().panel(Side::Left).tab().listing.entries.len() >= n
    });
}

fn toasts(core: &mut Core) -> Vec<String> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Gray+ / Gray−: соседний файл того же таба, каталоги пропускаются; экран тот же, режим
/// переносится, прежний файл — в историю.
#[test]
fn next_prev_file_skip_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    std::fs::create_dir_all(root.join("b-dir")).unwrap();
    for n in ["a.txt", "c.txt"] {
        std::fs::write(root.join(n), format!("{n}\nвторая строка\n")).unwrap();
    }
    let mut core = core_in(&root, dir.path(), ViewerCfg::default(), None);
    listed(&mut core, 3);
    open(&mut core, &root.join("a.txt"));
    core.dispatch(Command::new("viewer.mode").arg("mode", "hex"));
    core.dispatch(Command::new("viewer.next_file"));
    pump_until(&mut core, "next", |c| ready_at(c, &root.join("c.txt")));
    let v = view(&core);
    assert_eq!(v.screens.len(), 1, "тот же экран");
    assert_eq!(v.len(), 1, "прежний документ закрыт");
    assert_eq!(doc(&core).mode, Mode::Hex, "режим переносится");
    assert!(
        v.history
            .get(&Location::local(root.join("a.txt")))
            .is_some()
    );

    core.dispatch(Command::new("viewer.next_file"));
    core.pump(Duration::from_millis(5));
    assert!(ready_at(&core, &root.join("c.txt")), "дальше файлов нет");
    core.dispatch(Command::new("viewer.prev_file"));
    pump_until(&mut core, "prev", |c| ready_at(c, &root.join("a.txt")));
    // Перед a.txt — только каталог: остаёмся.
    core.dispatch(Command::new("viewer.prev_file"));
    core.pump(Duration::from_millis(5));
    assert!(ready_at(&core, &root.join("a.txt")));
    assert_eq!(core.state().focus, Focus::Viewer);
}

/// Ctrl+F10 и ссылка на файл: панель-источник идёт в папку и ставит курсор на файл,
/// фокус — в панель, вьювер остаётся экраном.
#[test]
fn reveal_moves_panel_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("x.txt"), "x").unwrap();
    std::fs::write(sub.join("y.txt"), "y").unwrap();
    std::fs::write(root.join("r.txt"), "r").unwrap();
    let mut core = core_in(&root, dir.path(), ViewerCfg::default(), None);
    listed(&mut core, 2);
    open(&mut core, &sub.join("y.txt"));
    core.dispatch(Command::new("viewer.reveal"));
    let on = |c: &Core, dir: &Path, name: &str| {
        let t = c.state().panel(Side::Left).tab();
        t.loc == Location::local(dir)
            && matches!(t.current(), Some(urfin_core::Row::Entry { entry, .. }) if entry.name == name)
    };
    pump_until(&mut core, "reveal", |c| on(c, &sub, "y.txt"));
    assert_eq!(core.state().focus, Focus::Panel);
    assert_eq!(view(&core).screens.len(), 1, "вьювер остаётся экраном");

    core.dispatch(Command::new("viewer.reveal").arg("path", Location::local(sub.join("x.txt"))));
    pump_until(&mut core, "reveal path", |c| on(c, &sub, "x.txt"));
    core.dispatch(Command::new("viewer.reveal").arg("path", Location::local(&sub)));
    pump_until(&mut core, "reveal dir", |c| on(c, &root, "sub"));
}

/// `viewer.screen doc=` — экран наверх, фокус ему; неизвестный — ошибка аргумента.
#[test]
fn screen_switches_active() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    let mut core = core_in(dir.path(), dir.path(), ViewerCfg::default(), None);
    open(&mut core, &a);
    let first = view(&core).active().unwrap();
    open(&mut core, &b);
    core.dispatch(Command::new("panel.focus"));
    core.pump(Duration::from_millis(5));
    core.dispatch(Command::new("viewer.screen").arg("doc", i64::from(first.0)));
    core.pump(Duration::from_millis(5));
    assert_eq!(view(&core).active(), Some(first));
    assert_eq!(core.state().focus, Focus::Viewer);
    let list: Vec<_> = view(&core)
        .screen_list()
        .map(|(id, _, v)| (id, v))
        .collect();
    assert_eq!(list.last(), Some(&(first, true)));
    core.take_ui_events();
    core.dispatch(Command::new("viewer.screen").arg("doc", 999));
    core.pump(Duration::from_millis(5));
    assert_eq!(toasts(&mut core).len(), 1);
}

/// История переживает перезапуск: файл `state/view-history.json` пишется через 2 с после
/// закрытия и при выходе; новый запуск открывает файл с прежней позиции.
#[test]
fn history_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let file = dir.path().join("state").join("view-history.json");
    let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
    let mut text = String::new();
    for i in 0..300 {
        let _ = writeln!(text, "строка {i}");
    }
    std::fs::write(&a, &text).unwrap();
    std::fs::write(&b, &text).unwrap();

    let mut core = core_in(
        dir.path(),
        dir.path(),
        ViewerCfg::default(),
        Some(cache.clone()),
    );
    pump_until(&mut core, "load", |c| view(c).history.loaded());
    open(&mut core, &a);
    core.dispatch(Command::new("viewer.goto").arg("to", "100"));
    pump_until(&mut core, "goto", |c| doc(c).top > 0);
    let line = doc(&core).top;
    core.dispatch(Command::new("viewer.close"));
    pump_until(&mut core, "debounced write", |_| file.exists());
    let saved = crate::ViewHistory::from_json(&std::fs::read_to_string(&file).unwrap());
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (saved[0].loc.clone(), saved[0].top),
        (Location::local(&a), line)
    );

    // Выход: позиция открытого документа пишется сразу.
    open(&mut core, &b);
    core.dispatch(Command::new("viewer.goto").arg("to", "200"));
    pump_until(&mut core, "goto b", |c| doc(c).top > 0);
    let line = doc(&core).top;
    assert_eq!(core.quit_decision(), urfin_core::QuitDecision::Allow);
    core.shutdown(Duration::from_secs(5));
    let saved = crate::ViewHistory::from_json(&std::fs::read_to_string(&file).unwrap());
    assert_eq!(saved.len(), 2);

    let mut core = core_in(dir.path(), dir.path(), ViewerCfg::default(), Some(cache));
    pump_until(&mut core, "reload", |c| view(c).history.len() == 2);
    open(&mut core, &b);
    pump_until(&mut core, "restored", |c| doc(c).top == line);
}

/// Alt+F3: пусто — программа Windows (`term.open` по пути), шаблон — `term.exec` с путём
/// в кавычках; маски `[[viewer.assoc]]` уводят F3 наружу, `internal` — нет.
#[test]
fn external_viewer_and_assoc() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("my doc.pdf");
    std::fs::write(&pdf, "%PDF").unwrap();
    let rec = Arc::new(Mutex::new(Vec::new()));
    let cfg = ViewerCfg {
        assoc: vec![Assoc {
            mask: "*.pdf".into(),
            cmd: "SumatraPDF.exe {path}".into(),
        }],
        ..ViewerCfg::default()
    };
    let mut core = core_in(dir.path(), dir.path(), cfg, None);
    core.register(Box::new(Recorder(Arc::clone(&rec))));
    let at = Location::local(&pdf);
    core.dispatch(Command::new("viewer.open").arg("path", at.clone()));
    core.pump(Duration::from_millis(5));
    assert!(view(&core).is_empty(), "маска — наружу");
    core.dispatch(
        Command::new("viewer.open")
            .arg("path", at.clone())
            .arg("alt", true),
    );
    core.pump(Duration::from_millis(5));
    core.dispatch(
        Command::new("viewer.open")
            .arg("path", at.clone())
            .arg("internal", true),
    );
    pump_until(&mut core, "internal", |c| ready_at(c, &pdf));
    let got = rec.lock().unwrap().clone();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].id.as_str(), "term.exec");
    assert_eq!(
        got[0].args.str("cmd"),
        Some(format!("SumatraPDF.exe \"{}\"", pdf.display()).as_str())
    );
    assert_eq!(got[1].id.as_str(), "term.open");
    assert_eq!(
        got[1].args.str("path"),
        Some(pdf.to_string_lossy().as_ref())
    );
}

#[test]
fn template_quotes_path_once() {
    let p = Path::new(r"C:\a b\c.txt");
    assert_eq!(
        crate::handler::expand_template("v.exe {path}", p),
        r#"v.exe "C:\a b\c.txt""#
    );
    assert_eq!(
        crate::handler::expand_template(r#"v.exe "{path}" -x"#, p),
        r#"v.exe "C:\a b\c.txt" -x"#
    );
}

/// Ссылка `.md` из документа открывается в этом же экране и встаёт на якорь;
/// `viewer.back` возвращает прежний файл на прежнюю позицию.
#[test]
fn md_link_opens_here_with_back() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    std::fs::write(&a, "# A\n\nСм. [второй](b.md#second).\n").unwrap();
    let mut body = String::from("# First\n\n");
    for i in 0..200 {
        let _ = write!(body, "Абзац {i}.\n\n");
    }
    body.push_str("## Second\n\nконец\n");
    std::fs::write(&b, &body).unwrap();
    let mut core = core_in(dir.path(), dir.path(), ViewerCfg::default(), None);
    open(&mut core, &a);
    pump_until(&mut core, "doc a", |c| {
        doc(c).md_doc().is_some_and(|d| d.complete)
    });
    let link = doc(&core).md_doc().unwrap().links[0].target.clone();
    assert!(
        matches!(&link, LinkTarget::Path { markdown: true, anchor: Some(s), .. } if s == "second")
    );
    let cmd = link.command().unwrap();
    assert_eq!(cmd.args.str("placement"), Some("here"));
    core.dispatch(cmd);
    pump_until(&mut core, "doc b", |c| {
        ready_at(c, &b) && doc(c).md_doc().is_some_and(|d| d.complete)
    });
    let d = doc(&core);
    let md = d.md_doc().unwrap();
    let want = md.anchor("second").unwrap();
    assert_eq!(d.md.as_ref().unwrap().top, want);
    assert_eq!(d.top, md.blocks[want as usize].src.start);
    assert_eq!(view(&core).screens.len(), 1);
    assert_eq!(d.files_back.len(), 1);

    // Тот же файл с якорем — переход внутри, «назад» — к прежнему месту.
    core.dispatch(
        Command::new("viewer.open")
            .arg("path", Location::local(&b))
            .arg("placement", "here")
            .arg("anchor", "first"),
    );
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).md.as_ref().unwrap().top, 0);
    core.dispatch(Command::new("viewer.back"));
    core.pump(Duration::from_millis(5));
    assert_eq!(doc(&core).md.as_ref().unwrap().top, want);
    core.dispatch(Command::new("viewer.back"));
    pump_until(&mut core, "back to a", |c| ready_at(c, &a));
    assert_eq!(doc(&core).files_back, []);
    assert_eq!(view(&core).screens.len(), 1);

    let file = LinkTarget::Path {
        path: Some(dir.path().join("x.txt")),
        anchor: None,
        markdown: false,
    };
    assert_eq!(file.command().unwrap().id.as_str(), "viewer.reveal");
    assert!(LinkTarget::Other.command().is_none());
}

/// `app.reload_config` (`ui.setting`): `viewer.markdown = "source"` переводит открытый
/// документ в исходник на той же позиции, `md_font_size` — в `ViewState`; обратно —
/// снова документ.
#[test]
fn reload_config_applies_to_open_docs() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("README.md");
    std::fs::write(&md, "# Заголовок\n\nтекст\n\n## Ещё\n\nабзац\n").unwrap();
    let mut core = core_in(dir.path(), dir.path(), ViewerCfg::default(), None);
    open(&mut core, &md);
    pump_until(&mut core, "doc", |c| doc(c).md_doc().is_some());
    let write = |s: &str| {
        let text = format!("version = {}\n[viewer]\n{s}\n", urfin_core::CONFIG_VERSION);
        std::fs::write(dir.path().join("config.toml"), text).unwrap();
    };
    write("markdown = \"source\"\nmd_font_size = 18");
    core.dispatch(Command::new("app.reload_config"));
    pump_until(&mut core, "source", |c| {
        view(c).viewer.md_font_size == 18 && !doc(c).md_mode()
    });
    assert_eq!(doc(&core).mode, Mode::Markup, "режим тот же — исходник");
    write("markdown = \"document\"");
    core.dispatch(Command::new("app.reload_config"));
    pump_until(&mut core, "document", |c| {
        doc(c).md_mode() && doc(c).md_doc().is_some()
    });
    assert_eq!(view(&core).viewer.md_font_size, 0);
}

/// UTF-32 (BOM) — Hex и тост «не поддерживается».
#[test]
fn utf32_toasts() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("u32.txt");
    let mut bytes = vec![0xFF, 0xFE, 0, 0];
    for c in "привет".chars() {
        bytes.extend_from_slice(&u32::from(c).to_le_bytes());
    }
    std::fs::write(&p, bytes).unwrap();
    let mut core = core_in(dir.path(), dir.path(), ViewerCfg::default(), None);
    core.take_ui_events();
    open(&mut core, &p);
    assert_eq!(doc(&core).mode, Mode::Hex);
    let want = urfin_i18n::current().get("ui-view-utf32");
    assert!(toasts(&mut core).contains(&want));
}

/// `ViewMemo::start` — начало файла по детекту.
#[test]
fn memo_start_is_plain() {
    let m = ViewMemo::start(Location::local(r"C:\a"));
    assert_eq!((m.top, m.enc, m.mode, m.wrap), (0, None, Mode::Text, None));
}
