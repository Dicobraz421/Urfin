//! Urfin — поиск (S8 §2): запрос и фильтры, обход общим обходчиком VFS, поиск по содержимому
//! движком ripgrep, job и команды `search.*`.
//!
//! Данные экрана — `urfin_core::search`: крейт дописывает строки в `SearchStore` из своих
//! потоков, `urfin-ui` от него не зависит. Handler подключает `urfin-app` (S8-07), панель
//! результатов — backend [`SearchVfs`] схемы `search:` (S8-10) с общим [`SearchStores`].

mod config;
pub mod content;
mod handler;
mod history;
mod job;
mod life;
pub mod matcher;
pub mod query;
pub mod svfs;
pub mod walk;

use std::path::PathBuf;
use std::sync::Arc;

use urfin_core::Core;
use urfin_vfs::VfsRegistry;

pub use config::SearchCfg;
pub use content::{CompiledText, ContentSearcher, FileHit};
pub use handler::{COMMANDS, SearchHandler};
pub use matcher::EntryFilter;
pub use query::{Accel, EncodingMode, KindFilter, QueryError, SearchQuery, TextMode, TextQuery};
pub use svfs::{SearchStores, SearchVfs};
pub use walk::{Outcome, TreeSearch, search_tree};

/// Регистрирует handler `search`. `state_dir` — каталог `search_history.toml`; `None` — без
/// истории на диске (`--smoke`, `--screens`, тесты).
pub fn register(core: &mut Core, reg: Arc<VfsRegistry>, state_dir: Option<PathBuf>) {
    register_with(core, reg, state_dir, SearchStores::new());
}

/// Handler `search` со сторами панели `search:`: те же `stores` — у
/// `SearchVfs::new(stores)` в реестре листинга. Enter по каталогу панели `search:` ведёт
/// в настоящий каталог (резолвер core).
pub fn register_with(
    core: &mut Core,
    reg: Arc<VfsRegistry>,
    state_dir: Option<PathBuf>,
    stores: Arc<SearchStores>,
) {
    core.add_open_resolver(svfs::enter_resolver(&stores));
    core.register(Box::new(SearchHandler::with_stores(reg, state_dir, stores)));
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
