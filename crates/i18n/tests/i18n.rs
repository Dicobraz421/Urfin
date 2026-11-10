//! Приёмка S2-01: тексты, плюрали, размеры, fallback, переопределения, глобальная смена языка.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use jiff::tz::{Offset, TimeZone};
use proptest::prelude::*;
use urfin_i18n::fmt::{bytes_exact_with, bytes_with, datetime_with};
use urfin_i18n::{Catalog, FluentArgs, I18n, I18nWarning, LanguageIdentifier};

fn lang(tag: &str) -> LanguageIdentifier {
    tag.parse().unwrap()
}

fn embedded(tag: &str) -> I18n {
    Catalog::embedded().build(&lang(tag)).0
}

fn with_user(
    files: &[(&str, &str)],
    tag: &str,
) -> (I18n, Vec<I18nWarning>, Vec<LanguageIdentifier>) {
    let dir = tempfile::tempdir().unwrap();
    write_files(dir.path(), files);
    let (catalog, mut warnings) = Catalog::load(Some(dir.path()));
    let (i18n, more) = catalog.build(&lang(tag));
    warnings.extend(more);
    (i18n, warnings, catalog.available())
}

fn write_files(config: &Path, files: &[(&str, &str)]) {
    for (rel, text) in files {
        let path = config.join("i18n").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn footer(i18n: &I18n, count: u64, size: u64) -> String {
    let mut args = FluentArgs::new();
    args.set("count", count);
    args.set("bytes", bytes_with(i18n, size));
    i18n.fmt("ui-footer-selected", &args)
}

fn items(i18n: &I18n, count: u64) -> String {
    let mut args = FluentArgs::new();
    args.set("count", count);
    args.set("bytes", "x");
    i18n.fmt("ui-footer-items", &args)
}

#[test]
fn footer_selected() {
    assert_eq!(footer(&embedded("ru"), 3, 50_688), "3 выбрано · 49,5 КБ");
    assert_eq!(footer(&embedded("en"), 3, 50_688), "3 selected · 49.5 KB");
    assert_eq!(footer(&embedded("ru"), 1, 118), "1 выбран · 118 Б");
}

#[test]
fn ru_plurals() {
    let ru = embedded("ru");
    let got: Vec<_> = [1, 2, 5, 21, 11, 22, 112].map(|n| items(&ru, n)).into();
    assert_eq!(
        got,
        [
            "1 объект · x",
            "2 объекта · x",
            "5 объектов · x",
            "21 объект · x",
            "11 объектов · x",
            "22 объекта · x",
            "112 объектов · x",
        ]
    );
    // Группировка разрядов не мешает выбору плюрали.
    assert_eq!(items(&ru, 1_021), "1\u{a0}021 объект · x");
    assert_eq!(items(&embedded("en"), 1), "1 item · x");
    assert_eq!(items(&embedded("en"), 1_000), "1,000 items · x");
}

/// Строка поиска со счётчиками `count`, `files` и готовым временем.
fn search_line(i18n: &I18n, key: &str, count: u64, files: u64) -> String {
    let mut args = FluentArgs::new();
    args.set("count", count);
    args.set("files", files);
    args.set("time", "0,4 с");
    i18n.fmt(key, &args)
}

#[test]
fn ru_search_plurals() {
    let ru = embedded("ru");
    let forms = |key: &str| [1, 2, 5, 21].map(|n| search_line(&ru, key, n, n));
    assert_eq!(
        forms("ui-search-summary"),
        [
            "Найдено 1 совпадение в 1 файле",
            "Найдено 2 совпадения в 2 файлах",
            "Найдено 5 совпадений в 5 файлах",
            "Найдено 21 совпадение в 21 файле",
        ]
    );
    assert_eq!(
        forms("ui-search-scanned"),
        [
            "просмотрено 1 файл · 0,4 с",
            "просмотрено 2 файла · 0,4 с",
            "просмотрено 5 файлов · 0,4 с",
            "просмотрено 21 файл · 0,4 с",
        ]
    );
    assert_eq!(
        forms("ui-search-panel-total"),
        [
            "1 файл · 0,4 с",
            "2 файла · 0,4 с",
            "5 файлов · 0,4 с",
            "21 файл · 0,4 с",
        ]
    );
    assert_eq!(
        forms("ui-search-denied"),
        [
            "нет доступа к 1 папке",
            "нет доступа к 2 папкам",
            "нет доступа к 5 папкам",
            "нет доступа к 21 папке",
        ]
    );
    // Значения макета `screen-search`.
    assert_eq!(
        search_line(&ru, "ui-search-summary", 9, 7),
        "Найдено 9 совпадений в 7 файлах"
    );
    assert_eq!(
        search_line(&ru, "ui-search-scanned", 1_284, 0),
        "просмотрено 1\u{a0}284 файла · 0,4 с"
    );
    let en = embedded("en");
    assert_eq!(
        search_line(&en, "ui-search-summary", 1, 1),
        "Found 1 match in 1 file"
    );
    assert_eq!(
        search_line(&en, "ui-search-summary", 2, 2),
        "Found 2 matches in 2 files"
    );
}

/// Статус-строка `screen-search` собирается из `ui-find-count` и `du-time-sec`.
#[test]
fn search_status_reuses_count_and_time() {
    let ru = embedded("ru");
    let mut args = FluentArgs::new();
    args.set("total", 9);
    assert_eq!(ru.fmt("ui-find-count", &args), "9 совпадений");
    let mut args = FluentArgs::new();
    args.set("sec", 0.4);
    let time = ru.fmt("du-time-sec", &args);
    assert_eq!(time, "0,4 с");
    let mut args = FluentArgs::new();
    args.set("time", time);
    assert_eq!(ru.fmt("ui-search-status-done", &args), "готово за 0,4 с");
}

/// S8 §3: формы числа RU в строках архивов и значения макетов `screen-archive`,
/// `screen-extract`, `screen-extractpw`.
#[test]
fn ru_arc_plurals() {
    let ru = embedded("ru");
    let one = |key: &str, var: &str, n: u64| {
        let mut args = FluentArgs::new();
        args.set(var, n);
        args.set("count", n);
        args.set("name", "a.zip");
        args.set("size", "1 КБ");
        ru.fmt(key, &args)
    };
    let forms = |key: &str, var: &str| [1, 2, 5, 21].map(|n| one(key, var, n));
    assert_eq!(
        forms("arc-pw-wrong", "left"),
        [
            "Неверный пароль — осталась 1 попытка",
            "Неверный пароль — осталось 2 попытки",
            "Неверный пароль — осталось 5 попыток",
            "Неверный пароль — осталась 21 попытка",
        ]
    );
    assert_eq!(
        forms("arc-test-ok", "count"),
        [
            "Проверен 1 файл — ошибок нет",
            "Проверено 2 файла — ошибок нет",
            "Проверено 5 файлов — ошибок нет",
            "Проверен 21 файл — ошибок нет",
        ]
    );
    assert_eq!(
        forms("arc-extract-title-many", "count"),
        [
            "Распаковать 1 архив · 1 КБ в",
            "Распаковать 2 архива · 1 КБ в",
            "Распаковать 5 архивов · 1 КБ в",
            "Распаковать 21 архив · 1 КБ в",
        ]
    );
    assert_eq!(
        forms("arc-extract-title-items", "count")[1],
        "Распаковать 2 объекта из a.zip · 1 КБ в"
    );
    let mut args = FluentArgs::new();
    args.set("errors", 2);
    args.set("count", 9);
    assert_eq!(ru.fmt("arc-test-failed", &args), "Ошибки в 2 файлах из 9");
    // Значения макетов.
    let mut args = FluentArgs::new();
    args.set("name", "backup_1019.7z");
    args.set("size", "1,4 ГБ");
    assert_eq!(
        ru.fmt("arc-extract-title", &args),
        "Распаковать backup_1019.7z · 1,4 ГБ в"
    );
    let mut args = FluentArgs::new();
    args.set("method", "AES-256");
    assert_eq!(
        ru.fmt("arc-pw-names-encrypted", &args),
        "AES-256 · имена зашифрованы"
    );
    let mut args = FluentArgs::new();
    args.set("size", "1,2 КБ");
    args.set("packed", "0,5 КБ");
    assert_eq!(
        ru.fmt("arc-status-packed", &args),
        "1,2 КБ, сжато до 0,5 КБ"
    );
    let mut args = FluentArgs::new();
    args.set("size", "8,9 МБ");
    args.set("pct", 38);
    assert_eq!(ru.fmt("arc-summary", &args), "архив 8,9 МБ · сжатие 38 %");
    let en = embedded("en");
    let mut args = FluentArgs::new();
    args.set("left", 1);
    assert_eq!(
        en.fmt("arc-pw-wrong", &args),
        "Wrong password — 1 attempt left"
    );
    for key in [
        "arc-toast-readonly",
        "arc-error-format",
        "arc-error-corrupt",
    ] {
        assert!(ru.has(key) && en.has(key), "{key}");
    }
}

#[test]
fn bytes_acceptance() {
    let ru = embedded("ru");
    let cases = [
        (118, "118 Б"),
        (48_213, "47,1 КБ"),
        (1_023, "1023 Б"),
        (1_024, "1,0 КБ"),
        (1_229, "1,2 КБ"),
        (3_891, "3,8 КБ"),
        (1_024_000_000, "977 МБ"),
    ];
    for (n, want) in cases {
        assert_eq!(bytes_with(&ru, n), want, "{n}");
    }
    assert_eq!(bytes_with(&embedded("en"), 48_213), "47.1 KB");
    assert_eq!(bytes_exact_with(&ru, 48_213), "48\u{a0}213 байт");
    assert_eq!(bytes_exact_with(&ru, 1), "1 байт");
    assert_eq!(bytes_exact_with(&ru, 3), "3 байта");
    assert_eq!(bytes_exact_with(&embedded("en"), 1), "1 byte");
    assert_eq!(bytes_exact_with(&embedded("en"), 48_213), "48,213 bytes");
}

#[test]
fn datetime_by_language() {
    let utc = TimeZone::fixed(Offset::UTC);
    // 2026-10-14 09:12:00 UTC.
    let ns = 1_791_969_120 * 1_000_000_000;
    assert_eq!(datetime_with(&embedded("ru"), ns, &utc), "14.10.26 09:12");
    assert_eq!(datetime_with(&embedded("en"), ns, &utc), "10/14/26 9:12 AM");
    let pm = ns + 6 * 3_600 * 1_000_000_000;
    assert_eq!(datetime_with(&embedded("en"), pm, &utc), "10/14/26 3:12 PM");
    assert_eq!(datetime_with(&embedded("ru"), i64::MIN, &utc), "");
    // Битый шаблон пользователя (обрыв после `%`) — ISO.
    let (bad, _, _) = with_user(&[("ru/fmt.ftl", "fmt-datetime = %d.%m %\n")], "ru");
    let log = capture_warnings(|| {
        assert_eq!(datetime_with(&bad, ns, &utc), "2026-10-14 09:12");
        assert_eq!(datetime_with(&bad, ns, &utc), "2026-10-14 09:12");
    });
    assert_eq!(log.matches("fmt-datetime").count(), 1, "{log}");
    // Системная зона читается без паники.
    let _ = datetime_with(&embedded("ru"), ns, &TimeZone::system());
}

#[test]
fn missing_key_falls_back_then_marks() {
    let (ru, warnings, _) = with_user(&[("en/extra.ftl", "only-en = Only EN\n")], "ru");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(ru.get("only-en"), "Only EN");
    assert!(ru.has("only-en"));
    assert!(!ru.has("no-such-key"));

    let log = capture_warnings(|| {
        assert_eq!(ru.get("no-such-key"), "⟨no-such-key⟩");
        assert_eq!(ru.get("no-such-key"), "⟨no-such-key⟩");
        assert_eq!(ru.attr("ui-footer-items", "nope"), "⟨ui-footer-items.nope⟩");
    });
    assert_eq!(log.matches("no-such-key").count(), 1, "{log}");
    assert_eq!(ru.missing_keys(), ["no-such-key", "ui-footer-items.nope"]);
}

#[test]
fn attributes() {
    let (ru, warnings, _) = with_user(
        &[(
            "ru/cmd.ftl",
            "cmd-ops-copy = Копировать…\n    .menu-key = К\n",
        )],
        "ru",
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(ru.get("cmd-ops-copy"), "Копировать…");
    assert_eq!(ru.attr("cmd-ops-copy", "menu-key"), "К");
}

#[test]
fn user_override_and_new_language() {
    let (ru, warnings, available) = with_user(
        &[
            (
                "ru/ui.ftl",
                "ui-conflict-head = Свой { $index }/{ $total }\n",
            ),
            (
                "de/ui.ftl",
                "ui-footer-selected = { $count } ausgewählt · { $bytes }\n",
            ),
        ],
        "ru",
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut args = FluentArgs::new();
    args.set("index", 1);
    args.set("total", 3);
    args.set("job", "j");
    assert_eq!(ru.fmt("ui-conflict-head", &args), "Свой 1/3");
    // Остальные вшитые сообщения на месте.
    assert_eq!(footer(&ru, 3, 50_688), "3 выбрано · 49,5 КБ");

    assert!(available.contains(&lang("de")), "{available:?}");
    let (de, _, _) = with_user(
        &[(
            "de/ui.ftl",
            "ui-footer-selected = { $count } ausgewählt · { $bytes }\n",
        )],
        "de",
    );
    // Нет своих единиц — en; разделители — немецкие.
    assert_eq!(footer(&de, 3, 50_688), "3 ausgewählt · 49,5 KB");
    assert_eq!(items(&de, 2_000), "2.000 items · x");
}

#[test]
fn broken_ftl_warns_and_keeps_the_rest() {
    let text = "good = ok\nbad = { $x\nalso-good = fine\n";
    let (ru, warnings, _) = with_user(&[("ru/bad.ftl", text)], "ru");
    let [I18nWarning::Parse { file, line, .. }] = warnings.as_slice() else {
        panic!("{warnings:?}");
    };
    assert!(file.ends_with("bad.ftl"), "{file:?}");
    assert_eq!(*line, 2);
    assert_eq!(ru.get("good"), "ok");
    assert_eq!(ru.get("also-good"), "fine");
    assert_eq!(ru.get("bad"), "⟨bad⟩");
}

#[test]
fn bad_language_dir_and_unknown_language() {
    let (_, warnings, _) = with_user(&[("not a tag!/x.ftl", "a = b\n")], "en");
    assert!(
        matches!(warnings.as_slice(), [I18nWarning::BadLanguage { .. }]),
        "{warnings:?}"
    );
    let catalog = Catalog::embedded();
    assert_eq!(catalog.resolve("ru-RU").0, lang("ru"));
    assert_eq!(catalog.resolve("ru_RU.UTF-8").0, lang("ru"));
    assert_eq!(catalog.resolve("EN").0, lang("en"));
    let (used, warning) = catalog.resolve("xx");
    assert_eq!(used, lang("en"));
    assert!(matches!(warning, Some(I18nWarning::UnknownLanguage { .. })));
    // `auto` берёт язык ОС; без предупреждений, даже если его нет.
    assert!(catalog.resolve("auto").1.is_none());
}

#[test]
fn no_isolation_marks() {
    for tag in ["en", "ru"] {
        let i18n = embedded(tag);
        let mut args = FluentArgs::new();
        for name in ["count", "index", "total"] {
            args.set(name, 7);
        }
        for name in ["bytes", "job", "size"] {
            args.set(name, "v");
        }
        for key in [
            "ui-footer-selected",
            "ui-footer-items",
            "ui-delete-trash",
            "ui-conflict-head",
            "fmt-bytes-exact",
        ] {
            let text = i18n.fmt(key, &args);
            assert!(
                !text.contains(['\u{2068}', '\u{2069}']),
                "{tag} {key}: {text:?}"
            );
            assert!(!text.contains('⟨'), "{tag} {key}: {text:?}");
        }
    }
}

/// Глобальное состояние — в одном тесте (тесты процесса делят его).
#[test]
fn global_switch_is_thread_safe() {
    let dir = tempfile::tempdir().unwrap();
    write_files(dir.path(), &[("ru/extra.ftl", "user-key = свой\n")]);
    let warnings = urfin_i18n::init("ru", Some(dir.path()));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(urfin_i18n::current().lang(), &lang("ru"));
    assert_eq!(urfin_i18n::tr!("user-key"), "свой");
    assert_eq!(
        urfin_i18n::tr!(
            "ui-footer-selected",
            count = 3,
            bytes = urfin_i18n::fmt::bytes(50_688)
        ),
        "3 выбрано · 49,5 КБ"
    );
    assert_eq!(urfin_i18n::fmt::count(12_345), "12\u{a0}345");
    assert!(urfin_i18n::available().contains(&lang("ru")));

    // Смены идут, пока каждый читатель не увидит оба языка (не меньше 200): под нагрузкой
    // поток читателя может не получить квант за фиксированные 200 смен.
    let (ru, en) = (1_u8, 2_u8);
    let start = urfin_i18n::epoch();
    let stop = Arc::new(AtomicBool::new(false));
    let seen: Arc<[AtomicU8; 4]> = Arc::default();
    let readers: Vec<_> = (0..4)
        .map(|k| {
            let (stop, seen) = (Arc::clone(&stop), Arc::clone(&seen));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let text = urfin_i18n::tr!("ui-footer-selected", count = 3, bytes = "b");
                    let bit = match text.as_str() {
                        "3 выбрано · b" => ru,
                        "3 selected · b" => en,
                        _ => panic!("{text:?}"),
                    };
                    seen[k].fetch_or(bit, Ordering::Relaxed);
                }
            })
        })
        .collect();
    // Срок — только против зависания при поломке (под 40 параллельными копиями — ≤ 3 с).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut switches = 0;
    while switches < 200 || seen.iter().any(|s| s.load(Ordering::Relaxed) != ru | en) {
        assert!(
            std::time::Instant::now() < deadline,
            "readers did not observe both languages: {seen:?}"
        );
        for tag in ["en", "ru"] {
            let warnings = urfin_i18n::set_language(tag);
            assert!(warnings.is_empty(), "{warnings:?}");
            switches += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(urfin_i18n::epoch(), start + switches);
    assert_eq!(urfin_i18n::current().lang(), &lang("ru"));
    // Пользовательский каталог сохраняется при смене языка.
    assert_eq!(urfin_i18n::tr!("user-key"), "свой");
}

#[test]
fn snapshot_formats() {
    let sizes = [
        0,
        1,
        999,
        1_023,
        1_024,
        1_536,
        10_239,
        10_240,
        102_400,
        1_023_487,
        1_023_488,
        1_048_576,
        5_368_709_120,
        1 << 40,
        1 << 50,
        u64::MAX,
    ];
    let mut out = String::new();
    for tag in ["en", "ru"] {
        let i18n = embedded(tag);
        writeln!(out, "[{tag}]").unwrap();
        for n in sizes {
            writeln!(
                out,
                "{n:>20}  {:<10}  {}",
                bytes_with(&i18n, n),
                bytes_exact_with(&i18n, n)
            )
            .unwrap();
        }
        for n in [0, 1, 2, 3, 5, 11, 21, 101, 1_000_001] {
            writeln!(out, "{}", items(&i18n, n)).unwrap();
        }
    }
    insta::assert_snapshot!(out.replace('\u{a0}', "\u{2423}"));
}

/// Число и множитель единицы из вывода `bytes` (en).
fn parse_bytes(text: &str) -> (String, f64) {
    let (number, unit) = text.split_once(' ').unwrap();
    let power = ["B", "KB", "MB", "GB", "TB", "PB", "EB"]
        .iter()
        .position(|u| *u == unit)
        .unwrap();
    #[allow(clippy::cast_possible_wrap)]
    let scale = 1024f64.powi(power as i32);
    (number.to_owned(), number.parse::<f64>().unwrap() * scale)
}

static EN: std::sync::LazyLock<I18n> = std::sync::LazyLock::new(|| embedded("en"));

proptest! {
    #[test]
    fn bytes_one_decimal(n in 1024u64..) {
        let text = bytes_with(&EN, n);
        let (number, value) = parse_bytes(&text);
        let digits = number.chars().filter(char::is_ascii_digit).count();
        prop_assert!(digits == 2 || digits == 3, "{}", text);
        let decimals = number.split_once('.').map_or(0, |(_, f)| f.len());
        prop_assert!(decimals <= 1, "{}", text);
        #[allow(clippy::cast_precision_loss)]
        let exact = n as f64;
        prop_assert!((value - exact).abs() / exact <= 0.05, "{} for {}", text, n);
    }

    #[test]
    fn bytes_monotonic(a: u64, b: u64) {
        let (lo, hi) = (a.min(b), a.max(b));
        let (_, lo_value) = parse_bytes(&bytes_with(&EN, lo));
        let (_, hi_value) = parse_bytes(&bytes_with(&EN, hi));
        prop_assert!(lo_value <= hi_value, "{} <= {}", lo, hi);
    }

    #[test]
    fn bytes_monotonic_near(n in 0u64..(1 << 44), step in 1u64..4096) {
        let (_, v0) = parse_bytes(&bytes_with(&EN, n));
        let (_, v1) = parse_bytes(&bytes_with(&EN, n + step));
        prop_assert!(v0 <= v1, "{} -> {}", n, n + step);
    }
}

/// Текст предупреждений `tracing`, выданных в `f` на этом потоке.
fn capture_warnings(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(data)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buf.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}
