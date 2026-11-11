//! Демо-дерево для `--screens`: одно и то же на любой машине (имена, размеры, атрибуты),
//! чтобы снимки сравнивались между прогонами. Каталог — `%TEMP%\urfin-screens\tree`,
//! пересоздаётся при каждом запуске.

use std::io;
use std::path::PathBuf;

const DIRS: [&str; 6] = [
    "Документы",
    "Фото",
    "Projects",
    "Projects/urfin",
    "Projects/postman",
    ".git",
];

/// (имя, размер в байтах).
const FILES: [(&str, usize); 15] = [
    ("Отчёт за квартал.pdf", 284_311),
    ("photo-2026-10-21.jpg", 3_145_728),
    ("archive.zip", 1_048_576),
    ("build.ps1", 2_048),
    ("README.md", 6_311),
    ("notes.txt", 812),
    (
        "Очень длинное имя файла, которое не помещается в колонку и обрезается многоточием.docx",
        41_200,
    ),
    ("setup.exe", 12_582_912),
    ("data.json", 19_500),
    ("Cargo.toml", 1_311),
    ("main.rs", 9_800),
    ("Projects/urfin/Cargo.lock", 120_000),
    ("Projects/urfin/CLAUDE.md", 4_100),
    ("Projects/postman/todo.txt", 300),
    // Тот же файл в обеих панелях — сцена конфликта копирования (S5-07).
    ("Projects/Cargo.toml", 980),
];

/// Скрытый (атрибут Hidden) и только для чтения.
const HIDDEN: &str = "desktop.ini";
const READ_ONLY: &str = "license.txt";

/// Временный каталог: `%LOCALAPPDATA%\Temp`, а не `temp_dir()` — тот бывает в
/// 8.3-форме (`A81F7~1.ANT`).
pub fn base() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, |d| PathBuf::from(d).join("Temp"))
}

pub fn create() -> io::Result<PathBuf> {
    let root = base().join(r"urfin-screens\tree");
    if root.exists() {
        // Read-only мешает удалению: снять и удалить заново.
        let ro = root.join(READ_ONLY);
        if let Ok(m) = std::fs::metadata(&ro) {
            let mut p = m.permissions();
            #[allow(
                clippy::permissions_set_readonly_false,
                reason = "пересоздание демо-дерева"
            )]
            p.set_readonly(false);
            let _ = std::fs::set_permissions(&ro, p);
        }
        std::fs::remove_dir_all(&root)?;
    }
    for d in DIRS {
        std::fs::create_dir_all(root.join(d))?;
    }
    for (name, size) in FILES {
        std::fs::write(root.join(name), vec![b'u'; size])?;
    }
    // Существующий в правой панели — старше (2026-01-01) и меньше: в вопросе
    // «новее, больше».
    let older = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_hours(490_896);
    std::fs::File::options()
        .write(true)
        .open(root.join("Projects/Cargo.toml"))?
        .set_modified(older)?;
    // Файлы сцен вьювера (S7-07): текст как на `screen-viewer.png`, исполняемый — как на
    // `screen-viewerhex.png`.
    std::fs::write(root.join("Projects/urfin/CHANGELOG.md"), changelog())?;
    // Сцена подсветки JSON (S7-21): настоящий JSON того же размера (панели не меняются).
    std::fs::write(root.join("data.json"), data_json())?;
    // Сцена быстрого просмотра (S7-13): Cargo.toml как на `screen-quickview.png`.
    std::fs::write(root.join("Cargo.toml"), CARGO_TOML)?;
    // Сцена редактора (S7-16): тот же Cargo.toml до правок сцены — версия 0.4.1, без
    // строк 17–18 (`screens.edit` вернёт их, как на `screen-editor.png`).
    std::fs::write(root.join("Projects/urfin/Cargo.toml"), editor_toml())?;
    std::fs::write(root.join("Projects/urfin/urfin.exe"), exe())?;
    // Сцена архива (S8-26): мок-архив под видом zip — как на `screen-archive.png`.
    std::fs::write(root.join("Projects/urfin/urfin-0.4.2.zip"), archive())?;
    // Сцена блочного выделения (S7-40a): строки с табами и CJK того же размера.
    std::fs::write(root.join("Projects/postman/todo.txt"), TODO)?;
    // Сцена Markdown-документа (S7-22a): README как на `screen-viewer-md.png` — логотип
    // из файла рядом, значок CI из сети (плашка), ссылки, список, таблица, код.
    std::fs::create_dir_all(root.join("Projects/urfin/assets"))?;
    std::fs::write(root.join("Projects/urfin/assets/logo.png"), LOGO)?;
    std::fs::write(root.join("Projects/urfin/README.md"), README)?;
    // Сцена `screen-editor-md` (S7-24): README с планом до правок сцены.
    std::fs::write(root.join("Projects/urfin/assets/README.md"), README_PLAN)?;
    // Сцена `hl-big-json` (S7-41): JSON на несколько точек подсветки (≈ 300 КБ), окно — в
    // середине многоэкранного объекта; в панелях `assets` не открывается.
    std::fs::write(root.join("Projects/urfin/assets/big.json"), big_json())?;
    dist(&root.join("Документы").join("dist"))?;
    hidden(&root.join(HIDDEN))?;
    let ro = root.join(READ_ONLY);
    std::fs::write(&ro, b"MIT")?;
    let mut p = std::fs::metadata(&ro)?.permissions();
    p.set_readonly(true);
    std::fs::set_permissions(&ro, p)?;
    Ok(root)
}

/// Записи архива сцены `screen-archive` (S8-26): (путь, размер, сжато, изменён — секунды
/// Unix: время макета по Москве). Видны строки макета; файлы в `crates` и `docs` дают «сжатие 38 %».
const ARCHIVE: [(&str, u64, u64, u64); 10] = [
    (
        "urfin-0.4.2/crates/ui.rs",
        9_000_000,
        1_800_000,
        1_792_438_860,
    ),
    (
        "urfin-0.4.2/crates/core.rs",
        8_000_000,
        1_600_000,
        1_792_433_400,
    ),
    (
        "urfin-0.4.2/docs/ARCH.md",
        2_812_133,
        562_427,
        1_792_483_320,
    ),
    (
        "urfin-0.4.2/docs/ROADMAP.md",
        2_000_000,
        400_000,
        1_792_480_500,
    ),
    ("urfin-0.4.2/Cargo.lock", 59_700, 12_390, 1_792_493_400),
    ("urfin-0.4.2/Cargo.toml", 1_229, 512, 1_792_438_800),
    ("urfin-0.4.2/LICENSE-APACHE", 11_366, 3_994, 1_788_253_200),
    ("urfin-0.4.2/LICENSE-MIT", 1_050, 614, 1_788_253_200),
    ("urfin-0.4.2/README.md", 4_915, 2_048, 1_792_261_980),
    (
        "urfin-0.4.2/urfin.exe",
        12_268_339,
        8_598_323,
        1_792_493_700,
    ),
];

/// Размер файла архива сцены: «архив 8,9 МБ».
const ARCHIVE_SIZE: usize = 9_332_326;

/// Мок-архив сцены (`urfin_arc::mock`): оглавление текстом, дополненное строкой пробелов
/// до [`ARCHIVE_SIZE`] (мок её пропускает).
fn archive() -> Vec<u8> {
    use std::fmt::Write as _;
    let mut s = format!("{}\n", urfin_arc::mock::MAGIC);
    for (path, size, packed, t) in ARCHIVE {
        let ns = u128::from(t) * 1_000_000_000;
        let _ = writeln!(s, "f {size} {packed} {ns} {path}");
    }
    let mut v = s.into_bytes();
    v.resize(ARCHIVE_SIZE.max(v.len()), b' ');
    v
}

/// Сборка сайта сцен SFTP (`screen-sftpcopy`, `screen-sftpsync`, F03-07): (имя, размер,
/// изменён — секунды «по Москве», как строки backend'а сцен `sftp`).
const DIST: [(&str, usize, u64); 7] = [
    ("404.html", 1_638, 1_788_358_800),
    ("app.4f2a91c.css", 88_064, 1_792_753_320),
    ("app.4f2a91c.js", 1_258_291, 1_792_753_320),
    ("favicon.ico", 15_086, 1_788_358_800),
    ("index.html", 2_150, 1_792_753_320),
    ("manifest.webmanifest", 612, 1_788_358_800),
    ("robots.txt", 68, 1_788_358_800),
];

/// Папка `dist` сцен SFTP: подпапки и файлы с датами макета (стенд — UTC+3).
fn dist(dir: &std::path::Path) -> io::Result<()> {
    const MSK: u64 = 3 * 3600;
    for d in ["assets", "fonts", "img"] {
        std::fs::create_dir_all(dir.join(d))?;
    }
    for (name, size, t) in DIST {
        let p = dir.join(name);
        std::fs::write(&p, vec![b'w'; size])?;
        std::fs::File::options()
            .write(true)
            .open(&p)?
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t - MSK))?;
    }
    Ok(())
}

/// Большой JSON сцены `hl-big-json`: 2500 записей по 8 строк после 3 строк шапки, 20 005
/// строк (строка 11 254 — запись 1406, поле `name`).
fn big_json() -> String {
    use std::fmt::Write as _;
    let mut s = String::from("{\n  \"name\": \"urfin-big\",\n  \"items\": [\n");
    for i in 0..2500 {
        let sep = if i + 1 < 2500 { "," } else { "" };
        let _ = writeln!(
            s,
            "    {{\n      \"id\": {i},\n      \"name\": \"item-{i}\",\n      \"tags\": [\"a\", \"b\"],\n      \"price\": {}.5,\n      \"active\": {},\n      \"note\": null\n    }}{sep}",
            i % 97,
            i % 2 == 0
        );
    }
    s.push_str("  ]\n}\n");
    s
}

/// JSON сцены `viewer-json` (S7-21), дополненный пробелами до размера в `FILES`.
fn data_json() -> Vec<u8> {
    let mut v = br#"{
  "name": "urfin",
  "version": "0.4.2",
  "private": true,
  "panels": { "left": "C:\\Dev\\urfin", "right": "D:\\backup" },
  "density": 24,
  "theme": "system",
  "icons": "color",
  "keymap": ["far", "dn"],
  "viewer": { "wrap": "off", "tab_size": 8, "markdown": "document", "mmap": true },
  "editor": { "tab_size": 4, "expand_tabs": true, "highlight": true, "autosave": null },
  "recent": [
    { "path": "C:\\Dev\\urfin\\Cargo.toml", "line": 12, "pos": 0.25 },
    { "path": "C:\\Dev\\urfin\\README.md", "line": 1, "pos": 0 }
  ]
}
"#
    .to_vec();
    let size = FILES
        .iter()
        .find(|(n, _)| *n == "data.json")
        .map_or(v.len(), |(_, s)| *s);
    v.resize(size.max(v.len()), b' ');
    v
}

/// Cargo.toml сцен `screen-quickview` и `screen-editor`: 41 строка, LF, ≈1,2 КБ; первые
/// 30 — как на макетах.
const CARGO_TOML: &str = r#"[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.4.2"
edition = "2024"
license = "MIT OR Apache-2.0"
publisher = "Aahz"

[workspace.dependencies]
pets-ui = { git = "https://github.com/Dicobraz/pets", rev = "a41c9e0" }
pets-commands = { git = "https://github.com/Dicobraz/pets", rev = "a41c9e0" }
rayon = "1.10"
thiserror = "2"
tracing = "0.1"
parking_lot = "0.12"
smallvec = "1.13"
windows = { version = "0.58", features = ["Win32_Storage_FileSystem"] }
# UI-поток не делает I/O

[profile.dev]
debug = "line-tables-only"

[profile.dev.package."*"]
debug = false

[profile.release]
lto = "thin"
codegen-units = 1
strip = "symbols"        # символы — в PDB рядом: для отчётов о сбоях
panic = "abort"          # без раскрутки стека: таблицы не нужны, exe меньше

[profile.bench]
inherits = "release"     # замеры — на той же сборке, что у людей, с LTO

[workspace.lints.rust]
unsafe_code = "deny"     # unsafe — только в модулях sys, с пояснением SAFETY

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
"#;

/// Строки 31–41 Cargo.toml сцены `screen-editor` — как на мини-карте макета.
const EDITOR_TAIL: &str = r#"strip = "symbols"
panic = "abort"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
module_name_repetitions = "allow"
missing_errors_doc = "allow"

[workspace.metadata.urfin]
winget = "Aahz.Urfin"
msi = true
"#;

/// Cargo.toml сцены `screen-editor` до правок: 39 строк.
fn editor_toml() -> String {
    let head: String = CARGO_TOML.split_inclusive('\n').take(30).collect();
    (head + EDITOR_TAIL)
        .replacen("version = \"0.4.2\"", "version = \"0.4.1\"", 1)
        .replacen("parking_lot = \"0.12\"\nsmallvec = \"1.13\"\n", "", 1)
}

/// todo.txt сцены `editor-block` (S7-40a): табы, кириллица и CJK под блоком; ровно 300
/// байт — как в списке `FILES` (панели и анализ места не меняются).
const TODO: &str = "Задачи на неделю\n\tпн\tПочта и 日本語-письма\n\tвт\tОтчёт\t\tв 12:00 (!)\n\
\tср\t会議 с командой\n\tчт\tРевью PR #258\n\tпт\tРелиз 0.4.2\n\tсб\tБэкап NAS-1 и 写真\n\
\tвс\tотдых\n\nБлок: Alt+Shift+стрелки или Alt+мышь, Ctrl+C\n";

const _: () = assert!(TODO.len() == 300);

/// Логотип README — иконка приложения.
static LOGO: &[u8] = include_bytes!("../../../assets/icon/urfin-256.png");

/// README сцены `screen-viewer-md`: 40 строк, LF.
const README: &str = r#"# <img src="assets/logo.png" width="48" alt=""> Урфин

[![CI](https://github.com/Dicobraz421/Urfin/actions/workflows/ci.yml/badge.svg)](https://github.com/Dicobraz421/Urfin/actions)

Двухпанельный файловый менеджер для Windows в духе **Far** и **DN**: быстрый, *офлайн*, с клавиатуры. Архитектура — [docs/00-ARCHITECTURE.md](docs/00-ARCHITECTURE.md), решения — [docs/decisions](docs/decisions).

## Возможности

- **Две панели** с вкладками, быстрый просмотр и поиск по содержимому
- Копирование через `CopyFileEx` с отменой и прогрессом
- Встроенный терминал: pwsh, cmd и wsl во вкладках
- ~~Плагины на Lua~~ — после версии 1.0

## Клавиши

| Клавиша | Действие | Far | DN |
|---|---|---|---|
| F3 | Просмотр | ✓ | ✓ |
| Ctrl+Q | Быстрый просмотр | ✓ | — |

## Сборка

```powershell
cargo build --release
.\target\release\urfin.exe
```

Нужны Rust 1.85+ и MSVC (Build Tools 2022); сеть при сборке — только для crates.io, сам Урфин в сеть не ходит: ни обновлений, ни телеметрии.

<details>
<summary>Сборка без сети</summary>

Сначала `cargo vendor` на машине с сетью, затем сборка с <kbd>--offline</kbd>.
</details>

## Лицензия

MIT или Apache-2.0 — на выбор[^1].

[^1]: Как у Rust и большинства крейтов: можно взять любую из двух лицензий, условия — в файлах LICENSE-MIT и LICENSE-APACHE.
"#;

/// README сцены `screen-editor-md` до правок (`screens.edit kind=md`): 39 строк, LF;
/// правки вернут жирный в строке 35 и строку 36, как на макете.
const README_PLAN: &str = r"# Урфин

Двухпанельный файловый менеджер для Windows в духе **Far** и **DN**.

## Возможности

- **Две панели** с вкладками
- Быстрый просмотр и поиск по содержимому
- Копирование с отменой и прогрессом
- Архивы как папки
- Просмотр файлов до 100 ГБ
- Встроенный терминал: pwsh, cmd и wsl во вкладках
- ~~Плагины на Lua~~ — после версии 1.0

## Клавиши

| Клавиша | Действие         | Far | DN |
|---------|------------------|-----|----|
| F3      | Просмотр         | ✓   | ✓  |
| Ctrl+Q  | Быстрый просмотр | ✓   | —  |

## Сборка

```powershell
cargo build --release
.\target\release\urfin.exe
```

> **Офлайн.** Урфин не ходит в сеть и не отправляет телеметрию.

## План M4

- [x] Вьювер: файлы до 100 ГБ без задержек
- [x] Кодировки, hex и слежение за логом
- [ ] Markdown как документ — [решение](docs/decisions/)

---

Лицензия — MIT или Apache-2.0.
";

/// Начало журнала — строки макета; дальше — старые версии до 212 строк, «Исправлено» — 5
/// раз (метки поиска). Переводы строк — CRLF.
fn changelog() -> String {
    const HEAD: &str = "# Changelog

## 0.4.2 — 2026-10-19
- Копирование: CopyFileEx с отменой и прогрессом, UI не блокируется
- Панели: окно строк читается в фоне с упреждением (100k строк — 1,6 мс на кадр)
- Редактор: подсветка в фоне; файлы больше 16 МБ открываются без подсветки
- Исправлено: потеря фокуса панели после закрытия inline-диалога

## 0.4.1 — 2026-10-12
- Встроенная командная строка: pwsh, cmd и wsl во вкладках
- Повторный запуск открывает путь в уже запущенном окне
- Исправлено: сортировка по дате для каталогов на сетевых дисках

## 0.4.0 — 2026-10-01
- Табы в заголовке окна, восстановление сессии
- Быстрый поиск по имени в панели — печать без модификаторов
- Ошибки и падения дублируются в журнал Windows «Приложение»

## 0.3.0 — 2026-09-14
- Две панели, F3/F4/F5/F6/F7/F8, inline-подтверждения без модальных окон

## 0.2.0 — 2026-08-30
- Ядро: асинхронное чтение каталогов, отмена любой операции
- Исправлено: зависание при обращении к спящему сетевому диску

## 0.1.0 — 2026-08-12
- Первая сборка: одна панель, навигация, F3
";
    let mut out: Vec<String> = HEAD.lines().map(str::to_owned).collect();
    let mut k = 0u32;
    while out.len() < 212 {
        out.push(String::new());
        out.push(format!("## 0.0.{} — 2026-07-{:02}", 60 - k, 28 - k % 28));
        out.push(format!("- Прототип {k}: панели, листинг, клавиши Far"));
        out.push(if k == 9 || k == 27 {
            format!("- Исправлено: падение прототипа {k} на пустом каталоге")
        } else {
            format!("- Сборка {k}: замеры кадра и памяти на ПК")
        });
        k += 1;
    }
    out.truncate(212);
    let mut s = out.join("\r\n");
    s.push_str("\r\n");
    s
}

/// Начало исполняемого файла — байты `screen-viewerhex.png` (`vwBytes` макета): заголовок
/// MZ/PE, заглушка DOS, «Rich», таблица секций.
const EXE_HEAD: &str = "\
    4D5A90000300000004000000FFFF0000B8000000000000004000000000000000\
    0000000000000000000000000000000000000000000000000000000000010000\
    0E1FBA0E00B409CD21B8014CCD21546869732070726F6772616D2063616E6E6F\
    742062652072756E20696E20444F53206D6F64652E0D0D0A2400000000000000\
    651E52C9217F3C9A217F3C9A217F3C9AA54DCA182530BB1D6D132CDED6237B2E\
    D91E3F721FCB1971174494D6493C9D5C3460BE31201E69FEDAA0EEE8B9997F5C\
    7C2999FDAFE593253CD654AF4DFAD71427A0AEB3FEE9232F52696368217F3C9A\
    0000000000000000000000000000000000000000000000000000000000000000\
    5045000064860600F4A213670000000000000000F00022000B020E29006C8700\
    00D02D0000000000301A7E000010000000000040010000000010000000020000\
    0600000000000000060000000000000000B0B100000400000000000002006081\
    0000100000000000001000000000000000001000000000000010000000000000\
    00000000100000000000000000000000C8E1A8001801000000F0AE00A8F80100\
    0000AA00F4E20400000000000000000000F0B000C4B10000E0F2A40054000000\
    00000000000000000000000000000000A0F3A4002800000060F1A40040010000\
    000000000000000000808700D809000000000000000000000000000000000000\
    00000000000000002E746578740000003C6A870000100000006C870000040000\
    000000000000000000000000200000602E7264617461000018CE2D0000808700\
    00D02D0000708700000000000000000000000000400000402E64617461000000\
    D0A300000050B500002000000040B500000000000000000000000000400000C0\
    2E70646174610000F4E204000000B60000E404000060B5000000000000000000\
    00000000400000402E72737263000000A8F8010000F0BA0000FA01000044BA00\
    000000000000000000000000400000402E72656C6F630000C4B1000000F0BC00\
    00B20000003EBC00000000000000000000000000400000420000000000000000\
    0000000000000000000000000000000000000000000000000000000000000000\
    0000000000000000000000000000000000000000000000000000000000000000\
    0000000000000000000000000000000000000000000000000000000000000000\
";

/// Исполняемый файл 11,7 МБ: начало как на макете, дальше нули (сцена смотрит только его).
fn exe() -> Vec<u8> {
    let mut b = vec![0u8; 12_268_339];
    let hex = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
            .unwrap_or(0)
    };
    for (at, [hi, lo]) in EXE_HEAD.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        b[at] = hex(*hi) << 4 | hex(*lo);
    }
    b
}

#[cfg(windows)]
fn hidden(path: &std::path::Path) -> io::Result<()> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    /// `FILE_ATTRIBUTE_HIDDEN`.
    const HIDDEN_ATTR: u32 = 0x2;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .attributes(HIDDEN_ATTR)
        .open(path)?
        .write_all(b"[.ShellClassInfo]")
}

#[cfg(not(windows))]
fn hidden(path: &std::path::Path) -> io::Result<()> {
    std::fs::write(path, b"[.ShellClassInfo]")
}
