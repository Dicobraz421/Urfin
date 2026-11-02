//! Backend схемы `pkg:` (F02 §2.5): `pkg:installed`, `pkg:updates` — строки
//! [`PkgBook`] с колонками Версия · Доступна · Менеджер · Издатель (`Caps::VIRTUAL |
//! READ_ONLY`). Листинг и ячейки — из памяти, процессов backend не запускает.

use std::path::PathBuf;
use std::sync::Arc;

use compact_str::CompactString;
use urfin_core::{Attrs, CancelToken, Entry, EntryKind, Location};
use urfin_vfs::{
    Caps, ColumnSpec, ColumnView, ExtCell, ListOpts, ListStream, Metadata, Space, Vfs, VfsError,
    VfsPath, VfsRead, WatchGuard, WatchTx,
};

use crate::api::Mgr;
use crate::book::{Filter, PkgBook, PkgRow};

/// Схема вкладки пакетов.
pub const SCHEME: &str = "pkg";

/// `pkg:/<фильтр>` (CONTRACTS, S0 v6 №5): фильтр — корень вкладки, `..` нет.
pub fn location(f: Filter) -> Location {
    Location {
        backend: SCHEME.into(),
        authority: None,
        path: PathBuf::from(format!("/{}", f.as_str())),
        outer: None,
    }
}

/// Фильтр адреса `pkg:`; другой адрес — `None`.
pub fn filter_of(loc: &Location) -> Option<Filter> {
    if loc.backend != SCHEME || loc.outer.is_some() || loc.authority.is_some() {
        return None;
    }
    let p = loc.path.to_str()?;
    Filter::parse(p.trim_start_matches(['/', '\\']))
}

const fn text_col(id: &'static str, title: &'static str, width: f32) -> ColumnSpec {
    ColumnSpec {
        id,
        title,
        width,
        right: false,
        view: ColumnView::Text,
        mono: false,
    }
}

/// Колонки после «Программы» (`screen-pkginstalled`).
static COLUMNS: [ColumnSpec; 4] = [
    text_col("version", "pkg-col-version", 96.0),
    text_col("available", "pkg-col-available", 88.0),
    text_col("mgr", "pkg-col-mgr", 116.0),
    text_col("publisher", "pkg-col-publisher", 136.0),
];

/// Текст ячейки колонки `col` строки: «вне менеджера», у scoop без издателя — бакет.
pub fn cell(row: &PkgRow, col: &str) -> Option<CompactString> {
    match col {
        "version" => Some(row.pkg.version.clone()),
        "available" => row.available.clone(),
        "mgr" if row.outside => Some(urfin_i18n::current().get("pkg-outside").into()),
        "mgr" => Some(row.pkg.id.mgr.as_str().into()),
        "publisher" => match (&row.pkg.publisher, &row.pkg.source) {
            (Some(p), _) => Some(p.clone()),
            (None, Some(b)) if row.pkg.id.mgr == Mgr::Scoop => {
                let mut a = urfin_i18n::FluentArgs::new();
                a.set("name", b.to_string());
                Some(urfin_i18n::current().fmt("pkg-bucket", &a).into())
            }
            _ => None,
        },
        _ => None,
    }
}

fn entry(r: &PkgRow) -> Entry {
    Entry {
        name: r.name.clone(),
        size: 0,
        mtime: i64::MIN,
        ctime: i64::MIN,
        atime: i64::MIN,
        attrs: Attrs(Attrs::READONLY),
        kind: EntryKind::File,
    }
}

/// Backend схемы `pkg:`.
#[derive(Debug)]
pub struct PkgVfs {
    book: Arc<PkgBook>,
}

impl PkgVfs {
    pub fn new(book: Arc<PkgBook>) -> PkgVfs {
        PkgVfs { book }
    }

    fn filter(p: &VfsPath) -> Result<Filter, VfsError> {
        filter_of(p).ok_or_else(|| VfsError::NotFound(p.path.clone()))
    }
}

/// Готовый список одним батчем.
struct Ready(Option<Vec<Entry>>);

impl ListStream for Ready {
    fn next_batch(
        &mut self,
        out: &mut Vec<Entry>,
        _cancel: &CancelToken,
    ) -> Result<bool, VfsError> {
        if let Some(v) = self.0.take() {
            out.extend(v);
        }
        Ok(false)
    }

    fn size_hint(&self) -> Option<u32> {
        self.0.as_ref().and_then(|v| u32::try_from(v.len()).ok())
    }
}

fn meta(kind: EntryKind) -> Metadata {
    Metadata {
        kind,
        attrs: Attrs(Attrs::READONLY),
        size: 0,
        alloc_size: None,
        mtime: i64::MIN,
        atime: i64::MIN,
        ctime: None,
        btime: None,
        mode: None,
        uid: None,
        gid: None,
        nlink: None,
        file_id: None,
        reparse_tag: None,
        link_target: None,
    }
}

impl Vfs for PkgVfs {
    fn scheme(&self) -> &'static str {
        SCHEME
    }

    fn caps(&self, _at: &VfsPath) -> Caps {
        Caps(Caps::VIRTUAL | Caps::READ_ONLY)
    }

    /// Программа · Версия · Доступна · Менеджер · Издатель при любом виде панели.
    fn columns(&self, _dir: &VfsPath) -> &'static [ColumnSpec] {
        &COLUMNS
    }

    fn list(&self, dir: &VfsPath, _opts: &ListOpts) -> Result<Box<dyn ListStream>, VfsError> {
        let f = Self::filter(dir)?;
        let view = self.book.view();
        Ok(Box::new(Ready(Some(
            view.rows(f).iter().map(entry).collect(),
        ))))
    }

    fn stat(&self, p: &VfsPath, _follow: bool) -> Result<Metadata, VfsError> {
        if filter_of(p).is_some() {
            return Ok(meta(EntryKind::Dir));
        }
        let parent = p
            .parent()
            .ok_or_else(|| VfsError::NotFound(p.path.clone()))?;
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        self.stat_entry(&parent, name, Attrs::default())
            .map(|_| meta(EntryKind::File))
    }

    fn stat_entry(&self, dir: &VfsPath, name: &str, _hint: Attrs) -> Result<Entry, VfsError> {
        let f = Self::filter(dir)?;
        self.book
            .view()
            .row(f, name)
            .map(entry)
            .ok_or_else(|| VfsError::NotFound(dir.path.join(name)))
    }

    fn open_read(&self, _p: &VfsPath) -> Result<Box<dyn VfsRead>, VfsError> {
        Err(VfsError::Unsupported("package rows have no content"))
    }

    fn space(&self, _at: &VfsPath) -> Result<Space, VfsError> {
        Err(VfsError::Unsupported("pkg: has no volume"))
    }

    fn local_path(&self, _p: &VfsPath) -> Option<PathBuf> {
        None
    }

    /// Изменения приходят от handler `pkg` (`panel.refresh` по ответам менеджеров).
    fn watch(&self, _dir: &VfsPath, _tx: WatchTx) -> Result<WatchGuard, VfsError> {
        Ok(WatchGuard::new(()))
    }

    /// Только память [`PkgBook`] (UI-поток).
    fn ext(&self, dir: &VfsPath, e: &Entry, col: &str) -> Option<ExtCell> {
        let f = filter_of(dir)?;
        let view = self.book.view();
        cell(view.row(f, &e.name)?, col).map(ExtCell::Text)
    }
}
