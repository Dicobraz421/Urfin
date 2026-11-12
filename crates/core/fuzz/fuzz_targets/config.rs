//! `Config::load` (в том числе миграции версии): `config.toml` из входа во
//! временном каталоге. Никогда не паникует (контракт `Config::load`); секции
//! других крейтов из `ext` читаются без паник.
#![no_main]

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use urfin_core::{Config, Paths};

fn dir() -> &'static PathBuf {
    static D: OnceLock<PathBuf> = OnceLock::new();
    D.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("urfin-fuzz-config-{}", std::process::id()));
        fs::create_dir_all(&d).expect("temp dir");
        d
    })
}

fuzz_target!(|data: &[u8]| {
    let paths = Paths::in_dir(dir());
    fs::write(paths.config_file(), data).expect("write config.toml");
    let (cfg, _warnings) = Config::load(&paths);
    let names: Vec<String> = cfg.ext.keys().cloned().collect();
    for n in names {
        let _ = cfg.section::<toml::Table>(&n);
    }
});
