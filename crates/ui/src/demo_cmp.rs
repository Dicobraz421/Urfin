//! Панель сравнения `cmp:` на фикстуре (S8-39, D-12): сцена `screen-cmppanel` без
//! обхода настоящей ФС. Строки, итоги и обе стороны пары — как на макете.
//!
//! `screens.search kind=cmp` — новая вкладка активной панели на стор сравнения
//! `C:\Dev\urfin ↔ D:\Temp\urfin-main`, итоги — командой `ui.cmp.result`.

use std::sync::{Arc, OnceLock};

use urfin_core::search::{Hit, SearchModel, SearchStore};
use urfin_core::{Args, Attrs, Command, Cx, Entry, EntryKind, Location};

use crate::demo_search::stamp;

/// Стор сцены — в реестр панели `cmp:` (его держит `urfin-app`).
pub type Adopt = Box<dyn Fn(&Arc<SearchStore>) + Send + Sync>;

static ADOPT: OnceLock<Adopt> = OnceLock::new();

/// `--screens`: чем стор сцены попадает в реестр панели `cmp:` (зовёт `urfin-app`).
pub fn set_adopt(f: Adopt) {
    let _ = ADOPT.set(f);
}

const LEFT: &str = r"C:\Dev\urfin";
const RIGHT: &str = r"D:\Temp\urfin-main";

/// Строка: путь от корня, папка, статус (`Hit.status`), размер и дата стороны строки,
/// у пары — размер и дата другой стороны.
type Row = (
    &'static str,
    bool,
    u8,
    u64,
    &'static str,
    Option<(u64, &'static str)>,
);

/// Как на `screen-cmppanel` (`CM_HITS` макета); вторая сторона пары — для статуса окна.
const ROWS: [Row; 12] = [
    (".git", true, 1, 0, "11.11.26 10:42", None),
    (
        r"assets\icon\urfin-256.png",
        false,
        5,
        9011,
        "21.10.26 12:30",
        Some((8806, "21.10.26 12:30")),
    ),
    (
        "clippy.toml",
        false,
        5,
        102,
        "15.10.26 10:11",
        Some((102, "15.10.26 10:11")),
    ),
    (
        r"crates\arc\src\formats\sevenz.rs",
        false,
        2,
        14_541,
        "11.11.26 09:58",
        None,
    ),
    (
        r"crates\ops\src\ops\compare.rs",
        false,
        3,
        7885,
        "11.11.26 10:31",
        Some((7270, "10.11.26 18:20")),
    ),
    (
        r"crates\ui\src\gpui_shell\compare.rs",
        false,
        3,
        5427,
        "11.11.26 10:37",
        Some((5018, "10.11.26 18:20")),
    ),
    (
        r"crates\ui\src\gpui_shell\extract_bar.rs",
        false,
        2,
        9626,
        "11.11.26 09:40",
        None,
    ),
    (
        r"docs\decisions\2026-11-11-small-sizes.md",
        false,
        2,
        912,
        "11.11.26 09:12",
        None,
    ),
    (
        r"docs\design\window.dc.html",
        false,
        4,
        552_960,
        "11.11.26 09:40",
        Some((548_864, "10.11.26 18:20")),
    ),
    (
        r"docs\tasks\D-12.md",
        false,
        1,
        1126,
        "10.11.26 21:05",
        None,
    ),
    (
        r"docs\tasks\README.md",
        false,
        4,
        6963,
        "11.11.26 10:02",
        Some((6656, "10.11.26 18:20")),
    ),
    ("target", true, 1, 0, "11.11.26 10:40", None),
];

fn hit(name: &str, dir: bool, root: u16, status: u8, size: u64, when: &str) -> Hit {
    Hit {
        e: Entry {
            name: name.into(),
            size,
            mtime: stamp(when),
            ctime: 0,
            atime: 0,
            attrs: Attrs::default(),
            kind: if dir { EntryKind::Dir } else { EntryKind::File },
        },
        root,
        line: 0,
        hl: (0, 0),
        preview: None,
        status,
    }
}

/// Панель сравнения на фикстуре: готовое сравнение по содержимому, новая вкладка
/// активной панели.
pub(crate) fn panel(cx: &mut Cx<'_>) {
    if cx.state.ext.get::<SearchModel>().is_none() {
        cx.state.ext.insert(SearchModel::default());
    }
    let Some(m) = cx.state.ext.get_mut::<SearchModel>() else {
        return;
    };
    let store = m.open(vec![Location::local(LEFT), Location::local(RIGHT)]);
    let mut hits = Vec::with_capacity(ROWS.len() * 2);
    for &(name, dir, status, size, when, other) in &ROWS {
        // Сторона строки: правая — у `only_right` и `newer_right` (контракт `Hit.root`).
        let root = u16::from(matches!(status, 2 | 4));
        hits.push(hit(name, dir, root, status, size, when));
        if let Some((size, when)) = other {
            hits.push(hit(name, dir, root, status, size, when));
        }
    }
    store.append(hits);
    store.finish(1200);
    if let Some(adopt) = ADOPT.get() {
        adopt(&store);
    }
    let id = store.id.0;
    let totals = Args::default()
        .with("mode", "content")
        .with("only_left", 3)
        .with("only_right", 3)
        .with("newer_left", 2)
        .with("newer_right", 2)
        .with("differs", 2)
        .with("equal", 1187);
    cx.dispatch(crate::cmp_panel::Totals::command(id, &totals));
    cx.dispatch(Command::new("tab.new"));
    let loc = Location {
        backend: crate::cmp_panel::SCHEME.into(),
        authority: None,
        path: format!("/{id}").into(),
        outer: None,
    };
    cx.dispatch(Command::new("panel.go").arg("path", loc));
}
