//! Копирование и сравнение из архива (S8-27, S8 §3.5, §4, §6): настоящий zip через
//! `urfin-arc`, архивы собираются здесь крейтом `zip`. «Побайтно как `7z x`» сверяется с
//! содержимым, из которого архив собран (7-Zip на машине может не быть).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use urfin_arc::{ArcCache, ArcCfg, root_of};
use urfin_core::{Boot, Command, Core, JobState, JobSummary, Location, Services, UiEvent, Waker};
use urfin_vfs::{VfsCfg, VfsListing, VfsRegistry};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Запись архива: имя (каталог — с `/` на конце) и данные; `stored` — без сжатия.
struct E<'a> {
    name: &'a str,
    data: Option<Vec<u8>>,
    stored: bool,
}

fn file(name: &str, data: Vec<u8>) -> E<'_> {
    E {
        name,
        data: Some(data),
        stored: false,
    }
}

fn stored(name: &str, data: Vec<u8>) -> E<'_> {
    E {
        name,
        data: Some(data),
        stored: true,
    }
}

fn dir(name: &str) -> E<'_> {
    E {
        name,
        data: None,
        stored: false,
    }
}

/// Псевдослучайные байты (плохо сжимаются).
fn noise(seed: u32, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x.to_le_bytes()[0]
        })
        .collect()
}

/// DOS-время записей: 2024-05-06 07:08:10 (местное).
fn when() -> DateTime {
    DateTime::from_date_and_time(2024, 5, 6, 7, 8, 10).unwrap()
}

fn make_zip(path: &Path, entries: &[E<'_>]) {
    let mut z = ZipWriter::new(fs::File::create(path).unwrap());
    for e in entries {
        let method = if e.stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let o = SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(when());
        match &e.data {
            None => z.add_directory(e.name, o).unwrap(),
            Some(d) => {
                z.start_file(e.name, o).unwrap();
                z.write_all(d).unwrap();
            }
        }
    }
    z.finish().unwrap();
}

fn setup(left: Location, right: Location) -> Core {
    let mut reg = VfsRegistry::with_local();
    let cache = ArcCache::new(64 << 20);
    for b in urfin_arc::backends(&cache, &ArcCfg::default()) {
        reg.register(b);
    }
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let mut core = Core::new(Boot::new(left, right), services, Arc::new(NoWake));
    urfin_vfs::register(&mut core, reg, VfsCfg::default());
    urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
    // Реестр схем (`Schemes`) handler `vfs` кладёт на первом pump.
    core.pump(Duration::from_millis(2));
    core
}

fn pump_until(core: &mut Core, what: &str, done: impl Fn(&Core) -> bool) {
    let t0 = Instant::now();
    while !done(core) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn listed(core: &mut Core, name: &str) {
    pump_until(core, "listed", |c| {
        c.state()
            .active_tab()
            .listing
            .entries
            .iter()
            .any(|e| e.name == name)
    });
}

/// Курсор на `name` в активной панели.
fn cursor(core: &mut Core, name: &str) {
    listed(core, name);
    core.dispatch(Command::new("panel.cursor.to").arg("name", name));
    core.pump(Duration::from_millis(2));
}

fn finished(core: &Core) -> Vec<&JobState> {
    core.state()
        .jobs
        .iter()
        .filter(|j| j.kind == "copy" && !j.state.is_active())
        .map(|j| &j.state)
        .collect()
}

/// Ждёт `n`-ю завершённую задачу копирования; её итог.
fn copy_done(core: &mut Core, n: usize) -> JobSummary {
    let t0 = Instant::now();
    while finished(core).len() < n {
        if t0.elapsed() > Duration::from_secs(30) {
            let ev: Vec<String> = core
                .take_ui_events()
                .iter()
                .map(|e| format!("{e:?}"))
                .collect();
            let jobs: Vec<String> = core
                .state()
                .jobs
                .iter()
                .map(|j| format!("{} {:?}", j.kind, j.state))
                .collect();
            panic!("timeout: copy job ends; events {ev:?}; jobs {jobs:?}");
        }
        core.pump(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(1));
    }
    match finished(core)[n - 1] {
        JobState::Done(s) => s.clone(),
        other => panic!("copy: {other:?}"),
    }
}

/// Отпечаток данных для сравнения деревьев: длина и FNV-1a.
fn print(d: &[u8]) -> (usize, u64) {
    let h = d.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    (d.len(), h)
}

type Tree = BTreeMap<String, Option<(usize, u64)>>;

/// Дерево каталога: относительный путь через `/` → отпечаток данных (каталог — `None`).
fn tree(root: &Path) -> Tree {
    fn walk(root: &Path, at: &Path, out: &mut Tree) {
        for e in fs::read_dir(at).unwrap() {
            let e = e.unwrap();
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().unwrap().is_dir() {
                out.insert(rel, None);
                walk(root, &e.path(), out);
            } else {
                out.insert(rel, Some(print(&fs::read(e.path()).unwrap())));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Что должно получиться из записей под `prefix` (как `7z x`).
fn expected(entries: &[E<'_>], prefix: &str) -> Tree {
    entries
        .iter()
        .filter(|e| e.name.starts_with(prefix))
        .map(|e| {
            let data = e.data.as_deref().map(print);
            (e.name.trim_end_matches('/').to_owned(), data)
        })
        .collect()
}

fn mtime_ns(p: &Path) -> i128 {
    let t = fs::metadata(p).unwrap().modified().unwrap();
    i128::try_from(t.duration_since(UNIX_EPOCH).unwrap().as_nanos()).unwrap()
}

/// Время записи `path` по индексу архива (то же, что в панели).
fn arc_mtime(core: &mut Core, name: &str) -> i128 {
    listed(core, name);
    let e = core
        .state()
        .active_tab()
        .listing
        .entries
        .iter()
        .find(|e| e.name == name)
        .unwrap()
        .mtime;
    i128::from(e)
}

fn sample() -> Vec<E<'static>> {
    vec![
        dir("src/"),
        file("src/main.rs", b"fn main() {}\n".repeat(200)),
        stored("src/raw.bin", noise(1, 300 << 10)),
        dir("src/sub/"),
        file("src/sub/big.dat", noise(2, 3 << 20)),
        file("src/sub/empty.txt", Vec::new()),
        dir("src/sub/void/"),
        file("readme.txt", b"hello".to_vec()),
    ]
}

/// Приёмка S8-27: каталог из zip побайтно как исходные данные, время сохранено, job —
/// без ошибок, `.urfin-part` не остаётся.
#[test]
fn folder_from_zip_matches_source_and_keeps_time() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("a.zip");
    let entries = sample();
    make_zip(&arc, &entries);
    let out = tmp.path().join("out");
    fs::create_dir(&out).unwrap();
    let mut core = setup(root_of("zip", Location::local(&arc)), Location::local(&out));
    cursor(&mut core, "src");
    let want_time = arc_mtime(&mut core, "src");
    let dest = format!("{}\\", out.display());
    core.dispatch(Command::new("ops.copy").arg("dest", dest.as_str()));
    let sum = copy_done(&mut core, 1);
    assert_eq!((sum.items, sum.errors, sum.skipped), (4, 0, 0), "{sum:?}");
    assert_eq!(tree(&out), expected(&entries, "src/"));
    // DOS-время записи — местное; панель и копия совпадают с точностью до секунды.
    assert_eq!(mtime_ns(&out.join("src/main.rs")), want_time);
    assert_eq!(mtime_ns(&out.join("src/sub/big.dat")), want_time);
    assert_eq!(mtime_ns(&out.join("src/sub")), want_time, "folder time");
    let local = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    assert!(want_time < i128::try_from(local.as_nanos()).unwrap());
}

/// Конфликты и пропуск — как у `sftp`: «пропустить» не трогает, «заменить» —
/// восстанавливает испорченный файл.
#[test]
fn conflicts_skip_and_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("a.zip");
    let entries = sample();
    make_zip(&arc, &entries);
    let out = tmp.path().join("out");
    fs::create_dir(&out).unwrap();
    let mut core = setup(root_of("zip", Location::local(&arc)), Location::local(&out));
    cursor(&mut core, "src");
    let dest = format!("{}\\", out.display());
    core.dispatch(Command::new("ops.copy").arg("dest", dest.as_str()));
    copy_done(&mut core, 1);
    let spoilt = out.join("src/main.rs");
    fs::write(&spoilt, b"local edit").unwrap();
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", dest.as_str())
            .arg("conflict", "skip"),
    );
    let sum = copy_done(&mut core, 2);
    assert_eq!((sum.items, sum.skipped, sum.errors), (0, 4, 0), "{sum:?}");
    assert_eq!(fs::read(&spoilt).unwrap(), b"local edit");
    core.dispatch(
        Command::new("ops.copy")
            .arg("dest", dest.as_str())
            .arg("conflict", "overwrite"),
    );
    let sum = copy_done(&mut core, 3);
    assert_eq!((sum.items, sum.errors), (4, 0), "{sum:?}");
    assert_eq!(tree(&out), expected(&entries, "src/"));
}

fn no_write(scheme: &str) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("scheme", scheme.to_owned());
    urfin_i18n::current().fmt("ops-no-write", &a)
}

fn toasts(core: &mut Core) -> Vec<String> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

/// F5 в архив (поле F5 — uri пассивной панели) — отказ до задачи; F6 из архива —
/// черновик копирования.
#[test]
fn archive_is_never_a_target_and_move_drafts_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("a.zip");
    make_zip(&arc, &sample());
    let local = tmp.path().join("l");
    fs::create_dir(&local).unwrap();
    fs::write(local.join("x.txt"), b"x").unwrap();
    let root = root_of("zip", Location::local(&arc));
    let mut core = setup(Location::local(&local), root.clone());
    cursor(&mut core, "x.txt");
    core.take_ui_events();
    let inner = Location {
        path: PathBuf::from("/src"),
        ..root.clone()
    };
    for dest in [root.to_uri(), inner.to_uri()] {
        core.dispatch(Command::new("ops.copy").arg("dest", dest.as_str()));
        core.dispatch(Command::new("ops.move").arg("dest", dest.as_str()));
        core.pump(Duration::from_millis(5));
        assert_eq!(
            toasts(&mut core),
            [no_write("zip"), no_write("zip")],
            "{dest}"
        );
    }
    assert!(
        core.state()
            .jobs
            .iter()
            .all(|j| j.kind != "copy" && j.kind != "move")
    );
    // F6 внутри архива: вместо переноса — черновик копирования.
    core.dispatch(Command::new("panel.focus").arg("side", "right"));
    cursor(&mut core, "readme.txt");
    core.take_ui_events();
    core.dispatch(Command::new("ops.move"));
    core.pump(Duration::from_millis(5));
    let drafts: Vec<String> = core
        .take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Ext { id, args } if id.as_str() == "ops.draft" => {
                args.str("command").map(str::to_owned)
            }
            _ => None,
        })
        .collect();
    assert_eq!(drafts, ["ops.copy"]);
}

/// `arc.extract`: весь архив (`src`) и «без путей» — файлы в одну папку, одноимённый
/// из другой папки — пропуск (остаётся первый).
#[test]
fn whole_archive_and_flat_extract() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("a.zip");
    let mut entries = sample();
    entries.push(dir("other/"));
    entries.push(file("other/readme.txt", b"second".to_vec()));
    make_zip(&arc, &entries);
    let root = root_of("zip", Location::local(&arc));
    let out = tmp.path().join("whole");
    let mut core = setup(Location::local(tmp.path()), Location::local(tmp.path()));
    core.dispatch(
        Command::new("ops.copy")
            .arg("src", root.to_uri().as_str())
            .arg("dest", out.display().to_string().as_str()),
    );
    let sum = copy_done(&mut core, 1);
    assert_eq!((sum.items, sum.errors), (6, 0), "{sum:?}");
    assert_eq!(tree(&out), expected(&entries, ""));
    let flat = tmp.path().join("flat");
    core.dispatch(
        Command::new("ops.copy")
            .arg("src", root.to_uri().as_str())
            .arg("dest", flat.display().to_string().as_str())
            .arg("paths", false),
    );
    let sum = copy_done(&mut core, 2);
    assert_eq!((sum.items, sum.skipped, sum.errors), (5, 1, 0), "{sum:?}");
    let got = tree(&flat);
    let names: Vec<&str> = got.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["big.dat", "empty.txt", "main.rs", "raw.bin", "readme.txt"]
    );
    assert!(got.values().all(Option::is_some), "no folders");
}

/// Zip-slip (S8 §6): `..` и абсолютные пути индекс кладёт внутрь корня, имена, которые
/// Windows не создаст (`CON`, `:`), — ошибка элемента; за пределы цели ничего не пишется.
#[test]
fn zip_slip_stays_inside_target() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("evil.zip");
    let entries = [
        file("../escape.txt", b"1".to_vec()),
        file("a/../../up.txt", b"2".to_vec()),
        file("/abs.txt", b"3".to_vec()),
        file("CON.txt", b"4".to_vec()),
        file("ads.txt:Zone.Identifier", b"5".to_vec()),
        file("ok.txt", b"6".to_vec()),
    ];
    make_zip(&arc, &entries);
    let box_dir = tmp.path().join("box");
    fs::create_dir(&box_dir).unwrap();
    let out = box_dir.join("out");
    let mut core = setup(Location::local(&box_dir), Location::local(&box_dir));
    let root = root_of("zip", Location::local(&arc));
    core.dispatch(
        Command::new("ops.copy")
            .arg("src", root.to_uri().as_str())
            .arg("dest", out.display().to_string().as_str()),
    );
    let sum = copy_done(&mut core, 1);
    assert_eq!(sum.errors, 2, "CON and ADS: {sum:?}");
    let names: Vec<String> = tree(&box_dir).into_keys().collect();
    assert!(
        names.iter().all(|n| n == "out" || n.starts_with("out/")),
        "{names:?}"
    );
    let inside: Vec<String> = tree(&out).into_keys().collect();
    assert!(inside.contains(&"ok.txt".to_owned()), "{inside:?}");
    assert!(inside.contains(&"escape.txt".to_owned()), "{inside:?}");
    assert!(!inside.iter().any(|n| n.starts_with("CON")), "{inside:?}");
}

/// Mark-of-the-Web (S8 §6): `Zone.Identifier` архива — на извлечённых файлах.
#[test]
fn mark_of_the_web_is_propagated() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("web.zip");
    make_zip(&arc, &sample());
    let zone = b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=about:internet\r\n";
    let ads = |p: &Path| PathBuf::from(format!("{}:Zone.Identifier", p.display()));
    if fs::write(ads(&arc), zone).is_err() {
        eprintln!("no NTFS streams in temp: skipped");
        return;
    }
    let out = tmp.path().join("out");
    let mut core = setup(Location::local(tmp.path()), Location::local(tmp.path()));
    let root = root_of("zip", Location::local(&arc));
    core.dispatch(
        Command::new("ops.copy")
            .arg("src", root.to_uri().as_str())
            .arg("dest", out.display().to_string().as_str()),
    );
    let sum = copy_done(&mut core, 1);
    assert_eq!(sum.errors, 0, "{sum:?}");
    for f in [
        "readme.txt",
        "src/main.rs",
        "src/sub/big.dat",
        "src/raw.bin",
    ] {
        assert_eq!(fs::read(ads(&out.join(f))).unwrap(), zone, "{f}");
    }
    // Время — архива, а не записи потока.
    let t = mtime_ns(&out.join("readme.txt"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    assert!(
        t < i128::try_from(now).unwrap() - 3_600_000_000_000,
        "time kept"
    );
}

/// Докачка только при `Caps::SEEK` (S8 §3.5): у stored-записи `part` с совпавшим
/// хвостом дописывается (голова `part` остаётся — видно по данным), у сжатой —
/// пишется заново.
#[test]
fn resume_only_with_seek() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("r.zip");
    let data = noise(7, 3 << 20);
    make_zip(
        &arc,
        &[stored("s.bin", data.clone()), file("d.bin", data.clone())],
    );
    let out = tmp.path().join("out");
    fs::create_dir(&out).unwrap();
    // `part` на 2 МиБ: первый мебибайт чужой, второй — как у источника.
    let mut part = vec![0xAA; 1 << 20];
    part.extend_from_slice(&data[1 << 20..2 << 20]);
    for n in ["s.bin", "d.bin"] {
        fs::write(out.join(format!("{n}.urfin-part")), &part).unwrap();
    }
    let mut core = setup(Location::local(tmp.path()), Location::local(tmp.path()));
    let root = root_of("zip", Location::local(&arc));
    core.dispatch(
        Command::new("ops.copy")
            .arg("src", root.to_uri().as_str())
            .arg("dest", out.display().to_string().as_str()),
    );
    let sum = copy_done(&mut core, 1);
    assert_eq!((sum.items, sum.errors), (2, 0), "{sum:?}");
    let s = fs::read(out.join("s.bin")).unwrap();
    assert_eq!(s.len(), data.len());
    assert_eq!(&s[..1 << 20], &part[..1 << 20], "stored: resumed from part");
    assert_eq!(&s[2 << 20..], &data[2 << 20..]);
    assert_eq!(
        fs::read(out.join("d.bin")).unwrap(),
        data,
        "deflate: rewritten"
    );
    let left: Vec<String> = tree(&out).into_keys().collect();
    assert_eq!(left, ["d.bin", "s.bin"]);
}

/// Сравнение с архивом (S8 §4): zip — DOS-время, допуск 2 с.
#[test]
fn compare_with_zip_uses_two_seconds() {
    let tmp = tempfile::tempdir().unwrap();
    let arc = tmp.path().join("c.zip");
    make_zip(
        &arc,
        &[
            file("same.txt", b"abc".to_vec()),
            file("newer.txt", b"abc".to_vec()),
            file("only.txt", b"abc".to_vec()),
        ],
    );
    let right = tmp.path().join("r");
    fs::create_dir(&right).unwrap();
    let mut core = setup(
        root_of("zip", Location::local(&arc)),
        Location::local(&right),
    );
    let t = arc_mtime(&mut core, "same.txt");
    let at = |ns: i128| UNIX_EPOCH + Duration::from_nanos(u64::try_from(ns).unwrap());
    for (name, shift) in [("same.txt", 1_900_000_000), ("newer.txt", 5_000_000_000)] {
        let p = right.join(name);
        fs::write(&p, b"abc").unwrap();
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(at(t + shift))
            .unwrap();
    }
    pump_until(&mut core, "right listed", |c| {
        c.state()
            .panel(urfin_core::Side::Right)
            .tab()
            .listing
            .entries
            .len()
            >= 2
    });
    core.take_ui_events();
    core.dispatch(Command::new("ops.compare"));
    let t0 = Instant::now();
    let args = loop {
        assert!(t0.elapsed() < Duration::from_secs(20), "no compare result");
        core.pump(Duration::from_millis(1));
        let found = core.take_ui_events().into_iter().find_map(|e| match e {
            UiEvent::Ext { id, args } if id.as_str() == "ops.compare.result" => Some(args),
            _ => None,
        });
        if let Some(a) = found {
            break a;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let n = |k: &str| args.int(k).unwrap_or(-1);
    assert_eq!(
        (
            n("equal"),
            n("newer_right"),
            n("only_left"),
            n("precision_s")
        ),
        (1, 1, 1, 2),
        "{args:?}"
    );
}
