//! Ссылки (S5 §4.14, S5-22): жёсткая (только файл), символическая, junction
//! (только каталог, без прав администратора). По умолчанию: файл — жёсткая,
//! каталог — junction. Symlink без прав (режим разработчика выключен) для
//! каталога — вопрос «Создать junction».

use std::path::{Path, PathBuf};

use urfin_core::{Cx, JobId, JobSpec, JobSummary, Lane, Level, Location};

use crate::api::{
    FailedItem, IoClass, Issue, IssueId, IssueKind, ItemMeta, ItemOp, Report, Resolution,
};
use crate::engine::{
    DirNode, ExecCx, ItemResult, JobEngine, Note, OpExec, Plan, PlanError, Resolved, Work,
};

/// Вид ссылки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Hard,
    Sym,
    Junction,
}

impl Kind {
    pub(crate) fn parse(s: &str) -> Option<Kind> {
        match s {
            "hard" => Some(Kind::Hard),
            "sym" | "symlink" => Some(Kind::Sym),
            "junction" => Some(Kind::Junction),
            _ => None,
        }
    }
}

/// `ERROR_PRIVILEGE_NOT_HELD`.
const NO_PRIVILEGE: i32 = 1314;

/// Работа воркера: junction после ответа.
#[derive(Debug)]
pub(crate) struct Item {
    src: PathBuf,
    dst: PathBuf,
}

/// Операция «создать ссылку».
#[derive(Debug)]
pub(crate) struct Link {
    /// `None` — по виду элемента.
    pub kind: Option<Kind>,
    /// Цель ссылки — элемент.
    pub src: PathBuf,
    /// Где создать: каталог (тогда — с именем элемента) или полный путь.
    pub dest: PathBuf,
    pub touch: [Location; 2],
}

/// Из handler (UI-поток): job «создать ссылку».
pub(crate) fn start(eng: &JobEngine, cx: &mut Cx<'_>, link: Link) -> JobId {
    let spec = JobSpec {
        kind: "link",
        title: urfin_i18n::current().get("ops-job-link"),
        lane: Lane::Interactive,
        priority: 1,
        owner_ns: "ops",
    };
    eng.start(cx, spec, 1, link)
}

fn tr(key: &str) -> String {
    urfin_i18n::current().get(key)
}

fn failed(p: &Path, error: &str) -> FailedItem {
    FailedItem {
        path: p.to_path_buf(),
        op: ItemOp::Link,
        error: error.into(),
        class: IoClass::Other,
        pair: None,
    }
}

/// Создать ссылку вида `kind` на `src` в `dst`.
pub(crate) fn create(kind: Kind, src: &Path, dst: &Path, dir: bool) -> std::io::Result<()> {
    match kind {
        Kind::Hard if dir => Err(std::io::Error::other(tr("ops-link-hard-dir"))),
        Kind::Hard => std::fs::hard_link(src, dst),
        Kind::Sym if dir => std::os::windows::fs::symlink_dir(src, dst),
        Kind::Sym => std::os::windows::fs::symlink_file(src, dst),
        Kind::Junction if !dir => Err(std::io::Error::other(tr("ops-link-junction-file"))),
        Kind::Junction => crate::sys::fs::make_junction(dst, src),
    }
}

impl OpExec for Link {
    type Item = Item;
    type Dir = ();

    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        let src = crate::names::os_path(&self.src);
        let dir = std::fs::metadata(&src).is_ok_and(|m| m.is_dir());
        let mut dst = crate::names::os_path(&self.dest);
        if dst.is_dir() {
            dst.push(self.src.file_name().unwrap_or_default());
        }
        let kind = self
            .kind
            .unwrap_or(if dir { Kind::Junction } else { Kind::Hard });
        match create(kind, &src, &dst, dir) {
            Ok(()) => p.record(None, ItemResult::Done),
            Err(e) if kind == Kind::Sym && dir && e.raw_os_error() == Some(NO_PRIVILEGE) => {
                let name = self.src.file_name().unwrap_or_default().to_string_lossy();
                let issue = Issue {
                    id: IssueId(0),
                    kind: IssueKind::Io(IoClass::Permission),
                    src: Some(ItemMeta {
                        name: name.as_ref().into(),
                        size: 0,
                        mtime: i64::MIN,
                        attrs: 0,
                    }),
                    dst: None,
                    op: ItemOp::Link,
                    temp: None,
                    error: Some(tr("ops-link-no-privilege").into()),
                    path: None,
                    code: None,
                };
                p.park(None, Item { src, dst }, 0, issue)?;
            }
            Err(e) => p.record(None, ItemResult::Failed(failed(&dst, &e.to_string()))),
        }
        for l in &self.touch {
            p.cx().touch(l);
        }
        Ok(())
    }

    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult {
        let it = &work.item;
        for l in &self.touch {
            cx.touch(l);
        }
        match crate::sys::fs::make_junction(&it.dst, &it.src) {
            Ok(()) => ItemResult::Done,
            Err(e) => ItemResult::Failed(failed(&it.dst, &e.to_string())),
        }
    }

    fn finalize_dir(&self, _dir: &DirNode<()>, _cx: &ExecCx<'_>) -> ItemResult {
        ItemResult::Done
    }

    fn place(&self, item: &Item) -> Option<PathBuf> {
        Some(item.dst.clone())
    }

    fn resolve(&self, item: Item, _issue: &Issue, r: &Resolution) -> Resolved<Item> {
        if *r == Resolution::Junction {
            Resolved::Run(item)
        } else {
            Resolved::Done(ItemResult::Skipped)
        }
    }

    fn finished(&self, r: &Report, outcome: &Result<JobSummary, String>) -> Option<Note> {
        outcome.as_ref().ok()?;
        let f = r.failed.first()?;
        Some((Level::Error, format!("{}: {}", f.path.display(), f.error)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("urfin-").tempdir().unwrap()
    }

    #[test]
    fn hard_link_and_junction() {
        let d = tmp();
        let f = d.path().join("a.txt");
        std::fs::write(&f, b"one").unwrap();
        create(Kind::Hard, &f, &d.path().join("h.txt"), false).unwrap();
        std::fs::write(d.path().join("h.txt"), b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two", "same file");
        let target = d.path().join("dir");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("x"), b"x").unwrap();
        let j = d.path().join("j");
        create(Kind::Junction, &target, &j, true).unwrap();
        assert_eq!(
            std::fs::read(j.join("x")).unwrap(),
            b"x",
            "junction leads to the dir"
        );
        assert!(
            std::fs::symlink_metadata(&j)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(create(Kind::Hard, &target, &d.path().join("hd"), true).is_err());
        assert!(create(Kind::Junction, &f, &d.path().join("jf"), false).is_err());
        std::fs::remove_dir(&j).unwrap();
        assert!(
            target.join("x").exists(),
            "removing the junction keeps the target"
        );
    }
}
