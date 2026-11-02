//! Обход локальных деревьев (S5 §3.4, S5-05) в потоке координатора: DFS со
//! своим стеком, каталог открывается ([`Plan::dir`]) до своих детей, файлы
//! каталога уходят в план по мере листинга (первая работа — после первой
//! порции первого каталога), подкаталоги — после файлов. Листинг —
//! `urfin_vfs::LocalFs` (Windows: `NtQueryDirectoryFile`, S4-07; S0 v5 №14 —
//! fast path не дублируется). Память — O(глубина × ширина фронта).
//!
//! Фильтр (`OpOptions.filter`) — только к файлам, каталоги не фильтруются.
//! Ссылки по [`LinkMode`]: `Preserve` — ссылка как элемент (копируется сама
//! ссылка), `Skip` — пропуск, `Follow` — внутрь, цикл ловится по
//! `(том, FileId)` предков.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use urfin_core::{Attrs, CancelToken, Entry, EntryKind, Location, Mask};
use urfin_vfs::{ListOpts, LocalFs, Vfs, VfsError};

use super::{DirNode, ItemResult, OpExec, Plan, PlanError};

/// Что обходчик делает с элементами; методы зовутся в потоке координатора.
pub(crate) trait Visit<E: OpExec> {
    /// Данные каталога для детей (каталог назначения, индекс имён).
    type Ctx;

    /// Файл (или ссылка при `Preserve`) каталога `dir`.
    fn file(
        &mut self,
        p: &mut Plan<'_, E>,
        dir: &Arc<DirNode<E::Dir>>,
        ctx: &Self::Ctx,
        src: PathBuf,
        e: &Entry,
    ) -> Result<(), PlanError>;

    /// Подкаталог: открыть узел и войти (`Some`) или обработать иначе (`None`:
    /// пропуск, перенос целиком).
    fn dir(
        &mut self,
        p: &mut Plan<'_, E>,
        parent: &Arc<DirNode<E::Dir>>,
        ctx: &Self::Ctx,
        src: PathBuf,
        e: &Entry,
    ) -> Result<Enter<E::Dir, Self::Ctx>, PlanError>;

    /// Каталог не прочитан (целиком или с середины): ошибка элемента.
    fn list_failed(
        &mut self,
        p: &mut Plan<'_, E>,
        dir: &Arc<DirNode<E::Dir>>,
        src: &Path,
        err: &VfsError,
    );
}

/// Вход в подкаталог: его узел и данные для детей; `None` — не входить.
pub(crate) type Enter<D, C> = Option<(Arc<DirNode<D>>, C)>;

/// Как обходить.
#[derive(Debug)]
pub(crate) struct Walker {
    fs: LocalFs,
    mask: Option<Mask>,
    links: crate::api::LinkMode,
    cancel: CancelToken,
    pub stats: WalkStats,
}

/// Итоги обхода.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WalkStats {
    pub dirs: u64,
    pub files: u64,
    /// Файлы не по маске.
    pub filtered: u64,
    /// Ссылки при `LinkMode::Skip` и циклы при `Follow`.
    pub links_skipped: u64,
    pub cycles: u64,
}

struct Frame<D, C> {
    node: Arc<DirNode<D>>,
    ctx: C,
    src: PathBuf,
    dirs: Vec<Entry>,
    next: usize,
    /// (том, `FileId`) — только при `Follow`.
    id: Option<(u64, u128)>,
}

impl Walker {
    /// `cancel` прерывает листинг большого каталога между порциями.
    pub(crate) fn new(
        mask: Option<Mask>,
        links: crate::api::LinkMode,
        cancel: CancelToken,
    ) -> Walker {
        Walker {
            fs: LocalFs::new(),
            mask,
            links,
            cancel,
            stats: WalkStats::default(),
        }
    }

    /// Имя записи для ОС (с учётом экранирования не-UTF-16).
    pub(crate) fn os_name(e: &Entry) -> std::borrow::Cow<'_, OsStr> {
        urfin_vfs::name::to_os(&e.name, e.attrs.has(Attrs::ESCAPED_NAME))
    }

    fn follow(&self) -> bool {
        self.links == crate::api::LinkMode::Follow
    }

    fn file_id(&self, p: &Path) -> Option<(u64, u128)> {
        self.fs.stat(&loc(p), true).ok().and_then(|m| m.file_id)
    }

    /// Обходит каталог `src` с уже открытым узлом `node` и закрывает его
    /// (и все вложенные) — [`Plan::close`].
    pub(crate) fn walk<E: OpExec, V: Visit<E>>(
        &mut self,
        p: &mut Plan<'_, E>,
        v: &mut V,
        src: PathBuf,
        node: Arc<DirNode<E::Dir>>,
        ctx: V::Ctx,
    ) -> Result<(), PlanError> {
        let id = if self.follow() {
            self.file_id(&src)
        } else {
            None
        };
        let mut stack = vec![self.enter(p, v, src, node, ctx, id)?];
        while let Some(top) = stack.last_mut() {
            let Some(e) = top.dirs.get(top.next).cloned() else {
                let f = stack.pop().expect("non-empty stack");
                p.close(&f.node);
                continue;
            };
            top.next += 1;
            let child = top.src.join(Self::os_name(&e));
            let id = if self.follow() {
                let id = self.file_id(&child);
                if id.is_some() && stack.iter().any(|f| f.id == id) {
                    tracing::warn!(dir = %child.display(), "link cycle, skipped");
                    self.stats.cycles += 1;
                    let parent = &stack.last().expect("non-empty stack").node;
                    p.record(Some(parent), ItemResult::Skipped);
                    continue;
                }
                id
            } else {
                None
            };
            let top = stack.last().expect("non-empty stack");
            if let Some((node, ctx)) = v.dir(p, &top.node, &top.ctx, child.clone(), &e)? {
                let f = self.enter(p, v, child, node, ctx, id)?;
                stack.push(f);
            }
        }
        Ok(())
    }

    /// Листинг каталога: файлы — сразу в план, подкаталоги — в кадр.
    fn enter<E: OpExec, V: Visit<E>>(
        &mut self,
        p: &mut Plan<'_, E>,
        v: &mut V,
        src: PathBuf,
        node: Arc<DirNode<E::Dir>>,
        ctx: V::Ctx,
        id: Option<(u64, u128)>,
    ) -> Result<Frame<E::Dir, V::Ctx>, PlanError> {
        self.stats.dirs += 1;
        let mut dirs = Vec::new();
        let opts = ListOpts {
            resolve_links: false,
            first_batch_hint: 256,
        };
        let mut batch = Vec::new();
        // Ошибка плана (отмена, сбой) внутри листинга: листинг прерывается
        // отменой, причина — здесь.
        let mut stop = None;
        let listed = self.fs.list(&loc(&src), &opts).and_then(|mut l| {
            loop {
                batch.clear();
                let more = l.next_batch(&mut batch, &self.cancel)?;
                for e in batch.drain(..) {
                    if let Err(err) = self.entry(p, v, &node, &ctx, &src, e, &mut dirs) {
                        stop = Some(err);
                        return Err(VfsError::Cancelled);
                    }
                }
                if !more {
                    return Ok(());
                }
            }
        });
        if let Some(err) = stop {
            return Err(err);
        }
        match listed {
            Ok(()) => {}
            Err(VfsError::Cancelled) => return Err(PlanError::Cancelled),
            Err(e) => v.list_failed(p, &node, &src, &e),
        }
        Ok(Frame {
            node,
            ctx,
            src,
            dirs,
            next: 0,
            id,
        })
    }

    /// Одна запись листинга.
    #[allow(clippy::too_many_arguments, reason = "внутренний шаг обхода")]
    fn entry<E: OpExec, V: Visit<E>>(
        &mut self,
        p: &mut Plan<'_, E>,
        v: &mut V,
        node: &Arc<DirNode<E::Dir>>,
        ctx: &V::Ctx,
        src: &Path,
        e: Entry,
        dirs: &mut Vec<Entry>,
    ) -> Result<(), PlanError> {
        use crate::api::LinkMode;
        let is_link = matches!(
            e.kind,
            EntryKind::LinkDir | EntryKind::LinkFile | EntryKind::LinkBroken
        );
        if is_link && self.links == LinkMode::Skip {
            self.stats.links_skipped += 1;
            return Ok(());
        }
        let descend = e.kind == EntryKind::Dir || (e.kind == EntryKind::LinkDir && self.follow());
        if descend {
            dirs.push(e);
            return Ok(());
        }
        if self.mask.as_ref().is_some_and(|m| !m.matches(&e.name)) {
            self.stats.filtered += 1;
            return Ok(());
        }
        self.stats.files += 1;
        let path = src.join(Self::os_name(&e));
        v.file(p, node, ctx, path, &e)
    }
}

/// Адрес для `LocalFs` без нормализации: verbatim-путь остаётся verbatim
/// (имена с хвостовой точкой и пробелом, S5 §4.11).
pub(crate) fn loc(p: &Path) -> Location {
    Location {
        backend: "local".into(),
        authority: None,
        path: p.to_path_buf(),
        outer: None,
    }
}
