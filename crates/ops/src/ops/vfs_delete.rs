//! Удаление на сервере схемы (F8/Shift+F8 в `sftp://`, F03 §4.1, F03-07): корзины на
//! сервере нет — только насовсем. Обход — план, файлы — воркеры, каталог удаляется в
//! Finalizer после всех детей (пустым). Ссылки удаляются как ссылки.

use std::path::PathBuf;

use compact_str::CompactString;
use urfin_core::{Cancelled, Cx, JobId, JobSpec, JobSummary, Lane, Level, Location};
use urfin_vfs::VfsError;

use crate::api::{FailedItem, IoClass, ItemOp, Report};
use crate::engine::{DirNode, ExecCx, ItemResult, JobEngine, Note, OpExec, Plan, PlanError, Work};
use crate::remote::End;

#[derive(Debug)]
pub(crate) struct Item(Location);

pub(crate) struct VfsDelete {
    end: End,
    base: Location,
    items: Vec<CompactString>,
    jobs: crate::config::DeviceJobs,
}

/// Из handler (UI-поток, без I/O): job удаления насовсем на сервере.
pub(crate) fn start(
    eng: &JobEngine,
    cx: &mut Cx<'_>,
    end: End,
    base: Location,
    items: Vec<CompactString>,
    jobs: crate::config::DeviceJobs,
) -> JobId {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("n", items.len());
    a.set(
        "name",
        items.first().map_or_else(String::new, ToString::to_string),
    );
    let spec = JobSpec {
        kind: "delete",
        title: urfin_i18n::current().fmt("ops-job-items", &a),
        lane: Lane::Bulk,
        priority: 1,
        owner_ns: "ops",
    };
    let workers = end.channels().unwrap_or(1);
    eng.start(
        cx,
        spec,
        workers,
        VfsDelete {
            end,
            base,
            items,
            jobs,
        },
    )
}

fn failed(l: &Location, e: &VfsError) -> FailedItem {
    let class = match e {
        VfsError::Denied(_) => IoClass::Permission,
        VfsError::NotFound(_) => IoClass::NotFound,
        _ => IoClass::Other,
    };
    FailedItem {
        path: PathBuf::from(End::shown(l)),
        op: ItemOp::Delete,
        error: match e {
            VfsError::NoWrite(_) => crate::remote::err_text(e),
            e => e.to_string(),
        }
        .into(),
        class,
        pair: None,
    }
}

impl VfsDelete {
    fn walk(
        &self,
        p: &mut Plan<'_, Self>,
        parent: Option<&std::sync::Arc<DirNode<Location>>>,
        loc: Location,
        dir: bool,
    ) -> Result<(), Cancelled> {
        if !dir {
            return p.push(parent, Item(loc), 0);
        }
        let node = p.dir(parent, loc.clone())?;
        match self.end.list(&loc, &p.cx().cancel_token()) {
            Ok(children) => {
                for e in children {
                    // Ссылка на каталог — сама ссылка, без захода внутрь.
                    let sub = e.kind == urfin_core::EntryKind::Dir;
                    let c = self.end.join(&loc, &e.name);
                    self.walk(p, Some(&node), c, sub)?;
                }
            }
            Err(VfsError::Cancelled) => return Err(Cancelled),
            Err(e) => p.record(Some(&node), ItemResult::Failed(failed(&loc, &e))),
        }
        p.close(&node);
        Ok(())
    }
}

impl OpExec for VfsDelete {
    type Item = Item;
    type Dir = Location;

    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        let key = self.end.device(&self.base);
        p.device_slot(&[(key, self.jobs.net.max(1))])?;
        for name in &self.items {
            let loc = self.end.join(&self.base, name);
            match self.end.stat(&loc) {
                Ok(Some(m)) => self.walk(p, None, loc, m.kind == urfin_core::EntryKind::Dir)?,
                Ok(None) => {}
                Err(e) => p.record(None, ItemResult::Failed(failed(&loc, &e))),
            }
        }
        Ok(())
    }

    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult {
        if cx.checkpoint().is_err() {
            return ItemResult::Cancelled;
        }
        let l = &work.item.0;
        match self.end.remove(l, false) {
            Ok(()) | Err(VfsError::NotFound(_)) => {
                if let Some(d) = l.parent() {
                    cx.touch(&d);
                }
                ItemResult::Done
            }
            Err(e) => ItemResult::Failed(failed(l, &e)),
        }
    }

    fn finalize_dir(&self, dir: &DirNode<Location>, cx: &ExecCx<'_>) -> ItemResult {
        if !dir.clean() {
            return ItemResult::Skipped;
        }
        match self.end.remove(&dir.data, true) {
            Ok(()) => {
                if let Some(d) = dir.data.parent() {
                    cx.touch(&d);
                }
                ItemResult::Done
            }
            Err(e) => ItemResult::Failed(failed(&dir.data, &e)),
        }
    }

    fn finished(&self, r: &Report, outcome: &Result<JobSummary, String>) -> Option<Note> {
        let first = r
            .failed
            .first()
            .map(|f| format!("{}: {}", f.path.display(), f.error));
        match (outcome, first) {
            (Err(e), _) if e == "cancelled" => None,
            (Err(e), _) => Some((Level::Error, e.clone())),
            (Ok(_), Some(first)) => {
                let mut a = urfin_i18n::FluentArgs::new();
                a.set("error", first);
                Some((
                    Level::Error,
                    urfin_i18n::current().fmt("ops-delete-failed", &a),
                ))
            }
            (Ok(_), None) => None,
        }
    }
}
