//! Найденный менеджер внутри адаптера: обнаружение — при первом обращении (или из
//! кэша `managers.toml`), версия и возможности — один раз, дальше из памяти.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use compact_str::CompactString;

use crate::api::{Detected, Mgr, MgrCaps, PkgError};
use crate::detect::{SystemProbe, detect};

#[derive(Debug)]
pub(crate) struct Found {
    mgr: Mgr,
    cell: Mutex<Option<Detected>>,
}

impl Found {
    /// `cached` — из `managers.toml`; `None` — обнаружение при первом обращении.
    pub(crate) fn new(mgr: Mgr, cached: Option<Detected>) -> Found {
        Found {
            mgr,
            cell: Mutex::new(cached.filter(|d| d.mgr == mgr)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Detected>> {
        self.cell.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn get(&self) -> Option<Detected> {
        let mut cell = self.lock();
        if cell.is_none() {
            *cell = detect(self.mgr, &SystemProbe);
        }
        cell.clone()
    }

    pub(crate) fn exe(&self) -> Result<PathBuf, PkgError> {
        self.get().map(|d| d.exe).ok_or(PkgError::NotFound)
    }

    pub(crate) fn caps(&self) -> MgrCaps {
        self.lock().as_ref().map_or(MgrCaps::empty(), |d| d.caps)
    }

    /// Версия: известная — сразу; иначе `query` по копии (процесс идёт без
    /// блокировки), найденное с возможностями пишется обратно.
    pub(crate) fn version(
        &self,
        query: impl FnOnce(&mut Detected) -> Result<CompactString, PkgError>,
    ) -> Result<CompactString, PkgError> {
        let mut d = self.get().ok_or(PkgError::NotFound)?;
        if let Some(v) = &d.version {
            return Ok(v.clone());
        }
        let v = query(&mut d)?;
        *self.lock() = Some(d);
        Ok(v)
    }
}
