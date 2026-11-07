//! В корзину (S0 №11, S5 §4.7, S5-13) на движке job. План — один пакет Shell
//! (`IFileOperation`, STA на потоке координатора) для всех элементов. То, что в
//! корзину нельзя (удалилось бы насовсем: больше лимита корзины, сетевой диск,
//! длинный путь), не трогается и паркуется вопросом `NoTrash`: «Удалить
//! насовсем» (выполняет воркер), «Пропустить», «Отмена»; «для всех» — один ответ.
//! Ссылки и junction Shell удаляет как ссылки (цель не трогается).

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use compact_str::CompactString;
use urfin_core::{Cx, JobId, JobSpec, JobSummary, Lane, Level, Location};

use crate::api::{
    FailedItem, IoClass, Issue, IssueId, IssueKind, ItemMeta, ItemOp, Report, Resolution,
};
use crate::engine::{
    DirNode, ExecCx, ItemResult, JobEngine, Note, OpExec, Plan, PlanError, Resolved, Work,
};

/// Воркеров на «насовсем» после вопроса.
const WORKERS: u16 = 2;

/// Работа воркера: удалить насовсем (ответ на `NoTrash`).
#[derive(Debug)]
pub(crate) struct Item {
    path: PathBuf,
}

/// Операция «в корзину».
#[derive(Debug)]
pub(crate) struct Trash {
    /// Каталог источников: обновляется по ходу (у результатов поиска — панель `search:`).
    base: Location,
    /// Полные пути элементов.
    paths: Vec<PathBuf>,
    /// Быстрая корзина (S5-26, №79): сначала напрямую в `$Recycle.Bin`.
    fast: bool,
}

/// Из handler (UI-поток, без I/O): job «в корзину».
pub(crate) fn start(
    eng: &JobEngine,
    cx: &mut Cx<'_>,
    base: Location,
    items: &[CompactString],
    fast: bool,
) -> JobId {
    let paths = items.iter().map(|n| base.path.join(n.as_str())).collect();
    start_paths(eng, cx, base, items, paths, fast)
}

/// Как [`start`], но с готовыми путями (результаты поиска, S8-12): элементы из разных
/// каталогов, `names` — имена строк для названия задачи, `base` — что обновить.
pub(crate) fn start_paths(
    eng: &JobEngine,
    cx: &mut Cx<'_>,
    base: Location,
    names: &[CompactString],
    paths: Vec<PathBuf>,
    fast: bool,
) -> JobId {
    let items = names;
    let spec = JobSpec {
        kind: "trash",
        title: {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("n", items.len());
            a.set(
                "name",
                items.first().map_or_else(String::new, ToString::to_string),
            );
            urfin_i18n::current().fmt("ops-job-trash-what", &a)
        },
        lane: Lane::Bulk,
        priority: 0,
        owner_ns: "ops",
    };
    eng.start(cx, spec, WORKERS, Trash { base, paths, fast })
}

fn tr(key: &str, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

fn failed(path: &std::path::Path, error: String) -> FailedItem {
    FailedItem {
        path: path.to_path_buf(),
        op: ItemOp::Trash,
        error: error.into(),
        class: IoClass::Other,
        pair: None,
    }
}

impl OpExec for Trash {
    type Item = Item;
    type Dir = ();

    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        let paths = self.paths.clone();
        let (fast_done, out) = {
            let cx = p.cx();
            cx.progress().set_phase(urfin_core::Phase::Transfer);
            // Быстрая корзина (S5-26): что перенесено напрямую — готово; остальное и
            // любые ошибки — обычным путём Shell.
            let (fast_done, paths) = if self.fast && !crate::sys::recycle_fast::policy_off() {
                let f = crate::sys::recycle_fast::recycle(&paths);
                (f.moved.len(), f.rest)
            } else {
                (0, paths)
            };
            let out = match crate::sys::trash_win::recycle(&paths, &cx.cancel_token()) {
                Ok(o) => o,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    return Err(PlanError::Cancelled);
                }
                Err(e) => return Err(PlanError::Failed(e.to_string().into())),
            };
            cx.touch(&self.base);
            let progress = cx.progress();
            let n = (fast_done + out.recycled.len() + out.failed.len()) as u64;
            progress.items_total.fetch_add(n, Ordering::Relaxed);
            progress.items_done.fetch_add(n, Ordering::Relaxed);
            (fast_done, out)
        };
        for _ in 0..fast_done + out.recycled.len() {
            p.record(None, ItemResult::Done);
        }
        for (path, e) in out.failed {
            p.record(None, ItemResult::Failed(failed(&path, e)));
        }
        for path in out.no_trash {
            let meta = std::fs::symlink_metadata(&path).ok();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let issue = Issue {
                id: IssueId(0),
                kind: IssueKind::NoTrash,
                src: Some(ItemMeta {
                    name: name.as_ref().into(),
                    size: meta.as_ref().map_or(0, std::fs::Metadata::len),
                    mtime: i64::MIN,
                    attrs: 0,
                }),
                dst: None,
                op: ItemOp::Trash,
                temp: None,
                error: Some(tr("ops-no-trash-why", &[]).into()),
                path: Some(path.clone()),
                code: None,
            };
            p.park(None, Item { path }, 0, issue)?;
        }
        Ok(())
    }

    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult {
        let r = crate::ops::delete::remove(&work.item.path);
        cx.touch(&self.base);
        match r {
            Ok(()) => ItemResult::Done,
            Err(e) => ItemResult::Failed(failed(&work.item.path, e.to_string())),
        }
    }

    fn finalize_dir(&self, _dir: &DirNode<()>, _cx: &ExecCx<'_>) -> ItemResult {
        ItemResult::Done
    }

    fn resolve(&self, item: Item, _issue: &Issue, r: &Resolution) -> Resolved<Item> {
        if *r == Resolution::DeletePermanently {
            Resolved::Run(item)
        } else {
            Resolved::Done(ItemResult::Skipped)
        }
    }

    fn finished(&self, r: &Report, outcome: &Result<JobSummary, String>) -> Option<Note> {
        let s = match outcome {
            Err(e) if e == "cancelled" => return None,
            Err(e) => return Some((Level::Error, tr("ops-delete-failed", &[("error", e)]))),
            Ok(s) => s,
        };
        if let Some(f) = r.failed.first() {
            let first = format!("{}: {}", f.path.display(), f.error);
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("n", s.errors);
            a.set("first", first);
            return Some((
                Level::Error,
                urfin_i18n::current().fmt("ops-copy-errors", &a),
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use urfin_core::{Command, JobState};

    use super::*;
    use crate::engine::tests::H;

    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("urfin-").tempdir().unwrap()
    }

    fn trash(base: &Path, items: &[&str]) -> Trash {
        Trash {
            base: Location::local(base),
            paths: items.iter().map(|s| base.join(s)).collect(),
            fast: false,
        }
    }

    /// Ждёт конца job, отвечая `choice` на вопросы; число вопросов.
    fn finish(h: &mut H, id: JobId, choice: &str, all: bool) -> u32 {
        let mut asked = 0;
        let t0 = Instant::now();
        while h.state(id).is_active() {
            assert!(t0.elapsed() < Duration::from_secs(60), "timeout");
            if let Some(p) = h.core.state().prompts.active() {
                asked += 1;
                let pid = i64::try_from(p.id.0).unwrap();
                h.core.dispatch(
                    Command::new("prompt.reply")
                        .arg("id", pid)
                        .arg("choice", choice)
                        .arg("all", all),
                );
            }
            h.pump();
        }
        asked
    }

    /// Каталог с путём длиннее `MAX_PATH` (Shell его не принимает).
    fn long_dir(root: &Path) -> std::path::PathBuf {
        let mut p = root.to_path_buf();
        while p.as_os_str().len() < 280 {
            p.push("каталог-с-длинным-именем-для-проверки");
        }
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Приёмка S5-13: в корзину нельзя (здесь — длинный путь) → вопрос `NoTrash`,
    /// а не тихое удаление; «Пропустить» оставляет, «Удалить насовсем» удаляет.
    #[test]
    fn no_trash_asks_instead_of_deleting() {
        let d = tmp();
        let dir = long_dir(d.path());
        for n in ["a.txt", "b.txt"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let mut h = H::new(64);
        let id = h.start(2, trash(&dir, &["a.txt"]));
        h.pump_until("question", |h| h.core.state().prompts.active().is_some());
        let spec = h.core.state().prompts.active().unwrap().spec.clone();
        assert_eq!(spec.choices[usize::from(spec.default)].id, "skip");
        assert_eq!(spec.choices[0].id, "delete");
        assert_eq!(finish(&mut h, id, "skip", false), 1);
        assert!(dir.join("a.txt").exists(), "skipped: left in place");
        match h.state(id) {
            JobState::Done(s) => assert_eq!((s.items, s.skipped), (0, 1)),
            st => panic!("{st:?}"),
        }
        let id = h.start(2, trash(&dir, &["a.txt", "b.txt"]));
        assert_eq!(finish(&mut h, id, "delete", true), 1, "one answer for all");
        assert!(!dir.join("a.txt").exists() && !dir.join("b.txt").exists());
        match h.state(id) {
            JobState::Done(s) => assert_eq!((s.items, s.errors), (2, 0)),
            st => panic!("{st:?}"),
        }
    }

    /// В корзину пакетом: файлы, папка, read-only — исчезли из каталога. Кладёт
    /// файлы в корзину ОС: только в CI (`URFIN_TEST_RECYCLE=1`).
    #[test]
    fn recycles_in_one_batch() {
        if std::env::var_os("URFIN_TEST_RECYCLE").is_none() {
            return;
        }
        let d = tmp();
        let p = d.path();
        std::fs::write(p.join("a.txt"), b"a").unwrap();
        std::fs::create_dir_all(p.join("dir/sub")).unwrap();
        std::fs::write(p.join("dir/sub/b.txt"), b"b").unwrap();
        std::fs::write(p.join("ro.txt"), b"r").unwrap();
        let mut perm = std::fs::metadata(p.join("ro.txt")).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(p.join("ro.txt"), perm).unwrap();
        let mut h = H::new(64);
        let id = h.start(2, trash(p, &["a.txt", "dir", "ro.txt"]));
        assert_eq!(finish(&mut h, id, "skip", false), 0);
        match h.state(id) {
            JobState::Done(s) => assert_eq!((s.items, s.errors), (3, 0)),
            st => panic!("{st:?}"),
        }
        for n in ["a.txt", "dir", "ro.txt"] {
            assert!(!p.join(n).exists(), "{n}");
        }
    }

    /// Приёмка S5-13 на стенде: сетевой путь (`\\localhost\C$\…` к своему
    /// временному файлу) → Shell не даёт корзину → вопрос `NoTrash`, файл цел.
    /// Нужен доступ к административной шаре `C$`.
    #[test]
    #[ignore = "стенд: нужна административная шара C$"]
    fn network_path_is_no_trash() {
        let d = tmp();
        std::fs::write(d.path().join("net.txt"), b"x").unwrap();
        let local = d.path().display().to_string();
        let unc = format!(r"\\localhost\{}${}", &local[..1], &local[2..]);
        let mut h = H::new(64);
        let id = h.start(2, trash(Path::new(&unc), &["net.txt"]));
        let asked = finish(&mut h, id, "skip", false);
        assert_eq!(asked, 1, "NoTrash question");
        assert!(d.path().join("net.txt").exists(), "not deleted");
    }

    /// Замер S5-13 (кладёт 10k файлов в корзину ОС — запускать осознанно):
    /// `cargo nextest run --release -p urfin-ops ten_thousand --run-ignored only --no-capture`.
    #[test]
    #[ignore = "кладёт 10k файлов в корзину ОС"]
    fn ten_thousand_in_one_batch() {
        let d = tmp();
        let names: Vec<String> = (0..10_000).map(|i| format!("f{i:05}.txt")).collect();
        for n in &names {
            std::fs::write(d.path().join(n), b"x").unwrap();
        }
        let items: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut h = H::new(64);
        let t0 = Instant::now();
        let id = h.start(2, trash(d.path(), &items));
        assert_eq!(finish(&mut h, id, "skip", false), 0);
        eprintln!("trash 10k: {:?}", t0.elapsed());
        match h.state(id) {
            JobState::Done(s) => assert_eq!(s.items, 10_000),
            st => panic!("{st:?}"),
        }
    }
}
