//! Urfin — VFS (S4): адрес `Location`, trait `Vfs`, реестр backend'ов,
//! локальная ФС, листинг для core (`VfsListing`), сортировка, иконки, наблюдение
//! (Windows — S4-12, опрос — S4-13). Hub и кэш — S4-14…16. Анализ места — [`du`] (F-01).

mod api;
pub mod batch;
pub mod cache;
pub mod diag;
pub mod dir_size;
pub mod du;
pub mod hub;
pub mod icons;
mod local;
pub mod name;
pub mod patch;
pub mod path;
pub mod poll;
pub mod service;
pub mod sort;
mod sys;
pub mod volume;
pub mod walk;

pub use api::*;
pub use local::{ListMode, LocalFs, ROOTS_TIMEOUT};
pub use poll::Poller;
pub use service::{VfsCfg, VfsListing};

/// Регистрирует handler `vfs` (`vfs.dir_size`, S4-17).
pub fn register(core: &mut urfin_core::Core, reg: std::sync::Arc<VfsRegistry>, cfg: VfsCfg) {
    core.register(Box::new(dir_size::VfsHandler::new(reg, cfg)));
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
