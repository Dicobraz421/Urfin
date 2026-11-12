//! Крючок кадра для профилировщика (S9-10): app в сборке `profile` ставит функцию,
//! `CoreHost::render` зовёт её раз на кадр (Tracy: граница кадра, графики). Без крючка —
//! одна проверка `OnceLock` на кадр.

use std::sync::OnceLock;

static HOOK: OnceLock<fn()> = OnceLock::new();

/// Поставить крючок; повторный вызов ничего не меняет.
pub fn set(f: fn()) {
    let _ = HOOK.set(f);
}

/// Кадр начат.
pub(crate) fn frame() {
    if let Some(f) = HOOK.get() {
        f();
    }
}
