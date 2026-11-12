//! Сочетания и `when` (S9-15c): вход — `сочетание\0when`. Без паник; разобранное
//! сочетание печатается и читается обратно тем же; `when` из `source` даёт тот же
//! ответ на состояниях.
#![no_main]

#[path = "common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|s: &str| {
    common::chord_when(s);
});
