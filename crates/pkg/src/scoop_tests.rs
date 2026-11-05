//! scoop по фикстурам (`fixtures/scoop`) и на подставном `scoop.ps1` под
//! PowerShell 5.1 и pwsh 7 (приёмка F02-05).

use std::fmt::Write as _;
use std::path::PathBuf;

use super::*;

const EXPORT: &str = include_str!("../fixtures/scoop/export.json");
const EXPORT_LEGACY: &str = include_str!("../fixtures/scoop/export.legacy.txt");
const STATUS_MANY: &str = include_str!("../fixtures/scoop/status.many.json");
const STATUS_ONE: &str = include_str!("../fixtures/scoop/status.one.json");
const SEARCH_MANY: &str = include_str!("../fixtures/scoop/search.many.json");
const SEARCH_ONE: &str = include_str!("../fixtures/scoop/search.one.json");
const OUT_STATUS: &str = include_str!("../fixtures/scoop/out.status.txt");

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn show_packages(pkgs: &[Package]) -> String {
    let mut s = String::new();
    for p in pkgs {
        let src = p.source.as_deref().unwrap_or("-");
        writeln!(s, "{} | {} | {} | {src}", p.id, p.name, p.version).unwrap();
    }
    s
}

fn show_updates(ups: &[Update]) -> String {
    let mut s = String::new();
    for u in ups {
        let pin = if u.pinned { " (pinned)" } else { "" };
        writeln!(
            s,
            "{} | {} -> {}{pin}",
            u.pkg.id, u.pkg.version, u.available
        )
        .unwrap();
    }
    s
}

fn show_hits(hits: &[Hit]) -> String {
    let mut s = String::new();
    for h in hits {
        let p = &h.pkg;
        writeln!(s, "{} | {} | {:?}", p.id, p.version, h.matched).unwrap();
    }
    s
}

#[test]
fn quote_for_powershell() {
    assert_eq!(ps_quote("7zip"), "'7zip'");
    assert_eq!(
        ps_quote(r"C:\Users\O'Brien\scoop"),
        r"'C:\Users\O''Brien\scoop'"
    );
    assert_eq!(ps_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
    assert_eq!(ps_quote("$env:x `n\0\r\n"), "'$env:x `n'");
}

#[test]
fn encoded_command_is_utf16le_base64() {
    let script = "'<<URFIN>> тест'";
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encode(script))
        .unwrap();
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), script);
}

/// Скрипт из argv обратно в текст.
fn decoded(argv: &[OsString]) -> String {
    let b64 = argv.last().unwrap().to_str().unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).unwrap()
}

#[test]
fn scripts() {
    let scoop = Path::new(r"C:\Users\me\scoop\shims\scoop.ps1");
    let argv = powershell_argv(Path::new("powershell.exe"), &export_script(scoop));
    let head: Vec<&str> = argv[1..argv.len() - 1]
        .iter()
        .map(|a| a.to_str().unwrap())
        .collect();
    assert_eq!(
        head,
        [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand"
        ]
    );
    let s = decoded(&argv);
    assert!(s.contains("UTF8Encoding"), "{s}");
    assert!(s.contains(r"$scoop = 'C:\Users\me\scoop\shims\scoop.ps1'"));
    assert!(s.contains("& $scoop export 6>$null"));
    assert!(s.contains("'<<URFIN>> export'"));

    let with = status_script(scoop, true);
    assert!(with.contains("& $scoop update *>&1"));
    assert!(with.find("update").unwrap() < with.find(MARKER).unwrap());
    assert!(with.contains("& $scoop status --local 6>$null"));
    assert!(with.contains("ConvertTo-Json -InputObject $r"));
    assert!(!status_script(scoop, false).contains("$scoop update"));

    let s = search_script(scoop, "it's");
    assert!(s.contains("& $scoop search 'it''s' 6>$null"), "{s}");
}

/// Приёмка: `scoop export` (JSON и старый текст) — без потерь; Id — по регэкспу scoop.
#[test]
fn export_json_and_legacy() {
    let pkgs = parse_export(EXPORT).unwrap();
    insta::assert_snapshot!(show_packages(&pkgs), @r"
    scoop:main/7zip | 7zip | 24.08 | main
    scoop:main/git | git | 2.47.0.2 | main
    scoop:extras/vcredist2022 | vcredist2022 | 14.40.33816.0 | extras
    scoop:main/nodejs-lts | nodejs-lts | 20.18.0 | main
    scoop:tool | tool | 1.0 | https://example.org/manifests/tool.json
    ");
    assert_eq!(parse_export(EXPORT_LEGACY).unwrap(), pkgs);
    for p in &pkgs {
        assert!(valid_id(Mgr::Scoop, &p.id.id), "{}", p.id);
    }
    assert_eq!(parse_export("").unwrap(), []);
    assert_eq!(parse_export(r#"{"apps":[]}"#).unwrap(), []);
    assert!(parse_export("Scoop is not installed").is_err());
}

/// Приёмка: `status` — массив и один объект (`ConvertTo-Json` по конвейеру), а
/// также обёртка PowerShell 5.1 `{value, Count}`.
#[test]
fn status_one_and_many() {
    let installed = parse_export(EXPORT).unwrap();
    let many = parse_status(STATUS_MANY, &installed).unwrap();
    insta::assert_snapshot!(show_updates(&many), @r"
    scoop:main/git | 2.47.0.2 -> 2.47.1
    scoop:main/nodejs-lts | 20.18.0 -> 22.11.0 (pinned)
    ");
    let one = parse_status(STATUS_ONE, &installed).unwrap();
    assert_eq!(one, many[..1]);
    let wrapped = format!(r#"{{"value":{STATUS_MANY},"Count":5}}"#);
    assert_eq!(parse_status(&wrapped, &installed).unwrap(), many);
    // Без export — только имя.
    let bare = parse_status(STATUS_ONE, &[]).unwrap();
    assert_eq!(bare[0].pkg.id.id, "git");
    assert_eq!(parse_status("", &installed).unwrap(), []);
    assert_eq!(parse_status("[]", &installed).unwrap(), []);
}

/// Приёмка: `search` — 1 и N элементов; строки в конвейере пропускаются.
#[test]
fn search_one_and_many() {
    let many = parse_search("7zip", SEARCH_MANY).unwrap();
    insta::assert_snapshot!(show_hits(&many), @r"
    scoop:main/7zip | 24.08 | Exact
    scoop:extras/7zip-zstd | 24.05 | Prefix
    scoop:versions/7zip19.00-helper | 19.00 | Prefix
    scoop:extras/peazip | 10.0.0 | Contains
    ");
    let one = parse_search("firefox", SEARCH_ONE).unwrap();
    assert_eq!(show_hits(&one), "scoop:extras/firefox | 132.0 | Exact\n");
    let stray = r#"["WARN  stray", {"Name":"firefox","Version":"132.0","Source":"extras"}]"#;
    assert_eq!(parse_search("firefox", stray).unwrap(), one);
    assert_eq!(parse_search("x", "").unwrap(), []);
}

/// Приёмка: вывод `scoop update` и мусор до маркера не мешают; маркер не в начале
/// строки — не маркер.
#[test]
fn garbage_before_marker() {
    let secs = sections(&lines(OUT_STATUS));
    let names: Vec<&str> = secs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["export", "status"]);
    let installed = parse_export(section(&secs, "export").unwrap()).unwrap();
    assert_eq!(installed, parse_export(EXPORT).unwrap());
    let ups = parse_status(section(&secs, "status").unwrap(), &installed).unwrap();
    assert_eq!(ups, parse_status(STATUS_MANY, &installed).unwrap());
    assert!(section(&secs, "search").is_err());
    assert_eq!(sections(&lines("Write-Host only\nno marker")).len(), 0);
}

#[test]
fn versions() {
    let v = |s: &str| parse_version(s).map(|v| v.to_string());
    assert_eq!(
        v("Current Scoop version:\nv0.5.2 - Released at 2024-07-26"),
        Some("0.5.2".into())
    );
    assert_eq!(v("0.3.1 (2022-11-15)"), Some("0.3.1".into()));
    assert_eq!(v("Current Scoop version:\n8f2e1c0 (HEAD -> master)"), None);
}

#[test]
fn action_plans() {
    let (ps, scoop) = (Path::new("pwsh.exe"), Path::new(r"C:\s\scoop.ps1"));
    let id: PkgId = "scoop:main/7zip".parse().unwrap();
    let plan = |op| action_plan(ps, scoop, op, &id).unwrap();
    let install = plan(PkgOp::Install);
    assert!(!install.elevate);
    assert_eq!(install.argv[0], "pwsh.exe");
    assert!(decoded(&install.argv).contains("& $scoop install 'main/7zip' *>&1"));
    assert!(decoded(&install.argv).contains("exit $LASTEXITCODE"));
    assert!(decoded(&plan(PkgOp::Upgrade).argv).contains("& $scoop update '7zip' *>&1"));
    assert!(decoded(&plan(PkgOp::Uninstall).argv).contains("& $scoop uninstall '7zip' *>&1"));
    for bad in [
        "scoop:7zip'; rm",
        "scoop:-g",
        "winget:Git.Git",
        "scoop:a/b/c",
    ] {
        let id: PkgId = bad.parse().unwrap();
        assert!(
            matches!(
                action_plan(ps, scoop, PkgOp::Install, &id),
                Err(PkgError::BadId(_))
            ),
            "{bad}"
        );
    }
}

/// Подставной `scoop.ps1`: `Write-Host` шумит, в конвейер попадают строки, данные —
/// объектами, как у настоящего scoop.
const FAKE_SCOOP: &str = r#"
$a = $args
Write-Host 'host noise <<URFIN>> fake'
switch ($a[0]) {
  'export' { Get-Content -Raw -Encoding UTF8 (Join-Path $PSScriptRoot 'export.json') }
  'status' {
    Write-Host 'Scoop is up to date.'
    'stray string'
    [pscustomobject]@{ Name = 'git'; 'Installed Version' = '2.47.0.2'; 'Latest Version' = '2.47.1'; 'Missing Dependencies' = @(); Info = @() }
  }
  'search' {
    if ($a[1] -eq 'one') {
      [pscustomobject]@{ Name = 'firefox'; Version = '132.0'; Source = 'extras'; Binaries = 'firefox.exe' }
    } elseif ($a[1] -eq 'none') {
      Write-Host 'WARN  No matches found.'
    } else {
      [pscustomobject]@{ Name = '7zip'; Version = '24.08'; Source = 'main'; Binaries = '' }
      [pscustomobject]@{ Name = 'тест-ü'; Version = '1.0'; Source = 'extras'; Binaries = '' }
    }
  }
  'update' { Write-Host 'Updating Scoop...'; 'Scoop was updated successfully!' }
  '--version' { Write-Host 'Current Scoop version:'; Write-Host 'v0.5.2 - Released at 2024-07-26' }
  'install' { Write-Host "Installing $($a[1])"; exit 0 }
  'uninstall' { Write-Host "Uninstalling $($a[1])"; exit 3 }
}
"#;

/// `pkg.info`: `scoop info` в скрипте PowerShell.
#[test]
fn info_argv_script() {
    let id: PkgId = "scoop:main/7zip".parse().unwrap();
    let a = info_argv(Path::new("powershell.exe"), Path::new("scoop.ps1"), &id).unwrap();
    assert_eq!(a[0], "powershell.exe");
    let script = base64::engine::general_purpose::STANDARD
        .decode(a.last().unwrap().to_str().unwrap())
        .unwrap();
    let utf16: Vec<u16> = script
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert!(String::from_utf16_lossy(&utf16).contains("& $scoop info 'main/7zip'"));
    let bad: PkgId = "scoop:7zip'; rm".parse().unwrap();
    assert!(info_argv(Path::new("powershell.exe"), Path::new("scoop.ps1"), &bad).is_err());
}

#[cfg(windows)]
mod on_powershell {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::api::{Detected, MgrCaps, PackageManager, RunCx};
    use crate::config::PkgConfig;
    use crate::detect::{SystemProbe, find_powershell, windows_powershell};
    use crate::proc::{ActionDone, run_action};

    fn shells() -> Vec<PathBuf> {
        let mut v = vec![windows_powershell(&SystemProbe)];
        let found = find_powershell(&SystemProbe);
        if !v.contains(&found) {
            v.push(found);
        }
        v.retain(|p| p.is_file());
        assert!(!v.is_empty(), "no PowerShell");
        v
    }

    fn adapter(dir: &Path, ps: &Path) -> ScoopAdapter {
        // BOM: Windows PowerShell 5.1 читает скрипт без BOM в ANSI.
        let script = format!("\u{feff}{FAKE_SCOOP}");
        std::fs::write(dir.join("scoop.ps1"), script).unwrap();
        std::fs::write(dir.join("export.json"), EXPORT).unwrap();
        let cached = Detected {
            mgr: Mgr::Scoop,
            exe: dir.join("scoop.ps1"),
            version: None,
            caps: MgrCaps::empty(),
        };
        ScoopAdapter::new(&PkgConfig::default(), Some(cached), Some(ps.to_owned()))
    }

    fn cx() -> RunCx {
        RunCx {
            timeout: Some(std::time::Duration::from_secs(60)),
            ..RunCx::default()
        }
    }

    /// Приёмка: работает с PowerShell 5.1 и pwsh 7 (какие есть на машине).
    #[test]
    fn fake_scoop_end_to_end() {
        for ps in shells() {
            let dir = tempfile::tempdir().unwrap();
            let a = adapter(dir.path(), &ps);
            let c = cx();
            let who = ps.display();
            assert_eq!(a.version(&c).unwrap(), "0.5.2", "{who}");
            assert_eq!(
                a.list_installed(&c).unwrap(),
                parse_export(EXPORT).unwrap(),
                "{who}"
            );
            let ups = a.list_updates(&c).unwrap();
            assert_eq!(
                show_updates(&ups),
                "scoop:main/git | 2.47.0.2 -> 2.47.1\n",
                "{who}"
            );
            let one = a.search("one", &c).unwrap();
            assert_eq!(show_hits(&one), "scoop:extras/firefox | 132.0 | Contains\n");
            assert_eq!(a.search("none", &c).unwrap(), [], "{who}");
            let many = a.search("7zip", &c).unwrap();
            let names: Vec<&str> = many.iter().map(|h| h.pkg.name.as_str()).collect();
            assert_eq!(names, ["7zip", "тест-ü"], "{who}");
            assert_eq!(a.search("  ", &c).unwrap(), []);

            let seen = Arc::new(Mutex::new(Vec::new()));
            let s = seen.clone();
            let mut c = cx();
            c.lines = Some(Arc::new(move |l: &str| {
                s.lock().unwrap().push(l.to_owned());
            }));
            let id: PkgId = "scoop:main/7zip".parse().unwrap();
            let plan = a.action(PkgOp::Install, &id, &c).unwrap();
            assert_eq!(run_action(&plan, &c), Ok(ActionDone::Ok), "{who}");
            assert!(
                seen.lock()
                    .unwrap()
                    .iter()
                    .any(|l| l == "Installing main/7zip"),
                "{who}: {:?}",
                seen.lock().unwrap()
            );
            let plan = a.action(PkgOp::Uninstall, &id, &c).unwrap();
            assert!(
                matches!(run_action(&plan, &c), Err(PkgError::Exit { code: 3, .. })),
                "{who}"
            );
        }
    }

    /// Стенд: настоящий scoop, только чтение (версия, export, status без `scoop
    /// update`, поиск по локальным бакетам). `cargo nextest run -p urfin-pkg
    /// --run-ignored only stand_scoop`.
    #[test]
    #[ignore = "стенд: нужен установленный scoop"]
    fn stand_scoop_read_only() {
        let Some(d) = crate::detect::detect(Mgr::Scoop, &SystemProbe) else {
            eprintln!("scoop not found");
            return;
        };
        for ps in shells() {
            let config = PkgConfig {
                scoop_update_buckets: false,
                ..PkgConfig::default()
            };
            let a = ScoopAdapter::new(&config, Some(d.clone()), Some(ps.clone()));
            let c = cx();
            eprintln!("{}: version {:?}", ps.display(), a.version(&c));
            let installed = a.list_installed(&c).unwrap();
            eprintln!("{}", show_packages(&installed));
            eprintln!("{}", show_updates(&a.list_updates(&c).unwrap()));
            eprintln!("{}", show_hits(&a.search("7zip", &c).unwrap()));
        }
    }
}
