# ADR

| № | Документ |
|---|---|
| 0001 | [Владение состоянием](0001-state-ownership.md) |
| 0002 | [Модель команд](0002-command-model.md) |
| 0003 | [Потоки и полосы](0003-threads-and-lanes.md) |
| 0004 | [S0-contracts-v2](../archive/S0-contracts-v2.md) |
| 0005 | [S0-decisions](../specs/S0-decisions.md) |
| 0006 | [S0-contracts-v3](../archive/S0-contracts-v3.md) |
| 0007 | [S0-contracts-v4](../archive/S0-contracts-v4.md) |
| 0008 | [S0-contracts-v5](../archive/S0-contracts-v5.md) |
| 0009 | [Строковая форма Location (uri)](0009-location-uri.md) |
| 0010 | [Поле поколения `generation`](0010-gen-field-name.md) |
| 0011 | [UI-фронтенд получает корневой `egui::Ui`](0011-ui-show-root-ui.md) |
| 0012 | [Рендер wgpu (DX12 на Windows) вместо glow](0012-renderer-wgpu.md) |
| 0013 | [Аргумент `ui.draft` — `command`, а не `cmd`](0013-ui-draft-command-arg.md) |
| 0014 | [Ввод пути и меню подкаталогов breadcrumb — через handler `ui`](0014-breadcrumb-io-commands.md) |
| 0015 | [Где действует контекст `global` keymap](0015-global-context.md) |
| 0016 | [Быстрый поиск ведёт keymap (S3), ui рисует](0016-quick-search-owner.md) |
| 0017 | [Финальный порядок большого листинга считает core](0017-core-final-order.md) |
| 0018 | [UI на GPUI вместо egui/eframe/wgpu](0018-ui-gpui.md) |
| 0019 | [Символ без своей клавиши в раскладке — по месту US-QWERTY](0019-layout-symbol-fallback.md) (предложение) |
| 0022 | [Регистрация фич в их модулях вместо горячих файлов](0022-feature-registration.md) (предложение) |
| 0023 | [Собственный движок редактора для файлов любого размера](0023-editor-engine.md) (направление принято, контракт — предложение) |

Новые ADR — со следующего свободного номера: `NNNN-кратко.md`.

До 2026-10-17 продукт назывался FileNav (крейты `filenav-*`); в ADR имя заменено на Urfin при переименовании (S0-decisions №49, R-01).
