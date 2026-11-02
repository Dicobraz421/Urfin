//! Фильтры вкладки `du:` (F01-08) и данные карты (F01-07): виды из снимка, без I/O при
//! показе, вычитание удалённого при перечитывании.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use urfin_core::{Attrs, Entry, EntryKind, Location};

use crate::api::{
    Caps, ListOpts, ListStream, Metadata, Space, Vfs, VfsError, VfsPath, VfsRead, WatchGuard,
    WatchTx,
};
use crate::du::dvfs::{DuRegistry, DuVfs, location, location_view, view};
use crate::du::filters::{Effect, TypeGroup, View, expand_rules};
use crate::du::model::{DuFilter, DuSnapshot, DuTree, DuType, SizeMode};
use crate::du::scan::{ScanCfg, scan};
use crate::du::views::MapKind;
use crate::local::LocalFs;

const K: usize = 4096;

fn put(root: &Path, rel: &str, blocks: usize) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![1u8; blocks * K]).unwrap();
}

/// Проект со сборкой, пакетами и кэшем, «системный» `Temp`, данные со старым образом.
fn sample(root: &Path) {
    put(root, r"proj\target\debug\app.exe", 4);
    put(root, r"proj\target\release\x.rlib", 3);
    put(root, r"proj\src\main.rs", 1);
    put(root, r"node_modules\pkg\index.js", 2);
    put(root, r".cache\sccache\blob.qqq", 6);
    put(root, r".cache\pip\w.qqq", 1);
    put(root, r"Temp\junk.tmp", 2);
    put(root, r"data\big.zip", 10);
    put(root, r"data\old.iso", 7);
    put(root, r"data\~$doc.docx", 1);
    put(root, "readme.md", 1);
    let old = SystemTime::now() - Duration::from_hours(3 * 365 * 24);
    fs::File::options()
        .write(true)
        .open(root.join(r"data\old.iso"))
        .unwrap()
        .set_modified(old)
        .unwrap();
}

fn snapshot(root: &Path) -> DuSnapshot {
    let cfg = ScanCfg {
        size_mode: SizeMode::Logical,
        track_min: 1,
        ..ScanCfg::default()
    };
    let tree = DuTree::new();
    scan(
        &Location::local(root),
        &LocalFs::new(),
        &cfg,
        &tree,
        &|| false,
        None,
        |_| {},
    )
    .snapshot
}

/// Реестр со снимком `root`; `%TEMP%` — `root\Temp`.
fn open(root: &Path) -> (std::sync::Arc<DuRegistry>, u32) {
    let reg = DuRegistry::new();
    let temp = root.join("Temp").display().to_string();
    reg.set_rules(expand_rules(&|k| (k == "TEMP").then(|| temp.clone())));
    let id = reg.open_snapshot(snapshot(root), false);
    (reg, id)
}

fn list(reg: &std::sync::Arc<DuRegistry>, loc: &Location) -> Vec<Entry> {
    let v = DuVfs::new(reg.clone());
    let mut s = v.list(loc, &ListOpts::default()).unwrap();
    let mut out = Vec::new();
    while s
        .next_batch(&mut out, &urfin_core::CancelToken::new())
        .unwrap()
    {}
    out
}

fn names(v: &[Entry]) -> Vec<&str> {
    v.iter().map(|e| e.name.as_str()).collect()
}

fn at(id: u32, f: DuFilter) -> Location {
    location_view(id, &View::new(f))
}

/// Backend, который нельзя трогать: показ вида из снимка — без I/O.
#[derive(Debug)]
struct NoIo;

impl Vfs for NoIo {
    fn scheme(&self) -> &'static str {
        "local"
    }
    fn caps(&self, _: &VfsPath) -> Caps {
        unreachable!("I/O")
    }
    fn list(&self, _: &VfsPath, _: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        unreachable!("I/O")
    }
    fn stat(&self, _: &VfsPath, _: bool) -> Result<Metadata, VfsError> {
        unreachable!("I/O")
    }
    fn stat_entry(&self, _: &VfsPath, _: &str, _: Attrs) -> Result<Entry, VfsError> {
        unreachable!("I/O")
    }
    fn open_read(&self, _: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        unreachable!("I/O")
    }
    fn space(&self, _: &VfsPath) -> Result<Space, VfsError> {
        unreachable!("I/O")
    }
    fn local_path(&self, _: &VfsPath) -> Option<PathBuf> {
        unreachable!("I/O")
    }
    fn watch(&self, _: &VfsPath, _: WatchTx) -> Result<WatchGuard, VfsError> {
        unreachable!("I/O")
    }
}

#[test]
fn view_addresses() {
    let loc = location_view(3, &View::new(DuFilter::Temp));
    let (id, rel, v) = view(&loc).unwrap();
    assert_eq!((id, rel.len(), v.filter), (3, 0, DuFilter::Temp));
    assert_eq!(Location::parse_uri(&loc.to_uri()).unwrap(), loc);
    // Вход в каталог из строки фильтра — снова «По папкам» этого каталога.
    let mut child = loc.clone();
    child.path = loc.path.join(r"proj\target");
    let (_, rel, v) = view(&child).unwrap();
    assert_eq!(rel, ["proj", "target"]);
    assert_eq!(v.filter, DuFilter::Dirs);
    assert!(!loc.has_parent(), "у вида фильтра нет `..`");
    assert_eq!(child.parent().unwrap().path, loc.path.join("proj"));
    assert_eq!(
        view(&location(3, Path::new(""))).unwrap().2,
        View::default()
    );
    let mut bad = loc;
    bad.path = PathBuf::from("/3?f=nope");
    assert!(view(&bad).is_none());
}

#[test]
fn large_and_old() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let (reg, id) = open(tmp.path());
    let large = list(&reg, &at(id, DuFilter::Large));
    assert_eq!(large.len(), 11);
    assert_eq!(large[0].name, r"data\big.zip");
    assert_eq!(large[0].size, (10 * K) as u64);
    assert_eq!(large[0].kind, EntryKind::File);
    assert!(names(&large).contains(&r"proj\target\debug\app.exe"));
    assert!(names(&large).contains(&"readme.md"));
    // Старше года — только образ; старше пяти — никого.
    let old = list(&reg, &at(id, DuFilter::Old));
    assert_eq!(names(&old), [r"data\old.iso"]);
    let five = View {
        filter: DuFilter::Old,
        age: "5y".into(),
    };
    assert_eq!(list(&reg, &location_view(id, &five)).len(), 0);
}

/// F01-08: смена порога «Старые» (и первый показ любого вида) — без I/O.
#[test]
fn old_age_change_without_io() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let (reg, id) = open(tmp.path());
    let loc = location(id, Path::new(""));
    for age in ["1y", "2y", "6m", "5y"] {
        let v = View {
            filter: DuFilter::Old,
            age: age.into(),
        };
        reg.list(&location_view(id, &v), &NoIo).unwrap();
    }
    for f in [DuFilter::Large, DuFilter::Types, DuFilter::Temp] {
        reg.list(&at(id, f), &NoIo).unwrap();
    }
    assert!(reg.info(&loc).is_some());
}

#[test]
fn types_groups_and_expand() {
    let _ = urfin_i18n::set_language("ru");
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let (reg, id) = open(tmp.path());
    let loc = at(id, DuFilter::Types);
    let rows = list(&reg, &loc);
    assert!(rows.iter().all(|e| e.kind == EntryKind::Other));
    let groups: Vec<TypeGroup> = rows
        .iter()
        .map(|e| TypeGroup::from_label(&e.name).unwrap())
        .collect();
    // По размеру ↓. «Сборки и кэши» (решение 2026-11-02):
    // `target` с `debug`/`release`, `node_modules`, `.cache` — 16 блоков, 5 файлов.
    assert_eq!(groups[0], TypeGroup::Build);
    assert_eq!(groups[1], TypeGroup::Type(DuType::Archive));
    // Всё «Прочее» примера — в сборках и кэшах (`.cache`).
    assert!(!groups.contains(&TypeGroup::Type(DuType::Other)));
    let build = &rows[0];
    assert_eq!(build.size, (16 * K) as u64);
    assert_eq!(reg.type_files(&loc, TypeGroup::Build), Some(5));
    // «Код» — без сборок: только `proj\src\main.rs`.
    let code_g = TypeGroup::Type(DuType::Code);
    let code = rows.iter().find(|e| e.name == code_g.label()).unwrap();
    assert_eq!(code.size, K as u64);
    assert_eq!(reg.type_files(&loc, code_g), Some(1));
    // Колонка «Файлов».
    let v = DuVfs::new(reg.clone());
    let cell = v.ext(&loc, build, "files");
    assert_eq!(cell, Some(crate::api::ExtCell::Text("5".into())));
    // Enter на группе — раскрыть: крупнейшие файлы группы под ней.
    assert!(reg.toggle_group(id, code_g));
    assert!(reg.is_open(&loc, code_g));
    let rows = list(&reg, &loc);
    let at_code = rows.iter().position(|e| e.name == code_g.label()).unwrap();
    assert_eq!(rows[at_code + 1].name, r"proj\src\main.rs");
    assert_eq!(rows[at_code + 1].kind, EntryKind::File);
    reg.toggle_group(id, code_g);
    assert!(reg.toggle_group(id, TypeGroup::Build));
    let rows = list(&reg, &loc);
    assert_eq!(rows[1].name, r".cache\sccache\blob.qqq");
    assert_eq!(rows[2].name, r"proj\target\debug\app.exe");
    reg.toggle_group(id, TypeGroup::Build);
    assert_eq!(list(&reg, &loc).len(), groups.len());
}

/// F01-08: «Временные» — правила по имени и пути (`%TEMP%` раскрыт), разделы,
/// подсказки; ничего не отмечено (отметку ставит только пользователь).
#[test]
fn temp_rules() {
    let _ = urfin_i18n::set_language("ru");
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let (reg, id) = open(tmp.path());
    let loc = at(id, DuFilter::Temp);
    let rows = list(&reg, &loc);
    let n = names(&rows);
    let sec = |i: usize| rows[i].kind == EntryKind::Other;
    assert!(sec(0), "{n:?}");
    // Проект: сборка Cargo, пакеты, кэш, одиночный временный файл.
    for want in [r"proj\target", "node_modules", ".cache", r"data\~$doc.docx"] {
        assert!(n.contains(&want), "{want} in {n:?}");
    }
    assert!(!n.contains(&r"proj\src"));
    // Система: %TEMP% целиком, его файл отдельно не повторяется.
    let sys = rows
        .iter()
        .rposition(|e| e.kind == EntryKind::Other)
        .unwrap();
    assert!(sys > 0);
    assert_eq!(rows[sys + 1].name, "Temp");
    assert!(!n.contains(&r"Temp\junk.tmp"));
    assert_eq!(rows[sys].size, (2 * K) as u64);
    let hints = reg.hints(&loc);
    assert_eq!(hints[".cache"].text, "sccache, pip");
    assert_ne!(hints[r"proj\target"].text, "");
    assert!(!hints[r"proj\target"].warn);
    assert_eq!(
        hints["Temp"].text,
        urfin_i18n::current().get("du-temp-what-temp")
    );
    // Понятное имя системной строки и что будет после удаления (решение 2026-11-02
    // «du-filters» №3, №4): имя строки — путь (F3/F8), подпись — «%TEMP%».
    assert_eq!(hints["Temp"].label.as_deref(), Some("%TEMP%"));
    assert_eq!(hints["Temp"].effect, Effect::Safe);
    assert_eq!(hints[r"proj\target"].label, None);
    assert_eq!(hints[r"proj\target"].effect, Effect::Recreated);
    assert_eq!(
        hints[r"proj\target"].after,
        urfin_i18n::current().get("du-temp-after-cargo")
    );
    assert_eq!(hints[r"data\~$doc.docx"].effect, Effect::Safe);
    assert!(hints.values().all(|h| !h.recycle && !h.after.is_empty()));
    // Каталоги — с размером, Enter ведёт в «По папкам».
    let target = rows.iter().find(|e| e.name == r"proj\target").unwrap();
    assert_eq!(target.kind, EntryKind::Dir);
    assert_eq!(target.size, (7 * K) as u64);
}

/// Удалённое после показа вида вычитается при перечитывании (F01 §3.7, решение
/// 2026-11-01): из каталога, предков и сумм по типам.
#[test]
fn reread_subtracts_deleted() {
    let _ = urfin_i18n::set_language("ru");
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let (reg, id) = open(tmp.path());
    let root = location(id, Path::new(""));
    let total = reg.info(&root).unwrap().bytes;
    let large = at(id, DuFilter::Large);
    list(&reg, &large);
    fs::remove_file(tmp.path().join(r"data\big.zip")).unwrap();
    fs::remove_dir_all(tmp.path().join("node_modules")).unwrap();
    let rows = list(&reg, &large);
    assert!(!names(&rows).contains(&r"data\big.zip"));
    assert!(!names(&rows).contains(&r"node_modules\pkg\index.js"));
    // Файлы удалённого каталога — тоже: строки «Крупных» проверяются по одной.
    assert_eq!(reg.info(&root).unwrap().bytes, total - (12 * K) as u64);
    assert_eq!(reg.dirty(), [id]);
    // Каталог ушёл при перечитывании «Временных».
    list(&reg, &at(id, DuFilter::Temp));
    let rows = list(&reg, &at(id, DuFilter::Temp));
    assert!(!names(&rows).contains(&"node_modules"));
    assert_eq!(reg.info(&root).unwrap().bytes, total - (12 * K) as u64);
    // Суммы по типам: архивов больше нет, у сборок — без index.js.
    let types = list(&reg, &at(id, DuFilter::Types));
    assert!(types.iter().all(|e| e.name != DuType::Archive.label()));
    assert_eq!(
        reg.type_files(&root, TypeGroup::Type(DuType::Code)),
        Some(1)
    );
    assert_eq!(reg.type_files(&root, TypeGroup::Build), Some(4));
    // «По папкам» — без удалённого каталога.
    let dirs = list(&reg, &root);
    assert!(dirs.iter().all(|e| e.name != "node_modules"));
}

/// F01-07: данные карты — дети каталога и их дети, мелкие файлы — одним блоком,
/// совпадения «Временных» помечены.
#[test]
fn map_two_levels() {
    let tmp = tempfile::tempdir().unwrap();
    sample(tmp.path());
    let reg = DuRegistry::new();
    let sys_dir = tmp.path().join("Temp").display().to_string();
    reg.set_rules(expand_rules(&|k| (k == "TEMP").then(|| sys_dir.clone())));
    let mut snap = snapshot(tmp.path());
    // Порог крупного файла выше readme: он уйдёт в «мелкие файлы».
    let readme = snap
        .files
        .iter()
        .position(|f| snap.file_name(f) == "readme.md")
        .unwrap();
    snap.files.remove(readme);
    let id = reg.open_snapshot(snap, false);
    // Каталоги «Временных» — задачей вне UI-потока: до неё карта без пометок.
    let first = reg.map(&location(id, Path::new(""))).unwrap();
    assert!(first.items.iter().all(|i| !i.temp));
    assert_eq!(reg.take_temp_wanted(), [id]);
    assert_eq!(reg.take_temp_wanted().len(), 0);
    reg.compute_temp(id);
    let m = reg.map(&location(id, Path::new(""))).unwrap();
    assert_eq!(reg.take_temp_wanted().len(), 0, "посчитано — не просит");
    assert_eq!(m.key, (id, 0));
    let sizes: Vec<u64> = m.items.iter().map(|i| i.size).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
    let data = m.items.iter().find(|i| i.name == "data").unwrap();
    assert_eq!(data.kind, MapKind::Dir);
    assert_eq!(data.kids[0].name, "big.zip");
    assert_eq!(data.kids[0].kind, MapKind::File(DuType::Archive));
    assert!(data.kids.iter().all(|k| k.kids.is_empty()));
    let small = m
        .items
        .iter()
        .find(|i| matches!(i.kind, MapKind::Small { .. }))
        .unwrap();
    assert_eq!(small.kind, MapKind::Small { files: 1 });
    assert_eq!(small.size, K as u64);
    let proj = m.items.iter().find(|i| i.name == "proj").unwrap();
    assert!(!proj.temp);
    assert!(proj.kids.iter().find(|k| k.name == "target").unwrap().temp);
    assert!(m.items.iter().find(|i| i.name == "Temp").unwrap().temp);
    // Пока идёт подсчёт — карты нет.
    let live = reg.open_live(
        tmp.path().join("x"),
        std::sync::Arc::new(DuTree::new()),
        urfin_core::JobId(1),
    );
    assert!(reg.map(&location(live, Path::new(""))).is_none());
}

/// F01-09: группы по размеру; жёсткие ссылки — не дубликаты; общий префикс 64 KiB и
/// разный хвост разделяются только полным хэшем; совпавшие — к отметке (все, кроме
/// первой). Только временный каталог.
#[test]
fn dups_levels_and_marks() {
    use crate::du::dups;
    use crate::du::model::DupLevel;
    let _ = urfin_i18n::set_language("ru");
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path();
    let mib = 1usize << 20;
    let same = vec![5u8; 2 * mib];
    let mut tail = vec![5u8; 2 * mib];
    tail[mib] = 9;
    for (rel, data) in [
        (r"a\x.bin", &same),
        (r"b\x.bin", &same),
        (r"c\y.dat", &tail),
        (r"d\y.dat", &same[..2 * mib].to_vec()),
    ] {
        let p = r.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }
    // Разный размер — в группу не идёт; жёсткая ссылка — тот же файл.
    put(r, r"e\one.bin", 300);
    fs::create_dir_all(r.join("f")).unwrap();
    fs::hard_link(r.join(r"e\one.bin"), r.join(r"f\link.bin")).unwrap();
    let (reg, id) = open(r);
    let loc = at(id, DuFilter::Dups);
    let rows = list(&reg, &loc);
    // Одна группа размера 2 МиБ: четыре файла (x и y одного размера).
    let heads: Vec<&Entry> = rows.iter().filter(|e| e.kind == EntryKind::Other).collect();
    assert_eq!(heads.len(), 1, "{:?}", names(&rows));
    assert_eq!(rows.len(), 5);
    assert!(
        !names(&rows)
            .iter()
            .any(|n| n.ends_with("link.bin") || n.ends_with("one.bin"))
    );
    let info = reg.dups_info(&loc).unwrap();
    assert_eq!((info.groups, info.checked, info.files), (1, 0, 4));

    let run = |full: bool| {
        let (root, plan) = reg.dup_plan(id, full).unwrap();
        let cancel = urfin_core::CancelToken::new();
        let hashes = plan
            .iter()
            .map(|(i, rel)| {
                let p = root.join(rel.as_str());
                let h = if full {
                    dups::full_hash(&p, &cancel, &mut |_| {}).unwrap()
                } else {
                    u128::from(dups::head_hash(&p).unwrap())
                };
                (*i, h)
            })
            .collect();
        reg.dup_apply(id, &hashes, full);
    };
    // Частичный: все начала одинаковы — одна группа `Partial`, отметок нет.
    run(false);
    assert!(reg.dup_plan(id, false).is_none(), "частичный — один раз");
    assert_eq!(
        reg.dup_heads(&loc),
        [crate::du::dvfs::DupHead {
            copies: 4,
            level: DupLevel::Partial
        }]
    );
    assert_eq!(reg.pending_marks().len(), 0);
    // Полный: x и d\y совпали, c\y — отдельно (хвост другой).
    run(true);
    let heads = reg.dup_heads(&loc);
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].copies, 3);
    assert_eq!(heads[0].level, DupLevel::Full);
    assert_eq!(reg.pending_marks(), [id]);
    let marks = reg.take_marks(id);
    assert_eq!(marks.len(), 2);
    assert!(!marks.iter().any(|m| m.as_str() == r"c\y.dat"));
    assert_eq!(reg.take_marks(id).len(), 0);
    let rows = list(&reg, &loc);
    assert!(!names(&rows).contains(&r"c\y.dat"));
    assert!(reg.dup_plan(id, true).is_none(), "всё проверено");
}
