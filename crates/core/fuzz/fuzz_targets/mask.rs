//! `Mask::parse` + `matches`: вход — `маска\0имя`. Без паник; `as_str` разбирается
//! в маску с тем же ответом.
#![no_main]

use libfuzzer_sys::fuzz_target;
use urfin_core::Mask;

fuzz_target!(|s: &str| {
    let (mask, name) = s.split_once('\0').unwrap_or((s, "file.txt"));
    let Ok(m) = Mask::parse(mask) else { return };
    let hit = m.matches(name);
    let again = Mask::parse(m.as_str()).expect("as_str parses");
    assert_eq!(again.matches(name), hit, "mask = {mask:?}, name = {name:?}");
});
