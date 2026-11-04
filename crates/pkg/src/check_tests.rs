//! `check.json` и проверка `--pkg-check` на подставных менеджерах: дубли сводятся как
//! во вкладке, «важные», замок (второй запуск выходит сразу), «Позже».

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::api::{Detected, Hit, MgrCaps, Package, PackageManager, PkgError, PkgOp};
use crate::arp::ArpEntry;
use crate::{ActionPlan, PkgCache};

fn pkg(id: &str, name: &str, version: &str) -> Package {
    let id: PkgId = id.parse().unwrap();
    Package {
        source: (id.mgr != Mgr::Choco).then(|| CompactString::from(id.mgr.as_str())),
        id,
        name: name.into(),
        version: version.into(),
        publisher: None,
    }
}

fn up(p: Package, available: &str) -> Update {
    Update {
        pkg: p,
        available: available.into(),
        pinned: false,
    }
}

fn report() -> CheckReport {
    let firefox = PkgId {
        mgr: Mgr::Winget,
        id: "Mozilla.Firefox".into(),
    };
    CheckReport {
        checked_at: "2026-10-29T09:12:00Z".parse().unwrap(),
        managers: vec![
            MgrStatus {
                mgr: Mgr::Winget,
                ok: true,
                error: None,
            },
            MgrStatus {
                mgr: Mgr::Choco,
                ok: false,
                error: Some("timed out".into()),
            },
        ],
        updates: vec![
            Update {
                pkg: Package {
                    id: firefox.clone(),
                    name: "Mozilla Firefox".into(),
                    version: "130.0".into(),
                    source: Some("winget".into()),
                    publisher: Some("Mozilla".into()),
                },
                available: "131.0.2".into(),
                pinned: false,
            },
            Update {
                pkg: Package {
                    id: "scoop:main/7zip".parse().unwrap(),
                    name: "7zip".into(),
                    version: "23.01".into(),
                    source: None,
                    publisher: None,
                },
                available: "24.08".into(),
                pinned: true,
            },
        ],
        important: vec![firefox],
        dismissed_at: None,
        manual: false,
    }
}

#[test]
fn roundtrip() {
    let r = report();
    let json = serde_json::to_string_pretty(&r).unwrap();
    let back: CheckReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

/// Формат на диске — снапшот: его читает следующий запуск, возможно другой версии.
#[test]
fn json_snapshot() {
    insta::assert_snapshot!(serde_json::to_string_pretty(&report()).unwrap());
}

/// Баннер: есть обновления и он не закрыт; «Проверить сейчас» — не «по расписанию».
#[test]
fn notice_of_report() {
    let mut r = report();
    let n = r.notice().unwrap();
    assert_eq!((n.updates, n.important, n.scheduled), (2, 1, true));
    assert_eq!(n.checked_at, nanos(r.checked_at));
    r.manual = true;
    assert!(!r.notice().unwrap().scheduled);
    r.dismissed_at = Some(r.checked_at);
    assert_eq!(r.notice(), None);
    r.dismissed_at = None;
    r.updates.clear();
    assert_eq!(r.notice(), None);
}

/// Подставной менеджер: ответ после паузы.
struct Fake {
    mgr: Mgr,
    installed: Result<Vec<Package>, PkgError>,
    updates: Vec<Update>,
    delay: Duration,
}

impl PackageManager for Fake {
    fn mgr(&self) -> Mgr {
        self.mgr
    }
    fn detect(&self) -> Option<Detected> {
        Some(found(self.mgr))
    }
    fn version(&self, _cx: &RunCx) -> Result<CompactString, PkgError> {
        Ok("1.0".into())
    }
    fn list_installed(&self, _cx: &RunCx) -> Result<Vec<Package>, PkgError> {
        std::thread::sleep(self.delay);
        self.installed.clone()
    }
    fn list_updates(&self, _cx: &RunCx) -> Result<Vec<Update>, PkgError> {
        Ok(self.updates.clone())
    }
    fn search(&self, _q: &str, _cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
        Ok(Vec::new())
    }
    fn action(&self, _op: PkgOp, _id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
        Err(PkgError::NotFound)
    }
}

fn found(mgr: Mgr) -> Detected {
    Detected {
        mgr,
        exe: format!("{mgr}.exe").into(),
        version: Some("1.0".into()),
        caps: MgrCaps::empty(),
    }
}

type Answer = (Mgr, Result<Vec<Package>, PkgError>, Vec<Update>);

struct Source(Vec<Answer>, Duration);

impl PkgSource for Source {
    fn detect(&self, _cached: &Managers) -> Vec<Detected> {
        self.0.iter().map(|(m, _, _)| found(*m)).collect()
    }
    fn arp(&self) -> Vec<ArpEntry> {
        Vec::new()
    }
    fn adapter(&self, d: &Detected, _cfg: &PkgConfig) -> Arc<dyn PackageManager> {
        let (mgr, installed, updates) = self.0.iter().find(|(m, _, _)| *m == d.mgr).unwrap();
        Arc::new(Fake {
            mgr: *mgr,
            installed: installed.clone(),
            updates: updates.clone(),
            delay: self.1,
        })
    }
}

/// winget и scoop: у 7-Zip две строки обновления (winget видит программу scoop) —
/// остаётся scoop; Firefox — «важный».
fn two_managers() -> Source {
    let ff = pkg("winget:Mozilla.Firefox", "Firefox", "143.0.4");
    let zip_w = pkg("winget:7zip.7zip", "7-Zip", "24.08");
    let zip_s = pkg("scoop:main/7zip", "7zip", "24.08");
    Source(
        vec![
            (
                Mgr::Winget,
                Ok(vec![ff.clone(), zip_w.clone()]),
                vec![up(ff, "144.0"), up(zip_w, "25.01")],
            ),
            (
                Mgr::Scoop,
                Ok(vec![zip_s.clone()]),
                vec![up(zip_s, "25.01")],
            ),
        ],
        Duration::from_millis(20),
    )
}

fn cfg() -> PkgConfig {
    PkgConfig {
        important: vec!["winget:Mozilla.Firefox".parse().unwrap()],
        ..PkgConfig::default()
    }
}

/// Проверка пишет кэш вкладки, `managers.toml` и итог; дубль winget скрыт.
#[test]
fn run_writes_report_and_cache() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(
        dir.path(),
        &cfg(),
        &two_managers(),
        &CancelToken::new(),
        false,
    )
    .unwrap();
    let ids: Vec<String> = r.updates.iter().map(|u| u.pkg.id.to_string()).collect();
    assert_eq!(ids, ["winget:Mozilla.Firefox", "scoop:main/7zip"]);
    assert_eq!(r.important, [id("winget:Mozilla.Firefox")]);
    assert!(r.managers.iter().all(|m| m.ok));
    assert_eq!(
        CheckReport::load(&dir.path().join(CHECK_FILE)),
        Some(r.clone())
    );
    let cache = PkgCache::load(&dir.path().join(CACHE_FILE)).unwrap();
    assert_eq!(cache.checked_at, Some(r.checked_at));
    assert_eq!(cache.installed.len(), 2);
    assert!(dir.path().join(MANAGERS_FILE).is_file());
    assert!(!dir.path().join("check.json.tmp").exists());
}

fn id(s: &str) -> PkgId {
    s.parse().unwrap()
}

/// Один менеджер не ответил — итог есть, его ошибка — в `managers`; не ответил ни
/// один — прежний итог остаётся.
#[test]
fn failures() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = two_managers();
    src.0[1].1 = Err(PkgError::Timeout);
    let r = run(dir.path(), &cfg(), &src, &CancelToken::new(), false).unwrap();
    let scoop = r.managers.iter().find(|m| m.mgr == Mgr::Scoop).unwrap();
    assert!(!scoop.ok);
    assert_eq!(scoop.error.as_deref(), Some("timed out"));
    src.0[0].1 = Err(PkgError::NotFound);
    let e = run(dir.path(), &cfg(), &src, &CancelToken::new(), false).unwrap_err();
    assert!(matches!(e, CheckError::NoAnswer(_)), "{e}");
    assert_eq!(CheckReport::load(&dir.path().join(CHECK_FILE)), Some(r));
    let none = Source(Vec::new(), Duration::ZERO);
    let e = run(dir.path(), &cfg(), &none, &CancelToken::new(), false).unwrap_err();
    assert!(matches!(e, CheckError::NoAnswer(_)), "{e}");
}

/// Двойной запуск: пока замок занят, вторая проверка выходит сразу, менеджеров не
/// спрашивая; замок отпускается с первой.
#[test]
fn second_run_exits_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let held = CheckLock::try_take(dir.path()).unwrap().unwrap();
    assert!(CheckLock::try_take(dir.path()).unwrap().is_none());
    let slow = Source(two_managers().0, Duration::from_secs(5));
    let t0 = Instant::now();
    let e = run(dir.path(), &cfg(), &slow, &CancelToken::new(), false).unwrap_err();
    assert!(matches!(e, CheckError::Busy));
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert!(!dir.path().join(CHECK_FILE).exists());
    drop(held);
    assert!(CheckLock::try_take(dir.path()).unwrap().is_some());
}

/// «Позже»: `dismissed_at` у той же проверки; другая проверка и занятый замок — без
/// записи.
#[test]
fn dismiss_marks_report() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CHECK_FILE);
    let r = report();
    r.save(&path).unwrap();
    dismiss(dir.path(), nanos(r.checked_at) + 1).unwrap();
    assert_eq!(CheckReport::load(&path).unwrap().dismissed_at, None);
    {
        let _held = CheckLock::try_take(dir.path()).unwrap().unwrap();
        dismiss(dir.path(), nanos(r.checked_at)).unwrap();
        assert_eq!(CheckReport::load(&path).unwrap().dismissed_at, None);
    }
    dismiss(dir.path(), nanos(r.checked_at)).unwrap();
    let back = CheckReport::load(&path).unwrap();
    assert!(back.dismissed_at.is_some());
    assert_eq!(back.notice(), None);
    // Нет файла — не ошибка.
    dismiss(&dir.path().join("none"), 0).unwrap();
}

/// Настоящие процессы: choco — подставной `fake-pkgmgr` с записанными `choco list` и
/// `choco outdated`.
#[cfg(windows)]
#[test]
fn fake_pkgmgr_check() {
    struct Proc;
    fn fixture(name: &str) -> std::ffi::OsString {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/choco")
            .join(name)
            .into_os_string()
    }
    fn query(name: &str, cx: &RunCx) -> Result<Vec<String>, PkgError> {
        let argv = vec![
            crate::testutil::fake().into_os_string(),
            "--fixture".into(),
            fixture(name),
        ];
        Ok(crate::proc::run_query(&argv, cx)?.ok()?.lines)
    }
    impl PackageManager for Proc {
        fn mgr(&self) -> Mgr {
            Mgr::Choco
        }
        fn detect(&self) -> Option<Detected> {
            None
        }
        fn version(&self, _cx: &RunCx) -> Result<CompactString, PkgError> {
            Ok("2.3.0".into())
        }
        fn list_installed(&self, cx: &RunCx) -> Result<Vec<Package>, PkgError> {
            Ok(crate::choco::parse_list(&query("list.v2.txt", cx)?))
        }
        fn list_updates(&self, cx: &RunCx) -> Result<Vec<Update>, PkgError> {
            Ok(crate::choco::parse_outdated(&query("outdated.txt", cx)?))
        }
        fn search(&self, _q: &str, _cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
            Ok(Vec::new())
        }
        fn action(&self, _op: PkgOp, _id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
            Err(PkgError::NotFound)
        }
    }
    struct ProcSource;
    impl PkgSource for ProcSource {
        fn detect(&self, _cached: &Managers) -> Vec<Detected> {
            vec![found(Mgr::Choco)]
        }
        fn arp(&self) -> Vec<ArpEntry> {
            Vec::new()
        }
        fn adapter(&self, _d: &Detected, _cfg: &PkgConfig) -> Arc<dyn PackageManager> {
            Arc::new(Proc)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = PkgConfig {
        important: vec!["choco:git".parse().unwrap()],
        ..PkgConfig::default()
    };
    let r = run(dir.path(), &cfg, &ProcSource, &CancelToken::new(), true).unwrap();
    let want = crate::choco::parse_outdated(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/choco/outdated.txt"),
        )
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    );
    assert_eq!(r.updates.len(), want.len());
    assert_eq!(r.important, [id("choco:git")]);
    assert!(r.manual);
    assert!(!r.notice().unwrap().scheduled);
}
