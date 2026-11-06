//! Urfin — поиск (S8 §2): запрос и фильтры, обход общим обходчиком VFS, поиск по содержимому
//! движком ripgrep, job и команды `search.*`.
//!
//! Данные экрана — `urfin_core::search`: крейт дописывает строки в `SearchStore` из своих
//! потоков, `urfin-ui` от него не зависит. Handler подключает `urfin-app` (S8-07).

mod config;
pub mod content;
mod handler;
mod history;
mod job;
pub mod matcher;
pub mod query;
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
pub use walk::{Outcome, TreeSearch, search_tree};

/// Регистрирует handler `search`. `state_dir` — каталог `search_history.toml`; `None` — без
/// истории на диске (`--smoke`, `--screens`, тесты).
pub fn register(core: &mut Core, reg: Arc<VfsRegistry>, state_dir: Option<PathBuf>) {
    core.register(Box::new(SearchHandler::new(reg, state_dir)));
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
