# T-01 — темы оформления

> **Архив** (2026-10-24, pets-dev R-07): открытые задачи — карточки [docs/tasks/](tasks/README.md); этот файл — история и порядок, не правится.

Решение владельца 2026-10-20 (S0-decisions №59–№61). Палитры и правила — `docs/THEMES.md`, эталон — холст «Урфин — темы оформления» (Claude Design). Делается на GPUI: в рамках G-05 (`docs/TASKS-GPUI.md`, ветка `g/05-…`) или сразу после — отдельным PR `g/05b-themes`. На egui не реализуем. Темы — только Урфина: у других приложений Pets свой дизайн, в `pets` не выносим.

## T-01-1 Модель темы (`ui::theme`, разбор TOML — без gpui)
- `struct Theme { id, name, roles: Roles, syntax: Syntax }`; `Roles` — роли из THEMES.md + error/warning/success; `Syntax` — keyword, string, number, comment (под tree-sitter, №53).
- Формат TOML, `#RRGGBB`. Встроенные 6 тем — `include_str!`, разбор при старте ≤1 мс суммарно (тест).
- Пользовательские темы: `%APPDATA%\Urfin\themes\*.toml`, `extends = "system-light"` (переопределяются только указанные роли).
- Валидация: неизвестная роль/кривой цвет → ошибка с файлом и строкой, тема не грузится, тост warn. Контраст text-ролей и syntax к panel <4,5:1 → warn в лог.
- Тесты: встроенные проходят контраст ≥4,5:1; ни у одной panel не `#FFFFFF`; exec ≠ accent по тону (ΔE > 20).

## T-01-2 Режим «как в ОС» (`ui::sys`)
- Светлая/тёмная: `HKCU\...\Themes\Personalize\AppsUseLightTheme`; акцент: `UISettings::GetColorValue(Accent)`, запасной путь — `DwmGetColorizationColor`.
- `WM_SETTINGCHANGE` (`ImmersiveColorSet`) → событие → смена темы на лету в течение 1 кадра.
- Акцент ОС подменяет `accent` только в system-light/system-dark; `cursor` = смесь акцента с panel (≈18 % светлая, ≈35 % тёмная), контраст cursor_text ≥4,5:1.
- Высокая контрастность (`SPI_GETHIGHCONTRAST`) → тема из `GetSysColor`, перекрывает выбор; в списке тем не показывается.

## T-01-3 Подключение к GPUI
- `Theme` — `gpui::Global`; адаптер в тему gpui-kit (таблица соответствия ролей — в модуле `theme`, doc-комментарием).
- Смена темы → перерисовка окон; в простое 0 % CPU (бенч).
- Плотность строк 20/24/28 px и режим иконок (нет / Segoe Fluent моно / цветные) — настройки `ui.density`, `ui.icons`.

## T-01-4 Команды
`ui.theme.set <id>`, `ui.theme.next`, `ui.theme.follow_os`, `ui.theme.reload` (и автоматически при активации окна, если mtime каталога тем изменился — без файлового вотчера), `ui.density.set`, `ui.icons.set`. Регистрация — по правилам реестра команд (`docs/COMMANDS.md` в том же PR). Имена тем — Fluent EN/RU.

## T-01-5 Политики и индикатор контекста
- `HKLM\Software\Policies\Aahz\Urfin`: `Theme` (REG_SZ), `ThemeLocked` (DWORD). Locked → смена темы недоступна, тост «Тема задана политикой организации».
- Повышенные права (`TokenElevation`) → полоса 3 px сверху окна + метка «Администратор» цветом `context_admin`; удалённая сессия (`SM_REMOTESESSION`) → `context_remote`. Не отключается.

## T-01-6 Запрет литералов цвета
Тест `ui` `no_color_literals`: в `crates/ui/src` (кроме `theme`) запрещены `rgb(0x`, `rgba(`, `hsla(`, `Hsla {`, строки `#[0-9A-Fa-f]{6}`; исключение — комментарий `// colors: allow <причина>` на строке.

## Приёмка
- Тесты T-01-1 и `no_color_literals` зелёные.
- `win-manual`: смена светлой/тёмной и акцента в Windows — Урфин меняется на лету; 6 тем через `ui.theme.next` без рестарта; правка пользовательского TOML + активация окна — применяется.
- В PR: скриншоты 6 тем рядом с холстом; прирост бинаря и памяти (было/стало); 0 % CPU в простое после смены темы.
