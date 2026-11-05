//! Модель вкладки: строки фильтров, «вне менеджера», «Доступна», «важные», кэш.

use super::*;
use crate::arp::ArpScope;

fn pkg(id: &str, name: &str, version: &str, source: Option<&str>) -> Package {
    let id: PkgId = id.parse().unwrap();
    Package {
        publisher: match id.mgr {
            Mgr::Winget if source.is_some() => crate::winget::publisher_of(&id.id),
            _ => None,
        },
        id,
        name: name.into(),
        version: version.into(),
        source: source.map(CompactString::from),
    }
}

fn up(p: Package, available: &str, pinned: bool) -> Update {
    Update {
        pkg: p,
        available: available.into(),
        pinned,
    }
}

fn arp(name: &str, publisher: &str) -> ArpEntry {
    ArpEntry {
        scope: ArpScope::Machine,
        key: name.into(),
        name: name.into(),
        publisher: Some(publisher.into()),
        version: None,
    }
}

/// Машина как на макете: winget с источником, ARP без источника (одна запись — пара
/// choco), scoop из бакета, choco.
fn filled(important: &[&str]) -> Arc<PkgBook> {
    let cfg = PkgConfig {
        important: important.iter().map(|s| s.parse().unwrap()).collect(),
        ..PkgConfig::default()
    };
    let book = PkgBook::new(&cfg);
    let found = |mgr| Detected {
        mgr,
        exe: format!("{mgr}.exe").into(),
        version: None,
        caps: crate::api::MgrCaps::empty(),
    };
    book.found(
        vec![found(Mgr::Choco), found(Mgr::Winget), found(Mgr::Scoop)],
        vec![
            arp("Mozilla Firefox (x64 ru)", "Mozilla"),
            arp("Total Commander 11.55 (x64)", "Ghisler Software GmbH"),
            arp("Git", "The Git Development Community"),
        ],
    );
    let firefox = pkg(
        "winget:Mozilla.Firefox",
        "Mozilla Firefox",
        "143.0.4",
        Some("winget"),
    );
    let vscode = pkg(
        "winget:Microsoft.VisualStudioCode",
        "Microsoft Visual Studio Code",
        "1.104.3",
        Some("winget"),
    );
    book.set_installed(
        Mgr::Winget,
        vec![
            firefox.clone(),
            vscode.clone(),
            pkg(
                r"winget:ARP\Machine\X64\Total Commander",
                "Total Commander 11.55 (x64)",
                "11.55",
                None,
            ),
            pkg(r"winget:ARP\Machine\X64\Git_is1", "Git", "2.51.0", None),
        ],
    );
    let jq = pkg("scoop:main/jq", "jq", "1.8.1", Some("main"));
    book.set_installed(Mgr::Scoop, vec![jq.clone()]);
    let git = pkg("choco:git", "git", "2.51.0", None);
    book.set_installed(Mgr::Choco, vec![git.clone()]);
    book.set_updates(
        Mgr::Winget,
        vec![up(firefox, "144.0", false), up(vscode, "1.105.1", false)],
        None,
    );
    book.set_updates(Mgr::Choco, vec![up(git, "2.51.1", true)], None);
    book.set_updates(Mgr::Scoop, Vec::new(), None);
    book
}

fn names(v: &View, f: Filter) -> Vec<&str> {
    v.rows(f).iter().map(|r| r.name.as_str()).collect()
}

/// «Установленные»: пара choco + ARP — одна строка choco с издателем ARP; ARP без
/// пары — «вне менеджера» без «Доступна»; обновление — у строки своего пакета.
#[test]
fn installed_rows() {
    let v = filled(&[]).view();
    assert_eq!(
        names(&v, Filter::Installed),
        [
            "git",
            "jq",
            "Microsoft Visual Studio Code",
            "Mozilla Firefox",
            "Total Commander 11.55 (x64)",
        ]
    );
    let git = v.row(Filter::Installed, "git").unwrap();
    assert_eq!(git.pkg.id.mgr, Mgr::Choco);
    assert_eq!(git.available.as_deref(), Some("2.51.1"));
    assert!(git.pinned);
    assert_eq!(
        git.pkg.publisher.as_deref(),
        Some("The Git Development Community")
    );
    let tc = v
        .row(Filter::Installed, "Total Commander 11.55 (x64)")
        .unwrap();
    assert!(tc.outside);
    assert_eq!(tc.available, None);
    let ff = v.row(Filter::Installed, "Mozilla Firefox").unwrap();
    assert_eq!(ff.available.as_deref(), Some("144.0"));
    assert_eq!(ff.pkg.publisher.as_deref(), Some("Mozilla"));
    assert_eq!(v.info.installed, 5);
    assert_eq!(v.info.updates, 3);
    // Менеджеры — в порядке `[pkg].order`, все ответили.
    let order: Vec<Mgr> = v.info.managers.iter().map(|m| m.mgr).collect();
    assert_eq!(order, [Mgr::Winget, Mgr::Scoop, Mgr::Choco]);
    assert!(!v.info.busy());
}

/// «Обновления»: важные сверху (и по двойнику ARP), затем по имени.
#[test]
fn updates_important_first() {
    let book = filled(&[
        "winget:Microsoft.VisualStudioCode",
        r"winget:ARP\Machine\X64\Git_is1",
    ]);
    let v = book.view();
    assert_eq!(
        names(&v, Filter::Updates),
        ["git", "Microsoft Visual Studio Code", "Mozilla Firefox"]
    );
    assert!(v.row(Filter::Updates, "git").unwrap().important);
    assert_eq!(v.info.important, 2);
    // Порядок важных меняется настройкой сразу.
    book.configure(&PkgConfig::default());
    let v = book.view();
    assert_eq!(v.info.important, 0);
    assert_eq!(
        names(&v, Filter::Updates),
        ["git", "Microsoft Visual Studio Code", "Mozilla Firefox"]
    );
}

/// Ячейки колонок: «вне менеджера», бакет scoop вместо издателя, «Доступна».
#[test]
fn cells() {
    let v = filled(&[]).view();
    let cell = |name: &str, col: &str| {
        crate::vfs::cell(v.row(Filter::Installed, name).unwrap(), col).map(String::from)
    };
    assert_eq!(
        cell("Mozilla Firefox", "available").as_deref(),
        Some("144.0")
    );
    assert_eq!(cell("Mozilla Firefox", "mgr").as_deref(), Some("winget"));
    assert_eq!(cell("jq", "publisher").as_deref(), Some("bucket main"));
    assert_eq!(cell("jq", "available"), None);
    assert_eq!(
        cell("Total Commander 11.55 (x64)", "mgr").as_deref(),
        Some("not managed")
    );
    assert_eq!(cell("git", "version").as_deref(), Some("2.51.0"));
}

/// Одноимённые строки получают менеджер в скобках: курсор держится за имя.
#[test]
fn unique_names() {
    let book = PkgBook::new(&PkgConfig::default());
    book.set_installed(
        Mgr::Winget,
        vec![pkg("winget:Git.Git", "Git", "2.51.0", Some("winget"))],
    );
    book.set_installed(
        Mgr::Scoop,
        vec![pkg("scoop:main/git", "git", "2.51.0", Some("main"))],
    );
    let v = book.view();
    assert_eq!(names(&v, Filter::Installed), ["Git", "git (scoop)"]);
    assert_eq!(
        v.row(Filter::Installed, "git (scoop)").unwrap().pkg.id.mgr,
        Mgr::Scoop
    );
}

/// Ошибка менеджера: прежний список остаётся, ход — «не удалось»; дата проверки —
/// если ответил хоть один.
#[test]
fn failure_keeps_list() {
    let book = filled(&[]);
    book.start();
    assert!(book.info().busy());
    book.failed(Mgr::Scoop, "timed out".into());
    book.set_updates(Mgr::Winget, Vec::new(), None);
    book.set_updates(Mgr::Choco, Vec::new(), None);
    book.finish("2026-11-02T09:00:00Z".parse().unwrap());
    let info = book.info();
    assert!(!info.busy());
    assert_eq!(info.installed, 5);
    assert_eq!(info.updates, 0);
    let scoop = info.managers.iter().find(|m| m.mgr == Mgr::Scoop).unwrap();
    assert_eq!(scoop.run, Run::Failed("timed out".into()));
    assert!(info.checked_at.is_some());
}

/// Кэш: строки, ошибки менеджеров и дата переживают перезапуск; формат на диске —
/// снапшот (его читает следующая версия).
#[test]
fn cache_roundtrip() {
    let book = filled(&[]);
    book.failed(Mgr::Scoop, "timed out".into());
    book.finish("2026-11-02T09:00:00Z".parse().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg").join(CACHE_FILE);
    book.cache().save(&path).unwrap();
    let back = PkgBook::new(&PkgConfig::default());
    assert!(!back.info().loaded);
    back.load(PkgCache::load(&path));
    let (a, b) = (book.view(), back.view());
    assert_eq!(a.info, b.info);
    assert_eq!(a.rows(Filter::Installed), b.rows(Filter::Installed));
    assert_eq!(a.rows(Filter::Updates), b.rows(Filter::Updates));
    let mut small = PkgCache {
        checked_at: Some("2026-11-02T09:00:00Z".parse().unwrap()),
        managers: vec![MgrStatus {
            mgr: Mgr::Winget,
            ok: true,
            error: None,
        }],
        ..PkgCache::default()
    };
    small.installed.insert(
        Mgr::Scoop,
        vec![pkg("scoop:main/jq", "jq", "1.8.1", Some("main"))],
    );
    small.arp = vec![arp("jq", "jqlang")];
    insta::assert_snapshot!(serde_json::to_string_pretty(&small).unwrap());
    // Битый файл — как нет кэша.
    std::fs::write(&path, "{").unwrap();
    assert_eq!(PkgCache::load(&path), None);
}

/// Пропавший менеджер уходит вместе со списками.
#[test]
fn lost_manager_drops_lists() {
    let book = filled(&[]);
    book.found(
        vec![Detected {
            mgr: Mgr::Winget,
            exe: "winget.exe".into(),
            version: None,
            caps: crate::api::MgrCaps::empty(),
        }],
        Vec::new(),
    );
    let v = book.view();
    assert!(
        v.rows(Filter::Installed)
            .iter()
            .all(|r| r.pkg.id.mgr == Mgr::Winget)
    );
    assert_eq!(v.info.managers.len(), 1);
}

/// Дубль winget в «Обновлениях» у программы, которую ведёт choco или scoop, скрыт
/// (решение 2026-11-02 «pkg-updates» №1); обновление winget своей программы — на месте.
#[test]
fn updates_without_winget_duplicates() {
    let book = PkgBook::new(&PkgConfig::default());
    let git_choco = pkg("choco:git.install", "git.install", "2.47.0", None);
    let node_scoop = pkg("scoop:main/nodejs", "nodejs", "20.18.0", Some("main"));
    let git_winget = pkg("winget:Git.Git", "Git", "2.47.0", Some("winget"));
    let node_winget = pkg("winget:OpenJS.NodeJS", "Node.js", "20.18.0", Some("winget"));
    let vlc = pkg(
        "winget:VideoLAN.VLC",
        "VLC media player",
        "3.0.20",
        Some("winget"),
    );
    book.set_installed(Mgr::Choco, vec![git_choco.clone()]);
    book.set_installed(Mgr::Scoop, vec![node_scoop.clone()]);
    book.set_installed(Mgr::Winget, vec![vlc.clone()]);
    book.set_updates(Mgr::Choco, vec![up(git_choco, "2.47.1", false)], None);
    book.set_updates(Mgr::Scoop, vec![up(node_scoop, "22.11.0", false)], None);
    book.set_updates(
        Mgr::Winget,
        vec![
            up(git_winget, "2.47.1", false),
            up(node_winget, "22.11.0", false),
            up(vlc, "3.0.21", false),
        ],
        None,
    );
    let v = book.view();
    let ids: Vec<String> = v
        .rows(Filter::Updates)
        .iter()
        .map(|r| r.pkg.id.to_string())
        .collect();
    assert_eq!(
        ids,
        [
            "choco:git.install",
            "scoop:main/nodejs",
            "winget:VideoLAN.VLC"
        ]
    );
}

/// Смена порядка переранжирует открытые результаты поиска и строку менеджеров; область
/// поиска по умолчанию применяется, пока запроса нет.
#[test]
fn configure_reorders_open_search() {
    let book = filled(&[]);
    let hit = |id: &str, name: &str| crate::api::Hit {
        matched: urfin_core::pkg::MatchKind::Exact,
        pkg: pkg(id, name, "1.0", Some("main")),
    };
    let mgrs = [Mgr::Winget, Mgr::Scoop];
    let g = book.search_start("fd", &mgrs);
    book.search_answer(g, Mgr::Winget, Ok(vec![hit("winget:sharkdp.fd", "fd")]), 0);
    book.search_answer(g, Mgr::Scoop, Ok(vec![hit("scoop:main/fd", "fd")]), 0);
    let first = || book.view().search.list[0].target().to_string();
    assert_eq!(first(), "winget:sharkdp.fd");
    let cfg = PkgConfig {
        order: vec![Mgr::Scoop, Mgr::Choco, Mgr::Winget],
        search_in: SearchIn::Scoop,
        ..PkgConfig::default()
    };
    book.configure(&cfg);
    assert_eq!(first(), "scoop:main/fd");
    let answers: Vec<Mgr> = book
        .view()
        .search
        .info
        .answers
        .iter()
        .map(|a| a.0)
        .collect();
    assert_eq!(answers, [Mgr::Scoop, Mgr::Winget]);
    let order: Vec<Mgr> = book.info().managers.iter().map(|m| m.mgr).collect();
    assert_eq!(order, [Mgr::Scoop, Mgr::Choco, Mgr::Winget]);
    // Запрос идёт: область не меняется; без запроса — из конфига.
    assert_eq!(book.view().search.info.scope, SearchIn::All);
    let fresh = PkgBook::new(&PkgConfig::default());
    fresh.configure(&cfg);
    assert_eq!(fresh.view().search.info.scope, SearchIn::Scoop);
}

/// `known` сверяет кэш с системой: пропавший уходит, новый появляется, списки и ход не
/// трогаются; версия менеджера — в сводке.
#[test]
fn known_follows_system() {
    let book = filled(&[]);
    let before = book.info().installed;
    let found = |mgr, version: &str| Detected {
        mgr,
        exe: format!("{mgr}.exe").into(),
        version: Some(version.into()),
        caps: crate::api::MgrCaps::empty(),
    };
    book.known(vec![found(Mgr::Winget, "1.9"), found(Mgr::Scoop, "0.5.3")]);
    let info = book.info();
    let lines: Vec<(Mgr, Option<&str>)> = info
        .managers
        .iter()
        .map(|m| (m.mgr, m.version.as_deref()))
        .collect();
    assert_eq!(
        lines,
        [(Mgr::Winget, Some("1.9")), (Mgr::Scoop, Some("0.5.3"))]
    );
    assert_eq!(info.installed, before);
}

/// «Важное» со сводной строки снимается вместе с двойниками.
#[test]
fn family_holds_twins() {
    let book = filled(&[]);
    let view = book.view();
    let row = view
        .rows(Filter::Installed)
        .iter()
        .find(|r| r.pkg.id.mgr == Mgr::Choco)
        .unwrap();
    let id = row.pkg.id.clone();
    let family = book.family(&id);
    assert!(family.contains(&id), "{family:?}");
    assert!(family.len() > 1, "{family:?}");
    let other: PkgId = "winget:Nobody.Here".parse().unwrap();
    assert_eq!(book.family(&other), [other]);
}
