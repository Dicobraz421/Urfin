//! `screens.pkg` (F02-08, только `--screens`): вкладка пакетов на фикстуре — программы,
//! версии и издатели как на макетах `screen-pkginstalled` и `screen-pkgupdates`, без
//! запуска менеджеров. Менеджеры — winget и scoop; «важные» — Firefox и Node.js LTS.
//! `stop=true` — закрыть вкладку пакетов (активную).

use compact_str::CompactString;
use urfin_core::{Command, Cx};
use urfin_pkg::{Detected, Mgr, MgrCaps, Package, PkgConfig, PkgId, Update};

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

pub(crate) fn run(stop: bool, cx: &mut Cx<'_>) {
    if stop {
        if crate::pkg_view::is_pkg(cx.state.active_tab()) {
            cx.dispatch(Command::new("sel.none"));
            cx.dispatch(Command::new("tab.close"));
        }
        return;
    }
    let Some(st) = cx.state.ext.get::<urfin_pkg::PkgState>() else {
        return;
    };
    let book = std::sync::Arc::clone(&st.book);
    let cfg = PkgConfig {
        important: ["winget:Mozilla.Firefox", "winget:OpenJS.NodeJS.LTS"]
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect(),
        ..st.config.clone()
    };
    book.configure(&cfg);
    let found = |mgr| Detected {
        mgr,
        exe: format!("{mgr}.exe").into(),
        version: None,
        caps: MgrCaps::empty(),
    };
    book.found(vec![found(Mgr::Winget), found(Mgr::Scoop)], Vec::new());
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
            let local = urfin_pkg::winget::is_local(&id.id);
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
        book.set_installed(mgr, installed);
        book.set_updates(mgr, updates, None);
    }
    book.finish(jiff_now());
    cx.dispatch(Command::new("pkg.open").arg("filter", "installed"));
}

/// Сейчас — через модель (jiff в ui только вне Windows).
fn jiff_now() -> urfin_pkg::book::Timestamp {
    urfin_pkg::book::Timestamp::now()
}
