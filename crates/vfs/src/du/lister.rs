//! Листеры анализа места (F01 §3.1): `NtLister` (NTFS/ReFS: `FileIdFullDirectoryInformation`,
//! открытие подкаталога относительно родителя), `FindLister` (запасной путь, без
//! `AllocationSize`/`FileId` — снимок `APPROX`), `VfsLister` (прочие ФС и backend'ы).
//!
//! Размер на диске: `AllocationSize`; сжатые/разреженные/WOF с логическим размером
//! ≥ `track_min` уточняются `GetCompressedFileSizeW`; облачные заглушки и `OFFLINE` —
//! 0 на диске, файлы не открываются (без гидрации).

use crate::api::{Attrs, EntryKind, ListOpts, Vfs, VfsError};
use crate::walk::{DirLister, DirRef, Parent, RawEntry, RawName};

/// Какой листер обходит дерево.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ListerKind {
    /// По ФС корня: NTFS/ReFS — `Nt` (при отказе ФС — `Find`), иначе `Vfs`.
    #[default]
    Auto,
    Nt,
    Find,
    Vfs,
}

/// `NtQueryDirectoryFile(FileIdFullDirectoryInformation)`.
#[derive(Debug)]
pub struct NtLister {
    pub track_min: u64,
}

/// `FindFirstFileExW`: размер на диске — логический, округлённый до кластера.
#[derive(Debug)]
pub struct FindLister {
    pub track_min: u64,
    /// Байт в кластере; 0 — без округления.
    pub cluster: u64,
}

/// `Vfs::list` (как `dir_size`): размер на диске = логический.
pub struct VfsLister<'a> {
    pub vfs: &'a dyn Vfs,
}

impl std::fmt::Debug for VfsLister<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VfsLister")
            .field("scheme", &self.vfs.scheme())
            .finish()
    }
}

impl DirLister for VfsLister<'_> {
    fn list(
        &self,
        dir: &DirRef,
        out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
    ) -> Result<Option<Parent>, VfsError> {
        let mut st = self.vfs.list(&dir.loc, &ListOpts::default())?;
        let cancel = urfin_core::CancelToken::new();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let more = st.next_batch(&mut buf, &cancel)?;
            for e in &buf {
                let a = e.attrs;
                let mut flags = 0;
                for (attr, f) in [
                    (Attrs::OFFLINE, RawEntry::OFFLINE),
                    (Attrs::COMPRESSED, RawEntry::COMPRESSED),
                    (Attrs::HIDDEN, RawEntry::HIDDEN),
                    (Attrs::SYSTEM, RawEntry::SYSTEM),
                ] {
                    if a.has(attr) {
                        flags |= f;
                    }
                }
                let logical = if e.kind == EntryKind::File { e.size } else { 0 };
                let raw = RawEntry {
                    name: RawName::Str {
                        name: &e.name,
                        escaped: a.has(Attrs::ESCAPED_NAME),
                    },
                    kind: e.kind,
                    logical,
                    disk: if flags & RawEntry::OFFLINE != 0 {
                        0
                    } else {
                        logical
                    },
                    mtime: e.mtime,
                    file_id: 0,
                    flags,
                };
                if !out(&raw) {
                    return Ok(None);
                }
            }
            if !more {
                return Ok(None);
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use std::path::Path;
    use std::sync::Arc;

    use smallvec::SmallVec;

    use super::{FindLister, NtLister};
    use crate::api::{EntryKind, VfsError};
    use crate::sys::win_du::{self, DirHandle, DuRec, ListErr};
    use crate::walk::{DirLister, DirRef, Parent, RawEntry, RawName};

    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    const SPARSE: u32 = 0x200;
    const COMPRESSED: u32 = 0x800;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    const IO_REPARSE_TAG_WOF: u32 = 0x8000_0017;

    /// Запись ОС → `RawEntry`. `alloc` — размер на диске до уточнения.
    fn raw<'a>(r: &DuRec<'a>, alloc: u64, track_min: u64, dir: &Path) -> RawEntry<'a> {
        let file = matches!(r.kind, EntryKind::File);
        let logical = if file { r.eof } else { 0 };
        let mut flags = 0;
        if r.attrs & HIDDEN != 0 {
            flags |= RawEntry::HIDDEN;
        }
        if r.attrs & SYSTEM != 0 {
            flags |= RawEntry::SYSTEM;
        }
        let packed = r.attrs & (COMPRESSED | SPARSE) != 0 || r.tag == IO_REPARSE_TAG_WOF;
        if packed {
            flags |= RawEntry::COMPRESSED;
        }
        let offline = r.attrs & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0;
        let disk = if !file {
            0
        } else if offline {
            flags |= RawEntry::OFFLINE;
            0
        } else if packed && logical >= track_min {
            let name = RawName::Wide(r.name);
            win_du::compressed_size(&dir.join(name.to_os())).unwrap_or(alloc)
        } else {
            alloc
        };
        RawEntry {
            name: RawName::Wide(r.name),
            kind: r.kind,
            logical,
            disk,
            mtime: r.mtime,
            file_id: r.file_id,
            flags,
        }
    }

    fn find(
        dir: &Path,
        track_min: u64,
        cluster: u64,
        out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
    ) -> Result<(), VfsError> {
        win_du::find_for_each(dir, &mut |r| {
            let alloc = if cluster > 0 {
                r.eof.div_ceil(cluster).saturating_mul(cluster)
            } else {
                r.eof
            };
            out(&raw(r, alloc, track_min, dir))
        })
    }

    impl DirLister for NtLister {
        fn list(
            &self,
            dir: &DirRef,
            out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
        ) -> Result<Option<Parent>, VfsError> {
            let path = &dir.loc.path;
            let parent = dir
                .parent
                .as_ref()
                .and_then(|p| p.downcast_ref::<DirHandle>());
            let h = match (parent, path.file_name()) {
                (Some(ph), Some(name)) => {
                    use std::os::windows::ffi::OsStrExt;
                    let w: SmallVec<[u16; 64]> = name.encode_wide().collect();
                    win_du::open_rel(ph, &w, path)?
                }
                _ => win_du::open_abs(path)?,
            };
            match win_du::nt_for_each(&h, path, &mut |r| {
                out(&raw(r, r.alloc, self.track_min, path))
            }) {
                Ok(()) => Ok(Some(Arc::new(h))),
                // Другая ФС внутри дерева (редко: точки монтирования не обходятся).
                Err(ListErr::Unsupported) => {
                    find(path, self.track_min, 0, out)?;
                    Ok(None)
                }
                Err(ListErr::Vfs(e)) => Err(e),
            }
        }
    }

    impl DirLister for FindLister {
        fn list(
            &self,
            dir: &DirRef,
            out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
        ) -> Result<Option<Parent>, VfsError> {
            find(&dir.loc.path, self.track_min, self.cluster, out)?;
            Ok(None)
        }
    }

    /// Поддерживает ли ФС каталога `FileIdFullDirectoryInformation`.
    pub(super) fn nt_supported(dir: &Path) -> Result<bool, VfsError> {
        let h = win_du::open_abs(dir)?;
        match win_du::nt_for_each(&h, dir, &mut |_| false) {
            Ok(()) => Ok(true),
            Err(ListErr::Unsupported) => Ok(false),
            Err(ListErr::Vfs(e)) => Err(e),
        }
    }
}

#[cfg(not(windows))]
mod other {
    use super::{FindLister, NtLister};
    use crate::api::VfsError;
    use crate::walk::{DirLister, DirRef, Parent, RawEntry};

    impl DirLister for NtLister {
        fn list(
            &self,
            _dir: &DirRef,
            _out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
        ) -> Result<Option<Parent>, VfsError> {
            Err(VfsError::Unsupported("NtLister"))
        }
    }

    impl DirLister for FindLister {
        fn list(
            &self,
            _dir: &DirRef,
            _out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
        ) -> Result<Option<Parent>, VfsError> {
            Err(VfsError::Unsupported("FindLister"))
        }
    }
}

/// Поддерживает ли ФС каталога быстрый листинг с `FileId` (вне Windows — нет).
pub fn nt_supported(dir: &std::path::Path) -> Result<bool, VfsError> {
    #[cfg(windows)]
    {
        win::nt_supported(dir)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Ok(false)
    }
}
