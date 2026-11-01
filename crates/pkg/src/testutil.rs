//! Общее для тестов: подставной менеджер `fake-pkgmgr` (urfin-testdata).

use std::path::PathBuf;
use std::sync::OnceLock;

/// `fake-pkgmgr.exe`: пример urfin-testdata рядом с тестом (`cargo nextest run
/// --workspace` собирает примеры); иначе — сборка в свой каталог (`-p urfin-pkg`
/// примеры соседей не собирает; свой `--target-dir` — без блокировки общего).
pub(crate) fn fake() -> PathBuf {
    static FAKE: OnceLock<PathBuf> = OnceLock::new();
    FAKE.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        // target/<профиль>/deps/<тест>.exe
        let profile = exe.parent().unwrap().parent().unwrap();
        let near = profile.join(r"examples\fake-pkgmgr.exe");
        if near.is_file() {
            return near;
        }
        let dir = profile.parent().unwrap().join("fake-pkgmgr");
        let ok = std::process::Command::new(env!("CARGO"))
            .args([
                "build",
                "-q",
                "-p",
                "urfin-testdata",
                "--example",
                "fake-pkgmgr",
            ])
            .arg("--target-dir")
            .arg(&dir)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .unwrap()
            .success();
        assert!(ok, "cargo build fake-pkgmgr");
        dir.join(r"debug\examples\fake-pkgmgr.exe")
    })
    .clone()
}
