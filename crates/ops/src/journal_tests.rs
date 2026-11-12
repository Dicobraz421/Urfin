//! Журнал: формат, битый хвост, живые журналы при сканировании (приёмка S5-12).

use compact_str::CompactString;
use urfin_core::Location;

use super::*;
use crate::api::{OpKind, OpOptions};

fn header(dest: &Path) -> Header {
    Header {
        v: VERSION,
        req: OpRequest {
            kind: OpKind::Copy,
            base: Location::local(r"C:\src"),
            items: vec![CompactString::from("a"), CompactString::from("файл.txt")],
            dest: None,
            opts: OpOptions::default(),
        },
        dest: dest.to_path_buf(),
        dest_raw: dest.display().to_string(),
        started: 1_700_000_000,
        pid: 42,
    }
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("urfin-").tempdir().unwrap()
}

#[test]
fn kept_journal_is_interrupted_with_open_temps() {
    let d = tmp();
    let j = Journal::create(d.path(), header(Path::new(r"D:\backup"))).unwrap();
    j.append(&Record::Root(PathBuf::from(r"D:\backup")));
    j.append_now(&Record::Temp(PathBuf::from(r"D:\backup\.a.fnv~1")));
    j.append_now(&Record::Temp(PathBuf::from(r"D:\backup\.b.fnv~1")));
    j.append(&Record::TempDone(PathBuf::from(r"D:\backup\.a.fnv~1")));
    j.append(&Record::Counters {
        items: 3,
        bytes: 63,
        items_total: 10,
        bytes_total: 100,
    });
    let path = j.path().to_path_buf();
    j.keep();
    let found = scan(d.path());
    assert_eq!(found.len(), 1);
    let i = &found[0];
    assert_eq!(i.path, path);
    assert_eq!(i.header, header(Path::new(r"D:\backup")));
    assert_eq!(i.temps, [PathBuf::from(r"D:\backup\.b.fnv~1")]);
    assert_eq!(i.percent(), Some(63));
    assert!(
        path.exists(),
        "interrupted journal stays for Continue/Clear"
    );
}

/// Приёмка S5-12: битый хвост (обрыв посреди записи, мусор) не мешает.
#[test]
fn broken_tail_is_ignored() {
    let d = tmp();
    let j = Journal::create(d.path(), header(Path::new(r"D:\b"))).unwrap();
    j.append_now(&Record::Temp(PathBuf::from(r"D:\b\.x.fnv~1")));
    let path = j.path().to_path_buf();
    j.keep();
    let good = std::fs::read(&path).unwrap();
    for tail in [
        &[0x10, 0, 0, 0, 1, 2][..],                // длина без тела
        &[3, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3][..],    // неверная сумма
        &[0xff, 0xff, 0xff, 0x7f, 0, 0, 0, 0][..], // огромная длина
    ] {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(tail).unwrap();
        drop(f);
        let found = scan(d.path());
        assert_eq!(found.len(), 1, "tail {tail:?}");
        assert_eq!(found[0].temps, [PathBuf::from(r"D:\b\.x.fnv~1")]);
        std::fs::write(&path, &good).unwrap();
    }
    // Обрыв посреди заголовка — продолжать нечего, журнал удаляется.
    std::fs::write(&path, &good[..good.len() / 3]).unwrap();
    assert_eq!(scan(d.path()).len(), 0);
    assert!(!path.exists());
}

/// Приёмка S5-12: живой журнал (job идёт — здесь или во втором экземпляре)
/// сканирование не трогает; завершённый — удаляется.
#[test]
fn live_journals_are_untouched_done_are_removed() {
    let d = tmp();
    let live = Journal::create(d.path(), header(Path::new(r"D:\live"))).unwrap();
    let done = Journal::create(d.path(), header(Path::new(r"D:\done"))).unwrap();
    let done_path = done.path().to_path_buf();
    // Как после падения между `Done` и удалением: запись есть, файл остался.
    done.append(&Record::Done { items: 1, bytes: 2 });
    done.keep();
    assert_eq!(scan(d.path()).len(), 0, "live skipped, done removed");
    assert!(live.path().exists());
    assert!(!done_path.exists());
    live.finish(1, 1);
    assert!(!live.path().exists(), "finish removes the journal");
}

/// S5-27: временный файл с докачкой «Продолжить» оставляет, «Очистить» удаляет.
#[test]
fn resumable_temps_survive_continue() {
    let d = tmp();
    let jdir = d.path().join("jobs");
    let (big, small) = (d.path().join(".big.fnv~t1"), d.path().join(".s.fnv~t1"));
    std::fs::write(&big, b"partial").unwrap();
    std::fs::write(&small, b"x").unwrap();
    let j = Journal::create(&jdir, header(d.path())).unwrap();
    j.append_now(&Record::Temp(big.clone()));
    j.append_now(&Record::Resumable(big.clone()));
    j.append_now(&Record::Temp(small.clone()));
    j.keep();
    let found = scan(&jdir);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].resumable, vec![big.clone()]);
    found[0].remove_temps(true);
    assert!(big.exists() && !small.exists());
    found[0].remove_temps(false);
    assert!(!big.exists());
}

/// План группового переименования (S5-29): запись и чтение, битый файл — «нет плана»,
/// сканирование журналов его не трогает.
#[test]
fn rename_log_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    assert!(load_rename(d.path()).is_none());
    let log = RenameLog {
        v: VERSION,
        base: urfin_core::Location::local(d.path().join("photos")),
        rows: vec![
            ("a.jpg".into(), "b.jpg".into()),
            ("b.jpg".into(), "a.jpg".into()),
        ],
    };
    save_rename(d.path(), &log).unwrap();
    assert_eq!(load_rename(d.path()), Some(log.clone()));
    assert_eq!(scan(d.path()).len(), 0);
    assert_eq!(load_rename(d.path()), Some(log.clone()), "scan keeps it");
    // Замена прежнего плана новым.
    let next = RenameLog {
        rows: vec![("x".into(), "y".into())],
        ..log
    };
    save_rename(d.path(), &next).unwrap();
    assert_eq!(load_rename(d.path()), Some(next));
    assert_eq!(
        std::fs::read_dir(d.path()).unwrap().count(),
        1,
        "no temp left"
    );
    let file = d.path().join(RENAME_FILE);
    let mut bytes = std::fs::read(&file).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&file, &bytes).unwrap();
    assert!(load_rename(d.path()).is_none(), "bad checksum");
    clear_rename(d.path());
    assert!(!file.exists());
    clear_rename(d.path());
}
