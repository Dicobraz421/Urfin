use crate::sys::Zone;
use proptest::prelude::*;
use urfin_core::{Attrs, Entry, EntryKind};

use super::*;

fn cfg(tag: &str) -> FmtCfg {
    let i18n = urfin_i18n::Catalog::embedded()
        .build(&tag.parse().unwrap())
        .0;
    FmtCfg::new(&i18n, false, Zone::utc())
}

fn entry(name: &str, size: u64, attrs: u32, kind: EntryKind) -> Entry {
    Entry {
        ctime: i64::MIN,
        atime: i64::MIN,
        name: name.into(),
        size,
        // 2026-10-14 09:12 UTC.
        mtime: 1_791_969_120 * 1_000_000_000,
        attrs: Attrs(attrs),
        kind,
    }
}

fn file(name: &str, size: u64) -> Entry {
    entry(name, size, Attrs::ARCHIVE, EntryKind::File)
}

fn text(col: ColKind, e: &Entry, c: &FmtCfg) -> String {
    let mut out = Cell::new();
    cell_text(col, e, c, &mut out);
    out.as_str().to_owned()
}

/// Строки мокапов S2 §2.1 и §2.3 (ru); размер — не больше одного знака после запятой
/// (решение владельца 2026-10-17).
#[test]
fn matches_mockups() {
    let c = cfg("ru");
    let rows = [
        (
            entry(
                ".gitignore",
                118,
                Attrs::HIDDEN | Attrs::ARCHIVE,
                EntryKind::File,
            ),
            ["118 Б", "Файл GITIGNORE", "HA"],
        ),
        (file("Cargo.lock", 48_213), ["47,1 КБ", "Файл LOCK", "A"]),
        (file("Cargo.toml", 1_229), ["1,2 КБ", "Файл TOML", "A"]),
        (file("README.md", 3_891), ["3,8 КБ", "Файл MD", "A"]),
        (file("deny.toml", 1_331), ["1,3 КБ", "Файл TOML", "A"]),
        (file("отчёт.docx", 2_097_152), ["2,0 МБ", "Файл DOCX", "A"]),
        (file("LICENSE-APACHE", 10_854), ["10,6 КБ", "Файл", "A"]),
        (file("LICENSE-MIT", 1023), ["1023 Б", "Файл", "A"]),
        (
            entry(
                "setup.exe",
                14_260_000,
                Attrs::READONLY | Attrs::ARCHIVE,
                EntryKind::File,
            ),
            ["13,6 МБ", "Приложение", "RA"],
        ),
        (
            file("backup.7z", 1_024_000_000),
            ["977 МБ", "Архив 7z", "A"],
        ),
        (entry("crates", 0, 0, EntryKind::Dir), ["", "Папка", "D"]),
    ];
    for (e, [size, ty, attrs]) in rows {
        assert_eq!(text(ColKind::Size, &e, &c), size, "{}", e.name);
        assert_eq!(text(ColKind::Type, &e, &c), ty, "{}", e.name);
        assert_eq!(text(ColKind::Attrs, &e, &c), attrs, "{}", e.name);
        assert_eq!(text(ColKind::Mtime, &e, &c), "14.10.26 09:12");
        assert_eq!(text(ColKind::Name, &e, &c), e.name.as_str());
    }
    assert_eq!(text(ColKind::Ext, &file("a.tar.gz", 1), &c), "gz");
    assert_eq!(
        text(ColKind::Ext, &entry("x.d", 0, 0, EntryKind::Dir), &c),
        ""
    );
}

#[test]
fn english_and_iso() {
    let c = cfg("en");
    let e = file("Cargo.lock", 48_213);
    assert_eq!(text(ColKind::Size, &e, &c), "47.1 KB");
    assert_eq!(text(ColKind::Type, &e, &c), "LOCK File");
    assert_eq!(text(ColKind::Mtime, &e, &c), "10/14/26 9:12 AM");
    assert_eq!(text(ColKind::Type, &file("a.zip", 1), &c), "zip Archive");
    let i18n = urfin_i18n::Catalog::embedded()
        .build(&"en".parse().unwrap())
        .0;
    let iso = FmtCfg::new(&i18n, true, Zone::utc());
    assert_eq!(text(ColKind::Mtime, &e, &iso), "2026-10-14 09:12");
}

#[test]
fn unknown_time_and_dir_sizes() {
    let c = cfg("ru");
    let mut e = file("a", 5);
    e.mtime = i64::MIN;
    assert_eq!(text(ColKind::Mtime, &e, &c), "");
    assert_eq!(
        text(ColKind::Ctime, &e, &c),
        "",
        "у схемы нет даты создания"
    );
    // 2026-09-01 10:00 и 2026-10-14 09:12 UTC (S2 §2.3).
    e.ctime = 1_788_256_800 * 1_000_000_000;
    e.atime = 1_791_969_120 * 1_000_000_000;
    assert_eq!(text(ColKind::Ctime, &e, &c), "01.09.26 10:00");
    assert_eq!(text(ColKind::Atime, &e, &c), "14.10.26 09:12");
    let mut d = entry("d", 3 * 1024 * 1024, 0, EntryKind::Dir);
    assert_eq!(text(ColKind::Size, &d, &c), "", "не посчитан — пусто");
    d.attrs = Attrs(Attrs::SIZE_KNOWN);
    assert_eq!(text(ColKind::Size, &d, &c), "3,0 МБ");
    assert_eq!(
        text(ColKind::Type, &entry("l", 0, 0, EntryKind::LinkBroken), &c),
        "Ярлык"
    );
}

#[test]
fn cell_text_truncates_on_char_boundary() {
    let mut t = CellText::<8>::new();
    let _ = t.write_str("абвгд");
    assert!(t.is_truncated());
    assert_eq!(t.as_str(), "аб…");
    t.clear();
    let _ = t.write_str("abc");
    assert_eq!(t.as_str(), "abc");
    assert!(!t.is_truncated());
}

#[test]
fn size_matches_i18n_bytes() {
    let i18n = urfin_i18n::Catalog::embedded()
        .build(&"ru".parse().unwrap())
        .0;
    let c = FmtCfg::new(&i18n, false, Zone::utc());
    for n in [0, 1, 1023, 1024, 48_213, 1 << 30, u64::MAX] {
        assert_eq!(
            text(ColKind::Size, &file("f", n), &c),
            urfin_i18n::fmt::bytes_with(&i18n, n)
        );
    }
}

proptest! {
    #[test]
    fn never_panics_and_fits(
        name in "\\PC{0,200}",
        size: u64,
        mtime: i64,
        attrs: u32,
        kind in 0usize..6,
    ) {
        let kinds = [
            EntryKind::File,
            EntryKind::Dir,
            EntryKind::LinkFile,
            EntryKind::LinkDir,
            EntryKind::LinkBroken,
            EntryKind::Other,
        ];
        let e = Entry { ctime: i64::MIN, atime: i64::MIN, name: name.as_str().into(), size, mtime, attrs: Attrs(attrs), kind: kinds[kind] };
        let c = cfg("ru");
        let mut out = Cell::new();
        for col in [
            ColKind::Name, ColKind::Ext, ColKind::Type, ColKind::Size,
            ColKind::Mtime, ColKind::Ctime, ColKind::Atime, ColKind::Attrs,
        ] {
            cell_text(col, &e, &c, &mut out);
            prop_assert!(out.len() <= 128);
            prop_assert!(std::str::from_utf8(out.as_str().as_bytes()).is_ok());
        }
    }

    #[test]
    fn size_cell_equals_bytes(n: u64) {
        let i18n = urfin_i18n::Catalog::embedded().build(&"en".parse().unwrap()).0;
        let c = FmtCfg::new(&i18n, false, Zone::utc());
        prop_assert_eq!(text(ColKind::Size, &file("f", n), &c), urfin_i18n::fmt::bytes_with(&i18n, n));
    }
}

#[test]
fn pattern_subset_and_fallback() {
    let t = LocalTime {
        year: 2026,
        month: 3,
        day: 7,
        hour: 0,
        minute: 5,
        second: 9,
    };
    let fmt = |p: &str| {
        let mut out = Cell::new();
        write_pattern(&mut out, p, &t).map(|()| out.as_str().to_owned())
    };
    assert_eq!(fmt("%d.%m.%y %H:%M").unwrap(), "07.03.26 00:05");
    assert_eq!(fmt("%m/%d/%y %-I:%M %p").unwrap(), "03/07/26 12:05 AM");
    assert_eq!(fmt("%-d.%-m.%Y %S %%").unwrap(), "7.3.2026 09 %");
    assert!(fmt("%Q").is_err());
    assert!(fmt("%").is_err());
    // Пользовательский шаблон с неизвестным спецификатором — ISO.
    let i18n = urfin_i18n::Catalog::embedded()
        .build(&"en".parse().unwrap())
        .0;
    let mut c = FmtCfg::new(&i18n, false, Zone::utc());
    c.datetime = "%d %Q".into();
    assert_eq!(text(ColKind::Mtime, &file("a", 1), &c), "2026-10-14 09:12");
}

/// Пояс ОС на Windows — без tzdb: дата 1 июля и 1 января сдвинуты на смещение пояса,
/// для UTC совпадают с UTC.
#[test]
fn system_zone_converts() {
    let ns = 1_782_864_000 * 1_000_000_000; // 2026-07-01 00:00 UTC
    let utc = Zone::utc().local(ns).unwrap();
    assert_eq!((utc.year, utc.month, utc.day, utc.hour), (2026, 7, 1, 0));
    assert!(Zone::system().local(ns).is_some());
    // Весь диапазон i64 в наносекундах (1677–2262) — внутри FILETIME.
    assert_eq!(Zone::utc().local(i64::MIN + 1).map(|t| t.year), Some(1677));
    assert_eq!(Zone::utc().local(i64::MAX).map(|t| t.year), Some(2262));
}
