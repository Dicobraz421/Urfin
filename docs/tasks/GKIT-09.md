# GKIT-09: gpui-kit 0.9.1, pets-крейты и форки — на один коммит pets main

lane: deps
deps: []
class: B (согласовано владельцем в чате, решение 2026-10-26; поручение 2026-11-13 «без разнобоя»)
files: [Cargo.toml, Cargo.lock, crates/ui/src/gpui_shell/]
read: [docs/DEV.md]
checks: [fmt, clippy --target x86_64-pc-windows-msvc, CI Windows, screens:все]
manual: docs/manual-tests/T-gkit-09.md
issue: —

## Сделать
- `gpui-kit` `=0.7.3` → `=0.9.1` (gpui-pre 0.3.9 → 0.3.12), как в Postman (GKIT-02).
- `pets-update`, `pets-shell`, `[patch.crates-io] gpui-pre-windows` — на pets main `0df0fe1` (с pets#80: закрытие окна
  без ложных ERROR). Добавлен `gpui-base` (pets-gpui-base) — он в графе Windows через gpui-kit; форки linux/wgpu/macos
  в графе Windows не участвуют — не подключены.
- Поломки API: `ScrollWheelEvent` → `ScrollEvent`, `on_scroll_wheel` → `on_scroll`; `Window::text_system()` теперь
  `Rc` (было `Arc`) — поле `Measure::sys`; `Keystroke::parse` берёт `&'static str` — тестовые помощники приняли
  `&'static str` (вызовы с литералами).

## Приёмка
- CI Windows зелёный; сцены без расхождений; ручная проверка T-gkit-09 на стенде.
