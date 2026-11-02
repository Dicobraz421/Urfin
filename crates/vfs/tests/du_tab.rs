//! Вкладка `du:` через core (F01-05): `du.open` → скан → вкладка с размерами папок,
//! по размеру ↓; Enter внутрь и Ctrl+PgUp назад; `du.refresh`; в сессию не попадает.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{
    Attrs, Boot, Command, Core, ListingStatus, Location, Row, Services, Session, Side, Waker,
};
use urfin_vfs::du::{DuCfg, DuRegistry, DuVfs, dvfs};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn setup(start: &Path) -> (Core, Arc<DuRegistry>) {
    let du = DuRegistry::new();
    let mut reg = VfsRegistry::with_local();
    reg.register(Arc::new(DuVfs::new(Arc::clone(&du))));
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
    let size = DuCfg {
        size: urfin_vfs::du::SizeMode::Logical,
        ..DuCfg::default()
    };
    urfin_vfs::du::register_with(&mut core, size, Arc::clone(&du), None);
    (core, du)
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn rows(core: &Core) -> Vec<(String, u64, u32)> {
    let t = core.state().active_tab();
    (0..t.rows())
        .filter_map(|r| match t.row(r) {
            Some(Row::Entry { entry, .. }) => {
                Some((entry.name.to_string(), entry.size, entry.attrs.0))
            }
            _ => None,
        })
        .collect()
}

fn settled(core: &Core) -> bool {
    let t = core.state().active_tab();
    t.loc.backend == "du"
        && matches!(t.listing.status, ListingStatus::Complete)
        && core.state().jobs.iter().all(|j| !j.state.is_active())
        && rows(core)
            .iter()
            .all(|(_, _, a)| a & Attrs::SIZE_PARTIAL == 0)
}

#[test]
fn open_scan_navigate_and_session() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("work");
    fs::create_dir_all(root.join("big").join("inner")).unwrap();
    fs::create_dir_all(root.join("small")).unwrap();
    fs::write(root.join("big").join("a.bin"), vec![0u8; 4000]).unwrap();
    fs::write(
        root.join("big").join("inner").join("b.bin"),
        vec![0u8; 2000],
    )
    .unwrap();
    fs::write(root.join("small").join("c.bin"), vec![0u8; 10]).unwrap();
    fs::write(root.join("mid.iso"), vec![0u8; 3000]).unwrap();
    let (mut core, du) = setup(&root);
    pump_until(&mut core, "local listing", |c| {
        matches!(
            c.state().active_tab().listing.status,
            ListingStatus::Complete
        )
    });
    let tabs = core.state().panel(Side::Left).tabs.len();
    core.dispatch(Command::new("du.open"));
    pump_until(&mut core, "du tab with sizes", |c| {
        settled(c) && rows(c).len() == 3 && rows(c)[0].0 == "big"
    });
    assert_eq!(
        core.state().panel(Side::Left).tabs.len(),
        tabs + 1,
        "new tab"
    );
    // По размеру ↓, файлы и папки вперемешку; у корня анализа нет «..».
    let names: Vec<_> = rows(&core).into_iter().map(|(n, s, _)| (n, s)).collect();
    assert_eq!(
        names,
        [
            ("big".to_owned(), 6000),
            ("mid.iso".to_owned(), 3000),
            ("small".to_owned(), 10)
        ]
    );
    assert!(!core.state().active_tab().has_parent_row());
    let loc = core.state().active_tab().loc.clone();
    let info = du.info(&loc).unwrap();
    assert_eq!((info.files, info.bytes), (4, 9010));
    assert_eq!(info.root, root);
    // Повторный `du.open` того же каталога — без нового скана и вкладки.
    let jobs = core.state().jobs.iter().count();
    core.dispatch(Command::new("du.open").arg("loc", Location::local(&root)));
    assert_eq!(core.state().jobs.iter().count(), jobs);
    // Enter — внутрь, Ctrl+PgUp — назад, курсор на папке.
    core.dispatch(Command::new("panel.cursor.to").arg("name", "big"));
    core.dispatch(Command::new("panel.enter"));
    pump_until(&mut core, "inside big", |c| {
        settled(c) && rows(c).iter().any(|(n, s, _)| n == "inner" && *s == 2000)
    });
    assert!(core.state().active_tab().has_parent_row());
    assert_eq!(
        dvfs::localize(core.state(), &core.state().active_tab().loc),
        Some(Location::local(root.join("big")))
    );
    core.dispatch(Command::new("panel.up"));
    pump_until(&mut core, "back to root", |c| {
        settled(c) && c.state().active_tab().loc == loc
    });
    // Вкладка анализа в сессию не попадает: на её месте — каталог до неё.
    let s = Session::capture(core.state(), None);
    assert!(
        s.panels
            .iter()
            .flat_map(|p| p.tabs.iter())
            .all(|t| t.loc.backend == "local"),
        "{s:?}"
    );
    // Пересчёт: снова задача, затем те же итоги.
    fs::write(root.join("small").join("d.bin"), vec![0u8; 90]).unwrap();
    core.dispatch(Command::new("du.refresh"));
    assert!(du.info(&loc).unwrap().scanning);
    pump_until(&mut core, "rescanned", |c| {
        settled(c) && rows(c).iter().any(|(n, s, _)| n == "small" && *s == 100)
    });
    assert_eq!(du.info(&loc).unwrap().bytes, 9100);
}

/// `du.stop` (Esc) — подсчёт остановлен, вкладка остаётся с посчитанным.
#[test]
fn stop_keeps_partial_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("w");
    for i in 0..40 {
        let d = root.join(format!("d{i}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f.bin"), vec![0u8; 10]).unwrap();
    }
    let (mut core, du) = setup(&root);
    core.dispatch(Command::new("du.open"));
    core.dispatch(Command::new("du.stop"));
    pump_until(&mut core, "scan over", |c| {
        c.state().active_tab().loc.backend == "du"
            && c.state().jobs.iter().all(|j| !j.state.is_active())
    });
    let info = du.info(&core.state().active_tab().loc).unwrap();
    assert!(!info.scanning);
    // Остановка не закрывает вкладку и не теряет посчитанное (могло досчитаться целиком).
    assert!(info.bytes <= 400);
}

/// F01-08: чипы-фильтры (`du.filter`, Ctrl+1…6) — вид вкладки в адресе; «Типы» —
/// группы, Enter раскрывает; порог «Старые» — `du.old_age`; «Временные» ничего не
/// отмечают; `du.map` (Ctrl+Tab) включает и выключает карту.
#[test]
fn filters_switch_views() {
    let _ = urfin_i18n::set_language("ru");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("p");
    fs::create_dir_all(root.join("app").join("target").join("debug")).unwrap();
    fs::write(
        root.join("app").join("target").join("debug").join("a.exe"),
        vec![1u8; 3 << 20],
    )
    .unwrap();
    fs::create_dir_all(root.join("data")).unwrap();
    fs::write(root.join("data").join("big.zip"), vec![1u8; 2 << 20]).unwrap();
    fs::write(root.join("note.txt"), vec![1u8; 100]).unwrap();
    let mut services_resolver = setup(&root);
    let (core, du) = (&mut services_resolver.0, &services_resolver.1);
    core.dispatch(Command::new("du.open"));
    pump_until(core, "du tab", |c| settled(c) && rows(c).len() == 3);
    let base = core.state().active_tab().loc.clone();
    // Крупные: файлы от 1 МиБ, путь от корня.
    core.dispatch(Command::new("du.filter").arg("kind", "large"));
    pump_until(core, "large", |c| {
        settled(c) && rows(c).first().is_some_and(|r| r.0.ends_with("a.exe"))
    });
    let names: Vec<String> = rows(core).into_iter().map(|r| r.0).collect();
    assert_eq!(names, [r"app\target\debug\a.exe", r"data\big.zip"]);
    assert_eq!(
        dvfs::filter_of(&core.state().active_tab().loc),
        Some(urfin_vfs::du::DuFilter::Large)
    );
    // Типы: группы в порядке построения (без сортировки панели); `app	arget` с
    // `debug` — «Сборки и кэши» (решение 2026-11-02).
    core.dispatch(Command::new("du.filter").arg("kind", "types"));
    pump_until(core, "types", |c| {
        settled(c) && rows(c).first().is_some_and(|r| r.0 == "Сборки и кэши")
    });
    assert_eq!(
        core.state().active_tab().sort.key,
        urfin_core::SortKey::Unsorted
    );
    // Enter на группе (разрешитель строк du) — раскрыть.
    let resolver = urfin_vfs::du::open_resolver();
    let t = core.state().active_tab();
    let Some(Row::Entry { entry, .. }) = t.row(0) else {
        panic!("no group row");
    };
    let cmd = resolver
        .resolve(&t.loc, entry, &urfin_core::Args::default())
        .unwrap();
    assert_eq!(cmd.args.str("group"), Some("build"));
    core.dispatch(cmd);
    pump_until(core, "group open", |c| {
        settled(c) && rows(c).get(1).is_some_and(|r| r.0.ends_with("a.exe"))
    });
    // Старые: порог меняется без пересчёта.
    core.dispatch(Command::new("du.filter").arg("kind", "old"));
    pump_until(core, "old", |c| {
        settled(c)
            && dvfs::filter_of(&c.state().active_tab().loc) == Some(urfin_vfs::du::DuFilter::Old)
    });
    assert!(rows(core).is_empty(), "всё свежее");
    let jobs = core.state().jobs.iter().count();
    core.dispatch(Command::new("du.old_age").arg("age", "2y"));
    pump_until(core, "old 2y", |c| {
        settled(c)
            && c.state()
                .active_tab()
                .loc
                .path
                .to_string_lossy()
                .contains("age=2y")
    });
    assert_eq!(core.state().jobs.iter().count(), jobs, "без пересчёта");
    // Временные: сборка Cargo найдена, ничего не отмечено.
    core.dispatch(Command::new("du.filter").arg("kind", "temp"));
    pump_until(core, "temp", |c| {
        settled(c) && rows(c).iter().any(|r| r.0 == r"app\target")
    });
    assert!(core.state().active_tab().selection.is_empty());
    // Дубликаты — следующая задача.
    core.dispatch(Command::new("du.filter").arg("kind", "dups"));
    // Карта: Ctrl+Tab включает и выключает.
    assert!(!dvfs::map_on(core.state()));
    core.dispatch(Command::new("du.map"));
    assert!(dvfs::map_on(core.state()));
    core.dispatch(Command::new("du.map"));
    assert!(!dvfs::map_on(core.state()));
    // По папкам — снова корень анализа.
    core.dispatch(Command::new("du.filter").arg("kind", "dirs"));
    pump_until(core, "dirs", |c| {
        settled(c) && c.state().active_tab().loc == base
    });
    assert!(du.info(&base).is_some());
}

/// «Временные» (решение 2026-11-02 «du-filters» №3): ничего не отмечено, пока не
/// попросили; «Отметить все» — все строки; «Очистить выбранное…» — вопрос со сводкой
/// и последствиями; «Отмена» — ничего не удалено. Только временный каталог.
#[test]
fn temp_clean_asks_first() {
    let _ = urfin_i18n::set_language("ru");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("p");
    let debug = root.join("app").join("target").join("debug");
    fs::create_dir_all(&debug).unwrap();
    fs::write(debug.join("a.exe"), vec![1u8; 3 << 20]).unwrap();
    fs::create_dir_all(root.join("node_modules").join("x")).unwrap();
    fs::write(root.join("node_modules").join("x").join("i.js"), b"1").unwrap();
    // Мелкий одиночный временный файл (меньше `track_min`) — не в снимке, не строка.
    fs::write(root.join("note.tmp"), vec![1u8; 100]).unwrap();
    let (mut core, _du) = setup(&root);
    let core = &mut core;
    core.dispatch(Command::new("du.open"));
    pump_until(core, "du tab", |c| settled(c) && rows(c).len() == 3);
    core.dispatch(Command::new("du.filter").arg("kind", "temp"));
    pump_until(core, "temp", |c| {
        settled(c) && rows(c).iter().any(|r| r.0 == "node_modules")
    });
    assert!(
        core.state().active_tab().selection.is_empty(),
        "сами не отмечаются"
    );
    core.dispatch(Command::new("du.clean_select"));
    let marked = core.state().active_tab().selection.len();
    assert_eq!(marked, 2, "{:?}", rows(core));
    core.dispatch(Command::new("du.clean"));
    let p = core.state().prompts.active().expect("вопрос очистки");
    assert!(p.spec.body.contains("2 строки"), "{}", p.spec.body);
    assert!(
        p.spec
            .body
            .contains(&urfin_i18n::current().get("du-temp-after-cargo")),
        "{}",
        p.spec.body
    );
    let id = p.id;
    core.dispatch(
        Command::new("prompt.reply")
            .arg("id", i64::try_from(id.0).unwrap())
            .arg("choice", "cancel"),
    );
    core.pump(Duration::from_millis(1));
    assert!(core.state().prompts.is_empty());
    assert!(debug.join("a.exe").exists() && root.join("note.tmp").exists());
}
