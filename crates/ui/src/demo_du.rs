//! `screens.du` (F01-05, только `--screens`): вкладка анализа места на снимке-фикстуре —
//! без скана настоящей ФС, размеры как на макетах. `state=scan` — идёт подсчёт (первая
//! папка «≥ X»), `done` — посчитано сейчас, `cached` — из кэша двухчасовой давности;
//! `stop=true` — закрыть вкладку анализа (активную). Папки — из уже загруженного листинга
//! активного таба (без I/O), файлы вкладка дочитывает сама.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use urfin_core::{Command, Cx, EntryKind, JobId};
use urfin_vfs::du::{DuTree, SizeMode, SnapMeta, VolumeKey, dvfs};
use urfin_vfs::volume::FsKind;
use urfin_vfs::walk::RawName;

const GIB: u64 = 1 << 30;
/// Размеры папок по порядку имён: крупная первая, как `target` на макете.
const SIZES: [u64; 6] = [
    GIB * 182 / 10,
    GIB * 46 / 10,
    GIB * 35 / 10,
    GIB * 21 / 10,
    GIB * 14 / 10,
    GIB * 6 / 10,
];
const FILES: [u32; 6] = [312_118, 64_012, 51_200, 9_870, 4_410, 2_100];

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

pub(crate) fn run(state: &str, stop: bool, cx: &mut Cx<'_>) {
    if stop {
        // Сцены открывают одну вкладку анализа и остаются на ней.
        if crate::du_view::is_du(cx.state.active_tab()) {
            cx.dispatch(Command::new("tab.close"));
        }
        return;
    }
    let Some(reg) = dvfs::registry(cx.state).cloned() else {
        return;
    };
    let t = cx.state.active_tab();
    // Корень — каталог до анализа (повторный вызов из вкладки `du:` — тот же корень).
    let root = match dvfs::localize(cx.state, &t.loc) {
        Some(l) => l.path,
        None if t.loc.is_local() => t.loc.path.clone(),
        None => return,
    };
    let in_du = crate::du_view::is_du(t);
    let mut dirs: Vec<String> = t
        .listing
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::Dir)
        .map(|e| e.name.to_string())
        .collect();
    dirs.sort();
    let files_bytes: u64 = t
        .listing
        .entries
        .iter()
        .filter(|e| !e.kind.is_dir_like())
        .map(|e| e.size)
        .sum();
    let own = t.listing.stats.files;
    let scan = state == "scan";
    let tree = DuTree::new();
    let now = now_nanos();
    tree.set_root_meta(now, 0);
    // Крупнейшая — «Projects» (как `target`), остальные — по порядку.
    dirs.sort_by_key(|n| n != "Projects");
    for (k, name) in dirs.iter().enumerate() {
        let Some(id) = tree.add_dir(
            0,
            &RawName::Str {
                name,
                escaped: false,
            },
            now,
            0,
        ) else {
            continue;
        };
        let size = SIZES[k % SIZES.len()];
        // Идёт подсчёт: у первой папки — доля посчитанного.
        let size = if scan && k == 0 {
            size * 83 / 100
        } else {
            size
        };
        tree.add_sizes(id, size, size, FILES[k % FILES.len()]);
        if !(scan && k == 0) {
            tree.complete(id);
        }
    }
    tree.add_sizes(0, files_bytes, files_bytes, own);
    let id = if scan {
        reg.open_live(root, Arc::new(tree), JobId(u64::MAX))
    } else {
        tree.complete(0);
        let cached = state == "cached";
        let snap = tree.freeze(SnapMeta {
            root,
            volume: VolumeKey {
                serial: 0,
                fs: FsKind::Ntfs,
            },
            scanned_at: if cached {
                now - 7_300 * 1_000_000_000
            } else {
                now
            },
            took_ms: 68_000,
            size_mode: SizeMode::Disk,
            track_min: 1 << 20,
            flags: 0,
        });
        reg.open_snapshot(snap, cached)
    };
    if !in_du {
        cx.dispatch(Command::new("tab.new"));
    }
    cx.dispatch(Command::new("sel.none"));
    let loc = dvfs::location(id, Path::new(""));
    cx.dispatch(Command::new("panel.go").arg("path", loc.clone()));
    // По размеру ↓, файлы и папки вперемешку — как `du.open` (ставит handler `du`).
    reg.opened(loc);
}
