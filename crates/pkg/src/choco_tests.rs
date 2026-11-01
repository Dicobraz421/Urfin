//! choco по фикстурам v1 и v2 (`fixtures/choco`), белый список, журнал и запуск
//! «с повышением» на подставном менеджере без UAC (приёмка F02-06).

use std::fmt::Write as _;

use super::*;
use crate::elevated::{Launch, Outcome, launch_script, outcome, win_arg};

const LIST_V2: &str = include_str!("../fixtures/choco/list.v2.txt");
const LIST_V1: &str = include_str!("../fixtures/choco/list.v1.txt");
const OUTDATED: &str = include_str!("../fixtures/choco/outdated.txt");
const SEARCH_V2: &str = include_str!("../fixtures/choco/search.v2.txt");
const SEARCH_V1: &str = include_str!("../fixtures/choco/search.v1.txt");
const LOG: &str = include_str!("../fixtures/choco/chocolatey.log");

const EXE: &str = r"C:\ProgramData\chocolatey\bin\choco.exe";

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn strs(argv: &[OsString]) -> Vec<&str> {
    argv[1..].iter().map(|a| a.to_str().unwrap()).collect()
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn argv_v1_and_v2() {
    let exe = Path::new(EXE);
    let (v1, v2) = (MgrCaps::CHOCO_V1, MgrCaps::empty());
    assert_eq!(
        strs(&list_argv(exe, v2)),
        ["list", "--limit-output", "--no-color"]
    );
    assert_eq!(
        strs(&list_argv(exe, v1)),
        ["list", "--local-only", "--limit-output"]
    );
    assert_eq!(
        strs(&outdated_argv(exe, v2)),
        ["outdated", "--limit-output", "--no-color"]
    );
    assert_eq!(
        strs(&search_argv(exe, "firefox", v2)),
        ["search", "firefox", "--limit-output", "--no-color"]
    );
    assert_eq!(
        strs(&search_argv(exe, "firefox", v1)),
        ["list", "firefox", "--limit-output"]
    );
    assert_eq!(search_query("  /y "), "y");
    assert_eq!(search_query("--force"), "force");
    assert_eq!(search_query("fire\nfox"), "firefox");
}

/// Приёмка: `list` v2 и v1 по фикстурам; посторонние строки отброшены.
#[test]
fn list_v1_v2() {
    let v2 = parse_list(&lines(LIST_V2));
    let mut s = String::new();
    for p in &v2 {
        writeln!(s, "{} | {}", p.id, p.version).unwrap();
    }
    insta::assert_snapshot!(s, @r"
    choco:chocolatey | 2.3.0
    choco:chocolatey-compatibility.extension | 1.0.0
    choco:chocolatey-core.extension | 1.4.0
    choco:7zip | 24.8.0
    choco:7zip.install | 24.8.0
    choco:git | 2.47.0
    choco:git.install | 2.47.0
    choco:nodejs-lts | 20.18.0
    ");
    let v1 = parse_list(&lines(LIST_V1));
    assert_eq!(v1.len(), v2.len());
    assert_eq!(v1[0].version, "1.4.0");
    assert_eq!(v1[1..], v2[1..]);
    assert!(v2.iter().all(|p| p.name == p.id.id && p.source.is_none()));
}

/// Приёмка: `outdated` — `id|current|available|pinned`.
#[test]
fn outdated() {
    let ups = parse_outdated(&lines(OUTDATED));
    let mut s = String::new();
    for u in &ups {
        let pin = if u.pinned { " (pinned)" } else { "" };
        writeln!(
            s,
            "{} | {} -> {}{pin}",
            u.pkg.id, u.pkg.version, u.available
        )
        .unwrap();
    }
    insta::assert_snapshot!(s, @r"
    choco:git | 2.47.0 -> 2.47.1
    choco:git.install | 2.47.0 -> 2.47.1
    choco:nodejs-lts | 20.18.0 -> 22.11.0 (pinned)
    choco:googlechrome | 130.0.6723.92 -> 131.0.6778.70
    ");
    assert_eq!(parse_outdated(&lines("git|2.47.0")), []);
}

/// Приёмка: `search` v2 и v1 (`list <q>`) дают одно и то же.
#[test]
fn search_v1_v2() {
    let v2 = parse_search("firefox", &lines(SEARCH_V2));
    let got: Vec<(&str, MatchKind)> = v2
        .iter()
        .map(|h| (h.pkg.id.id.as_str(), h.matched))
        .collect();
    assert_eq!(
        got,
        [
            ("firefox", MatchKind::Exact),
            ("firefox-nightly", MatchKind::Prefix),
            ("firefox-dev", MatchKind::Prefix),
            ("tor-browser", MatchKind::Contains),
        ]
    );
    assert_eq!(parse_search("firefox", &lines(SEARCH_V1)), v2);
}

#[test]
fn action_plans() {
    let exe = Path::new(EXE);
    let id: PkgId = "choco:git.install".parse().unwrap();
    let p = action_plan(exe, PkgOp::Upgrade, &id).unwrap();
    assert!(p.elevate);
    assert_eq!(p.argv[0], EXE);
    assert_eq!(
        strs(&p.argv),
        ["upgrade", "git.install", "-y", "--no-progress"]
    );
    assert_eq!(p.success_codes, [0]);
    assert_eq!(p.reboot_codes, [1641, 3010]);
    assert!(whitelisted(&p.argv, Some(exe)));
    for op in [PkgOp::Install, PkgOp::Uninstall] {
        assert!(whitelisted(
            &action_plan(exe, op, &id).unwrap().argv,
            Some(exe)
        ));
    }
    for bad in ["choco:-y", "choco:git&calc", "winget:Git.Git"] {
        let id: PkgId = bad.parse().unwrap();
        assert!(
            matches!(
                action_plan(exe, PkgOp::Install, &id),
                Err(PkgError::BadId(_))
            ),
            "{bad}"
        );
    }
}

/// Приёмка: argv вне белого списка отвергается.
#[test]
fn whitelist_rejects() {
    let exe = Path::new(EXE);
    let ok = os(&[EXE, "upgrade", "git", "-y", "--no-progress", "--noop"]);
    assert!(whitelisted(&ok, Some(exe)));
    assert!(whitelisted(&ok, None));
    let bad: [&[&str]; 11] = [
        &[],
        &[EXE],
        &[EXE, "upgrade"],
        &[EXE, "source", "add", "-y"],
        &[EXE, "upgrade", "git", "--source", "http://evil"],
        &[EXE, "upgrade", "git", "-s=http://evil"],
        &[EXE, "upgrade", "git", "--params", "/x"],
        &[EXE, "upgrade", "git", "extra"],
        &[EXE, "upgrade", "-y", "-y"],
        &[r"C:\Windows\System32\cmd.exe", "upgrade", "git", "-y"],
        &["choco.exe", "upgrade", "git", "-y"],
    ];
    for argv in bad {
        assert!(!whitelisted(&os(argv), Some(exe)), "{argv:?}");
    }
    let other = os(&[r"D:\tools\choco.exe", "upgrade", "git"]);
    assert!(!whitelisted(&other, Some(exe)));
}

/// Приёмка: вывод choco — из журнала построчно, без `DEBUG` и его продолжений.
#[test]
fn log_lines_without_debug() {
    let mut f = LogFilter::default();
    let shown: Vec<&str> = LOG.lines().filter_map(|l| f.line(l)).collect();
    insta::assert_snapshot!(shown.join("\n"), @r#"
    ============================================================
    Chocolatey v2.3.0
    Upgrading the following packages:
    git
    By upgrading, you accept licenses for the packages.
    You have git.install v2.47.0 installed. Version 2.47.1 is available based on your source(s).
    Обновление git.install: готово
    ERROR: Running ["C:\Program Files\Git\unins000.exe"] was not successful.
     Exit code was '1'.
    Chocolatey upgraded 1/1 packages.
    "#);
    assert_eq!(
        log_record("2026-11-01 12:00:00,015 4242 [WARN ] - x [y] - z"),
        Some((LogLevel::Warn, "x [y] - z"))
    );
    assert_eq!(log_record("Configuration: [x] - y"), None);
    assert_eq!(
        log_path(Path::new(EXE)),
        Some(PathBuf::from(
            r"C:\ProgramData\chocolatey\logs\chocolatey.log"
        ))
    );
    assert_eq!(
        real_exe(Path::new(EXE)),
        Some(PathBuf::from(r"C:\ProgramData\chocolatey\choco.exe"))
    );
    assert_eq!(real_exe(Path::new(r"C:\tools\choco.exe")), None);
}

#[test]
fn notice_strings_en_ru() {
    for lang in ["en", "ru"] {
        let cat = urfin_i18n::Catalog::embedded()
            .build(&lang.parse().unwrap())
            .0;
        for key in [UAC_NOTICE, UAC_DECLINED, CANCEL_ELEVATED] {
            assert!(cat.has(key), "{lang}: {key}");
        }
    }
}

#[test]
fn windows_args() {
    assert_eq!(win_arg("git").unwrap(), "git");
    assert_eq!(win_arg(r"C:\a b\x.log").unwrap(), r#""C:\a b\x.log""#);
    assert_eq!(win_arg(r"C:\a b\").unwrap(), r#""C:\a b\\""#);
    assert!(win_arg("a\"b").is_err());
    assert!(win_arg("a\nb").is_err());
    assert!(win_arg("").is_err());
}

#[test]
fn launch_script_runas() {
    let argv = os(&[EXE, "upgrade", "git", "-y"]);
    let s = launch_script(&argv, Launch::RunAs).unwrap();
    assert!(
        s.contains(r"$si.FileName = 'C:\ProgramData\chocolatey\bin\choco.exe'"),
        "{s}"
    );
    assert!(s.contains("$si.Arguments = 'upgrade git -y'"));
    assert!(s.contains("$si.UseShellExecute = $true"));
    assert!(s.contains("$si.Verb = 'runas'"));
    assert!(s.contains("-eq 1223"));
    assert!(!s.contains("Hidden"));
}

#[test]
fn outcomes() {
    let o = |t: &str| outcome(&lines(t));
    assert_eq!(o("<<URFIN>> declined"), Outcome::Declined);
    assert_eq!(
        o("<<URFIN>> started 42\n<<URFIN>> exit 0"),
        Outcome::Exit(0)
    );
    assert_eq!(o("<<URFIN>> exit -1978335212"), Outcome::Exit(0x8A15_0014));
    assert_eq!(
        o("<<URFIN>> failed The system cannot find the file"),
        Outcome::Failed("The system cannot find the file".into())
    );
    assert_eq!(o("garbage\n<<URFIN>> started 42"), Outcome::Unknown);
    assert_eq!(o(""), Outcome::Unknown);
}

#[cfg(windows)]
mod on_windows {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::api::{Detected, Elevation, PackageManager, RunCx};
    use crate::elevated::run_with_launch;
    use crate::proc::ActionDone;
    use crate::testutil::fake;

    fn sink() -> (RunCx, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let store = seen.clone();
        let cx = RunCx {
            lines: Some(Arc::new(move |l: &str| {
                store.lock().unwrap().push(l.to_owned());
            })),
            ..RunCx::default()
        };
        (cx, seen)
    }

    fn plan(extra: &[&str]) -> ActionPlan {
        let mut argv = vec![fake().into_os_string()];
        argv.extend(extra.iter().map(OsString::from));
        ActionPlan {
            argv,
            elevate: true,
            success_codes: vec![0],
            reboot_codes: REBOOT.to_vec(),
        }
    }

    /// Приёмка: строки журнала — в ленту по мере записи, код выхода — итог. Путь с
    /// пробелом проходит в кавычках.
    #[test]
    fn log_to_sink_and_exit_codes() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("with space").join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log = log_dir.join("chocolatey.log");
        std::fs::write(&log, "2026-11-01 11:00:00,000 1 [INFO ] - old run\r\n").unwrap();
        let log_arg = log.to_str().unwrap();

        let (cx, seen) = sink();
        let p = plan(&[
            "--log",
            log_arg,
            "--line",
            "Upgrading",
            "--line",
            "done",
            "--exit",
            "3010",
        ]);
        assert_eq!(
            run_with_launch(&p, Some(&log), &cx, Launch::Hidden),
            Ok(ActionDone::Reboot)
        );
        assert_eq!(*seen.lock().unwrap(), ["Upgrading", "done"]);

        let (cx, seen) = sink();
        let p = plan(&["--log", log_arg, "--line", "failing", "--exit", "5"]);
        assert_eq!(
            run_with_launch(&p, Some(&log), &cx, Launch::Hidden),
            Err(PkgError::Exit {
                code: 5,
                tail: "failing".into()
            })
        );
        assert_eq!(*seen.lock().unwrap(), ["failing"]);

        let (cx, _) = sink();
        assert_eq!(
            run_with_launch(&plan(&[]), None, &cx, Launch::Hidden),
            Ok(ActionDone::Ok)
        );
    }

    #[test]
    fn missing_program_fails() {
        let p = ActionPlan {
            argv: os(&[r"C:\nonexistent\urfin-choco-missing.exe", "upgrade", "git"]),
            elevate: true,
            success_codes: vec![0],
            reboot_codes: Vec::new(),
        };
        let r = run_with_launch(&p, None, &RunCx::default(), Launch::Hidden);
        assert!(matches!(r, Err(PkgError::Spawn(_))), "{r:?}");
    }

    /// Отмена убивает обёртку быстро (повышенный процесс жил бы дальше).
    #[test]
    fn cancel_wrapper() {
        let (cx, _) = sink();
        let token = cx.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            token.cancel();
        });
        let t = Instant::now();
        let r = run_with_launch(&plan(&["--sleep-ms", "60000"]), None, &cx, Launch::Hidden);
        assert_eq!(r, Err(PkgError::Cancelled));
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    }

    /// Приёмка: argv вне белого списка — отказ без запуска чего-либо.
    #[test]
    fn adapter_rejects_outside_whitelist() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("bin").join("choco.exe");
        let a = ChocoAdapter::new(Some(Detected {
            mgr: Mgr::Choco,
            exe: exe.clone(),
            version: Some("2.3.0".into()),
            caps: MgrCaps::empty(),
        }));
        let id: PkgId = "choco:git".parse().unwrap();
        let mut p = a.action(PkgOp::Upgrade, &id, &RunCx::default()).unwrap();
        assert_eq!(p.argv[0], exe.as_os_str());
        p.argv.extend(os(&["--source", "http://evil"]));
        let t = Instant::now();
        assert!(matches!(
            a.run_action(&p, &RunCx::default()),
            Err(PkgError::Spawn(_))
        ));
        assert!(t.elapsed() < Duration::from_millis(100));
        p.argv[0] = fake().into_os_string();
        p.argv.truncate(5);
        assert!(matches!(
            a.run_action(&p, &RunCx::default()),
            Err(PkgError::Spawn(_))
        ));
    }

    /// Ручная проверка UAC (`docs/manual-tests/F02-06.md`): настоящий choco,
    /// `upgrade chocolatey --noop` (пробный прогон, система не меняется). Запуск:
    /// `cargo nextest run -p urfin-pkg --run-ignored only stand_choco_uac --no-capture`.
    #[test]
    #[ignore = "ручная проверка: окно UAC"]
    fn stand_choco_uac() {
        let a = ChocoAdapter::new(None);
        let Some(d) = a.detect() else {
            eprintln!("choco not found");
            return;
        };
        let cx = RunCx {
            lines: Some(Arc::new(|l: &str| eprintln!("  | {l}"))),
            ..RunCx::default()
        };
        eprintln!("version: {:?}", a.version(&cx));
        let id: PkgId = "choco:chocolatey".parse().unwrap();
        let mut p = a.action(PkgOp::Upgrade, &id, &cx).unwrap();
        p.argv.push("--noop".into());
        eprintln!("{}: {:?}", d.exe.display(), p.argv);
        let r = a.run_action(&p, &cx);
        eprintln!("result: {r:?}");
        if r == Err(PkgError::Elevation(Elevation::Declined)) {
            eprintln!("UAC declined → job Cancelled «отклонено»");
        }
    }
}
