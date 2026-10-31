//! Urfin — UI-фронтенд на GPUI (ADR-0018).
//!
//! [`gpui_shell`] — окно: `Core` в `Entity`, панели, каркас, оверлеи. Без отрисовки:
//! handler `ui` и [`UiState`] в `AppState.ext` ([`register`]), токены Fluent
//! ([`tokens`]) по теме и акценту ОС ([`sys`]), раскраска файлов ([`highlight`]),
//! формат ячеек ([`fmt_cell`]), колонки и их наборы ([`columns`]), меню F9 ([`menu`]),
//! строка F-клавиш ([`keybar`]), модели дерева, «Сведений» и просмотра ([`views`]).

pub mod accent;
pub mod bench;
pub mod color;
pub mod columns;
pub mod commands_doc;
pub mod config;
mod demo;
mod demo_term;
pub mod drives;
pub mod field_history;
pub mod fmt_cell;
mod fs_tasks;
pub mod gpui_shell;
mod handler;
pub mod highlight;
pub mod icons;
pub mod keybar;
pub mod layout;
pub mod menu;
mod screens_cmd;
mod settings_cmd;
pub mod state;
pub mod status;
pub mod sys;
pub mod theme;
mod theme_cmd;
pub mod tokens;
pub mod views;
mod welcome_cmd;

use urfin_core::{Core, Side};

pub use demo::register_demo;
pub use handler::UiHandler;
pub use state::{Draft, ThemePolicy, UiState};
pub use theme_cmd::ThemeEnv;

/// Регистрирует handler `ui`; `UiState` появится в `ext` в первом pump.
pub fn register(core: &mut Core) {
    register_with(core, ThemeEnv::default());
}

/// Как [`register`], с каталогом своих тем и политикой организации (T-01).
pub fn register_with(core: &mut Core, env: ThemeEnv) {
    core.register(Box::new(UiHandler::new(env)));
}

/// Значение аргумента `side` команды.
pub(crate) fn side_arg(side: Side) -> &'static str {
    if side == Side::Left { "left" } else { "right" }
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use urfin_core::fake::FakeListing;
    use urfin_core::{Boot, Location, Services, Waker};

    use super::*;

    struct NoWake;
    impl Waker for NoWake {
        fn wake(&self) {}
        fn wake_after(&self, _d: Duration) {}
    }

    /// Реестр всех команд приложения: core, ui, vfs, du, ops, keymap.
    pub(crate) fn registry() -> urfin_core::CommandRegistry {
        let root = Location::local(std::env::temp_dir().join("urfin-ui-test"));
        let mut core = Core::new(
            Boot::new(root.clone(), root),
            Services {
                listing: Arc::new(FakeListing::new(64)),
                ..Services::null()
            },
            Arc::new(NoWake),
        );
        register(&mut core);
        urfin_vfs::register(
            &mut core,
            Arc::new(urfin_vfs::VfsRegistry::with_local()),
            urfin_vfs::VfsCfg::default(),
        );
        urfin_vfs::du::register(&mut core, urfin_vfs::du::DuCfg::default());
        urfin_ops::register(&mut core, urfin_ops::OpsConfig::default());
        urfin_term::register(&mut core, urfin_term::TermConfig::default());
        let dir = tempfile::Builder::new()
            .prefix("urfin-ui-keymap-")
            .tempdir()
            .unwrap();
        let _ = urfin_keymap::register(
            &mut core,
            urfin_core::Paths::in_dir(dir.path().to_path_buf()),
            urfin_keymap::KeymapCfg::default(),
            urfin_keymap::FlagRegistry::with_builtin(),
        );
        menu::copy_registry(core.registry())
    }

    /// `docs/COMMANDS.md` совпадает с реестром команд (CLAUDE.md).
    /// Обновить: `URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-ui commands_doc`.
    #[test]
    fn commands_doc_is_current() {
        let reg = registry();
        let catalog = urfin_i18n::Catalog::embedded();
        let en = catalog.build(&"en".parse().unwrap()).0;
        let ru = catalog.build(&"ru".parse().unwrap()).0;
        let (menus, _) = menu::parse(menu::MENU_TOML, &reg).unwrap();
        let doc = commands_doc::render(&reg, &en, &ru, &menus);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/COMMANDS.md");
        if std::env::var_os("URFIN_UPDATE_DOCS").is_some() {
            std::fs::write(&path, &doc).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        // Генерируемый документ обновляет полоса docs-gen раз в день (скилл pets-dev):
        // устаревший не блокирует PR; строго — с `URFIN_DOCS_STRICT=1`.
        if on_disk != doc {
            let msg = "docs/COMMANDS.md устарел: URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-ui commands_doc";
            assert!(std::env::var_os("URFIN_DOCS_STRICT").is_none(), "{msg}");
            eprintln!("{msg}");
        }
    }

    /// Секция `[du]` по умолчанию (F01 §2.1) — снапшот: в urfin-vfs нет insta.
    #[test]
    fn du_default_config_snapshot() {
        insta::assert_snapshot!(toml::to_string(&urfin_vfs::du::DuCfg::default()).unwrap());
    }
}
