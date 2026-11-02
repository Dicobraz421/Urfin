//! `screens.du` (F01-05…F01-08, только `--screens`): вкладка анализа места на
//! снимке-фикстуре — без скана настоящей ФС, размеры как на макетах. `state=scan` — идёт
//! подсчёт (первая папка «≥ X»), `done` — посчитано сейчас, `cached` — из кэша
//! двухчасовой давности; `stop=true` — закрыть вкладку анализа (активную). Папки
//! первого уровня — из уже загруженного листинга активного таба (без I/O), файлы
//! вкладка дочитывает сама. Внутри папок, крупные файлы и суммы по типам — придуманы
//! (карта, «Типы», «Временные», «Дубликаты» — `screen-dumap`, `screen-dutypes`,
//! `screen-dutemp`, `screen-dudups`). Группы дубликатов заданы фикстурой (хэширования
//! нет): совпавшими по хэшу считаются файлы из [`DUPS_FULL`].

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use urfin_core::{Command, Cx, EntryKind, JobId};
use urfin_vfs::du::filters::expand_rules;
use urfin_vfs::du::{
    DirId, DuTree, DuType, DupLevel, FileRec, NameRef, SizeMode, SnapMeta, VolumeKey, dvfs,
};
use urfin_vfs::volume::FsKind;
use urfin_vfs::walk::RawName;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;
const DAY: i64 = 86_400 * 1_000_000_000;
/// Дубликаты фикстуры, «совпадает по хэшу»; прочие группы — «только размер».
const DUPS_FULL: [&str; 3] = ["LLVM-19.1.7-win64.exe", "chrome-win.zip", "rustup-init.exe"];
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

/// Папка фикстуры: свои мелкие файлы (байты, число, тип), подпапки, крупные файлы
/// (имя, байты, тип, возраст в днях).
struct D {
    name: &'static str,
    own: (u64, u32, DuType),
    kids: &'static [D],
    large: &'static [(&'static str, u64, DuType, i64)],
}

const fn leaf(name: &'static str, bytes: u64, files: u32, ty: DuType) -> D {
    D {
        name,
        own: (bytes, files, ty),
        kids: &[],
        large: &[],
    }
}

/// Папка из одной подпапки (цепочка `AppData\Local\…`).
const fn via(name: &'static str, kids: &'static [D]) -> D {
    D {
        name,
        own: (0, 0, DuType::Other),
        kids,
        large: &[],
    }
}

/// Внутри папок первого уровня демо-дерева (`screen-dumap`).
const INSIDE: [D; 4] = [
    D {
        name: "Projects",
        own: (GIB / 5, 3_000, DuType::Code),
        kids: &[
            via(
                "target",
                &[
                    leaf("release", GIB * 98 / 10, 120_000, DuType::Code),
                    leaf("debug", GIB * 71 / 10, 90_000, DuType::Code),
                    leaf("doc", MIB * 819, 30_000, DuType::Doc),
                ],
            ),
            via(
                "node_modules",
                &[
                    leaf(".pnpm", GIB * 76 / 10, 200_000, DuType::Code),
                    leaf(".cache", MIB * 819, 5_000, DuType::Other),
                ],
            ),
            via(
                ".cache",
                &[
                    leaf("sccache", GIB * 23 / 10, 9_000, DuType::Other),
                    D {
                        name: "playwright",
                        own: (MIB * 300, 1_200, DuType::Exec),
                        kids: &[
                            D {
                                name: "chromium-1140",
                                own: (MIB * 150, 300, DuType::Exec),
                                kids: &[],
                                large: &[("chrome-win.zip", MIB * 156, DuType::Archive, 51)],
                            },
                            D {
                                name: "chromium-1148",
                                own: (MIB * 150, 300, DuType::Exec),
                                kids: &[],
                                large: &[("chrome-win.zip", MIB * 156, DuType::Archive, 30)],
                            },
                        ],
                        large: &[],
                    },
                    leaf("pip", MIB * 280, 800, DuType::Archive),
                ],
            ),
            D {
                name: "tools",
                own: (MIB * 40, 300, DuType::Exec),
                kids: &[D {
                    name: "installers",
                    own: (MIB * 25, 40, DuType::Exec),
                    kids: &[],
                    large: &[
                        ("LLVM-19.1.7-win64.exe", MIB * 362, DuType::Exec, 649),
                        ("rustup-init.exe", MIB * 91 / 10, DuType::Exec, 62),
                    ],
                }],
                large: &[],
            },
            D {
                name: "urfin",
                own: (GIB * 3 / 10, 5_000, DuType::Code),
                kids: &[
                    via(
                        "target",
                        &[leaf("debug", GIB * 21 / 10, 30_000, DuType::Code)],
                    ),
                    D {
                        name: "scripts",
                        own: (MIB, 30, DuType::Code),
                        kids: &[],
                        large: &[("rustup-init.exe", MIB * 91 / 10, DuType::Exec, 61)],
                    },
                    D {
                        name: "assets",
                        own: (MIB * 12, 200, DuType::Image),
                        kids: &[],
                        large: &[("icons-fluent.zip", MIB * 42 / 10, DuType::Archive, 62)],
                    },
                ],
                large: &[("urfin-0.4.2.zip", MIB * 89 / 10, DuType::Archive, 40)],
            },
            D {
                name: "postman",
                own: (GIB / 5, 3_000, DuType::Code),
                kids: &[
                    via(
                        "target",
                        &[leaf("release", GIB * 11 / 10, 9_000, DuType::Code)],
                    ),
                    D {
                        name: "scripts",
                        own: (MIB, 30, DuType::Code),
                        kids: &[],
                        large: &[("rustup-init.exe", MIB * 91 / 10, DuType::Exec, 58)],
                    },
                    D {
                        name: "assets",
                        own: (MIB * 9, 150, DuType::Image),
                        kids: &[],
                        large: &[("icons-fluent.zip", MIB * 42 / 10, DuType::Archive, 43)],
                    },
                ],
                large: &[("fixtures.7z", MIB * 312 / 10, DuType::Archive, 900)],
            },
            D {
                name: "datasets",
                own: (MIB * 100, 900, DuType::Other),
                kids: &[D {
                    name: "osm",
                    own: (MIB * 520, 40, DuType::Other),
                    kids: &[D {
                        name: "old",
                        own: (MIB * 4, 3, DuType::Other),
                        kids: &[],
                        large: &[("moscow-latest.osm.pbf", MIB * 964 / 10, DuType::Other, 144)],
                    }],
                    large: &[("moscow-latest.osm.pbf", MIB * 964 / 10, DuType::Other, 31)],
                }],
                large: &[
                    ("mail-corpus.7z", MIB * 410, DuType::Archive, 500),
                    ("fixtures.7z", MIB * 312 / 10, DuType::Archive, 900),
                ],
            },
        ],
        large: &[
            ("Win11_24H2_x64.iso", GIB * 54 / 10, DuType::DiskImage, 400),
            ("LLVM-19.1.7-win64.exe", MIB * 362, DuType::Exec, 650),
        ],
    },
    D {
        name: "Фото",
        own: (GIB * 12 / 10, 2_000, DuType::Image),
        kids: &[leaf("2019", MIB * 900, 700, DuType::Image)],
        large: &[
            ("отпуск-2026.mp4", GIB * 21 / 10, DuType::Video, 90),
            ("DSC_0001.cr3", MIB * 41, DuType::Image, 2_400),
        ],
    },
    D {
        name: "Документы",
        own: (MIB * 600, 4_000, DuType::Doc),
        kids: &[via(
            "AppData",
            &[via(
                "Local",
                &[
                    leaf("Temp", GIB * 38 / 10, 4_000, DuType::Temp),
                    via(
                        "Microsoft",
                        &[via(
                            "Edge",
                            &[via(
                                "User Data",
                                &[via(
                                    "Default",
                                    &[leaf("Cache", GIB * 12 / 10, 6_000, DuType::Other)],
                                )],
                            )],
                        )],
                    ),
                ],
            )],
        )],
        large: &[("~$Отчёт.docx", MIB * 2, DuType::Temp, 3)],
    },
    D {
        name: ".git",
        own: (MIB * 700, 1_500, DuType::Other),
        kids: &[leaf("objects", GIB * 39 / 10, 9_000, DuType::Other)],
        large: &[],
    },
];

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

fn raw(name: &str) -> RawName<'_> {
    RawName::Str {
        name,
        escaped: false,
    }
}

/// Суммы по типам (по каталогам) и крупные файлы фикстуры.
#[derive(Default)]
struct Acc {
    types: Vec<(DirId, DuType, u64, u64)>,
    large: Vec<(FileRec, CompactString, bool)>,
}

impl Acc {
    fn add(&mut self, dir: DirId, ty: DuType, bytes: u64, count: u64) {
        self.types.push((dir, ty, bytes, count));
    }

    /// Суммы — в дерево по каталогам (сборки и кэши узнаются при заморозке).
    fn flush(&self, tree: &DuTree) {
        for &(dir, ty, bytes, count) in &self.types {
            let mut disk = [0; DuType::COUNT];
            let mut files = [0; DuType::COUNT];
            disk[ty as usize] = bytes;
            files[ty as usize] = count;
            tree.add_types_at(dir, &disk, &files);
        }
    }
}

fn rec(dir: DirId, ty: DuType, size: u64, mtime: i64) -> FileRec {
    FileRec {
        dir,
        name: NameRef::default(),
        ty,
        attrs: 0,
        disk: size,
        logical: size,
        mtime,
        file_id: 0,
    }
}

/// Папка `d` под `id`: подпапки, свои файлы, крупные; досчитана.
fn fill(tree: &DuTree, id: DirId, d: &D, now: i64, acc: &mut Acc) {
    for k in d.kids {
        if let Some(kid) = tree.add_dir(id, &raw(k.name), now, 0) {
            fill(tree, kid, k, now, acc);
        }
    }
    let (bytes, count, ty) = d.own;
    tree.add_sizes(id, bytes, bytes, count);
    acc.add(id, ty, bytes, u64::from(count));
    for &(name, size, ty, age) in d.large {
        tree.add_sizes(id, size, size, 1);
        acc.add(id, ty, size, 1);
        acc.large
            .push((rec(id, ty, size, now - age * DAY), name.into(), false));
    }
    tree.complete(id);
}

#[allow(
    clippy::too_many_lines,
    reason = "фикстура: дерево, файлы корня, снимок"
)]
pub(crate) fn run(state: &str, stop: bool, cx: &mut Cx<'_>) {
    if stop {
        // Вопрос «Очистить временные?» сцены `dutemp-clean` — закрыть без ответа.
        while let Some(id) = cx.state.prompts.active().map(|p| p.id) {
            cx.state.prompts.cancel(id);
        }
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
    // «Временные» фикстуры: %TEMP% и кэш браузера — внутри демо-дерева.
    let local = root.join(r"Документы\AppData\Local");
    reg.set_rules(expand_rules(&|k| match k {
        "TEMP" => Some(local.join("Temp").display().to_string()),
        "LOCALAPPDATA" => Some(local.display().to_string()),
        _ => None,
    }));
    let in_du = crate::du_view::is_du(t);
    let mut dirs: Vec<String> = t
        .listing
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::Dir)
        .map(|e| e.name.to_string())
        .collect();
    dirs.sort();
    let own_files: Vec<(String, u64)> = t
        .listing
        .entries
        .iter()
        .filter(|e| !e.kind.is_dir_like())
        .map(|e| (e.name.to_string(), e.size))
        .collect();
    let files_bytes: u64 = own_files.iter().map(|f| f.1).sum();
    let own = t.listing.stats.files;
    let scan = state == "scan";
    let tree = DuTree::new();
    let now = now_nanos();
    tree.set_root_meta(now, 0);
    let mut acc = Acc::default();
    // Крупнейшая — «Projects» (как `target`), остальные — по порядку.
    dirs.sort_by_key(|n| n != "Projects");
    for (k, name) in dirs.iter().enumerate() {
        let Some(id) = tree.add_dir(0, &raw(name), now, 0) else {
            continue;
        };
        // Посчитано: внутри — как на макете карты.
        if let Some(d) = INSIDE.iter().find(|d| d.name == name).filter(|_| !scan) {
            fill(&tree, id, d, now, &mut acc);
            continue;
        }
        let size = SIZES[k % SIZES.len()];
        // Идёт подсчёт: у первой папки — доля посчитанного.
        let size = if scan && k == 0 {
            size * 83 / 100
        } else {
            size
        };
        tree.add_sizes(id, size, size, FILES[k % FILES.len()]);
        acc.add(id, DuType::Other, size, u64::from(FILES[k % FILES.len()]));
        if !(scan && k == 0) {
            tree.complete(id);
        }
    }
    tree.add_sizes(0, files_bytes, files_bytes, own);
    // Файлы корня: крупные — в снимок, все — в суммы по типам.
    for (name, size) in &own_files {
        let ty = DuType::of(&raw(name));
        acc.add(0, ty, *size, 1);
        if *size >= MIB {
            acc.large
                .push((rec(0, ty, *size, now - 30 * DAY), name.into(), false));
        }
    }
    acc.flush(&tree);
    tree.add_large(&acc.large);
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
            track_min: MIB,
            flags: 0,
        });
        // Дубликаты — без хэширования: уровни задаёт фикстура.
        let mut groups = urfin_vfs::du::dups::candidates(&snap, MIB, |_| true);
        for g in &mut groups {
            let full = g
                .files
                .first()
                .is_some_and(|&i| DUPS_FULL.contains(&snap.file_name(&snap.files[i as usize])));
            g.level = if full { DupLevel::Full } else { DupLevel::Size };
        }
        let id = reg.open_snapshot(snap, cached);
        reg.seed_dups(id, groups);
        id
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
