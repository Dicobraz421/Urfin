//! Urfin — архивы как VFS (S8 §3).
//!
//! Архив — VFS-бэкенд своей схемы ([`ArcVfs`] над [`Format`]): адрес внутри —
//! `Location { backend: "zip", path: "/dir", outer: Some(файл архива) }`. Оглавление —
//! [`ArchiveIndex`], строится при первом входе и живёт в общем [`ArcCache`] (LRU, по умолчанию 256 мебибайт).
//! Сводку для экрана (`urfin_core::arc::ArcModel`) публикует handler `arc`; `urfin-ui` от
//! крейта не зависит. Только чтение (M5a): `Caps::READ_ONLY`.
//!
//! Подключает `urfin-app` (S8-26): [`backends`] — в оба реестра VFS (листинг и
//! handler'ы), [`register`] — handler и резолвер Enter, всё с одним [`ArcCache`].

mod avfs;
pub mod cache;
mod config;
pub mod detect;
mod extract;
pub mod format;
pub mod formats;
mod handler;
pub mod index;
pub mod mock;
pub mod password;
pub mod path;
pub mod spill;

use std::sync::Arc;

use urfin_core::Core;
use urfin_vfs::{Vfs, VfsRegistry};

pub use avfs::{ArcVfs, PACKED, root_of};
pub use cache::{ArcCache, ArcEvent};
pub use config::ArcCfg;
pub use detect::ArcKind;
pub use format::{ArcSource, Format, OpenCtx, Scan};
#[cfg(feature = "rar")]
pub use formats::rar::RarFormat;
pub use formats::sevenz::SevenZFormat;
pub use formats::tar::TarFormat;
pub use formats::zip::ZipFormat;
pub use handler::{ArcHandler, COMMANDS, MENU, open_resolver};
pub use index::{ArchiveIndex, EntryLoc, IndexBuilder, Node, NodeId, OuterKey, RawEntry};
pub use password::{Ask, Question};
pub use spill::{Over, Spill, SpillQuestion};

/// Кэш индексов с лимитом из `[arc] cache_mib`: один на процесс.
pub fn cache(cfg: &ArcCfg) -> Arc<ArcCache> {
    let c = ArcCache::new(cfg.cache_bytes());
    c.set_spill_limits(cfg.spill_max, cfg.ratio_max);
    c
}

/// Backend'ы подключённых форматов над общим кэшем — в каждый реестр VFS; из `cfg` —
/// кодовая страница имён zip. Форматы добавляются сюда по мере готовности (zip — S8-25,
/// tar — S8-30, 7z — S8-32, rar — S8-34, только с feature `rar`); Enter входит только в
/// архивы схем из реестра, остальные открывает ОС.
pub fn backends(cache: &Arc<ArcCache>, cfg: &ArcCfg) -> Vec<Arc<dyn Vfs>> {
    vec![
        Arc::new(ArcVfs::new(ZipFormat::new(cfg), Arc::clone(cache))),
        Arc::new(ArcVfs::new(TarFormat::new(), Arc::clone(cache))),
        Arc::new(ArcVfs::new(SevenZFormat::new(), Arc::clone(cache))),
        #[cfg(feature = "rar")]
        Arc::new(ArcVfs::new(RarFormat::new(), Arc::clone(cache))),
    ]
}

/// Handler `arc` и резолвер Enter (первым). `reg` — реестр handler'ов, уже с
/// [`backends`]; `cache` — тот же, что у них.
pub fn register(core: &mut Core, reg: Arc<VfsRegistry>, cache: Arc<ArcCache>, cfg: &ArcCfg) {
    // Вложенные архивы и архивы на сервере читаются через этот реестр (S8-31).
    cache.set_vfs(&reg);
    core.add_open_resolver(open_resolver(cfg, Arc::clone(&reg)));
    core.register(Box::new(ArcHandler::new(reg, cache)));
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    /// Строки, на которые ссылается код крейта, есть в EN и RU (`ui-arc.ftl`, S8-22).
    #[test]
    fn ui_strings_exist() {
        for lang in ["en", "ru"] {
            urfin_i18n::set_language(lang);
            let t = urfin_i18n::current();
            for key in [
                super::PACKED.title,
                "arc-error-format",
                "arc-error-too-deep",
                "arc-spill-ask",
                "arc-spill-big",
                "arc-spill-ratio",
                "arc-spill-go",
                "arc-error-corrupt",
                "arc-toast-readonly",
                "arc-pw-exhausted",
                "arc-pw-ask",
                "arc-pw-wrong",
                "arc-error-no-password",
                "arc-job-test",
                "arc-test-ok",
                "arc-test-failed",
                "ui-button-ok",
                "ui-button-cancel",
                "ops-not-yet",
                "ops-nothing",
                "ops-dest-empty",
            ] {
                assert!(t.has(key), "{lang}: {key}");
            }
        }
    }

    #[test]
    fn version_smoke() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert_ne!(super::version(), "");
    }
}
