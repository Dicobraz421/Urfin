//! `screens.pkg` (F02-08, только `--screens`): вкладка пакетов на фикстуре — программы,
//! версии и издатели как на макетах `screen-pkginstalled` и `screen-pkgupdates`, без
//! запуска менеджеров. Менеджеры — winget и scoop; «важные» — Firefox и Node.js LTS.
//! `stop=true` — закрыть вкладку пакетов (активную) и задачи сцены установки.
//! `step=search` — поиск «7zip» на фикстуре (`screen-pkgsearch`), `step=install` —
//! установка и обновление в job-баре (`screen-pkginstall`).

use compact_str::CompactString;
use urfin_core::pkg::{Busy, Hit, MatchKind, Mgr, Package, PkgId, PkgModel, PkgOp, Update};
use urfin_core::{Command, Cx};

/// Программа: менеджер:id, имя, версия, доступная, издатель (у scoop — бакет).
type Item = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
);

const ITEMS: [Item; 16] = [
    (
        "winget:7zip.7zip",
        "7-Zip",
        "25.01",
        Some("26.00"),
        Some("Igor Pavlov"),
    ),
    (
        "winget:voidtools.Everything",
        "Everything",
        "1.4.1.1028",
        None,
        Some("voidtools"),
    ),
    ("scoop:main/fd", "fd", "10.3.0", None, None),
    (
        "winget:Mozilla.Firefox",
        "Firefox",
        "143.0.4",
        Some("144.0"),
        Some("Mozilla"),
    ),
    (
        "winget:Git.Git",
        "Git",
        "2.51.0",
        Some("2.51.1"),
        Some("Git Development Community"),
    ),
    ("scoop:main/jq", "jq", "1.8.1", None, None),
    (
        "winget:OpenJS.NodeJS.LTS",
        "Node.js LTS",
        "22.20.0",
        Some("22.21.0"),
        Some("OpenJS Foundation"),
    ),
    (
        "winget:Notepad++.Notepad++",
        "Notepad++",
        "8.8.5",
        Some("8.8.6"),
        Some("Notepad++ Team"),
    ),
    (
        "winget:Microsoft.PowerShell",
        "PowerShell 7",
        "7.5.3",
        Some("7.5.4"),
        Some("Microsoft"),
    ),
    (
        "winget:Python.Python.3.12",
        "Python 3.12",
        "3.12.10",
        None,
        Some("Python Software Foundation"),
    ),
    ("scoop:main/ripgrep", "ripgrep", "14.1.1", None, None),
    (
        "winget:Rustlang.Rustup",
        "Rust (rustup)",
        "1.28.2",
        None,
        Some("Rust Foundation"),
    ),
    (
        r"winget:ARP\Machine\X64\Totalcmd64",
        "Total Commander",
        "11.55",
        None,
        Some("Ghisler Software"),
    ),
    (
        "winget:Microsoft.VisualStudioCode",
        "Visual Studio Code",
        "1.104.3",
        Some("1.105.1"),
        Some("Microsoft"),
    ),
    (
        "winget:VideoLAN.VLC",
        "VLC",
        "3.0.21",
        None,
        Some("VideoLAN"),
    ),
    (
        "winget:Microsoft.WindowsTerminal",
        "Windows Terminal",
        "1.23.12811.0",
        None,
        Some("Microsoft"),
    ),
];

/// Поиск «7zip» (`screen-pkgsearch`): менеджер:id, имя, версия, источник, издатель.
const HITS: [(&str, &str, &str, &str, Option<&str>); 9] = [
    (
        "winget:7zip.7zip",
        "7-Zip",
        "24.08",
        "winget",
        Some("Igor Pavlov"),
    ),
    ("scoop:main/7zip", "7zip", "24.08", "main", None),
    (
        "winget:M2Team.NanaZip",
        "NanaZip",
        "5.0.1263.0",
        "winget",
        Some("M2Team"),
    ),
    (
        "winget:9N8G7TSCL18R",
        "NanaZip",
        "",
        "msstore",
        Some("M2Team"),
    ),
    (
        "scoop:extras/nanazip",
        "nanazip",
        "5.0.1263.0",
        "extras",
        None,
    ),
    (
        "winget:Giorgio.PeaZip",
        "PeaZip",
        "10.6.0",
        "winget",
        Some("Giorgio Tani"),
    ),
    ("scoop:extras/peazip", "peazip", "10.6.1", "extras", None),
    (
        "winget:mcmilk.7zip-zstd",
        "7-Zip-zstd",
        "24.08",
        "winget",
        Some("Tino Reichardt"),
    ),
    (
        "scoop:extras/7zip-zstd",
        "7zip-zstd",
        "24.08",
        "extras",
        None,
    ),
];

/// Задачи сцены установки: снимаются `stop=true`.
static JOBS: std::sync::Mutex<Vec<urfin_core::JobId>> = std::sync::Mutex::new(Vec::new());

/// Поиск «7zip» на фикстуре: ответы winget и scoop, строка поиска открыта.
pub(crate) fn search(cx: &mut Cx<'_>) {
    let Some(st) = cx.state.ext.get::<PkgModel>() else {
        return;
    };
    let book = std::sync::Arc::clone(&st.fixture);
    let q = "7zip";
    let g = book.search(q, &[Mgr::Winget, Mgr::Scoop]);
    for mgr in [Mgr::Winget, Mgr::Scoop] {
        let hits: Vec<Hit> = HITS
            .iter()
            .filter_map(|(id, name, version, source, publisher)| {
                let id = id.parse::<PkgId>().ok().filter(|i| i.mgr == mgr)?;
                Some(Hit {
                    matched: MatchKind::of(q, &id.id, name),
                    pkg: Package {
                        id,
                        name: (*name).into(),
                        version: (*version).into(),
                        source: Some((*source).into()),
                        publisher: publisher.map(CompactString::from),
                    },
                })
            })
            .collect();
        book.answer(g, mgr, hits);
    }
    cx.dispatch(Command::new("pkg.filter").arg("name", "search"));
}

/// Установка 7-Zip (41 %, 0,6 из 1,5 МБ) и обновление Git, ждущее UAC
/// (`screen-pkginstall`): строка группы и job-бар. Строка поиска закрыта.
pub(crate) fn install(cx: &mut Cx<'_>) {
    use std::sync::atomic::Ordering;
    let Some(st) = cx.state.ext.get::<PkgModel>() else {
        return;
    };
    let book = std::sync::Arc::clone(&st.fixture);
    cx.dispatch(Command::new("ui.cancel"));
    if let Ok(id) = "winget:7zip.7zip".parse::<PkgId>() {
        book.busy(
            &id,
            Some(Busy {
                op: PkgOp::Install,
                queued: false,
                pct: Some(41),
                uac: false,
            }),
        );
    }
    let uac = urfin_i18n::current().get("pkg-busy-uac");
    for (kind, title, done, total, current) in [
        (
            "pkg_install",
            "winget install 7zip.7zip",
            644_245_u64,
            1_572_864_u64,
            None,
        ),
        (
            "pkg_upgrade",
            "winget upgrade Git.Git",
            0,
            0,
            Some(uac.clone()),
        ),
    ] {
        let job = cx.state.jobs.start(
            urfin_core::JobSpec {
                kind,
                title: title.into(),
                lane: urfin_core::Lane::Bulk,
                priority: 0,
                owner_ns: "screens",
            },
            cx.rt.sink(),
        );
        let p = job.progress();
        p.bytes_done.store(done, Ordering::Relaxed);
        p.bytes_total.store(total, Ordering::Relaxed);
        p.items_total.store(1, Ordering::Relaxed);
        p.total_final.store(true, Ordering::Relaxed);
        if let Some(c) = &current {
            p.set_current(c);
        }
        JOBS.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(job.id());
        let _ = cx
            .rt
            .spawn_dedicated("screens.pkg.job".into(), cx.rt.token(), move |tcx| {
                job.set_state(urfin_core::JobState::Running);
                while !tcx.cancel.is_cancelled() && job.checkpoint().is_ok() {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                job.finish(Err(String::new()));
            });
    }
    cx.dispatch(Command::new("panel.refresh"));
}

pub(crate) fn run(stop: bool, cx: &mut Cx<'_>) {
    if stop {
        let jobs = std::mem::take(
            &mut *JOBS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for id in jobs {
            cx.state.jobs.cancel(id);
        }
        if let Some(st) = cx.state.ext.get::<PkgModel>()
            && let Ok(id) = "winget:7zip.7zip".parse::<PkgId>()
        {
            st.fixture.busy(&id, None);
        }
        if crate::pkg_view::is_pkg(cx.state.active_tab()) {
            cx.dispatch(Command::new("sel.none"));
            cx.dispatch(Command::new("tab.close"));
        }
        return;
    }
    let Some(st) = cx.state.ext.get::<PkgModel>() else {
        return;
    };
    let book = std::sync::Arc::clone(&st.fixture);
    book.important(
        ["winget:Mozilla.Firefox", "winget:OpenJS.NodeJS.LTS"]
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect(),
    );
    book.managers(&[Mgr::Winget, Mgr::Scoop]);
    for mgr in [Mgr::Winget, Mgr::Scoop] {
        let mut installed = Vec::new();
        let mut updates = Vec::new();
        for (id, name, version, available, publisher) in ITEMS {
            let Ok(id) = id.parse::<PkgId>() else {
                continue;
            };
            if id.mgr != mgr {
                continue;
            }
            let local = id.is_local();
            let pkg = Package {
                source: match mgr {
                    Mgr::Scoop => Some("main".into()),
                    _ if local => None,
                    _ => Some("winget".into()),
                },
                id,
                name: name.into(),
                version: version.into(),
                publisher: publisher.map(CompactString::from),
            };
            if let Some(a) = available {
                updates.push(Update {
                    pkg: pkg.clone(),
                    available: a.into(),
                    pinned: false,
                });
            }
            installed.push(pkg);
        }
        book.installed(mgr, installed);
        book.updates(mgr, updates);
    }
    book.checked_now();
    cx.dispatch(Command::new("pkg.open").arg("filter", "installed"));
}

/// Баннер проверки по расписанию (`screen-pkgbanner`): «Доступно 7 обновлений · 2
/// важных — проверено по расписанию сегодня в 09:00».
pub(crate) fn banner(cx: &mut Cx<'_>) {
    let now = crate::pkg_view::now_ns();
    let at = crate::sys::Zone::system().local(now).map_or(now, |t| {
        let since = i64::from(t.hour) * 3600 + i64::from(t.minute) * 60 + i64::from(t.second);
        now - (since - 9 * 3600) * 1_000_000_000
    });
    if let Some(m) = cx.state.ext.get_mut::<PkgModel>() {
        m.notice = Some(urfin_core::pkg::Notice {
            updates: 7,
            important: 2,
            checked_at: at,
            scheduled: true,
        });
    }
    cx.state.touch();
}
