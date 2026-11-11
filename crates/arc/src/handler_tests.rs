//! Handler `arc` в core с настоящим листингом VFS: Enter в архив, выход, сводки.

use std::time::{Duration, Instant};

use urfin_core::arc::ArcModel;
use urfin_core::{
    Attrs, Boot, CommandRegistry, Core, EntryKind, ListingStatus, Services, UiEvent, Waker,
};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

use super::*;
use crate::ArcVfs;
use crate::avfs::tests::Tmp;
use crate::mock::{self, MockFormat};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Реестр с локальной ФС и моком под видом `zip`.
fn registry(cache: &Arc<ArcCache>) -> Arc<VfsRegistry> {
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(ArcVfs::new(
        MockFormat::catalog().with_scheme("zip"),
        Arc::clone(cache),
    )));
    Arc::new(reg)
}

fn core(dir: &Location, cache: &Arc<ArcCache>) -> Core {
    let reg = registry(cache);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(
        Boot::new(dir.clone(), dir.clone()),
        services,
        Arc::new(NoWake),
    );
    crate::register(&mut core, reg, Arc::clone(cache), &ArcCfg::default());
    core
}

fn settle(core: &mut Core, what: &str, mut done: impl FnMut(&Core) -> bool) {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(5));
        if done(core) {
            return;
        }
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "не дождались: {what}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn loaded(core: &Core, loc: &Location) -> bool {
    let t = core.state().active_tab();
    t.loc == *loc
        && !matches!(
            t.listing.status,
            ListingStatus::Loading { .. } | ListingStatus::Idle
        )
}

fn names(core: &Core) -> Vec<String> {
    let t = core.state().active_tab();
    let mut v: Vec<String> = t
        .listing
        .entries
        .iter()
        .map(|e| e.name.to_string())
        .collect();
    v.sort();
    v
}

fn cursor_name(core: &Core) -> Option<String> {
    match core.state().active_tab().current()? {
        Row::Entry { entry, .. } => Some(entry.name.to_string()),
        Row::Parent => None,
    }
}

fn point(core: &mut Core, name: &str) {
    core.dispatch(Command::new("panel.cursor.to").arg("name", name));
    settle(core, "курсор", |c| {
        cursor_name(c).as_deref() == Some(name)
    });
}

fn entry(name: &str) -> Entry {
    Entry {
        name: name.into(),
        size: 1,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind: EntryKind::File,
    }
}

#[test]
fn commands_validate() {
    let mut r = CommandRegistry::new();
    r.register(&COMMANDS).unwrap();
    for m in &COMMANDS {
        assert!(m.id.starts_with("arc."));
        r.validate(&Command::new(m.id)).unwrap();
    }
    assert!(r.validate(&Command::new("arc.enter").arg("x", 1)).is_err());
}

#[test]
fn resolver_enters_known_archives_only() {
    let cache = ArcCache::new(crate::cache::DEFAULT_LIMIT);
    let reg = registry(&cache);
    let dir = Location::local(r"C:\Dev");
    let mut cfg = ArcCfg::default();
    cfg.enter_exts.push("cbz".into());
    let r = open_resolver(&cfg, reg);
    let args = Args::default();
    let cmd = r.resolve(&dir, &entry("Урфин.ZIP"), &args).unwrap();
    assert_eq!(cmd.id.as_str(), "panel.go");
    let to = cmd.args.loc("path").unwrap();
    assert_eq!(to.backend, "zip");
    assert_eq!(
        to.outer.as_deref(),
        Some(&Location::local(r"C:\Dev\Урфин.ZIP"))
    );
    assert!(
        r.resolve(&dir, &entry("report.docx"), &args).is_none(),
        "документ — ОС"
    );
    assert!(
        r.resolve(&dir, &entry("a.7z"), &args).is_none(),
        "формат не подключён"
    );
    assert_eq!(
        r.resolve(&dir, &entry("comic.cbz"), &args)
            .map(|c| c.id.as_str().to_owned())
            .as_deref(),
        Some("arc.enter"),
        "своё расширение — по сигнатуре"
    );
    let folder = Entry {
        kind: EntryKind::Dir,
        ..entry("x.zip")
    };
    assert!(r.resolve(&dir, &folder, &args).is_none());
}

#[test]
fn enter_lists_archive_and_up_returns_to_the_file() {
    let tmp = Tmp::new();
    let a = tmp.file("a.zip", &mock::text(&[("src/main.rs", 10), ("../evil", 1)]));
    let b = tmp.file("b.zip", &mock::text(&[("x", 1)]));
    let here = Location::local(&tmp.0);
    // Лимит в байт: в кэше живёт только последний индекс.
    let cache = ArcCache::new(1);
    let mut core = core(&here, &cache);
    settle(&mut core, "каталог", |c| {
        loaded(c, &here) && names(c).len() == 2
    });
    point(&mut core, "a.zip");
    core.dispatch(Command::new("panel.enter"));
    let root = root_of("zip", a.clone());
    settle(&mut core, "вход в архив", |c| loaded(c, &root));
    assert_eq!(names(&core), ["evil", "src"]);
    let model = |c: &Core| {
        c.state()
            .ext
            .get::<ArcModel>()
            .map(|m| (m.get(&a).cloned(), m.get(&b).cloned()))
    };
    settle(&mut core, "сводка", |c| {
        model(c).is_some_and(|(a, _)| a.is_some())
    });
    let s = model(&core).unwrap().0.unwrap();
    assert_eq!((s.format.as_str(), s.files, s.complete), ("zip", 2, true));

    core.dispatch(Command::new("panel.up"));
    settle(&mut core, "выход", |c| loaded(c, &here));
    assert_eq!(
        cursor_name(&core).as_deref(),
        Some("a.zip"),
        "курсор — на файле архива"
    );

    point(&mut core, "b.zip");
    core.dispatch(Command::new("panel.enter"));
    settle(&mut core, "второй архив", |c| {
        loaded(c, &root_of("zip", b.clone()))
    });
    settle(&mut core, "вытеснение", |c| {
        model(c).is_some_and(|(a, b)| a.is_none() && b.is_some())
    });
}

#[test]
fn arc_enter_uses_the_signature() {
    let tmp = Tmp::new();
    let docx = tmp.0.join("report.docx");
    std::fs::write(&docx, b"PK\x03\x04 not really a zip").unwrap();
    tmp.file("notes.txt", "hello");
    let here = Location::local(&tmp.0);
    let cache = ArcCache::new(crate::cache::DEFAULT_LIMIT);
    let mut core = core(&here, &cache);
    settle(&mut core, "каталог", |c| {
        loaded(c, &here) && names(c).len() == 2
    });

    point(&mut core, "report.docx");
    core.dispatch(Command::new("arc.enter"));
    let root = root_of("zip", Location::local(&docx));
    settle(&mut core, "вход в .docx", |c| {
        c.state().active_tab().loc == root
    });
}

#[test]
fn not_an_archive_toasts_format_error() {
    let tmp = Tmp::new();
    tmp.file("notes.txt", "hello");
    let here = Location::local(&tmp.0);
    let cache = ArcCache::new(crate::cache::DEFAULT_LIMIT);
    let mut core = core(&here, &cache);
    settle(&mut core, "каталог", |c| {
        loaded(c, &here) && names(c).len() == 1
    });
    point(&mut core, "notes.txt");
    core.take_ui_events();
    core.dispatch(Command::new("arc.enter"));
    let t = Instant::now();
    let mut seen = Vec::new();
    while t.elapsed() < Duration::from_secs(10) {
        core.pump(Duration::from_millis(5));
        seen.extend(core.take_ui_events());
        if seen.iter().any(|e| {
            matches!(
                e,
                UiEvent::Toast {
                    level: Level::Warn,
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        seen.iter()
            .any(|e| matches!(e, UiEvent::Toast { level: Level::Warn, text } if !text.is_empty())),
        "{seen:?}"
    );
    assert_eq!(core.state().active_tab().loc, here, "остались на месте");
    for id in ["arc.add", "arc.extract", "arc.commands", "arc.test"] {
        core.dispatch(Command::new(id));
    }
    core.pump(Duration::from_millis(5));
    let stubs = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(
        stubs, 4,
        "заглушки и распаковка не-архива отказывают тостом"
    );
}
