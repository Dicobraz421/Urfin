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
pub mod format;
mod handler;
pub mod index;
pub mod mock;
pub mod path;

use std::sync::Arc;

use urfin_core::Core;
use urfin_vfs::{Vfs, VfsRegistry};

pub use avfs::{ArcVfs, PACKED, root_of};
pub use cache::{ArcCache, ArcEvent};
pub use config::ArcCfg;
pub use detect::ArcKind;
pub use format::{ArcSource, Format, Scan};
pub use handler::{ArcHandler, COMMANDS, open_resolver};
pub use index::{ArchiveIndex, EntryLoc, IndexBuilder, Node, NodeId, OuterKey, RawEntry};

/// Кэш индексов с лимитом из `[arc] cache_mib`: один на процесс.
pub fn cache(cfg: &ArcCfg) -> Arc<ArcCache> {
    ArcCache::new(cfg.cache_bytes())
}

/// Backend'ы подключённых форматов над общим кэшем — в каждый реестр VFS. Форматы
/// добавляются сюда по мере готовности (zip — S8-25, tar — S8-30, 7z — S8-32, rar —
/// S8-34); Enter входит только в архивы схем из реестра.
pub fn backends(cache: &Arc<ArcCache>) -> Vec<Arc<dyn Vfs>> {
    let _ = cache;
    Vec::new()
}

/// Handler `arc` и резолвер Enter (первым). `reg` — реестр handler'ов, уже с
/// [`backends`]; `cache` — тот же, что у них.
pub fn register(core: &mut Core, reg: Arc<VfsRegistry>, cache: Arc<ArcCache>, cfg: &ArcCfg) {
    core.add_open_resolver(open_resolver(cfg, Arc::clone(&reg)));
    core.register(Box::new(ArcHandler::new(reg, cache)));
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_smoke() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert_ne!(super::version(), "");
    }
}
