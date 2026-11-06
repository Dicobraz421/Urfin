//! Urfin — поиск (S8 §2): запрос и фильтры, обход общим обходчиком VFS, поиск по содержимому
//! движком ripgrep, job и команды `search.*`.
//!
//! Данные экрана — `urfin_core::search`: крейт дописывает строки в `SearchStore` из своих
//! потоков, `urfin-ui` от него не зависит. Handler подключает `urfin-app` (S8-07).

mod config;
pub mod content;
pub mod matcher;
pub mod query;
pub mod walk;

pub use config::SearchCfg;
pub use content::{CompiledText, ContentSearcher, FileHit};
pub use matcher::EntryFilter;
pub use query::{Accel, EncodingMode, KindFilter, QueryError, SearchQuery, TextMode, TextQuery};
pub use walk::{Outcome, TreeSearch, search_tree};

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
