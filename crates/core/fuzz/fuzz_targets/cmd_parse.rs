//! `Command::parse`: без паник; разобранная команда печатается и читается обратно
//! той же командой; аргументы-пути разбираются в `Location` без паник.
#![no_main]

use libfuzzer_sys::fuzz_target;
use urfin_core::Command;

fuzz_target!(|s: &str| {
    let Ok(c) = Command::parse(s) else { return };
    let text = c.to_string();
    assert_eq!(Command::parse(&text).as_ref(), Ok(&c), "text = {text:?}");
    for (k, _) in c.args.iter() {
        let _ = c.args.loc(k);
        let _ = c.args.locs(k);
        let _ = c.args.side(k);
    }
});
