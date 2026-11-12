//! Откат группового переименования командой (S5 §4.14, S5-29): `ops.multi_rename`
//! пишет план в журнал, `ops.multi_rename.undo` возвращает имена — и после перезапуска
//! (другой `Core` над тем же каталогом журналов).

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, JobState, Location, Services, UiEvent, Waker};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn setup(left: &Path, journal: &Path) -> Core {
    let reg = Arc::new(VfsRegistry::with_local());
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(Location::local(left), Location::local(left)),
        services,
        Arc::new(NoWake),
    );
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    urfin_ops::register_with(
        &mut core,
        urfin_ops::OpsConfig::default(),
        Some(journal.to_path_buf()),
    );
    core
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Ждёт `n` завершённых задач переименования; последняя — без ошибок.
fn renamed(core: &mut Core, n: usize) {
    pump_until(core, "rename job ends", |c| {
        let jobs = &c.state().jobs;
        jobs.iter()
            .filter(|j| j.kind == "rename" && !j.state.is_active())
            .count()
            == n
    });
    let last = core
        .state()
        .jobs
        .iter()
        .filter(|j| j.kind == "rename")
        .last()
        .unwrap();
    assert!(
        matches!(&last.state, JobState::Done(s) if s.errors == 0),
        "{:?}",
        last.state
    );
}

fn names_in(dir: &Path) -> Vec<(String, String)> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read_to_string(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(n, c)| ((*n).to_owned(), (*c).to_owned()))
        .collect()
}

/// Выделить всё в активной панели и переименовать по шаблону `[N]a`: `a → aa`,
/// `aa → aaa` — цепочка.
fn rename_chain(core: &mut Core) {
    pump_until(core, "listed", |c| {
        c.state().active_tab().listing.entries.len() == 2
    });
    core.dispatch(Command::new("sel.all"));
    core.pump(Duration::from_millis(2));
    core.dispatch(
        Command::new("ops.multi_rename")
            .arg("pattern", "[N]a")
            .arg("apply", true),
    );
}

#[test]
fn undo_command_restores_names() {
    let tmp = tempfile::tempdir().unwrap();
    let jnl = tempfile::tempdir().unwrap();
    for n in ["a", "aa"] {
        fs::write(tmp.path().join(n), n).unwrap();
    }
    let before = names_in(tmp.path());
    let mut core = setup(tmp.path(), jnl.path());
    rename_chain(&mut core);
    renamed(&mut core, 1);
    assert_eq!(
        names_in(tmp.path()),
        pairs(&[("aa", "a"), ("aaa", "aa")]),
        "chain applied"
    );
    core.dispatch(Command::new("ops.multi_rename.undo"));
    renamed(&mut core, 2);
    assert_eq!(names_in(tmp.path()), before);
    // Откатывать больше нечего: тост, новой задачи нет.
    core.take_ui_events();
    core.dispatch(Command::new("ops.multi_rename.undo"));
    let want = urfin_i18n::current().get("ops-mr-nothing");
    let t0 = Instant::now();
    let mut toasts = Vec::new();
    while toasts.is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: toast");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
        toasts.extend(core.take_ui_events().into_iter().filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        }));
    }
    assert_eq!(toasts, [want]);
    assert_eq!(
        core.state()
            .jobs
            .iter()
            .filter(|j| j.kind == "rename")
            .count(),
        2
    );
}

/// План лежит в каталоге журналов: откат работает из другого экземпляра (после
/// перезапуска).
#[test]
fn undo_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let jnl = tempfile::tempdir().unwrap();
    for n in ["a", "aa"] {
        fs::write(tmp.path().join(n), n).unwrap();
    }
    let before = names_in(tmp.path());
    let mut first = setup(tmp.path(), jnl.path());
    rename_chain(&mut first);
    renamed(&mut first, 1);
    assert_ne!(names_in(tmp.path()), before);
    let mut second = setup(tmp.path(), jnl.path());
    second.dispatch(Command::new("ops.multi_rename.undo"));
    renamed(&mut second, 1);
    assert_eq!(names_in(tmp.path()), before);
}
