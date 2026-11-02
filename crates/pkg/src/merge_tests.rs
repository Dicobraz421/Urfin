//! Объединение по фикстурам (`fixtures/merge`, приёмка F02-07).

use super::*;
use crate::arp::ArpScope;

const SEARCH: &str = include_str!("../fixtures/merge/search.txt");

struct Block {
    label: String,
    hits: Vec<Hit>,
}

fn pkg(mgr: Mgr, id: &str, name: &str, version: &str, source: Option<&str>) -> Package {
    let id = PkgId { mgr, id: id.into() };
    let publisher = match mgr {
        Mgr::Winget => crate::winget::publisher_of(&id.id),
        _ => None,
    };
    Package {
        id,
        name: name.into(),
        version: version.into(),
        source: source.map(CompactString::from),
        publisher,
    }
}

fn hit(q: &str, p: Package) -> Hit {
    Hit {
        matched: MatchKind::of(q, &p.id.id, &p.name),
        pkg: p,
    }
}

fn blocks() -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut q = String::new();
    for line in SEARCH.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(head) = line.strip_prefix('=') {
            let (label, query) = head.split_once('|').unwrap();
            q = query.trim().to_owned();
            out.push(Block {
                label: label.trim().to_owned(),
                hits: Vec::new(),
            });
            continue;
        }
        let f: Vec<&str> = line.split('|').map(str::trim).collect();
        let mgr = Mgr::parse(f[0]).unwrap();
        let source = Some(f[4]).filter(|s| !s.is_empty());
        let p = pkg(mgr, f[1], f[2], f[3], source);
        out.last_mut().unwrap().hits.push(hit(&q, p));
    }
    out
}

fn of(hits: &[Hit], mgr: Mgr) -> Vec<Hit> {
    hits.iter()
        .filter(|h| h.pkg.id.mgr == mgr)
        .cloned()
        .collect()
}

/// Ответы в порядке choco → scoop → winget (медленный winget — последним).
fn merged(order: &[Mgr], hits: &[Hit]) -> SearchMerge {
    let mut m = SearchMerge::new(order);
    for mgr in [Mgr::Choco, Mgr::Scoop, Mgr::Winget] {
        m.add(mgr, of(hits, mgr), 0);
    }
    m
}

fn ids(g: &Group) -> Vec<String> {
    g.variants
        .iter()
        .map(|v| v.hit.pkg.id.to_string())
        .collect()
}

/// ≥30 реальных троек: каждая — одна группа со всеми вариантами.
#[test]
fn triples_group() {
    let blocks = blocks();
    assert!(blocks.len() >= 30, "{}", blocks.len());
    for b in &blocks {
        let m = merged(&Mgr::ALL, &b.hits);
        let groups: Vec<Vec<String>> = m.groups().iter().map(ids).collect();
        assert_eq!(groups.len(), 1, "{}: {groups:?}", b.label);
        assert_eq!(m.groups()[0].variants.len(), b.hits.len(), "{}", b.label);
        assert_eq!(
            m.groups()[0].recommended().pkg.id.mgr,
            Mgr::Winget,
            "{}",
            b.label
        );
        // Порядок ответов не меняет группировку.
        let mut w = SearchMerge::new(&Mgr::ALL);
        for mgr in Mgr::ALL {
            w.add(mgr, of(&b.hits, mgr), 0);
        }
        assert_eq!(
            w.groups().iter().map(ids).collect::<Vec<_>>(),
            groups,
            "{}",
            b.label
        );
    }
}

/// Все результаты разом: группа на блок, блоки не смешиваются.
#[test]
fn triples_do_not_mix() {
    let blocks = blocks();
    let all: Vec<Hit> = blocks.iter().flat_map(|b| b.hits.clone()).collect();
    let m = merged(&Mgr::ALL, &all);
    for g in m.groups() {
        let owners: Vec<&str> = blocks
            .iter()
            .filter(|b| g.variants.iter().any(|v| b.hits.contains(&v.hit)))
            .map(|b| b.label.as_str())
            .collect();
        assert_eq!(owners.len(), 1, "{}: {:?} ← {owners:?}", g.title, ids(g));
    }
    assert_eq!(
        m.groups().len(),
        blocks.len(),
        "{:#?}",
        m.groups().iter().map(ids).collect::<Vec<_>>()
    );
}

fn block(label: &str) -> Vec<Hit> {
    blocks()
        .into_iter()
        .find(|b| b.label == label)
        .unwrap()
        .hits
}

/// «Рекомендуется» — первый менеджер `[pkg].order`; у winget источник `winget` раньше
/// `msstore`, основной Id раньше языкового.
#[test]
fn recommended_follows_order() {
    let ff = block("Firefox");
    let m = merged(&Mgr::ALL, &ff);
    assert_eq!(
        ids(&m.groups()[0]),
        [
            "winget:Mozilla.Firefox",
            "winget:Mozilla.Firefox.ru",
            "winget:9NZVDKPMR9RD",
            "scoop:extras/firefox",
            "choco:firefox",
        ]
    );
    assert_eq!(m.groups()[0].title, "Mozilla Firefox");
    let m = merged(&[Mgr::Scoop, Mgr::Choco], &ff);
    assert_eq!(
        ids(&m.groups()[0])[..3],
        [
            "scoop:extras/firefox",
            "choco:firefox",
            "winget:Mozilla.Firefox"
        ]
    );
    // Название — по-прежнему из winget.
    assert_eq!(m.groups()[0].title, "Mozilla Firefox");
    let m = merged(&[Mgr::Choco], &ff);
    assert_eq!(
        m.groups()[0].recommended().pkg.id.to_string(),
        "choco:firefox"
    );
}

/// Новейшая — `accent`; есть только у другого менеджера — рекомендация по порядку.
#[test]
fn newest() {
    let m = merged(&Mgr::ALL, &block("Git"));
    let g = &m.groups()[0];
    assert_eq!(g.recommended().pkg.id.to_string(), "winget:Git.Git");
    assert!(g.newest_elsewhere());
    let newest: Vec<String> = g
        .variants
        .iter()
        .filter(|v| v.newest)
        .map(|v| v.hit.pkg.id.to_string())
        .collect();
    assert_eq!(newest, ["scoop:main/git"]);

    // У winget две ветки Python: рекомендуется новейшая, она же у scoop и choco.
    let m = merged(&Mgr::ALL, &block("Python"));
    let g = &m.groups()[0];
    assert_eq!(
        g.recommended().pkg.id.to_string(),
        "winget:Python.Python.3.13"
    );
    assert!(!g.newest_elsewhere());
    let newest: Vec<String> = g
        .variants
        .iter()
        .filter(|v| v.newest)
        .map(|v| v.hit.pkg.id.to_string())
        .collect();
    assert_eq!(
        newest,
        [
            "winget:Python.Python.3.13",
            "scoop:main/python",
            "choco:python"
        ]
    );

    // `Unknown` не сравнивается и новейшей не бывает.
    let m = merged(&Mgr::ALL, &block("Firefox"));
    let store = m.groups()[0]
        .variants
        .iter()
        .find(|v| v.hit.pkg.source.as_deref() == Some("msstore"))
        .unwrap();
    assert!(!store.newest);
}

fn h(mgr: Mgr, id: &str, name: &str, matched: MatchKind) -> Hit {
    Hit {
        pkg: pkg(mgr, id, name, "1.0", None),
        matched,
    }
}

fn titles(m: &SearchMerge) -> Vec<&str> {
    m.groups().iter().map(|g| g.title.as_str()).collect()
}

/// Порядок групп: совпадение, затем число менеджеров, затем имя.
#[test]
fn group_order() {
    use MatchKind::{Contains, Exact, IdTail, Prefix};
    let mut m = SearchMerge::new(&Mgr::ALL);
    m.add(
        Mgr::Winget,
        vec![
            h(Mgr::Winget, "B.Beta", "Beta", Contains),
            h(Mgr::Winget, "A.Alpha", "Alpha", Contains),
            h(Mgr::Winget, "G.Gamma", "Gamma", Prefix),
            h(Mgr::Winget, "D.Delta", "Delta", Exact),
            h(Mgr::Winget, "E.Eps", "Eps", IdTail),
        ],
        0,
    );
    assert_eq!(titles(&m), ["Delta", "Eps", "Gamma", "Alpha", "Beta"]);
    m.add(
        Mgr::Scoop,
        vec![h(Mgr::Scoop, "main/beta", "beta", Contains)],
        0,
    );
    assert_eq!(titles(&m), ["Delta", "Eps", "Gamma", "Beta", "Alpha"]);
    assert_eq!(m.answered(), [Mgr::Winget, Mgr::Scoop]);
}

/// Ответ менеджера не переставляет группы выше курсора и группу под ним.
#[test]
fn keeps_groups_above_cursor() {
    use MatchKind::{Contains, Exact};
    let mut m = SearchMerge::new(&Mgr::ALL);
    m.add(
        Mgr::Winget,
        vec![
            h(Mgr::Winget, "A.Alpha", "Alpha", Contains),
            h(Mgr::Winget, "B.Beta", "Beta", Contains),
            h(Mgr::Winget, "C.Gamma", "Gamma", Contains),
        ],
        0,
    );
    let before: Vec<u32> = m.groups().iter().map(|g| g.id).collect();
    // Курсор на второй группе: две первые закреплены.
    let late = vec![
        h(Mgr::Choco, "zeta", "zeta", Exact),
        h(Mgr::Choco, "gamma", "gamma", Exact),
        h(Mgr::Choco, "beta", "beta", Exact),
    ];
    let mut kept = m.clone();
    kept.add(Mgr::Choco, late.clone(), 2);
    let after: Vec<u32> = kept.groups().iter().map(|g| g.id).collect();
    assert_eq!(after[..2], before[..2]);
    assert_eq!(titles(&kept), ["Alpha", "Beta", "Gamma", "zeta"]);
    // Вариант дошёл и до закреплённой группы.
    assert_eq!(ids(&kept.groups()[1]), ["winget:B.Beta", "choco:beta"]);
    // Без курсора — обычный порядок.
    m.add(Mgr::Choco, late, 0);
    assert_eq!(titles(&m), ["Beta", "Gamma", "zeta", "Alpha"]);
}

/// Разные издатели winget с одним ключом — разные группы; scoop и choco примыкают
/// к первой.
#[test]
fn publisher_subgroups() {
    use MatchKind::Exact;
    let mut m = SearchMerge::new(&Mgr::ALL);
    m.add(
        Mgr::Winget,
        vec![
            h(Mgr::Winget, "Acme.Tool", "Tool", Exact),
            h(Mgr::Winget, "Other.Tool", "Tool", Exact),
            h(Mgr::Winget, "Acme.Tool.Beta", "Tool (Beta)", Exact),
        ],
        0,
    );
    m.add(
        Mgr::Scoop,
        vec![h(Mgr::Scoop, "main/tool", "tool", Exact)],
        0,
    );
    let groups: Vec<Vec<String>> = m.groups().iter().map(ids).collect();
    assert_eq!(
        groups,
        [
            vec![
                "winget:Acme.Tool",
                "winget:Acme.Tool.Beta",
                "scoop:main/tool"
            ],
            vec!["winget:Other.Tool"],
        ]
    );
}

/// Повторный ответ менеджера заменяет прежний; пустой ответ — тоже ответ.
#[test]
fn answer_replaces() {
    use MatchKind::Exact;
    let mut m = SearchMerge::new(&Mgr::ALL);
    m.add(Mgr::Scoop, vec![h(Mgr::Scoop, "main/a", "a", Exact)], 0);
    m.add(Mgr::Choco, Vec::new(), 0);
    m.add(Mgr::Scoop, vec![h(Mgr::Scoop, "main/b", "b", Exact)], 0);
    assert_eq!(titles(&m), ["b"]);
    assert_eq!(m.answered(), [Mgr::Scoop, Mgr::Choco]);
}

#[test]
fn norm_names() {
    let cases = [
        ("Mozilla Firefox (x64 ru)", "mozillafirefox"),
        ("7-Zip 24.08 (x64)", "7zip"),
        ("Python 3.12.7 (64-bit)", "python"),
        ("Notepad++ (64-bit x64)", "notepadplusplus"),
        (
            "Microsoft Visual Studio Code (User)",
            "microsoftvisualstudiocode",
        ),
        ("NVIDIA Graphics Driver 565.90", "nvidiagraphicsdriver"),
        ("Git version 2.47.0", "gitversion"),
        ("Node.js", "nodejs"),
        ("Яндекс Браузер", "яндексбраузер"),
        ("ＦｉｒｅＦｏｘ", "firefox"),
        ("Sublime Text 4", "sublimetext"),
        ("2048", "2048"),
        ("WinRAR 7.01 x64", "winrar"),
        ("Java 8 Update 431 x86", "java8update"),
    ];
    for (name, want) in cases {
        assert_eq!(norm(name), want, "{name}");
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

fn local(name: &str, version: &str, publisher: Option<&str>) -> Package {
    let mut p = pkg(
        Mgr::Winget,
        &format!(r"ARP\Machine\X64\{name}"),
        name,
        version,
        None,
    );
    p.publisher = publisher.map(CompactString::from);
    p
}

fn show(rows: &[InstalledRow]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            let twins: Vec<String> = r.twins.iter().map(ToString::to_string).collect();
            format!(
                "{} | {} | {}{}{}",
                r.pkg.id,
                r.pkg.name,
                r.pkg.publisher.as_deref().unwrap_or("-"),
                if r.outside { " | вне" } else { "" },
                if twins.is_empty() {
                    String::new()
                } else {
                    format!(" | = {}", twins.join(", "))
                }
            )
        })
        .collect()
}

/// Сводный список: пары choco/scoop с ARP-записями winget, «вне менеджера», издатели;
/// ответы приходят по одному.
#[test]
fn installed() {
    let arp_list = vec![
        arp("Mozilla Firefox (x64 ru)", "Mozilla"),
        arp("Git", "The Git Development Community"),
        arp("7-Zip 24.08 (x64)", "Igor Pavlov"),
        arp("Яндекс Браузер", "ООО «ЯНДЕКС»"),
        arp("Node.js", "Node.js Foundation"),
    ];
    let winget = vec![
        pkg(
            Mgr::Winget,
            "Mozilla.Firefox",
            "Mozilla Firefox (x64 ru)",
            "131.0.2",
            Some("winget"),
        ),
        pkg(
            Mgr::Winget,
            "Python.Python.3.12",
            "Python 3.12.7 (64-bit)",
            "3.12.7",
            Some("winget"),
        ),
        local("Git", "2.47.0", Some("The Git Development Community")),
        local("7-Zip 24.08 (x64)", "24.08", Some("Igor Pavlov")),
        local("Яндекс Браузер", "24.10.1.598", Some("ООО «ЯНДЕКС»")),
        local("Node.js", "20.18.0", Some("Node.js Foundation")),
    ];
    let mut m = InstalledMerge::new(arp_list);
    m.set(Mgr::Winget, winget);
    assert_eq!(
        show(&m.rows()),
        [
            r"winget:ARP\Machine\X64\7-Zip 24.08 (x64) | 7-Zip 24.08 (x64) | Igor Pavlov | вне",
            r"winget:ARP\Machine\X64\Git | Git | The Git Development Community | вне",
            "winget:Mozilla.Firefox | Mozilla Firefox (x64 ru) | Mozilla",
            r"winget:ARP\Machine\X64\Node.js | Node.js | Node.js Foundation | вне",
            "winget:Python.Python.3.12 | Python 3.12.7 (64-bit) | Python",
            r"winget:ARP\Machine\X64\Яндекс Браузер | Яндекс Браузер | ООО «ЯНДЕКС» | вне",
        ]
    );
    m.set(
        Mgr::Choco,
        vec![
            pkg(Mgr::Choco, "7zip", "7zip", "24.8.0", None),
            pkg(Mgr::Choco, "7zip.install", "7zip.install", "24.8.0", None),
            pkg(Mgr::Choco, "git.install", "git.install", "2.47.0", None),
            pkg(Mgr::Choco, "chocolatey", "chocolatey", "2.3.0", None),
        ],
    );
    m.set(
        Mgr::Scoop,
        vec![
            pkg(Mgr::Scoop, "main/nodejs", "nodejs", "20.18.0", Some("main")),
            pkg(
                Mgr::Scoop,
                "extras/firefox",
                "firefox",
                "131.0.2",
                Some("extras"),
            ),
        ],
    );
    assert_eq!(
        show(&m.rows()),
        [
            r"choco:7zip | 7zip | Igor Pavlov | = winget:ARP\Machine\X64\7-Zip 24.08 (x64)",
            "choco:7zip.install | 7zip.install | Igor Pavlov",
            "choco:chocolatey | chocolatey | -",
            "scoop:extras/firefox | firefox | Mozilla",
            r"choco:git.install | git.install | The Git Development Community | = winget:ARP\Machine\X64\Git",
            "winget:Mozilla.Firefox | Mozilla Firefox (x64 ru) | Mozilla",
            r"scoop:main/nodejs | nodejs | Node.js Foundation | = winget:ARP\Machine\X64\Node.js",
            "winget:Python.Python.3.12 | Python 3.12.7 (64-bit) | Python",
            r"winget:ARP\Machine\X64\Яндекс Браузер | Яндекс Браузер | ООО «ЯНДЕКС» | вне",
        ]
    );
    assert_eq!(
        m.answered().collect::<Vec<_>>(),
        [Mgr::Winget, Mgr::Choco, Mgr::Scoop]
    );
    // Повторный ответ заменяет прежний: без choco 7-Zip снова «вне менеджера».
    m.set(Mgr::Choco, Vec::new());
    assert!(
        show(&m.rows())
            .iter()
            .any(|r| r.ends_with("7-Zip 24.08 (x64) | Igor Pavlov | вне"))
    );
}
