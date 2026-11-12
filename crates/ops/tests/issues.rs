//! `ops.issues {job}` командой (S5 §5.5, S5-30): у задачи с пропусками и ошибками список
//! проблем — в `AppState.ext` ([`urfin_ops::JobIssues`]) с путями и причинами, окну —
//! событие `ops.issues` со счётчиками. Удаление насовсем (задача мимо движка) — тоже.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{Boot, Command, Core, JobId, JobState, Location, Services, UiEvent, Waker};
use urfin_ops::{IssueState, JobIssues};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn setup(left: &Path) -> Core {
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
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
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

fn cursor(core: &mut Core, name: &str) {
    pump_until(core, "listed", |c| {
        let tab = c.state().active_tab();
        tab.listing.entries.iter().any(|e| e.name == name)
    });
    core.dispatch(Command::new("panel.cursor.to").arg("name", name));
    core.pump(Duration::from_millis(2));
}

/// Ждёт конца задачи вида `kind`; её номер и итог.
fn job_end(core: &mut Core, kind: &str) -> (JobId, JobState) {
    pump_until(core, "job ends", |c| {
        let mut jobs = c.state().jobs.iter();
        jobs.any(|j| j.kind == kind && !j.state.is_active())
    });
    let j = core.state().jobs.iter().find(|j| j.kind == kind).unwrap();
    (j.id, j.state.clone())
}

/// `ops.issues {job}`: список из `AppState.ext` (ждём отчёт задачи) и счётчики события.
fn issues(core: &mut Core, id: JobId) -> (JobIssues, urfin_core::Args) {
    let job = i64::try_from(id.0).unwrap();
    let t0 = Instant::now();
    loop {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: issues");
        core.take_ui_events();
        core.dispatch(Command::new("ops.issues").arg("job", job));
        core.pump(Duration::from_millis(2));
        let mut events = core.take_ui_events().into_iter();
        let args = events.find_map(|e| match e {
            UiEvent::Ext { id, args } if id.as_str() == "ops.issues" => Some(args),
            _ => None,
        });
        let list = core.state().ext.get::<JobIssues>().cloned();
        // Отчёт задачи доходит сообщением: до него у задачи мимо движка списка нет.
        if let (Some(list), Some(args)) = (list, args)
            && list.job == id
        {
            return (list, args);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn text(key: &str) -> String {
    let t = urfin_i18n::current().get(key);
    assert_ne!(t, key, "fluent key exists");
    t
}

/// Приёмка S5-30: копия с пропуском (политика «пропустить») и ошибкой (на месте каталога
/// — файл) отдаёт обе проблемы с путями и причинами.
#[test]
fn issues_command_lists_skips_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (left, right) = (tmp.path().join("l"), tmp.path().join("r"));
    fs::create_dir_all(left.join("pack").join("sub")).unwrap();
    fs::write(left.join("pack").join("a.txt"), b"new").unwrap();
    fs::write(left.join("pack").join("b.txt"), b"b").unwrap();
    fs::write(left.join("pack").join("sub").join("x.txt"), b"x").unwrap();
    fs::create_dir_all(right.join("pack")).unwrap();
    fs::write(right.join("pack").join("a.txt"), b"old").unwrap();
    fs::write(right.join("pack").join("sub"), b"file").unwrap();
    let mut core = setup(&left);
    cursor(&mut core, "pack");
    let dest = format!("{}{}", right.display(), std::path::MAIN_SEPARATOR);
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", dest.as_str())
            .arg("conflict", "skip"),
    );
    let (id, st) = job_end(&mut core, "copy");
    assert!(
        matches!(&st, JobState::Done(s) if s.items >= 1 && s.skipped == 1 && s.errors == 1),
        "{st:?}"
    );
    let (list, args) = issues(&mut core, id);
    let tail = |p: &Path| {
        let mut c = p.components().rev().take(2).collect::<Vec<_>>();
        c.reverse();
        c.iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    };
    let rows: Vec<_> = list
        .rows
        .iter()
        .map(|r| (r.state, tail(&r.path), r.reason.to_string()))
        .collect();
    assert_eq!(
        rows,
        [
            (
                IssueState::Failed,
                "pack/sub".to_owned(),
                text("ops-copy-type")
            ),
            (
                IssueState::Skipped,
                "pack/a.txt".to_owned(),
                text("ops-conflict-exists")
            ),
        ]
    );
    assert!(list.rows.iter().all(|r| r.path.is_absolute()), "full paths");
    assert_eq!(list.more, 0);
    assert_eq!(
        (
            args.int("job"),
            args.int("errors"),
            args.int("skipped"),
            args.int("waiting")
        ),
        (i64::try_from(id.0).ok(), Some(1), Some(1), Some(0))
    );
    assert_eq!(
        fs::read(right.join("pack").join("a.txt")).unwrap(),
        b"old",
        "skipped file is kept"
    );
    // Задачи нет — отказ, список прежний.
    core.dispatch(Command::new("ops.issues").arg("job", 9_999_i64));
    core.pump(Duration::from_millis(2));
    assert_eq!(core.state().ext.get::<JobIssues>().unwrap().job, id);
}

/// Удаление насовсем идёт мимо движка job, но отчёт оставляет: ошибки — в списке.
#[cfg(windows)]
#[test]
fn issues_command_lists_delete_errors() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("held.txt"), b"x").unwrap();
    // Открыт без `FILE_SHARE_DELETE`: удалить нельзя.
    let hold = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(tmp.path().join("held.txt"))
        .unwrap();
    let mut core = setup(tmp.path());
    cursor(&mut core, "held.txt");
    core.dispatch(
        Command::new("ops.delete")
            .arg("permanent", true)
            .arg("confirmed", true),
    );
    let (id, st) = job_end(&mut core, "delete");
    assert!(matches!(&st, JobState::Done(s) if s.errors == 1), "{st:?}");
    let (list, args) = issues(&mut core, id);
    drop(hold);
    assert_eq!(list.rows.len(), 1, "{:?}", list.rows);
    let row = &list.rows[0];
    assert_eq!(row.state, IssueState::Failed);
    assert!(row.path.ends_with("held.txt"), "{:?}", row.path);
    assert!(!row.reason.is_empty());
    assert_eq!(args.int("errors"), Some(1));
}
