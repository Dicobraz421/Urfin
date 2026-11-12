//! Разбор `keymap.toml` (S9-15c): произвольный файл — пользовательский слой
//! (`extends` вшитых пресетов, `[os.*]`, `when`, аргументы команд). Без паник;
//! поиск, keybar и `keys_for` по каждой привязке; `keymap.dump` читается обратно
//! той же действующей раскладкой. Первый байт — ОС.
#![no_main]

#[path = "common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (os, text) = common::split_os(data);
    common::keymap_toml(os, &text);
});
