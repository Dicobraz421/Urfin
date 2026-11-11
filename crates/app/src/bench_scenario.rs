//! `--bench-scenario <файл.toml>` (S9-05): сценарий без окна и GPU — Core с настоящими
//! сервисами (VFS, ops), путь как у `--smoke`. Шаги — `list`, `sort`, `copy`, `move`,
//! `delete`; итог — строка `bench scenario k=v` и строка JSON со стабильными ключами
//! (`urfin_ui::bench::json_line`): `t_first` и `t_done` первого `list`, `t_sorted` первого
//! `sort`, `peak_rss`, `allocs` (сборка `count-alloc`).
//!
//! ```toml
//! # Пути — от каталога файла сценария (или абсолютные).
//! [[step]]
//! op = "list"          # открыть каталог в панели: t_first — первая порция, t_done — весь
//! path = "src"
//! [[step]]
//! op = "sort"          # panel.sort: t_sorted — новый порядок на экране
//! key = "size"
//! [[step]]
//! op = "copy"          # ops.copy каталога/файла `from` в каталог `to` (move — так же)
//! from = "src"
//! to = "out"
//! [[step]]
//! op = "delete"        # ops.delete насовсем
//! path = "out/src"
//! ```
//!
//! `toml` не в зависимостях `urfin-app` (Cargo.toml — полоса deps): файл читает загрузчик
//! конфига core — копия во временном каталоге как `config.toml`, шаги — секция `step`.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use serde::Deserialize;
use urfin_core::{Boot, Command, Config, Core, JobState, ListingStatus, Location, Paths, Waker};

/// Шаг сценария.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Step {
    pub op: String,
    pub path: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub key: Option<String>,
}

/// Предел ожидания одного шага.
const STEP_TIMEOUT: Duration = Duration::from_secs(600);

/// Шаги из файла сценария.
pub(crate) fn load(file: &Path) -> anyhow::Result<Vec<Step>> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("cannot read scenario {}", file.display()))?;
    let dir = std::env::temp_dir().join(format!("urfin-scenario-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let paths = Paths::in_dir(&dir);
    let written = std::fs::write(paths.config_file(), text);
    let (config, warnings) = Config::load(&paths);
    let _ = std::fs::remove_dir_all(&dir);
    written?;
    if let Some(w) = warnings.first() {
        bail!("scenario {}: {w:?}", file.display());
    }
    let (steps, warning) = config.section::<Vec<Step>>("step");
    if let Some(w) = warning {
        bail!("scenario {}: {w:?}", file.display());
    }
    ensure!(
        !steps.is_empty(),
        "scenario {}: no [[step]]",
        file.display()
    );
    Ok(steps)
}

/// Будит цикл сценария, когда воркеры прислали `Msg`.
#[derive(Default)]
struct Wake {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Wake {
    fn wait(&self, max: Duration) {
        let Ok(g) = self.flag.lock() else { return };
        let (mut g, _) = self
            .cv
            .wait_timeout_while(g, max, |woken| !*woken)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *g = false;
    }
}

impl Waker for Wake {
    fn wake(&self) {
        if let Ok(mut g) = self.flag.lock() {
            *g = true;
            self.cv.notify_one();
        }
    }
    fn wake_after(&self, _d: Duration) {
        // Цикл и так просыпается не реже раза в 2 мс.
    }
}

/// Прогон: Core без окна, шаги по порядку, отчёт.
struct Run {
    core: Core,
    wake: Arc<Wake>,
    base: PathBuf,
}

impl Run {
    /// Качать pump, пока не выполнится условие.
    fn until(&mut self, what: &str, done: impl Fn(&Core) -> bool) -> anyhow::Result<Duration> {
        let t = Instant::now();
        loop {
            self.core.pump(Duration::from_millis(20));
            if done(&self.core) {
                return Ok(t.elapsed());
            }
            ensure!(t.elapsed() < STEP_TIMEOUT, "scenario: timeout: {what}");
            self.wake.wait(Duration::from_millis(2));
        }
    }

    fn abs(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        let p = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base.join(p)
        };
        std::path::absolute(&p).unwrap_or(p)
    }

    /// Открыть каталог в активной панели: (первая порция, весь листинг).
    fn list(&mut self, dir: &Path) -> anyhow::Result<(Duration, Duration)> {
        let loc = Location::local(dir);
        let t = Instant::now();
        self.core
            .dispatch(Command::new("panel.go").arg("path", loc.clone()));
        let first = self.until("first chunk", |c| {
            let tab = c.state().active_tab();
            tab.loc == loc && (!tab.listing.entries.is_empty() || listed(tab))
        })?;
        self.until("listing done", |c| {
            let tab = c.state().active_tab();
            tab.loc == loc && listed(tab)
        })?;
        let tab = self.core.state().active_tab();
        if let ListingStatus::Failed(e) = &tab.listing.status {
            bail!("scenario: cannot list {}: {e}", dir.display());
        }
        Ok((first.min(t.elapsed()), t.elapsed()))
    }

    fn sort(&mut self, key: &str) -> anyhow::Result<Duration> {
        let before = Arc::clone(&self.core.state().active_tab().listing.order);
        let t = Instant::now();
        self.core
            .dispatch(Command::new("panel.sort").arg("key", key));
        self.until("sorted", |c| {
            let l = &c.state().active_tab().listing;
            !Arc::ptr_eq(&l.order, &before) && matches!(l.status, ListingStatus::Complete)
        })?;
        Ok(t.elapsed())
    }

    /// Курсор на `item` в его каталоге (активная панель).
    fn point(&mut self, item: &Path) -> anyhow::Result<()> {
        let (Some(dir), Some(name)) = (item.parent(), item.file_name()) else {
            bail!("scenario: no parent: {}", item.display());
        };
        self.list(dir)?;
        let name = name.to_string_lossy().into_owned();
        ensure!(
            self.core
                .state()
                .active_tab()
                .listing
                .entries
                .iter()
                .any(|e| e.name == name.as_str()),
            "scenario: not found: {}",
            item.display()
        );
        self.core
            .dispatch(Command::new("panel.cursor.to").arg("name", name.as_str()));
        Ok(())
    }

    /// Команда операции над `item` и ожидание конца её job.
    fn job(&mut self, item: &Path, cmd: Command) -> anyhow::Result<Duration> {
        self.point(item)?;
        let known: Vec<_> = self.core.state().jobs.iter().map(|j| j.id).collect();
        let kind = cmd.id.to_string();
        let t = Instant::now();
        self.core.dispatch(cmd);
        self.until(&kind, |c| {
            c.state()
                .jobs
                .iter()
                .any(|j| !known.contains(&j.id) && !j.state.is_active())
        })?;
        let took = t.elapsed();
        let job = self
            .core
            .state()
            .jobs
            .iter()
            .find(|j| !known.contains(&j.id))
            .context("job")?;
        match &job.state {
            JobState::Done(sum) if sum.errors == 0 => Ok(took),
            s => bail!("scenario: {kind} {}: {s:?}", item.display()),
        }
    }

    fn dest(&self, step: &Step) -> anyhow::Result<String> {
        let to = self.abs(step.to.as_deref().context("scenario: `to` missing")?);
        std::fs::create_dir_all(&to).with_context(|| format!("cannot create {}", to.display()))?;
        // Каталог назначения — со слешем: «внутрь».
        Ok(format!("{}{}", to.display(), std::path::MAIN_SEPARATOR))
    }
}

fn listed(tab: &urfin_core::Tab) -> bool {
    matches!(
        tab.listing.status,
        ListingStatus::Complete | ListingStatus::Failed(_)
    )
}

/// Выполнить сценарий: строка `k=v` и строка JSON.
pub(crate) fn run(
    file: &Path,
    config: &Config,
    scratch: &Path,
) -> anyhow::Result<(String, String)> {
    let steps = load(file)?;
    let base = std::path::absolute(file)?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let home = Location::local(&base);
    let mut boot = Boot::new(home.clone(), home);
    boot.paths = Paths::in_dir(scratch);
    boot.config = config.clone();
    let wake = Arc::new(Wake::default());
    let mut core = Core::new(
        boot,
        crate::vfs_services(config, false),
        Arc::clone(&wake) as Arc<dyn Waker>,
    );
    crate::register_handlers(&mut core, config, None, urfin_ui::ThemeEnv::default());
    let mut run = Run { core, wake, base };
    run.until("start", |c| listed(c.state().active_tab()))?;

    let allocs0 = urfin_ui::bench::ALLOCS.load(Ordering::Relaxed);
    let t0 = Instant::now();
    let mut kv = vec![format!("steps={}", steps.len())];
    let mut known: Vec<(&str, f64)> = Vec::new();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let put = |known: &mut Vec<(&'static str, f64)>, k: &'static str, d: Duration| {
        if !known.iter().any(|(n, _)| *n == k) {
            known.push((k, ms(d)));
        }
    };
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let took = match step.op.as_str() {
            "list" => {
                let dir = run.abs(step.path.as_deref().context("scenario: `path` missing")?);
                let (first, done) = run.list(&dir)?;
                put(&mut known, "t_first", first);
                put(&mut known, "t_done", done);
                kv.push(format!(
                    "s{n}_list_rows={}",
                    run.core.state().active_tab().listing.entries.len()
                ));
                done
            }
            "sort" => {
                let d = run.sort(step.key.as_deref().unwrap_or("name"))?;
                put(&mut known, "t_sorted", d);
                d
            }
            "copy" | "move" => {
                let from = run.abs(step.from.as_deref().context("scenario: `from` missing")?);
                let dest = run.dest(step)?;
                let cmd = Command::new(if step.op == "copy" {
                    "ops.copy"
                } else {
                    "ops.move"
                })
                .arg("dest", dest.as_str())
                .arg("confirmed", true);
                run.job(&from, cmd)?
            }
            "delete" => {
                let item = run.abs(step.path.as_deref().context("scenario: `path` missing")?);
                let cmd = Command::new("ops.delete")
                    .arg("permanent", true)
                    .arg("confirmed", true);
                run.job(&item, cmd)?
            }
            op => bail!("scenario: unknown op `{op}` (list, sort, copy, move, delete)"),
        };
        kv.push(format!("s{n}_{}_ms={:.3}", step.op, ms(took)));
    }
    kv.push(format!("total_ms={:.3}", ms(t0.elapsed())));
    let allocs = urfin_ui::bench::ALLOCS.load(Ordering::Relaxed) - allocs0;
    if allocs0 > 0 {
        #[allow(clippy::cast_precision_loss, reason = "штуки в JSON")]
        known.push(("allocs", allocs as f64));
    }
    let _ = run.core.shutdown(Duration::from_millis(500));
    let report = kv.join(" ");
    let json = urfin_ui::bench::json_line("scenario", &report, &known);
    Ok((report, json))
}

#[cfg(test)]
mod tests {
    /// Строка JSON каждого режима разбирается; стабильные ключи — всегда, `raw` — ключи
    /// строки `k=v` режима (S9-05).
    #[test]
    fn bench_json_of_every_mode_parses() {
        let reports = [
            (
                "ui",
                "rows=100000 list_p50=1.25 list_p99=3.50 steady_allocs_p50=0",
            ),
            ("editor", "frames_ms=12,13 scroll_p50=0.80 scroll_p99=2.10"),
            ("viewer", "frames_ms=9 page_p50=0.70 page_p99=1.90"),
            ("search", "rows=1000000 page_p50=0.40 page_p99=1.10"),
            ("startup", "startup.paths=0.120 startup.first_frame=180.500"),
            ("scenario", "steps=5 s1_list_ms=12.0"),
        ];
        for (mode, report) in reports {
            let known: &[(&str, f64)] = if mode == "scenario" {
                &[("t_first", 1.5), ("t_done", 4.0), ("t_sorted", 6.0)]
            } else {
                &[]
            };
            let line = urfin_ui::bench::json_line(mode, report, known);
            let v: serde_json::Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("{mode}: {e}: {line}"));
            assert_eq!(v["bench"], mode, "{line}");
            for k in urfin_ui::bench::JSON_KEYS {
                assert!(v.get(k).is_some(), "{mode}: no {k}: {line}");
            }
            for (k, _) in report.split(' ').filter_map(|kv| kv.split_once('=')) {
                assert!(v["raw"].get(k).is_some(), "{mode}: raw {k}: {line}");
            }
            let p99 = match mode {
                "ui" => Some("list_p99"),
                "editor" => Some("scroll_p99"),
                "viewer" | "search" => Some("page_p99"),
                _ => None,
            };
            match p99 {
                Some(k) => assert_eq!(v["frame_p99"], v["raw"][k], "{line}"),
                None => assert!(v["frame_p99"].is_null(), "{line}"),
            }
            if mode == "scenario" {
                assert_eq!(v["t_sorted"].as_f64(), Some(6.0), "{line}");
            }
        }
    }

    /// Малое дерево как у `gen_tree` (`urfin-testdata` не в зависимостях app): вложенные
    /// каталоги, пустые и разные по размеру файлы, имена не ASCII.
    fn small_tree(root: &std::path::Path) {
        for d in 0..4u32 {
            let dir = root.join(format!("dir-{d}")).join(format!("вложенный-{d}"));
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..12u32 {
                let n = (d * 12 + f) as usize;
                #[allow(clippy::cast_possible_truncation, reason = "байты образца")]
                let body: Vec<u8> = (0..n * 997).map(|i| (i * 31 + n) as u8).collect();
                std::fs::write(dir.join(format!("файл {f}.bin")), body).unwrap();
            }
        }
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("readme.txt"), b"urfin").unwrap();
    }

    /// Относительный путь → содержимое (каталоги — `None`).
    fn snapshot(root: &std::path::Path) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
        let mut out = std::collections::BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                if p.is_dir() {
                    out.insert(rel, None);
                    stack.push(p);
                } else {
                    out.insert(rel, Some(std::fs::read(&p).unwrap()));
                }
            }
        }
        out
    }

    /// Приёмка S9-05: copy малого дерева сценарием даёт те же файлы, что исходник; move и
    /// delete доходят до конца; строка JSON разбирается, времена листинга и сортировки есть.
    /// Окна нет: Core без GPUI.
    #[test]
    fn scenario_copy_gives_same_files() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        small_tree(&src);
        let f = tmp.path().join("scenario.toml");
        std::fs::write(
            &f,
            r#"
[[step]]
op = "list"
path = "src"
[[step]]
op = "sort"
key = "size"
[[step]]
op = "copy"
from = "src"
to = "out"
[[step]]
op = "copy"
from = "src"
to = "tmp"
[[step]]
op = "move"
from = "tmp/src"
to = "moved"
[[step]]
op = "delete"
path = "moved/src"
"#,
        )
        .unwrap();
        let scratch = tmp.path().join("scratch");
        let (report, json) =
            super::run(&f, &urfin_core::Config::default(), &scratch).expect("scenario");
        assert_eq!(snapshot(&tmp.path().join("out/src")), snapshot(&src));
        assert!(!tmp.path().join("tmp/src").exists(), "moved away");
        assert!(!tmp.path().join("moved/src").exists(), "deleted");
        assert!(report.contains("s3_copy_ms="), "{report}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["bench"], "scenario");
        for k in ["t_first", "t_done", "t_sorted", "peak_rss"] {
            assert!(v[k].as_f64().is_some_and(|t| t >= 0.0), "{k}: {json}");
        }
        assert!(v["frame_p50"].is_null(), "{json}");
    }

    /// Шаги читаются из TOML; неизвестный ключ — не ошибка, пустой файл — ошибка.
    #[test]
    fn scenario_file_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("s.toml");
        std::fs::write(
            &f,
            "[[step]]\nop = \"list\"\npath = \"src\"\n[[step]]\nop = \"copy\"\nfrom = \"src\"\nto = \"out\"\n",
        )
        .unwrap();
        let steps = super::load(&f).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(
            (steps[1].op.as_str(), steps[1].to.as_deref()),
            ("copy", Some("out"))
        );
        std::fs::write(&f, "# пусто\n").unwrap();
        assert!(super::load(&f).is_err());
    }
}
