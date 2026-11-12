# Урфин — разработка: запуск, стенд, сборки

Перенесено из `CLAUDE.md` (R-02, скилл pets-dev). Читать нужный раздел.

## Стенд

Работа нативно на **Windows 11** владельца (до первой задачи — `docs/HANDOFF-stand-windows.md`). Linux и macOS не собираются и не проверяются (S0-decisions №57).

Проверка лестницей: L0 у агента — в облачной сессии или на стенде (fmt, check/clippy своего крейта, свои новые тесты) → L1 CI (`gate`: fmt, deny, clippy и nextest всего workspace, doc-тесты на Windows; снимки сцен — задача `screens`) → L2 ручная GUI-приёмка владельцем пакетом по бете (`docs/manual-tests/<ID>.md`). Перф-бюджеты меряются на стенде в release (подготовка — «Стенд для замеров»).

## Сборки и проверки

- **CI не дублировать** (решение 2026-11-02): полный `cargo clippy --workspace --all-targets -D warnings`, `cargo nextest run --workspace` и doc-тесты гоняет CI на PR и в очереди.
- У агента: `cargo check`/clippy своего крейта по ходу; nextest только своих новых/изменённых тестов (`-E 'test(...)'`); в конце волны `cargo fmt --all --check`. Где работает агент и откуда брать снимки — раздел «Где что собирается».
- Красный CI — `gh run view <id> --log-failed | tail -40`, одна правка с причиной; `rdeps` локально не гонять.
- На стенде — не больше одного агента со сборкой (остальные — в облаке). `~/.cargo/config.toml`: `incremental = false`, `jobs = 6`, `sccache` ≤ 4 ГБ; у каждого worktree свой `CARGO_TARGET_DIR`.
- Release (fat LTO, ~6 мин) — только для замеров по заданию (перф или размер в карточке); в CI — только main и бета.
- Реестр команд `docs/COMMANDS.md` и `docs/keys.md` генерирует тест: `URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-ui commands_doc` (полоса `docs-gen`).

## Где что собирается

Решение 2026-11-11 (`docs/decisions/2026-11-11-cloud-pipeline.md`): машину владельца сборками не грузим.

- **Облачная сессия** (Ubuntu, 4 ядра / 15 ГБ / 30 ГБ) — задачи без окна. Запускать в окружении **Urfin**: в нём стоят заголовки Windows SDK (проверка 2026-11-11, комментарии в PR #713 и #716):
  - `cargo fmt --all --check` — всё;
  - `cargo check -p <крейт> --target x86_64-pc-windows-msvc` и clippy так же — все 14 крейтов (весь workspace с чистого листа — около 4 минут);
  - nextest под Linux — только `core`, `keymap`, `i18n` (`vfs` и всё, что от него зависит, под Linux не собирается); остальные тесты гоняет CI;
  - в окружении по умолчанию заголовков нет: там не собираются `arc`, `view`, `net`, `ui`, `app` (C-зависимости zstd, unrar, tree-sitter, ring);
  - PR открывается встроенными инструментами GitHub — `gh` в сессии не авторизован.
- **Окружение Urfin** настраивается скриптом `scripts/cloud/env-setup.sh` (xwin → `/opt/xwin`, готовый `cargo-nextest`): его текст вставлен в поле Setup script окружения, адреса и переменные — в шапке скрипта. Поле принимает не больше 100 строк по 200 байт. Поменяли скрипт в репозитории — вставить заново в настройки окружения (claude.ai/code → выбор окружения → значок настроек).
- **Снимки сцен** — задача `screens` в CI на каждом PR (dev-сборка, WARP). Взять: `gh run list --branch <ветка> --workflow ci.yml --limit 1 --json databaseId`, затем `gh run download <id> -n screens -D <каталог>`. Задача вне `gate`; если она красная — смотреть `gh run view <id> --log-failed`, слияние это не держит.
- **Стенд владельца** — замеры в release, ручная проверка (exe — из артефакта `urfin-Windows` сборки main: `gh run download <id> -n urfin-Windows`), задачи с интерфейсом, которым нужно много кругов снимков подряд (`--screens` локально). Одновременно — не больше одного агента со сборкой.
- **Уборка**: после слияния удалить свой `CARGO_TARGET_DIR` и рабочую копию. Размер каталога быстро: `robocopy <каталог> NULL /L /S /NJH /BYTES /NFL /NDL /NC /XJ`.

## Сверка подсветки с tree-sitter

Трекеры кода (ADR-0024 §8) сверяются с полным разбором tree-sitter; в PR и merge queue этого нет: пакетов грамматик в `test (windows)` нет, идут только тесты трекера.

- **Ночью** — `.github/workflows/hl-corpus.yml`, раз в сутки и вручную: собирает DLL грамматик, сверяет код репозитория и крейты cargo registry раннера с границей на каждом кандидате и случайными срезами окна. На PR и очередь не влияет; падение видно в Actions, сводка по языкам — в Summary прогона.
- **Запустить вручную**: `gh workflow run hl-corpus.yml --repo Dicobraz421/Urfin`, смотреть — `gh run list --repo Dicobraz421/Urfin --workflow hl-corpus.yml --limit 1`, причину — `gh run view <id> --repo Dicobraz421/Urfin --log-failed | tail -40`.
- **На стенде**: DLL — как в шаге workflow, в свою папку; затем `HL_PACK=<папка DLL> HL_CORPUS=<папки через ;> cargo test -p urfin-view --lib java_corpus -- --nocapture`. `HL_SHOW` — сколько расхождений показать (12), `HL_MAX` — предел файла в байтах (512 КиБ), `HL_EXT` — другие расширения; один файл — `HL_ONE=<файл> HL_LANG=<язык> cargo test -p urfin-view --lib code_one -- --nocapture`. Случайные тексты — `crates/view/src/syntax/lex/code/samples/gen.py`. HTML — лексер со своей сверкой: `HL_PACK=<папка с html.dll> HL_CORPUS=<папки> cargo test -p urfin-view --lib html_corpus -- --nocapture`, `HL_ERRS=1` — чем лексер отличается от восстановления tree-sitter на файлах с ошибкой.

## Запуск

- Переносной режим: `urfin.portable` рядом с `urfin.exe` — настройки и состояние в `data\` рядом. Тестовые копии — только так, в `%TEMP%`.
- Снимки без человека: `urfin.exe --screens <каталог>` с dev-сборки (`target/debug/urfin.exe`) — сцены демо-дерева в PNG; картинка та же, что у release. Синтетический ввод в окно (SendInput и т. п.) не используем.
- Свой ConPTY (№76): `python ci/conpty.py target/debug` кладёт `conpty.dll` и `OpenConsole.exe` рядом с exe; без них терминал работает на системном ConPTY (`[term] conpty = auto`). Дистрибутив CI собирает с ними.
- Замер: `urfin.exe --bench [каталог]` (датасет `%LOCALAPPDATA%\Pets\testdata\urfin\gen-100000-s1`); `--bench-sftp` — тот же сценарий на панели `sftp://` из памяти (10k строк); `--bench-editor <файл>`, `--bench-viewer <файл>` — кадры, память, открытие; файлы для них — генераторы датасетов (S7-00a): `cargo run -p urfin-testdata --example testdata --release -- v1|v2|v3|v4|all|v5|v6 [--seed N] [--size 10g]`, V1–V4 — в `%LOCALAPPDATA%\Pets\testdata\urfin`, путь печатается — его и передавать (команды — в шапке `crates/testdata/examples/testdata.rs`); `--bench-search [строк]` — кадр экрана поиска на фикстуре (1M строк). Поиск F1–F5 — `cargo run -p urfin-search --example search_bench --release -- gen|run <каталог>` (шапка примера), итоги — `docs/perf/s8.md`. Переменные и профиль фазы (`URFIN_PROF`, `URFIN_PROF_PHASE`) — в шапках `view/edit_bench.rs` и `view/view_bench.rs`, итоги — `docs/perf/s7.md`, `docs/perf/s2.md`.

## Стенд для замеров

Подготовка стенда владельца к перф-прогонам S9 (`docs/specs/S9.md` §3.2, §5.3): тома под датасеты, Defender, питание, обновления, холодный кэш. Только Windows — WSL2, VMware и Docker отменены (S0 №57). Цель — с нуля до готовности к `python scripts/stand/perf.py run all` (S9-04) ≤ 1 ч без учёта генерации датасетов. **(адм.)** — PowerShell от имени администратора, остальное — обычный. Агенты этот раздел не выполняют: шаги меняют систему.

**0. База.** `scripts/stand/setup-windows.ps1` (Build Tools, rustup, nextest, deny); Python 3 — для `perf.py`. Exe не собирать: артефакт сборки main (`gh run download <id> -n urfin-Windows`) распаковать в `C:\Bench\urfin\` и положить рядом `urfin.portable`.

**1. Тома (адм.).** Два динамических VHDX на NVMe: Dev Drive (ReFS; меньше 50 ГБ не создаётся) и NTFS. Отдельный том нужен для холодного кэша (п. 5) и чтобы датасеты не лежали на системном. Буквы `R:` и `N:` — пример.

```powershell
New-Item -ItemType Directory -Force C:\Bench | Out-Null
@'
create vdisk file="C:\Bench\urfin-refs.vhdx" maximum=102400 type=expandable
create vdisk file="C:\Bench\urfin-ntfs.vhdx" maximum=102400 type=expandable
'@ | Set-Content C:\Bench\vdisk.txt -Encoding ascii
diskpart /s C:\Bench\vdisk.txt
function New-BenchVolume($vhd, $letter, $label, [switch]$DevDrive) {
  $disk = Mount-DiskImage -ImagePath $vhd -PassThru | Get-Disk
  Initialize-Disk -Number $disk.Number -PartitionStyle GPT
  $p = New-Partition -DiskNumber $disk.Number -UseMaximumSize -DriveLetter $letter
  if ($DevDrive) { $p | Format-Volume -DevDrive -NewFileSystemLabel $label -Confirm:$false }
  else { $p | Format-Volume -FileSystem NTFS -NewFileSystemLabel $label -Confirm:$false }
}
New-BenchVolume C:\Bench\urfin-refs.vhdx R UrfinRefs -DevDrive
New-BenchVolume C:\Bench\urfin-ntfs.vhdx N UrfinNtfs
fsutil devdrv query R:      # «trusted»; если нет — fsutil devdrv trust R:
Get-CimInstance Win32_Volume -Filter "DriveLetter='R:' OR DriveLetter='N:'" |
  Set-CimInstance -Property @{ IndexingEnabled = $false }   # без индексации Windows Search
```

- После перезагрузки VHDX сами не подключаются: `Mount-DiskImage -ImagePath C:\Bench\urfin-refs.vhdx` (и `-ntfs`), буквы сохраняются.
- Ссылки в датасетах (symlink) без прав администратора создаются только в режиме разработчика: `start ms-settings:developers` → «Режим разработчика». Без него наборы создаются, ссылки уходят в «не создано N».

**2. Датасеты.** Генератор — `crates/testdata/examples/testdata.rs` (S9-01…03, все команды — в шапке). Из корня репо:

```powershell
$gen = { cargo run -p urfin-testdata --example testdata --release -- @args }
& $gen all --root N:\ds               # B1–B6, L1–L4, M1, D1 в N:\ds\<ID> с манифестом
& $gen u1 --root N:\ds                # U1, печатает skipped
foreach ($s in 'l1','l2','l3','l4','b1','b5') { & $gen $s --root R:\ds }   # ReFS: листинг, клон
& $gen u1 --root R:\ds
& $gen all --root N:\ds --verify      # перед серией: дерево совпадает с манифестом
```

- `all` при масштабах по умолчанию — ≈ 70 ГиБ (B1 20, B6 ≈ 25 и M1 ≈ 5 при `--scale` 0,25 по умолчанию, B3 10, B5 5, B2 3). Место проверяется до записи; не хватает — крупные наборы отдельно с `--scale 0.5` (масштаб пишется в манифест).
- Повторный запуск при совпадающем манифесте — no-op; `--force` пересоздаёт; сид не менять (по умолчанию 1), иначе прогоны несравнимы.

**3. Defender.** Режим попадает в отпечаток прогона; защиту целиком не выключать.
- Dev Drive — performance mode (асинхронная проверка), включён у доверенного Dev Drive: `Get-MpPreference | Select-Object PerformanceModeStatus` (0 — включён) или «Безопасность Windows → Защита от вирусов и угроз → Защита Dev Drive».
- NTFS — штатный режим (как у пользователя). Для A/B своих сборок с меньшим шумом — исключение папки **(адм.)**, после серии убрать:

```powershell
Add-MpPreference -ExclusionPath N:\ds
(Get-MpPreference).ExclusionPath
Remove-MpPreference -ExclusionPath N:\ds
```

**4. Питание, обновления, фон.**

```powershell
powercfg -duplicatescheme e9a42b02-d5df-448d-aa00-03f14749eb61   # (адм.) один раз: «Максимальная производительность», печатает GUID
powercfg /setactive <GUID>
powercfg /getactivescheme
(Get-CimInstance Win32_Battery).BatteryStatus                     # 2 — от сети
start ms-settings:windowsupdate        # поставить ожидающие, перезагрузить, «Приостановить на 1 неделю»
1..10 | ForEach-Object {               # фон < 2 % CPU
  (Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'").PercentProcessorTime
  Start-Sleep 1 }
```

- На ноутбуке с Modern Standby схема может не появиться — тогда «Параметры → Система → Питание → Режим питания» на максимум.
- `Get-Counter '\Processor(_Total)\...'` на русской Windows не работает (имена счётчиков локализованы) — поэтому CIM.
- Закрыть браузеры, мессенджеры, сборки агентов; OneDrive — на паузу. Интернет не нужен.

**5. Холодный кэш (адм.).** Данные на VHDX — переподключение тома: сбрасывает и кэш файлов, и метаданные (перед этим закрыть всё, что держит том: Урфин, окна Проводника, терминалы с каталогом на нём). Системный том — RAMMap `-Et` (очистить standby). Затем 10 с простоя.

```powershell
Dismount-DiskImage -ImagePath C:\Bench\urfin-ntfs.vhdx
Mount-DiskImage -ImagePath C:\Bench\urfin-ntfs.vhdx
# RAMMap (Sysinternals) — один раз:
New-Item -ItemType Directory -Force C:\Tools\Sysinternals | Out-Null
Invoke-WebRequest https://download.sysinternals.com/files/RAMMap.zip -OutFile $env:TEMP\RAMMap.zip
Expand-Archive $env:TEMP\RAMMap.zip C:\Tools\Sysinternals -Force
C:\Tools\Sysinternals\RAMMap64.exe -accepteula -Et
```

Прогоны `--cold` — из PowerShell администратора; warm — без прав.

**6. Прогон.** `python scripts/stand/perf.py run <сюита> [--warm|--cold]` (S9-04; сюиты и схема JSON — в шапке скрипта), результаты — `docs/perf/runs/`. A/B двух сборок — `scripts/stand/bench-ab.ps1 -A <a\urfin.exe> -B <b\urfin.exe>`.

**7. После серии.** `powercfg /setactive SCHEME_BALANCED`; возобновить обновления; убрать исключение Defender; `Dismount-DiskImage` обоих VHDX. Датасеты целиком — удалить файлы VHDX (наборы детерминированы, пересоздаются п. 2). Сборки, `%LOCALAPPDATA%\Pets\testdata`, `%TEMP%` — `scripts/stand/clean.ps1 [-Testdata]` и `scripts/stand/clean-temp.ps1` (сначала список, удаление — с `-Apply`).

**Чек-лист владельца (права администратора).** Засечь время от п. 0 до п. 6 без генерации — ≤ 1 ч.
1. Создать VHDX и тома, проверить доверие Dev Drive, выключить индексацию (п. 1).
2. Включить режим разработчика (п. 1).
3. Схема «Максимальная производительность» (п. 4).
4. Поставить обновления и приостановить Windows Update (п. 4).
5. Проверить performance mode Dev Drive; для A/B — исключение `N:\ds` (п. 3).
6. Скачать RAMMap, принять EULA, проверить `-Et` и переподключение VHDX (п. 5).
7. После каждой перезагрузки — `Mount-DiskImage` обоих VHDX; `--cold` — из PowerShell администратора.
8. После серии — вернуть питание, обновления, Defender (п. 7).

## Бюджеты (S0-decisions 2026-10-16, №80)

Первый кадр < 250 мс (своё время старта < 20 мс), кадр p50 ≤ 3 мс, p99 ≤ 6 мс при 100k строк (смена всего экрана — p99 ≤ 8 мс; нажатие в файле в одну строку — p50 ≤ 3,5, p99 ≤ 7 мс; ресайз панелей — p50 ≤ 9, p99 ≤ 12 мс; первый экран вьювера ≤ 50 мс, на файлах от 50 МБ — известное отступление, решение 2026-11-06), 0 % CPU в простое, своя память < 50 МБ (процесс ≤ 200 МБ private), бинарь ≤ 50 МиБ (гейт CI; растёт вместе с функциями, библиотеки ради размера не патчим).

## Контракты и решения

Межкрейтовые контракты (публичные типы core, Command id, Focus, ListingUpdate и т. п.) сам не меняй: предложение — `docs/adr/NNNN-*.md`, PR класса B, вопрос владельцу.
