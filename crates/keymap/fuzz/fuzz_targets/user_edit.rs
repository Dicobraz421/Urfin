//! Пользовательские привязки (S9-15c, `keymap.bind`/`unbind`/`reset`): вход —
//! `файл\0сочетание\0контекст\0команда`, первый байт — ОС. Планы правок по
//! раскладке из файла и `edit::apply` без паник; результат — снова TOML, правка
//! идемпотентна, правленый файл загружается.
#![no_main]

#[path = "common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (os, text) = common::split_os(data);
    common::user_edit(os, &text);
});
