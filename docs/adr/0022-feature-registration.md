# ADR-0022: регистрация фич в их модулях вместо горячих файлов

Статус: **принято** владельцем 2026-10-26 (предложено в R-06, скилл pets-dev, 2026-10-24). Волна полосы `ui` — карточки `docs/tasks/A22-01…04`.

## Разведка (git log с 2026-10-14)

| Файл | Коммитов | Строк | Что повторяется в правках |
|---|---:|---:|---|
| `crates/ui/src/gpui_shell/mod.rs` | 22 | 2428 | поля `CoreHost` под каждую фичу (`welcome_*`, `compare`, `term`, `term_area`, `strip_rows`, …), ветки `UiEvent::Ext { id }` в `take_events`, ветки клавиш, дети `render` (оверлеи, строки, вид терминала) |
| `crates/app/src/main.rs` | 29 | 460 | флаги запуска (`--screens`, `--bench`, `--smoke`, …) и их разбор |
| `crates/ui/src/handler.rs` | 22 | 836 | `CommandMeta` и ветки `match` команд `ui.*` |
| `crates/ui/src/gpui_shell/screens.rs` | 15 | 653 | центральный список сцен |
| `crates/ui/src/state.rs` | 16 | 222 | поля `UiState` под фичи |

Каждая фича правит 3–5 этих файлов — отсюда конфликты при параллельных волнах и чтение больших файлов целиком.

## Предложение

1. **Фича окна — модуль с трейтом.** `trait ShellFeature` в `gpui_shell`: своё состояние, `fn ext(&mut self, id, args, cx) -> Vec<Command>` (свой префикс `UiEvent::Ext`), `fn key(&mut self, …) -> bool`, `fn overlay(&self, s, theme) -> Option<AnyElement>`, `fn reserve(&self, s) -> f32` (высота над cmdline). `CoreHost` держит `Vec<Box<dyn ShellFeature>>`; список фич — по строке на фичу в `gpui_shell/features.rs`. Поля и ветки уходят из `mod.rs` в модули (`term_view`, `jobs`, `compare`, `welcome`, `settings`, …).
2. **Команды `ui.*` — handler на фичу.** Как в core: у модуля свой `&[CommandMeta]` и `handle`; `ui` собирает их списком. `UiState` — ext-состояние фичи (`AppState.ext`, контракт v2 №10), а не поле общей структуры.
3. **Сцены — в модуле фичи.** `pub const SCENES: &[Scene]` у модуля; `screens.rs` только сцепляет списки.
4. **Флаги app — таблица.** `struct Flag { name, takes_value, help }` у владельца флага; `main.rs` разбирает общей функцией.

Новые зависимости не нужны (без `inventory`/`linkme`): списки явные, по строке на фичу — такая правка конфликтует редко и сливается механически.

## Последствия

- Волна `ui` (3–4 PR): трейт и перенос `term_view`, `jobs`; затем остальные фичи; сцены; флаги.
- Межкрейтовые контракты не меняются (всё внутри `urfin-ui` и `urfin-app`).
- Риск — регрессии отрисовки: каждую волну сверять `--screens` с эталонными снимками.
