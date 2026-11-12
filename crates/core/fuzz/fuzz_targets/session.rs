//! `Session::load` (разбор с миграциями) и `Session::apply`: `session.toml` из
//! входа во временном каталоге. Битый файл — `None` без паник; разобранная
//! сессия сохраняется/читается обратно и применяется к состоянию без паник.
#![no_main]

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use urfin_core::{AppState, Location, Panel, Paths, Session, Tab, TabId};

fn dir() -> &'static PathBuf {
    static D: OnceLock<PathBuf> = OnceLock::new();
    D.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("urfin-fuzz-session-{}", std::process::id()));
        Paths::in_dir(&d).ensure().expect("temp dir");
        d
    })
}

fuzz_target!(|data: &[u8]| {
    let paths = Paths::in_dir(dir());
    fs::write(paths.session_file(), data).expect("write session.toml");
    let Some(s) = Session::load(&paths) else {
        return;
    };
    // Текст `save` читается обратно той же сессией. Сравнение текстом: `NaN` в
    // геометрии окна не равен себе. В памяти — без fsync `save` на каждый вход.
    if let Ok(text) = toml::to_string(&s) {
        let back: Session = toml::from_str(&text).expect("saved session parses");
        assert_eq!(toml::to_string(&back).ok(), Some(text), "save → load");
    }
    let tab = |id, p: &str| Tab::new(TabId(id), Location::local(p));
    let mut st = AppState::new(Panel::new(tab(1, r"C:\a")), Panel::new(tab(2, r"C:\b")));
    s.apply(&mut st);
});
