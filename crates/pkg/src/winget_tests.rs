//! Разбор вывода winget по фикстурам EN и RU (`fixtures/winget`, приёмка F02-04).

use std::fmt::Write as _;

use super::*;
use crate::api::valid_id;

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

const LIST_EN: &str = include_str!("../fixtures/winget/list.en.txt");
const LIST_RU: &str = include_str!("../fixtures/winget/list.ru.txt");
const UPGRADE_EN: &str = include_str!("../fixtures/winget/upgrade.en.txt");
const UPGRADE_RU: &str = include_str!("../fixtures/winget/upgrade.ru.txt");
const SEARCH_EN: &str = include_str!("../fixtures/winget/search.en.txt");
const SEARCH_RU: &str = include_str!("../fixtures/winget/search.ru.txt");
const SEARCH_NO_MATCH: &str = include_str!("../fixtures/winget/search_nomatch.en.txt");
const EXPORT: &str = include_str!("../fixtures/winget/export.json");

fn arp(scope: ArpScope, key: &str, name: &str, publisher: &str, version: &str) -> ArpEntry {
    ArpEntry {
        scope,
        key: key.into(),
        name: name.into(),
        publisher: Some(publisher.into()),
        version: Some(version.into()),
    }
}

/// ARP машины фикстур.
fn arp_entries() -> Vec<ArpEntry> {
    vec![
        arp(
            ArpScope::Machine,
            "{B2FE1952-0186-46C3-BAEC-A80AA35AC5B8}_Display.Driver",
            "NVIDIA Graphics Driver 565.90",
            "NVIDIA Corporation",
            "565.90",
        ),
        arp(
            ArpScope::User,
            "YandexBrowser",
            "Яндекс Браузер",
            "ООО «ЯНДЕКС»",
            "24.10.1.598",
        ),
        arp(
            ArpScope::Machine,
            "{8BDFE669-9705-4184-9368-DB9CE581E0E7}",
            "Microsoft Visual C++ 2015-2022 Redistributable (x64) - 14.40.33810",
            "Microsoft Corporation",
            "14.40.33810.0",
        ),
        arp(
            ArpScope::Machine32,
            "{5AF95FD8-A22E-458F-ACEE-C61BD787178E}",
            "Microsoft Visual C++ 2015-2022 Redistributable (x86) - 14.40.33810",
            "Microsoft Corporation",
            "14.40.33810.0",
        ),
        arp(
            ArpScope::User,
            "JetBrains Toolbox",
            "JetBrains Toolbox App Long Display Name Edition",
            "JetBrains s.r.o.",
            "2.5.1.34629",
        ),
        arp(
            ArpScope::Machine,
            "7-Zip",
            "7-Zip 24.08 (x64)",
            "Igor Pavlov",
            "24.08",
        ),
    ]
}

fn opt(s: Option<&CompactString>) -> &str {
    s.map_or("-", CompactString::as_str)
}

fn show_packages(pkgs: &[Package]) -> String {
    let mut out = String::new();
    for p in pkgs {
        writeln!(
            out,
            "{} | {} | {} | {} | {}",
            p.id.id,
            p.name,
            p.version,
            opt(p.source.as_ref()),
            opt(p.publisher.as_ref())
        )
        .unwrap();
    }
    out
}

fn installed(text: &str) -> Vec<Package> {
    let export = parse_export(EXPORT).unwrap();
    let arp = arp_entries();
    let mut rows = rows(&lines(text));
    repair(&mut rows, &export, &arp);
    rows.iter().map(|r| package(r, &arp)).collect()
}

/// Все 14 строк `winget list`, включая ARP/MSIX без Source и строки без Available;
/// усечённые Id и имена починены, где однозначно. RU разбирается так же.
#[test]
fn list_en_ru() {
    let en = installed(LIST_EN);
    assert_eq!(en.len(), 14);
    assert_eq!(installed(LIST_RU), en);
    let by_id = |id: &str| en.iter().find(|p| p.id.id == id).unwrap();
    assert_eq!(
        by_id("Microsoft.VisualStudio.2022.Community").name,
        "Microsoft Visual Studio Community 2022"
    );
    let nvidia = by_id(r"ARP\Machine\X64\{B2FE1952-0186-46C3-BAEC-A80AA35AC5B8}_Display.Driver");
    assert_eq!(nvidia.source, None);
    assert_eq!(nvidia.publisher.as_deref(), Some("NVIDIA Corporation"));
    assert_eq!(
        by_id("JetBrains.Toolbox").name,
        "JetBrains Toolbox App Long Display Name Edition"
    );
    // Два VC++ с одинаковым началом имени и версией — имя остаётся усечённым.
    assert!(is_truncated(&by_id("Microsoft.VCRedist.2015+.x64").name));
    // MSIX без источника не чинится, но и не теряется.
    assert!(
        en.iter()
            .any(|p| p.id.id.starts_with(r"MSIX\Microsoft.UI.Xaml") && is_truncated(&p.id.id))
    );
    insta::assert_snapshot!(show_packages(&en));
}

/// Id в argv: все неусечённые Id фикстур проходят регэксп winget.
#[test]
fn parsed_ids_pass_regex() {
    for p in installed(LIST_EN) {
        assert_eq!(
            valid_id(Mgr::Winget, &p.id.id),
            !is_truncated(&p.id.id),
            "{}",
            p.id.id
        );
    }
}

fn show_updates(ups: &[Update]) -> String {
    let mut out = String::new();
    for u in ups {
        writeln!(
            out,
            "{} | {} | {} -> {} | {}{}",
            u.pkg.id.id,
            u.pkg.name,
            u.pkg.version,
            u.available,
            opt(u.pkg.source.as_ref()),
            if u.pinned { " | explicit" } else { "" }
        )
        .unwrap();
    }
    out
}

fn upgrades(text: &str) -> Vec<Update> {
    let arp = arp_entries();
    let mut rows = rows(&lines(text));
    repair(&mut rows, &parse_export(EXPORT).unwrap(), &arp);
    updates(&rows, &arp)
}

/// `winget upgrade`: хвостовые строки и пины отброшены, вторая таблица
/// («требуют явного указания») — `pinned`.
#[test]
fn upgrade_en_ru() {
    let en = upgrades(UPGRADE_EN);
    assert_eq!(en.len(), 5);
    assert_eq!(upgrades(UPGRADE_RU), en);
    assert_eq!(en.iter().filter(|u| u.pinned).count(), 1);
    assert!(
        en.iter()
            .any(|u| u.pinned && u.pkg.id.id == "Discord.Discord")
    );
    insta::assert_snapshot!(show_updates(&en));
}

fn show_hits(hits: &[Hit]) -> String {
    let mut out = String::new();
    for h in hits {
        writeln!(
            out,
            "{:?} | {} | {} | {} | {} | {}",
            h.matched,
            h.pkg.id.id,
            h.pkg.name,
            h.pkg.version,
            opt(h.pkg.source.as_ref()),
            opt(h.pkg.publisher.as_ref())
        )
        .unwrap();
    }
    out
}

/// `winget search`: колонка Match не путается с Available, msstore без издателя.
#[test]
fn search_en_ru() {
    let en = hits("firefox", &rows(&lines(SEARCH_EN)));
    assert_eq!(en.len(), 5);
    assert_eq!(hits("firefox", &rows(&lines(SEARCH_RU))), en);
    insta::assert_snapshot!(show_hits(&en));
}

#[test]
fn search_without_match_column() {
    let r = rows(&lines(SEARCH_NO_MATCH));
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].source.as_deref(), Some("winget"));
    assert_eq!(r[1].source.as_deref(), Some("msstore"));
    assert_eq!(r[1].available, None);
}

#[test]
fn no_table_no_rows() {
    assert_eq!(
        rows(&lines("No package found matching input criteria.\n")),
        []
    );
    assert_eq!(rows(&[]), []);
}

#[test]
fn export_file() {
    let e = parse_export(EXPORT).unwrap();
    assert_eq!(e.len(), 14);
    assert_eq!(e[0].id, "7zip.7zip");
    assert_eq!(e[0].version.as_deref(), Some("24.08"));
    assert_eq!(e[0].source, "winget");
    let store = e.last().unwrap();
    assert_eq!(
        (store.source.as_str(), store.version.as_ref()),
        ("msstore", None)
    );
    assert!(parse_export("{").is_err());
}

#[test]
fn output_flag_from_help() {
    let help = lines(
        "Usage: winget list [[-q] <query>] [<options>]\n\
         \x20 -n,--count                  Show no more than specified number of results\n\
         \x20 --output                    Output format: text, json\n",
    );
    assert!(knows_output(&help));
    assert!(!knows_output(&help[..2]));
}

#[test]
fn json_rows_tolerant() {
    let r = json_rows(
        "progress\n[{\"Name\":\"Git\",\"Id\":\"Git.Git\",\"Version\":\"2.46.0\",\"Available\":\"2.47.0\",\"Source\":\"winget\"}]",
    )
    .unwrap();
    assert_eq!(r[0].id, "Git.Git");
    assert_eq!(r[0].available.as_deref(), Some("2.47.0"));
    let r = json_rows(
        "{\"Packages\":[{\"PackageIdentifier\":\"7zip.7zip\",\"InstalledVersion\":\"24.08\"}]}",
    )
    .unwrap();
    assert_eq!(
        (r[0].id.as_str(), r[0].version.as_str()),
        ("7zip.7zip", "24.08")
    );
    assert!(json_rows("[{\"Name\":\"no id\"}]").is_err());
    assert!(json_rows("text").is_err());
}

fn strs(argv: &[OsString]) -> Vec<String> {
    argv.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn argv_shapes() {
    let exe = Path::new(r"C:\winget.exe");
    assert_eq!(
        strs(&list_argv(exe)),
        [
            r"C:\winget.exe",
            "list",
            "--accept-source-agreements",
            "--disable-interactivity"
        ]
    );
    assert_eq!(
        strs(&upgrade_list_argv(exe, true))[1..],
        [
            "upgrade",
            "--accept-source-agreements",
            "--disable-interactivity",
            "--include-unknown"
        ]
    );
    assert_eq!(
        strs(&search_argv(exe, &search_query("  --firefox\u{7} "))),
        [
            r"C:\winget.exe",
            "search",
            "--query",
            "firefox",
            "--accept-source-agreements",
            "--disable-interactivity"
        ]
    );
    assert_eq!(
        strs(&export_argv(exe, Path::new(r"C:\t\e.json")))[1..5],
        ["export", "-o", r"C:\t\e.json", "--include-versions"]
    );
}

#[test]
fn action_plans() {
    let exe = Path::new(r"C:\winget.exe");
    let id: PkgId = "winget:Mozilla.Firefox".parse().unwrap();
    let p = action_plan(exe, PkgOp::Upgrade, &id, true).unwrap();
    assert_eq!(
        strs(&p.argv),
        [
            r"C:\winget.exe",
            "upgrade",
            "--id",
            "Mozilla.Firefox",
            "--exact",
            "--accept-source-agreements",
            "--disable-interactivity",
            "--accept-package-agreements",
            "--silent"
        ]
    );
    assert!(!p.elevate);
    assert!(p.success_codes.contains(&0));
    assert!(p.success_codes.contains(&code::UPDATE_NOT_APPLICABLE));
    assert!(
        p.reboot_codes
            .contains(&code::INSTALL_REBOOT_REQUIRED_TO_FINISH)
    );
    let p = action_plan(exe, PkgOp::Install, &id, false).unwrap();
    assert!(!strs(&p.argv).contains(&"--silent".to_owned()));
    assert!(p.success_codes.contains(&code::PACKAGE_ALREADY_INSTALLED));
    let local: PkgId = r"winget:ARP\Machine\X64\7-Zip".parse().unwrap();
    let p = action_plan(exe, PkgOp::Uninstall, &local, true).unwrap();
    assert_eq!(
        strs(&p.argv)[1..5],
        ["uninstall", "--id", r"ARP\Machine\X64\7-Zip", "--exact"]
    );
    assert!(!strs(&p.argv).contains(&"--accept-package-agreements".to_owned()));
    assert_eq!(p.success_codes, [0]);
    for (op, bad) in [
        (PkgOp::Install, r"winget:ARP\Machine\X64\7-Zip"),
        (PkgOp::Install, "winget:--override"),
        (PkgOp::Upgrade, "winget:Microsoft.VisualStudio.2022.Commu…"),
        (PkgOp::Upgrade, "choco:git"),
    ] {
        let id: PkgId = bad.parse().unwrap();
        assert!(
            matches!(action_plan(exe, op, &id, true), Err(PkgError::BadId(_))),
            "{bad}"
        );
    }
}

/// `pkg.info`: `winget show`; локальный Id в источниках не найти.
#[test]
fn info_show_argv() {
    let id: PkgId = "winget:7zip.7zip".parse().unwrap();
    let a = info_argv(Path::new("winget.exe"), &id).unwrap();
    let text: Vec<_> = a.iter().map(|s| s.to_string_lossy()).collect();
    assert_eq!(
        text[..5],
        ["winget.exe", "show", "--id", "7zip.7zip", "--exact"]
    );
    let local: PkgId = r"winget:ARP\Machine\X64\Foo".parse().unwrap();
    assert!(info_argv(Path::new("winget.exe"), &local).is_err());
    let scoop: PkgId = "scoop:7zip".parse().unwrap();
    assert!(info_argv(Path::new("winget.exe"), &scoop).is_err());
}
