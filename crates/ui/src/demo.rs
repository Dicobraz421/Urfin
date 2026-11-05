//! Задачи для сцен `--screens` (D-06): job-бару нужны идущие задачи, а демо-дерево
//! копируется мгновенно. Handler регистрирует только app под `--screens`, поэтому в
//! реестре команд и `docs/COMMANDS.md` его нет. `screens.jobs` — запустить три задачи,
//! которые медленно «идут» сами, `stop=true` — отменить их. `screens.error kind=access|busy`
//! — вопрос об ошибке операции (D-04), `screens.banner` — баннер журнала,
//! `screens.term kind=frames|unicode|colors|termhl|sshrec` — экран терминала за спрятанными панелями
//! (S6-06); `stop=true` — убрать. `screens.edit` — правки документа редактора сцены
//! `screen-editor` (S7-16), `kind=goto|replace` — тексты строк перехода и замены сцен
//! `screen-editgoto` / `screen-editreplace` (S7-18). `screens.du state=scan|done|cached` —
//! вкладка анализа места на фикстуре (F01-05), `stop=true` — закрыть. `screens.pkg` —
//! вкладка пакетов на фикстуре (F02-08), `stop=true` — закрыть.

use std::sync::atomic::Ordering;
use std::time::Duration;

use urfin_core::{
    ArgSpec, CmdError, Command, CommandMeta, Cx, Handler, JobId, JobSpec, Lane, ValueKind,
};
use urfin_i18n::FluentArgs;

const STOP: ArgSpec = ArgSpec {
    name: "stop",
    kind: ValueKind::Bool,
    required: false,
};

/// `screens.pkg step=search|install|banner|settings|task` — сцены поиска пакетов (F02-09, F02-10) и
/// баннер проверки по расписанию (F02-11).
const STEP: ArgSpec = ArgSpec {
    name: "step",
    kind: ValueKind::Str,
    required: false,
};

static COMMANDS: &[CommandMeta] = &[
    CommandMeta {
        id: "screens.ssh",
        title: "Demo SSH profiles and questions (--screens)",
        category: "screens",
        args: &[
            STOP,
            ArgSpec {
                name: "kind",
                kind: ValueKind::Str,
                required: false,
            },
        ],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.du",
        title: "Demo disk usage tab (--screens)",
        category: "screens",
        args: &[
            STOP,
            ArgSpec {
                name: "state",
                kind: ValueKind::Str,
                required: false,
            },
        ],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.pkg",
        title: "Demo packages tab (--screens)",
        category: "screens",
        args: &[STOP, STEP],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.jobs",
        title: "Demo tasks (--screens)",
        category: "screens",
        args: &[
            STOP,
            ArgSpec {
                name: "kind",
                kind: ValueKind::Str,
                required: false,
            },
        ],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.error",
        title: "Demo operation error (--screens)",
        category: "screens",
        args: &[
            STOP,
            ArgSpec {
                name: "kind",
                kind: ValueKind::Str,
                required: false,
            },
        ],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.banner",
        title: "Demo journal banner (--screens)",
        category: "screens",
        args: &[STOP],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.edit",
        title: "Demo editor edits (--screens)",
        category: "screens",
        args: &[ArgSpec {
            name: "kind",
            kind: ValueKind::Str,
            required: false,
        }],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.quick_focus",
        title: "Focus quick view of the right panel (--screens)",
        category: "screens",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.cast",
        title: "Demo session recording (--screens)",
        category: "screens",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "screens.term",
        title: "Demo terminal screen (--screens)",
        category: "screens",
        args: &[
            STOP,
            ArgSpec {
                name: "kind",
                kind: ValueKind::Str,
                required: false,
            },
        ],
        repeatable: false,
    },
];

/// Демо-задачи сцен.
#[derive(Default)]
pub struct DemoJobs {
    /// Сколько раз показан экран терминала (`dense` в `--bench` каждый раз другой).
    term_seed: usize,
    ids: Vec<JobId>,
    prompts: Vec<urfin_core::PromptId>,
}

/// Зарегистрировать демо-задачи (только `--screens`).
pub fn register_demo(core: &mut urfin_core::Core) {
    core.register(Box::new(DemoJobs::default()));
}

/// (вид, текст «что → куда», всего байт, всего штук, сделано, шаг за 100 мс).
const DEMO: [(&str, u64, u64, u64, u64); 3] = [
    ("copy", 3_650_722_201, 48, 1_288_490_188, 5_000_000),
    ("trash", 0, 12_870, 9_012, 3),
    ("move", 1_073_741_824, 214, 644_245_094, 0),
];

impl Handler for DemoJobs {
    fn namespace(&self) -> &'static str {
        "screens"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        COMMANDS
    }

    #[allow(clippy::too_many_lines, reason = "демо-команды сцен подряд")]
    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "screens.error" => {
                self.error(cmd, cx);
                return Ok(());
            }
            // Правки сцены `screen-editor` делает фича редактора событиями документа (S7-34).
            "screens.edit" => {
                cx.ui(urfin_core::UiEvent::Ext {
                    id: cmd.id.clone(),
                    args: cmd.args.clone(),
                });
                return Ok(());
            }
            // Tab в быстрый просмотр (S7-25, `screen-quickview-enc`): путь документа
            // области известен только в рантайме — сцена не может его написать.
            "screens.quick_focus" => {
                let loc = cx
                    .state
                    .ext
                    .get::<urfin_view::ViewState>()
                    .and_then(|v| v.in_panel(urfin_core::Side::Right))
                    .map(|d| d.loc.clone());
                if let Some(loc) = loc {
                    cx.dispatch(
                        Command::new("viewer.quick")
                            .arg("side", "right")
                            .arg("path", loc)
                            .arg("focus", true),
                    );
                }
                return Ok(());
            }
            "screens.banner" => {
                banner(cmd.args.bool("stop").unwrap_or(false), cx);
                return Ok(());
            }
            // Профили и вопросы SSH на фикстуре (F03-05): `screen-ssh*`.
            "screens.ssh" => {
                crate::demo_ssh::run(
                    cmd.args.str("kind").unwrap_or("clear"),
                    cmd.args.bool("stop").unwrap_or(false),
                    cx,
                );
                return Ok(());
            }
            // Вкладка анализа места на фикстуре (F01-05): `screen-duscan`, `screen-dulist`.
            "screens.du" => {
                crate::demo_du::run(
                    cmd.args.str("state").unwrap_or("done"),
                    cmd.args.bool("stop").unwrap_or(false),
                    cx,
                );
                return Ok(());
            }
            // Вкладка пакетов на фикстуре (F02-08): `screen-pkginstalled`, `screen-pkgupdates`.
            "screens.pkg" => {
                match cmd.args.str("step") {
                    Some("search") => crate::demo_pkg::search(cx),
                    Some("install") => crate::demo_pkg::install(cx),
                    Some("banner") => crate::demo_pkg::banner(cx),
                    Some("settings") => crate::demo_pkg::settings(cx),
                    Some("task") => crate::demo_pkg::task(cx),
                    _ => crate::demo_pkg::run(cmd.args.bool("stop").unwrap_or(false), cx),
                }
                return Ok(());
            }
            // Запись сеанса для сцены `screen-sshrecview`: файл во временной папке, открыт F3.
            "screens.cast" => {
                let dir = std::env::temp_dir()
                    .join("urfin-screens")
                    .join("recordings");
                let path = dir.join("srv1-2026-10-23-1402.cast");
                let written = std::fs::create_dir_all(&dir)
                    .and_then(|()| std::fs::write(&path, crate::demo_term::cast_file()));
                match written {
                    Ok(()) => cx.dispatch(
                        Command::new("viewer.open").arg("path", urfin_core::Location::local(path)),
                    ),
                    Err(e) => tracing::warn!(error = %e, "demo cast file"),
                }
                return Ok(());
            }
            "screens.term" => {
                let kind = cmd
                    .args
                    .str("kind")
                    .filter(|_| !cmd.args.bool("stop").unwrap_or(false));
                self.term_seed += 1;
                let seed = self.term_seed;
                let shared = kind.and_then(|k| crate::demo_term::screen(k, seed));
                let shown = shared.is_some();
                if let Some(t) = cx.state.ext.get_mut::<urfin_term::TermState>() {
                    t.preview = shared;
                    // Запись SSH-сеанса (`screen-sshrec`): обязательна по политике TOML.
                    t.preview_rec = (kind == Some("sshrec")).then(|| {
                        urfin_term::rec::Recording::demo(
                            r"%LOCALAPPDATA%\Urfin\recordings\srv1-2026-10-23-1402.cast",
                            1_258_291,
                            std::time::Duration::from_secs(4 * 60 + 12),
                            Some(urfin_core::PolicySource::Toml),
                        )
                    });
                }
                if let Some(u) = cx.state.ext.get_mut::<crate::UiState>() {
                    u.panels_hidden = shown;
                }
                return Ok(());
            }
            _ => {}
        }
        if cmd.args.bool("stop").unwrap_or(false) {
            for id in self.ids.drain(..) {
                cx.state.jobs.cancel(id);
            }
            return Ok(());
        }
        if cmd.args.str("kind") == Some("sftp") {
            self.sftp_jobs(cx);
            return Ok(());
        }
        let i18n = urfin_i18n::current();
        for (n, (kind, bytes, items, done, step)) in DEMO.into_iter().enumerate() {
            let mut a = FluentArgs::new();
            a.set("n", items);
            a.set("name", "");
            a.set(
                "dest",
                if n == 0 {
                    "\\\\fs01\\share\\presale"
                } else {
                    "D:\\backup\\clients"
                },
            );
            let title = match kind {
                "trash" => i18n.fmt("ops-job-trash-what", &a),
                _ => i18n.fmt("ops-job-transfer", &a),
            };
            let job = cx.state.jobs.start(
                JobSpec {
                    kind,
                    title,
                    lane: Lane::Bulk,
                    priority: 0,
                    owner_ns: "screens",
                },
                cx.rt.sink(),
            );
            let id = job.id();
            self.ids.push(id);
            if n == 2 {
                // Третья — на паузе (кнопка «Продолжить»).
                cx.state.jobs.pause(id);
            }
            cx.rt
                .spawn_dedicated("screens.job".into(), cx.rt.token(), move |tcx| {
                    let p = job.progress();
                    let total = if bytes > 0 { bytes } else { items };
                    p.bytes_total.store(bytes, Ordering::Relaxed);
                    p.items_total.store(items, Ordering::Relaxed);
                    p.total_final.store(true, Ordering::Relaxed);
                    let done_at = if bytes > 0 {
                        &p.bytes_done
                    } else {
                        &p.items_done
                    };
                    done_at.store(done, Ordering::Relaxed);
                    while !tcx.cancel.is_cancelled() && job.checkpoint().is_ok() {
                        if done_at.load(Ordering::Relaxed) + step < total {
                            done_at.fetch_add(step, Ordering::Relaxed);
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    job.finish(Err(String::new()));
                });
        }
        Ok(())
    }
}

/// Задачи `screen-sftpcopy` (F03-07): закачка папки и докачка файла на сервер —
/// (что, куда, всего байт, сделано, шаг за 100 мс, докачка с N %).
#[allow(clippy::type_complexity, reason = "таблица сцены")]
const SFTP_JOBS: [(&str, &str, u64, u64, u64, Option<u64>); 2] = [
    (
        "media",
        "sftp://srv1/var/www/app/shared",
        530_579_456,
        222_298_112,
        1_027_604,
        None,
    ),
    (
        "db-1023.sql.gz",
        "sftp://srv1/var/backups",
        1_231_028_224,
        750_927_216,
        828_375,
        Some(61),
    ),
];

impl DemoJobs {
    /// Две задачи копирования на сервер (`screens.jobs kind=sftp`): адреса `sftp://` (№87),
    /// вторая — «докачка с 61 %» (фаза `Recover`, F03 §4.2).
    fn sftp_jobs(&mut self, cx: &mut Cx<'_>) {
        let i18n = urfin_i18n::current();
        for (name, dest, total, done, step, resume) in SFTP_JOBS {
            let mut a = FluentArgs::new();
            a.set("n", 1);
            a.set("name", name);
            a.set("dest", dest);
            let job = cx.state.jobs.start(
                JobSpec {
                    kind: "copy",
                    title: i18n.fmt("ops-job-transfer", &a),
                    lane: Lane::Bulk,
                    priority: 0,
                    owner_ns: "screens",
                },
                cx.rt.sink(),
            );
            self.ids.push(job.id());
            let note = resume.map(|pct| {
                let mut a = FluentArgs::new();
                a.set("pct", pct);
                i18n.fmt("ops-resume-from", &a)
            });
            cx.rt
                .spawn_dedicated("screens.job".into(), cx.rt.token(), move |tcx| {
                    let p = job.progress();
                    p.bytes_total.store(total, Ordering::Relaxed);
                    p.items_total.store(1, Ordering::Relaxed);
                    p.total_final.store(true, Ordering::Relaxed);
                    p.bytes_done.store(done, Ordering::Relaxed);
                    if let Some(n) = &note {
                        p.set_phase(urfin_core::Phase::Recover);
                        p.set_current(n);
                    } else {
                        p.set_phase(urfin_core::Phase::Transfer);
                    }
                    while !tcx.cancel.is_cancelled() && job.checkpoint().is_ok() {
                        if p.bytes_done.load(Ordering::Relaxed) + step < total {
                            p.bytes_done.fetch_add(step, Ordering::Relaxed);
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    job.finish(Err(String::new()));
                });
        }
    }

    /// Вопрос об ошибке, как его задаёт ops (тело — путь и вторая строка с причиной):
    /// `access` — «Нет доступа», `busy` — «Файл занят» с процессом-владельцем.
    fn error(&mut self, cmd: &Command, cx: &mut Cx<'_>) {
        for id in self.prompts.drain(..) {
            cx.state.prompts.cancel(id);
        }
        if cmd.args.bool("stop").unwrap_or(false) {
            return;
        }
        let i18n = urfin_i18n::current();
        let busy = cmd.args.str("kind") == Some("busy");
        let code = if busy { 32 } else { 5 };
        let reason = {
            let e = std::io::Error::from_raw_os_error(code).to_string();
            let e = e
                .split(" (os error")
                .next()
                .unwrap_or_default()
                .trim()
                .trim_end_matches('.')
                .to_owned();
            let mut c = e.chars();
            c.next()
                .map(|f| f.to_lowercase().chain(c).collect::<String>())
                .unwrap_or_default()
        };
        let mut parts = Vec::with_capacity(3);
        if busy {
            let mut a = FluentArgs::new();
            a.set("exe", "EXCEL.EXE");
            a.set("pid", "8812");
            parts.push(i18n.fmt("ops-issue-holder", &a));
        }
        if !busy {
            parts.push(reason);
        }
        let mut a = FluentArgs::new();
        a.set("code", code);
        parts.push(i18n.fmt("ops-issue-code", &a));
        let path = if busy {
            "report_q3.xlsx"
        } else {
            "C:\\Windows\\System32\\config\\SAM"
        };
        let choice = |id: &str, key: &str, k: char| urfin_core::Choice {
            id: id.into(),
            label: i18n.get(key).into(),
            key: k,
        };
        let spec = urfin_core::PromptSpec {
            anchor: urfin_core::Anchor::JobBar,
            title: i18n.get(if busy {
                "ops-issue-sharing"
            } else {
                "ops-issue-permission"
            }),
            body: format!("{path}\n{}", parts.join(" · ")),
            choices: [
                choice("retry", "ops-choice-retry", 'r'),
                choice("skip", "ops-choice-skip", 's'),
                choice("cancel", "ops-choice-cancel", 'c'),
            ]
            .into_iter()
            .collect(),
            default: 0,
            input: None,
            allow_all: true,
            detail: None,
            secret: false,
        };
        let job = self.ids.first().copied();
        let (id, _rx) = cx.state.prompts.open(spec, job);
        self.prompts.push(id);
    }
}

/// Баннер журнала «Прервано при выходе» (`screen-journal.png`), как его ставит ops.
fn banner(stop: bool, cx: &mut Cx<'_>) {
    if stop {
        cx.state.banner = None;
        return;
    }
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    a.set("n", 1);
    a.set("name", "big.bin");
    let what = i18n.fmt("ops-job-copy", &a);
    let mut b = FluentArgs::new();
    b.set("what", what);
    b.set("dest", "D:\\backup\\b");
    b.set("n", 62);
    let mut m = FluentArgs::new();
    m.set("n", 1);
    let text = format!(
        "{} · {}",
        i18n.fmt("ops-recover-banner-pct", &b),
        i18n.fmt("ops-recover-more", &m)
    );
    let stop = Command::new("screens.banner").arg("stop", true);
    cx.state.banner = Some(urfin_core::Banner {
        level: urfin_core::Level::Warn,
        text,
        actions: [
            (i18n.get("ops-recover-continue").into(), stop.clone()),
            (i18n.get("ops-recover-clear").into(), stop),
        ]
        .into_iter()
        .collect(),
    });
}
