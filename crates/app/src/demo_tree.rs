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

pub fn create() -> io::Result<PathBuf> {
    // `%LOCALAPPDATA%\Temp`, а не `temp_dir()`: тот бывает в 8.3-форме (`A81F7~1.ANT`).
    let base = std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, |d| PathBuf::from(d).join("Temp"));
    let root = base.join(r"urfin-screens\tree");
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
    // Сцена быстрого просмотра (S7-13): Cargo.toml как на `screen-quickview.png`.
    std::fs::write(root.join("Cargo.toml"), CARGO_TOML)?;
    std::fs::write(root.join("Projects/urfin/urfin.exe"), exe())?;
    // Сцена Markdown-документа (S7-22a): README как на `screen-viewer-md.png` — логотип
    // из файла рядом, значок CI из сети (плашка), ссылки, список, таблица, код.
    std::fs::create_dir_all(root.join("Projects/urfin/assets"))?;
    std::fs::write(root.join("Projects/urfin/assets/logo.png"), LOGO)?;
    std::fs::write(root.join("Projects/urfin/README.md"), README)?;
    hidden(&root.join(HIDDEN))?;
    let ro = root.join(READ_ONLY);
    std::fs::write(&ro, b"MIT")?;
    let mut p = std::fs::metadata(&ro)?.permissions();
    p.set_readonly(true);
    std::fs::set_permissions(&ro, p)?;
    Ok(root)
}

/// Cargo.toml сцены `screen-quickview`: 41 строка, LF; первые 26 — как на макете.
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
lto = "fat"              # один модуль: exe меньше, сборка дольше
codegen-units = 1        # вместе с LTO — ещё минус 5 %
panic = "abort"          # без раскрутки стека: таблицы не нужны
strip = "symbols"        # символы — в PDB рядом

[profile.bench]
inherits = "release"     # замеры — на той же сборке, что у людей

[workspace.lints.rust]
unsafe_code = "deny"     # unsafe — только в модулях sys

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
"#;

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

/// Начало журнала — строки макета; дальше — старые версии до 212 строк, «Исправлено» — 5
/// раз (метки поиска). Переводы строк — CRLF.
fn changelog() -> String {
    const HEAD: &str = "# Changelog

## v0.4.2  2026-10-19
- Копирование: CopyFileEx с отменой и прогрессом, UI не блокируется
- Панели: окно строк читается в фоне с упреждением (100k строк — 1,6 мс на кадр)
- Редактор: подсветка в фоне; файлы больше 16 МБ открываются без подсветки
- Исправлено: потеря фокуса панели после закрытия inline-диалога

## v0.4.1  2026-10-12
- Встроенная командная строка: pwsh, cmd и wsl во вкладках
- Повторный запуск открывает путь в уже запущенном окне
- Исправлено: сортировка по дате для каталогов на сетевых дисках

## v0.4.0  2026-10-01
- Табы в заголовке окна, восстановление сессии
- Быстрый поиск по имени в панели — печать без модификаторов
- Ошибки и падения дублируются в журнал Windows «Приложение»

## v0.3.0  2026-09-14
- Две панели, F3/F4/F5/F6/F7/F8, inline-подтверждения без модальных окон

## v0.2.0  2026-08-30
- Ядро: асинхронное чтение каталогов, отмена любой операции
- Исправлено: зависание при обращении к спящему сетевому диску

## v0.1.0  2026-08-12
- Первая сборка: одна панель, навигация, F3
";
    let mut out: Vec<String> = HEAD.lines().map(str::to_owned).collect();
    let mut k = 0u32;
    while out.len() < 212 {
        out.push(String::new());
        out.push(format!("## v0.0.{}  2026-07-{:02}", 60 - k, 28 - k % 28));
        out.push(format!("- Прототип {k}: панели, листинг, клавиши Far"));
        out.push(if k == 9 || k == 27 {
            format!("- Исправлено: падение прототипа {k} на пустом каталоге")
        } else {
            format!("- Сборка {k}: замеры кадра и памяти на стенде")
        });
        k += 1;
    }
    out.truncate(212);
    let mut s = out.join("\r\n");
    s.push_str("\r\n");
    s
}

/// Исполняемый файл 11,7 МБ: заголовок MZ/PE с заглушкой DOS и таблицей секций, дальше
/// нули (сцена смотрит только начало).
fn exe() -> Vec<u8> {
    let mut b = vec![0u8; 12_268_339];
    let mut put = |at: usize, bytes: &[u8]| b[at..at + bytes.len()].copy_from_slice(bytes);
    put(
        0,
        &[
            0x4D, 0x5A, 0x90, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xB8, 0, 0, 0, 0, 0, 0,
            0, 0x40,
        ],
    );
    put(0x3C, &[0, 1, 0, 0]);
    put(
        0x40,
        &[
            0x0E, 0x1F, 0xBA, 0x0E, 0, 0xB4, 0x09, 0xCD, 0x21, 0xB8, 0x01, 0x4C, 0xCD, 0x21,
        ],
    );
    put(0x4E, b"This program cannot be run in DOS mode.\r\r\n$");
    // «Rich»-заголовок компоновщика: детерминированный шум.
    let mut x = 0x9E37_79B9_u32;
    for i in 0x80..0xE8 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        put(i, &[x.to_le_bytes()[0]]);
    }
    put(0xE8, b"Rich");
    put(
        0x100,
        &[
            b'P', b'E', 0, 0, 0x64, 0x86, 0x06, 0, 0xF4, 0xA2, 0x13, 0x67, 0, 0, 0, 0,
        ],
    );
    put(
        0x114,
        &[0xF0, 0, 0x22, 0, 0x0B, 0x02, 0x0E, 0x29, 0, 0x6C, 0x87, 0],
    );
    for (i, name) in [".text", ".rdata", ".data", ".pdata", ".rsrc", ".reloc"]
        .iter()
        .enumerate()
    {
        let at = 0x208 + i * 0x28;
        put(at, name.as_bytes());
        put(
            at + 8,
            &[0x6C, 0x87, u8::try_from(i).unwrap_or(0), 0, 0, 0x10, 0, 0],
        );
        put(at + 0x24, &[0x20, 0, 0, 0x60]);
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
