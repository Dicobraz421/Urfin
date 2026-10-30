//! Handlers `ops` и `file` (S5 §2.5): UI-поток, без I/O. Без подтверждения
//! (`confirmed`, `dest`, `name`) — черновик в окне (`UiEvent::Ext{"ops.draft"}`,
//! S0 v3 №15); с ним — запрос операции. Копирование и перенос — задачей
//! движка (S5-06); остальные операции — S5-09, S5-10, до них — «ещё не
//! сделано» тостом. `file.open` — системным открытием (`ShellExecuteExW`) на
//! `Lane::Interactive`.

use std::sync::Arc;

use compact_str::CompactString;
use urfin_core::{
    ArgSpec, Args, CmdError, Command, CommandId, CommandMeta, Cx, Handler, Lane, Level, MinVerify,
    PolicyKey, PolicySet, Row, Side, UiEvent, ValueKind,
};

use crate::api::{ConflictPolicy, OpKind, OpRequest, RenameScheme};
use crate::config::OpsConfig;
use crate::dest::{DestError, parse_dest};
use crate::engine::JobEngine;

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "ops",
        args,
        repeatable: false,
    }
}

/// Аргументы копирования и переноса.
const COPY_ARGS: [ArgSpec; 7] = [
    arg("dest", ValueKind::Str, false),
    arg("current", ValueKind::Bool, false),
    arg("confirmed", ValueKind::Bool, false),
    arg("conflict", ValueKind::Str, false),
    arg("verify", ValueKind::Str, false),
    arg("filter", ValueKind::Str, false),
    arg("queue", ValueKind::Str, false),
];

static OPS_COMMANDS: [CommandMeta; 15] = [
    meta("ops.copy", "Copy", &COPY_ARGS),
    meta("ops.move", "Move", &COPY_ARGS),
    meta(
        "ops.delete",
        "Delete",
        &[
            arg("permanent", ValueKind::Bool, false),
            arg("current", ValueKind::Bool, false),
            arg("confirmed", ValueKind::Bool, false),
        ],
    ),
    meta(
        "ops.mkdir",
        "Make folder",
        &[
            arg("name", ValueKind::Str, false),
            arg("current", ValueKind::Bool, false),
        ],
    ),
    meta("ops.rename", "Rename", &[arg("to", ValueKind::Str, false)]),
    meta(
        "ops.multi_rename",
        "Multi-rename",
        &[
            arg("pattern", ValueKind::Str, false),
            arg("regex", ValueKind::Bool, false),
            arg("apply", ValueKind::Bool, false),
        ],
    ),
    meta(
        "ops.attrs",
        "Attributes",
        &[
            arg("set", ValueKind::Str, false),
            arg("clear", ValueKind::Str, false),
        ],
    ),
    meta(
        "ops.touch",
        "Set time",
        &[arg("mtime", ValueKind::Str, false)],
    ),
    meta(
        "ops.link",
        "Make link",
        &[
            arg("kind", ValueKind::Str, false),
            arg("dest", ValueKind::Str, false),
        ],
    ),
    meta(
        "ops.compare",
        "Compare folders",
        &[arg("mode", ValueKind::Str, false)],
    ),
    meta(
        "ops.sync",
        "Synchronize folders",
        &[arg("dir", ValueKind::Str, false)],
    ),
    meta(
        "ops.retry",
        "Retry failed",
        &[arg("id", ValueKind::Int, true)],
    ),
    meta(
        "ops.priority",
        "Operation priority",
        &[
            arg("id", ValueKind::Int, true),
            arg("prio", ValueKind::Str, false),
        ],
    ),
    meta("ops.run_now", "Run now", &[arg("id", ValueKind::Int, true)]),
    meta(
        "ops.recover",
        "Recover operation",
        &[
            arg("journal", ValueKind::Str, true),
            arg("action", ValueKind::Str, true),
        ],
    ),
];

static FILE_COMMANDS: [CommandMeta; 6] = [
    CommandMeta {
        category: "file",
        ..meta(
            "file.open",
            "Open",
            &[arg("elevated", ValueKind::Bool, false)],
        )
    },
    CommandMeta {
        category: "file",
        ..meta("file.open_with", "Open with", &[])
    },
    CommandMeta {
        category: "file",
        ..meta("file.reveal", "Show in Explorer", &[])
    },
    CommandMeta {
        category: "file",
        ..meta("file.clip.copy", "Copy to clipboard", &[])
    },
    CommandMeta {
        category: "file",
        ..meta("file.clip.cut", "Cut to clipboard", &[])
    },
    CommandMeta {
        category: "file",
        ..meta(
            "file.clip.paste",
            "Paste from clipboard",
            &[arg("move", ValueKind::Bool, false)],
        )
    },
];

fn text(key: &str, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

fn not_yet(cmd: &Command) -> CmdError {
    CmdError::Rejected(text("ops-not-yet", &[("what", cmd.id.as_str())]).into())
}

/// Handler `ops`.
#[derive(Debug)]
pub struct OpsHandler {
    cfg: OpsConfig,
    /// Конфиг, из которого прочитан `cfg`: сменился (`app.reload_config`, настройки
    /// D-08) — `[ops]` перечитывается, новые задачи идут с ним.
    seen: Option<Arc<urfin_core::Config>>,
    engine: JobEngine,
    /// Каталог журналов job (S5-12).
    journal_dir: Option<std::path::PathBuf>,
    /// Журналы просканированы (первый pump).
    scanned: bool,
}

impl OpsHandler {
    pub fn new(cfg: OpsConfig) -> OpsHandler {
        OpsHandler::with_journal(cfg, None)
    }

    pub fn with_journal(cfg: OpsConfig, journal_dir: Option<std::path::PathBuf>) -> OpsHandler {
        let engine = JobEngine::new(cfg.max_threads);
        OpsHandler {
            cfg,
            seen: None,
            engine,
            journal_dir,
            scanned: false,
        }
    }

    /// Копия или перенос задачей движка (S5-06). Назначение проверяется на ФС
    /// в плане задачи, не здесь (UI-поток без I/O).
    fn transfer(
        &self,
        cx: &mut Cx<'_>,
        mut req: OpRequest,
        dest: urfin_core::Location,
        raw: String,
        a: &Args,
    ) -> Result<(), CmdError> {
        if !req.base.is_local() || !dest.is_local() {
            // Архивы и сеть через VFS — S8 (S0 v5 №16).
            return Err(CmdError::Rejected(
                text("ops-not-yet", &[("what", "vfs")]).into(),
            ));
        }
        if let Some(c) = a.str("conflict") {
            req.opts.conflict = parse_conflict(c).ok_or_else(|| {
                CmdError::Rejected(text("ops-conflict-bad", &[("what", c)]).into())
            })?;
        }
        if let Some(v) = a.str("verify") {
            req.opts.verify = match v {
                "off" => crate::api::Verify::Off,
                "size" => crate::api::Verify::Size,
                "hash" => crate::api::Verify::Hash(crate::api::HashAlgo::Xxh3_128),
                other => {
                    return Err(CmdError::Rejected(
                        text("ops-verify-bad", &[("what", other)]).into(),
                    ));
                }
            };
        }
        // Минимальная проверка политики организации (ADR-0020) — и для `verify=`.
        let min = cx.state.ext.get::<PolicySet>().and_then(PolicySet::verify);
        req.opts.verify = raise_verify(req.opts.verify, min);
        if let Some(f) = a.str("filter") {
            req.opts.filter = Some(f.into());
        }
        let mask =
            match req.opts.filter.as_deref() {
                None | Some("") => None,
                Some(m) => Some(urfin_core::Mask::parse(m).map_err(|_| {
                    CmdError::Rejected(text("ops-filter-bad", &[("mask", m)]).into())
                })?),
            };
        let start = crate::ops::copy::Start {
            req,
            dest,
            dest_raw: raw,
            mask,
            small_file: u64::from(self.cfg.small_file),
            journal_dir: self.journal_dir.clone(),
            strategy: crate::strategy::StrategyTable::from_config(&self.cfg),
            resume_tag: None,
        };
        crate::ops::copy::start(&self.engine, cx, start);
        Ok(())
    }
}

/// `conflict=…` команды: политика по имени.
fn parse_conflict(s: &str) -> Option<ConflictPolicy> {
    Some(match s {
        "ask" => ConflictPolicy::Ask,
        "overwrite" => ConflictPolicy::Overwrite,
        "skip" => ConflictPolicy::Skip,
        "newer" => ConflictPolicy::OverwriteIfNewer,
        "differs" => ConflictPolicy::OverwriteIfDiffers,
        "identical" => ConflictPolicy::SkipIdentical,
        "rename" => ConflictPolicy::RenameNew(RenameScheme::Counter),
        "rename_time" => ConflictPolicy::RenameNew(RenameScheme::Timestamp),
        "rename_existing" => ConflictPolicy::RenameExisting(RenameScheme::Counter),
        "rename_existing_time" => ConflictPolicy::RenameExisting(RenameScheme::Timestamp),
        _ => return None,
    })
}

impl Handler for OpsHandler {
    fn namespace(&self) -> &'static str {
        "ops"
    }

    /// Первый pump: прерванные job из журналов → баннер (S5 §6.3).
    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        let fresh = self
            .seen
            .as_ref()
            .is_none_or(|c| !Arc::ptr_eq(c, &cx.state.config));
        if fresh {
            if self.seen.is_some() {
                let (cfg, _) = cx.state.config.section::<OpsConfig>("ops");
                self.cfg = cfg;
            }
            self.seen = Some(Arc::clone(&cx.state.config));
        }
        if self.scanned {
            return;
        }
        self.scanned = true;
        let Some(dir) = self.journal_dir.clone() else {
            return;
        };
        let sink = cx.rt.sink();
        cx.rt
            .spawn(Lane::Bulk, "ops.journal.scan", cx.rt.token(), move |_| {
                let found = crate::journal::scan(&dir);
                if found.is_empty() {
                    return;
                }
                let m = urfin_core::msg("ops.journal.found", move |cx| {
                    cx.state.ext.insert(Interrupted(found));
                    next_banner(cx);
                });
                let _ = sink.send(m, &urfin_core::CancelToken::new());
            });
    }

    /// Выход при идущих операциях — вопрос (S5 §6.4): «Прервать и выйти»
    /// оставляет журналы для «Продолжить».
    fn on_quit(&mut self, cx: &mut Cx<'_>) -> urfin_core::QuitDecision {
        let n = cx
            .state
            .jobs
            .iter()
            .filter(|j| j.owner_ns == "ops" && j.state.is_active())
            .count();
        if n == 0 {
            return urfin_core::QuitDecision::Allow;
        }
        crate::ops::copy::QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
        let i18n = urfin_i18n::current();
        let choice = |id: &str, key: &str, k: char| urfin_core::Choice {
            id: id.into(),
            label: i18n.get(key).into(),
            key: k,
        };
        urfin_core::QuitDecision::Veto(urfin_core::PromptSpec {
            anchor: urfin_core::Anchor::JobBar,
            title: text("ops-quit-title", &[("n", &n.to_string())]),
            body: i18n.get("ops-quit-body"),
            choices: [
                choice("wait", "ops-quit-wait", 'w'),
                choice("quit", "ops-quit-abort", 'q'),
            ]
            .into_iter()
            .collect(),
            default: 0,
            input: None,
            allow_all: false,
            detail: None,
            secret: false,
        })
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &OPS_COMMANDS
    }

    #[allow(
        clippy::too_many_lines,
        reason = "диспетчер команд ops: ветка на команду"
    )]
    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let a = &cmd.args;
        match cmd.id.as_str() {
            "ops.copy" | "ops.move" => {
                let kind = if cmd.id.as_str() == "ops.copy" {
                    OpKind::Copy
                } else {
                    OpKind::Move
                };
                // Цели проверяются до черновика: на `..` без выделения — сразу «нечего».
                let req = self.request(cx, kind, a.bool("current").unwrap_or(false))?;
                let Some(dest) = a.str("dest") else {
                    draft(cx, cmd, false);
                    return Ok(());
                };
                let raw = dest.to_owned();
                let dest = parse_dest(&req.base, dest).map_err(dest_error)?;
                tracing::info!(target: "ops", kind = ?req.kind, items = req.items.len(), dest = %dest.path.display(), "request");
                self.transfer(cx, req, dest, raw, a)
            }
            "ops.delete" => {
                // Политика «в корзину» (ADR-0020): безвозвратное (Shift) недоступно.
                let forced = cx
                    .state
                    .ext
                    .get::<PolicySet>()
                    .and_then(|p| p.bool(PolicyKey::DeleteToTrash))
                    == Some(true);
                let asked = a.bool("permanent").unwrap_or(false) && !forced;
                let permanent = asked || !self.cfg.delete_to_trash;
                let kind = if permanent {
                    OpKind::Delete
                } else {
                    OpKind::Trash
                };
                let req = self.request(cx, kind, a.bool("current").unwrap_or(false))?;
                if !a.bool("confirmed").unwrap_or(false) {
                    draft(cx, cmd, asked);
                    return Ok(());
                }
                if req.kind == OpKind::Trash && req.base.is_local() {
                    crate::ops::trash::start(
                        &self.engine,
                        cx,
                        req.base,
                        req.items,
                        self.cfg.experimental_fast_trash,
                    );
                    return Ok(());
                }
                delete(cx, req)
            }
            "ops.mkdir" => {
                let Some(name) = a.str("name") else {
                    draft(cx, cmd, false);
                    return Ok(());
                };
                mkdir(cx, name)
            }
            "ops.priority" | "job.priority" | "ops.run_now" => self.job_control(cmd),
            "ops.multi_rename" => self.multi_rename(cx, cmd),
            "ops.link" => self.link(cx, cmd),
            "ops.compare" => compare(cx, cmd.args.str("mode") == Some("content"), None),
            "ops.sync" => {
                let side = match cmd.args.str("dir") {
                    Some("rl") => Side::Right,
                    Some("lr") | None => Side::Left,
                    Some(other) => {
                        return Err(CmdError::Rejected(
                            text("ops-sync-bad", &[("what", other)]).into(),
                        ));
                    }
                };
                compare(cx, false, Some(side))
            }
            "ops.attrs" => attrs(cx, cmd),
            "ops.touch" => touch(cx, cmd),
            "ops.recover" => {
                let path = std::path::PathBuf::from(a.str("journal").unwrap_or_default());
                let go = a.str("action") == Some("continue");
                self.recover(cx, path, go);
                Ok(())
            }
            "ops.retry" | "job.retry" => {
                // `id` обязателен (проверен реестром); чужой или отрицательный — «нечего».
                let id = a
                    .int("id")
                    .and_then(|v| u64::try_from(v).ok())
                    .map(urfin_core::JobId);
                let found = id.and_then(|id| {
                    cx.state
                        .ext
                        .get::<crate::engine::Reports>()
                        .and_then(|r| r.retry(id))
                });
                let started = found.and_then(|(report, retry)| {
                    crate::ops::copy::start_retry(&self.engine, cx, &report, &retry)
                });
                if started.is_none() {
                    return Err(CmdError::Rejected(text("ops-retry-nothing", &[]).into()));
                }
                Ok(())
            }
            "ops.rename" => {
                let Some(to) = a.str("to") else {
                    // Без имени — поле на месте (Shift+F6).
                    cx.dispatch(Command::new("ui.rename"));
                    return Ok(());
                };
                rename(cx, to)
            }
            _ => Err(not_yet(cmd)),
        }
    }
}

/// Одиночная операция активного таба вне UI-потока (S5-10): по успеху курсор —
/// на `focus`, листинг перечитывается; ошибка — тостом.
fn quick(
    cx: &mut Cx<'_>,
    name: &'static str,
    work: impl FnOnce(&std::path::Path) -> Result<Option<CompactString>, String> + Send + 'static,
) -> Result<(), CmdError> {
    let t = cx.state.panel(cx.state.active).tab();
    if !t.loc.is_local() {
        return Err(CmdError::Rejected(
            text("ops-not-yet", &[("what", name)]).into(),
        ));
    }
    let (tab, dir) = (t.id, t.loc.path.clone());
    let sink = cx.rt.sink();
    cx.rt
        .spawn(Lane::Interactive, name, cx.rt.token(), move |tcx| {
            let result = work(&dir);
            let m = urfin_core::msg(name, move |cx| match result {
                Ok(focus) => {
                    if let Some(t) = cx.state.tab_mut(tab)
                        && focus.is_some()
                    {
                        t.pending_focus = focus;
                    }
                    cx.dispatch(Command::new("panel.refresh").arg("tab", i64::from(tab.0)));
                }
                Err(e) => cx.toast(Level::Error, e),
            });
            let _ = sink.send(m, &tcx.cancel);
        });
    Ok(())
}

/// `ops.mkdir {name}`: `a/b/c`, `x;y` (Far); курсор — на первый созданный.
fn mkdir(cx: &mut Cx<'_>, spec: &str) -> Result<(), CmdError> {
    let spec = spec.to_owned();
    quick(cx, "ops.mkdir", move |dir| {
        crate::ops::mkdir::mkdir(dir, &spec)
            .map(|made| made.into_iter().next())
            .map_err(|e| {
                text(
                    "ops-mkdir-failed",
                    &[("name", e.name.as_str()), ("error", &io_text(&e.error))],
                )
            })
    })
}

/// `ops.rename {to}`: элемент под курсором активного таба, в его каталоге.
fn rename(cx: &mut Cx<'_>, to: &str) -> Result<(), CmdError> {
    let t = cx.state.panel(cx.state.active).tab();
    let Some(Row::Entry { entry, .. }) = t.current() else {
        return Err(CmdError::Rejected(text("ops-nothing", &[]).into()));
    };
    let to = to.trim();
    if !crate::ops::rename::is_plain_name(to) {
        return Err(CmdError::Rejected(text("ops-rename-bad", &[]).into()));
    }
    let (from, to) = (entry.name.clone(), CompactString::from(to));
    quick(cx, "ops.rename", move |dir| {
        crate::ops::rename::rename(dir, &from, &to)
            .map(|()| Some(to.clone()))
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    text("ops-exists", &[("name", to.as_str())])
                } else {
                    text(
                        "ops-rename-failed",
                        &[("name", from.as_str()), ("error", &io_text(&e))],
                    )
                }
            })
    })
}

/// Удаление насовсем задачей (прогресс, отмена в job-баре), по элементам; по
/// окончании листинг перечитывается. Корзина — job движка ([`crate::ops::trash`]).
fn delete(cx: &mut Cx<'_>, req: OpRequest) -> Result<(), CmdError> {
    if !req.base.is_local() {
        return Err(CmdError::Rejected(
            text("ops-not-yet", &[("what", "ops.delete")]).into(),
        ));
    }
    let tab = cx.state.panel(cx.state.active).tab().id;
    let job = cx.state.jobs.start(
        urfin_core::JobSpec {
            kind: "delete",
            title: {
                let mut a = urfin_i18n::FluentArgs::new();
                a.set("n", req.items.len());
                a.set(
                    "name",
                    req.items
                        .first()
                        .map_or_else(String::new, ToString::to_string),
                );
                urfin_i18n::current().fmt("ops-job-items", &a)
            },
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "ops",
        },
        cx.rt.sink(),
    );
    let sink = cx.rt.sink();
    let (dir, items) = (req.base.path, req.items);
    cx.rt
        .spawn_dedicated("ops.delete".into(), cx.rt.token(), move |tcx| {
            use std::sync::atomic::Ordering;
            let _span = tracing::info_span!("ops.job", kind = "delete").entered();
            let p = job.progress();
            let n = items.len() as u64;
            p.items_total.store(n, Ordering::Relaxed);
            p.total_final.store(true, Ordering::Relaxed);
            let stop = || tcx.cancel.is_cancelled() || job.checkpoint().is_err();
            let done = |name: &str| {
                p.set_current(name);
                p.items_done.fetch_add(1, Ordering::Relaxed);
            };
            let errors: Vec<String> =
                crate::ops::delete::delete_permanent(&dir, &items, &stop, &done)
                    .into_iter()
                    .map(|e| format!("{}: {}", e.name, e.error))
                    .collect();
            let failed = errors.len() as u64;
            let m = urfin_core::msg("ops.delete", move |cx| {
                if let Some(first) = errors.first() {
                    let more = if errors.len() > 1 {
                        format!(" (+{})", errors.len() - 1)
                    } else {
                        String::new()
                    };
                    cx.toast(
                        Level::Error,
                        text("ops-delete-failed", &[("error", &format!("{first}{more}"))]),
                    );
                }
                cx.dispatch(Command::new("panel.refresh").arg("tab", i64::from(tab.0)));
            });
            let _ = sink.send(m, &tcx.cancel);
            let cancelled = job.cancel_token().is_cancelled();
            let items = p.items_done.load(Ordering::Relaxed);
            job.finish(if cancelled {
                Err("cancelled".into())
            } else {
                Ok(urfin_core::JobSummary {
                    items,
                    bytes: 0,
                    skipped: 0,
                    errors: failed,
                })
            });
        });
    Ok(())
}

/// Текст ошибки ОС без «(os error N)».
fn io_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

fn dest_error(e: DestError) -> CmdError {
    match e {
        DestError::Empty => CmdError::Rejected(text("ops-dest-empty", &[]).into()),
        DestError::NotADirectory(p) => {
            CmdError::Rejected(text("ops-dest-file", &[("path", &p)]).into())
        }
    }
}

/// Черновик операции в окне (S0 v3 №15): строка с полем или кнопками.
/// `ops.compare {mode?}` (S5-24): сравнение каталогов панелей вне UI-потока
/// (ФС сторон — для допуска времени; в режиме `content` — чтение файлов); итог —
/// выделение в обеих панелях и тост. `sync` — сторона-источник `ops.sync`: после
/// сравнения в ней остаётся выделенным то, что копировать, она становится
/// активной, и открывается черновик копирования в другую панель (превью — само
/// выделение; Enter — копия, конфликт «новее» — вопрос «Заменить, если новее»).
fn compare(cx: &mut Cx<'_>, content: bool, sync: Option<Side>) -> Result<(), CmdError> {
    let (l, r) = (
        cx.state.panel(Side::Left).tab(),
        cx.state.panel(Side::Right).tab(),
    );
    if !l.loc.is_local() || !r.loc.is_local() {
        return Err(CmdError::Rejected(
            text("ops-not-yet", &[("what", "vfs")]).into(),
        ));
    }
    let (le, re) = (
        Arc::clone(&l.listing.entries),
        Arc::clone(&r.listing.entries),
    );
    let (lp, rp) = (l.loc.path.clone(), r.loc.path.clone());
    let (lt, rt) = (l.id, r.id);
    let sink = cx.rt.sink();
    cx.rt.spawn(
        Lane::Interactive,
        "ops.compare",
        cx.rt.token(),
        move |tcx| {
            use crate::ops::compare::{compare as cmp, content_checker, time_tolerance};
            let fs = |p: &std::path::Path| urfin_vfs::volume::of(p).ok().map(|v| v.fs);
            let tol = time_tolerance(fs(&lp)).max(time_tolerance(fs(&rp)));
            let check = content.then(|| content_checker(lp.clone(), rp.clone()));
            let res = cmp(
                &le,
                &re,
                tol,
                check.as_ref().map(|c| c as &dyn Fn(&str) -> Option<bool>),
            );
            let m = urfin_core::msg("ops.compare", move |cx| {
                apply_compare(cx, lt, rt, &res, sync);
            });
            let _ = sink.send(m, &tcx.cancel);
        },
    );
    Ok(())
}

/// Итог сравнения: выделение в табах `lt`/`rt` (если они ещё показывают те же
/// каталоги) и тост; при `sync` — черновик копирования со стороны-источника.
fn apply_compare(
    cx: &mut Cx<'_>,
    lt: urfin_core::TabId,
    rt: urfin_core::TabId,
    res: &crate::ops::compare::CompareResult,
    sync: Option<Side>,
) {
    for (tab, keys, keep) in [
        (lt, &res.select_left, sync != Some(Side::Right)),
        (rt, &res.select_right, sync != Some(Side::Left)),
    ] {
        if let Some(t) = cx.state.tab_mut(tab) {
            let listing = t.listing.clone();
            t.selection.clear();
            if keep {
                t.selection.select_where(&listing, true, |e| {
                    !e.kind.is_dir_like() && keys.contains(&crate::names::name_key(&e.name))
                });
            }
        }
    }
    cx.state.touch();
    // Итог — строкой внизу (D-04, `screen-compare.png`), не тостом.
    let n = |v: u32| i64::from(v);
    let mut args = Args::default()
        .with("only_left", n(res.only_left))
        .with("only_right", n(res.only_right))
        .with("newer_left", n(res.newer_left))
        .with("newer_right", n(res.newer_right))
        .with("differs", n(res.differs))
        .with("equal", n(res.equal));
    if let Some(side) = sync {
        args = args.with("sync", if side == Side::Left { "left" } else { "right" });
    }
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("ops.compare.result"),
        args,
    });
    let Some(side) = sync else {
        return;
    };
    let count = if side == Side::Left {
        &res.select_left
    } else {
        &res.select_right
    }
    .len();
    if count == 0 {
        cx.toast(Level::Info, text("ops-sync-nothing", &[]));
        return;
    }
    cx.dispatch(
        Command::new("panel.focus").arg("side", if side == Side::Left { "left" } else { "right" }),
    );
    cx.dispatch(Command::new("ops.copy"));
}

/// Черновик с начальным текстом поля (`ops.link`, `ops.touch`).
fn draft_init(cx: &mut Cx<'_>, cmd: &Command, init: &str) {
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("ops.draft"),
        args: Args::default()
            .with("command", cmd.id.as_str())
            .with("init", init),
    });
}

/// Имена целей активного таба (выделение или строка курсора).
fn target_names(cx: &Cx<'_>) -> Result<Vec<CompactString>, CmdError> {
    let t = cx.state.panel(cx.state.active).tab();
    t.targets()
        .map(|tg| tg.items.iter().map(|e| e.name.clone()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| CmdError::Rejected(text("ops-nothing", &[]).into()))
}

/// `+r -h` или буквы: (поставить, снять) — маски `FILE_ATTRIBUTE_*`.
pub(crate) fn parse_attrs(set: Option<&str>, clear: Option<&str>) -> Result<(u32, u32), String> {
    let bit = |c: char| match c.to_ascii_lowercase() {
        'r' => Some(crate::sys::fs::ATTR_READONLY),
        'h' => Some(crate::sys::fs::ATTR_HIDDEN),
        's' => Some(crate::sys::fs::ATTR_SYSTEM),
        'a' => Some(crate::sys::fs::ATTR_ARCHIVE),
        _ => None,
    };
    let (mut on, mut off) = (0u32, 0u32);
    let mut sign = '+';
    for c in set
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
    {
        match c {
            '+' | '-' | '\u{2212}' => sign = if c == '+' { '+' } else { '-' },
            _ => {
                let b = bit(c).ok_or_else(|| c.to_string())?;
                if sign == '+' { on |= b } else { off |= b }
            }
        }
    }
    for c in clear
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphabetic())
    {
        off |= bit(c).ok_or_else(|| c.to_string())?;
    }
    Ok((on, off))
}

/// `ops.attrs {set?, clear?}` (S5-22): атрибуты целей; без аргументов — черновик.
fn attrs(cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
    let a = &cmd.args;
    if a.str("set").is_none() && a.str("clear").is_none() {
        draft(cx, cmd, false);
        return Ok(());
    }
    let (on, off) = parse_attrs(a.str("set"), a.str("clear"))
        .map_err(|c| CmdError::Rejected(text("ops-attrs-bad", &[("what", &c)]).into()))?;
    let names = target_names(cx)?;
    quick(cx, "ops.attrs", move |dir| {
        let mut errs = Vec::new();
        for n in &names {
            let p = crate::names::os_path(&dir.join(n.as_str()));
            let r = crate::sys::fs::attrs(&p).and_then(|cur| {
                // Каталог: бит каталога не пишется (`SetFileAttributesW` его не принимает).
                let next = (cur | on) & !off & !0x10;
                crate::sys::fs::set_attrs(&p, next)
            });
            if let Err(e) = r {
                errs.push(format!("{n}: {}", io_text(&e)));
            }
        }
        if errs.is_empty() {
            Ok(None)
        } else {
            Err(errs.join("; "))
        }
    })
}

/// `2026-10-21`, `2026-10-21 18:05`, `2026-10-21T18:05:07`, `now` → время.
pub(crate) fn parse_time(text: &str) -> Option<std::time::SystemTime> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("now") || text.is_empty() {
        return Some(std::time::SystemTime::now());
    }
    let (date, time) = text.split_once([' ', 'T']).unwrap_or((text, "00:00:00"));
    let ymd: Vec<u16> = date
        .split('-')
        .map(str::parse::<u16>)
        .collect::<Result<_, _>>()
        .ok()?;
    let hms: Vec<u16> = time
        .split(':')
        .map(str::parse::<u16>)
        .collect::<Result<_, _>>()
        .ok()?;
    let [year, month, day] = ymd[..] else {
        return None;
    };
    let (h, mi, sec) = match hms[..] {
        [h, mi] => (h, mi, 0),
        [h, mi, sec] => (h, mi, sec),
        _ => return None,
    };
    crate::sys::fs::local_time(year, month, day, h, mi, sec)
}

/// `ops.touch {mtime?}` (S5-22): время изменения целей; без аргумента — черновик
/// с текущими датой и временем.
fn touch(cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
    let Some(m) = cmd.args.str("mtime") else {
        draft_init(cx, cmd, &crate::sys::fs::local_now_text());
        return Ok(());
    };
    let when = parse_time(m)
        .ok_or_else(|| CmdError::Rejected(text("ops-touch-bad", &[("what", m)]).into()))?;
    let names = target_names(cx)?;
    quick(cx, "ops.touch", move |dir| {
        let mut errs = Vec::new();
        for n in &names {
            let p = crate::names::os_path(&dir.join(n.as_str()));
            if let Err(e) = crate::sys::fs::set_dir_mtime(&p, when) {
                errs.push(format!("{n}: {}", io_text(&e)));
            }
        }
        if errs.is_empty() {
            Ok(None)
        } else {
            Err(errs.join("; "))
        }
    })
}

/// Проверка не ниже минимума политики.
fn raise_verify(v: crate::api::Verify, min: Option<MinVerify>) -> crate::api::Verify {
    use crate::api::{HashAlgo, Verify};
    let rank = match v {
        Verify::Off => MinVerify::None,
        Verify::Size => MinVerify::Size,
        Verify::Hash(_) => MinVerify::Hash,
    };
    match min {
        Some(m) if m > rank => match m {
            MinVerify::None => v,
            MinVerify::Size => Verify::Size,
            MinVerify::Hash => Verify::Hash(HashAlgo::Xxh3_128),
        },
        _ => v,
    }
}

fn draft(cx: &mut Cx<'_>, cmd: &Command, permanent: bool) {
    let mut args = Args::default().with("command", cmd.id.as_str());
    if permanent {
        args = args.with("permanent", true);
    }
    if cmd.args.bool("current").unwrap_or(false) {
        args = args.with("current", true);
    }
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("ops.draft"),
        args,
    });
}

/// Строк в превью группового переименования.
const MR_PREVIEW_ROWS: usize = 12;

/// Прерванные job, ещё не разобранные баннером (S5-12).
#[derive(Debug, Default)]
struct Interrupted(Vec<crate::journal::Interrupted>);

/// Баннер о первой прерванной job: «Продолжить» / «Очистить».
fn next_banner(cx: &mut Cx<'_>) {
    if cx.state.banner.is_some() {
        return;
    }
    let Some(i) = cx
        .state
        .ext
        .get_mut::<Interrupted>()
        .and_then(|l| (!l.0.is_empty()).then(|| l.0.remove(0)))
    else {
        return;
    };
    let h = &i.header;
    let mode = if h.req.kind == OpKind::Move {
        "ops-job-move"
    } else {
        "ops-job-copy"
    };
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("n", h.req.items.len());
    a.set(
        "name",
        h.req
            .items
            .first()
            .map_or_else(String::new, ToString::to_string),
    );
    let what = urfin_i18n::current().fmt(mode, &a);
    let dest = h.dest.display().to_string();
    let text_ = match i.percent() {
        Some(p) => text(
            "ops-recover-banner-pct",
            &[("what", &what), ("dest", &dest), ("n", &p.to_string())],
        ),
        None => text("ops-recover-banner", &[("what", &what), ("dest", &dest)]),
    };
    let queued = cx.state.ext.get::<Interrupted>().map_or(0, |l| l.0.len());
    let text_ = if queued > 0 {
        format!(
            "{text_} · {}",
            text("ops-recover-more", &[("n", &queued.to_string())])
        )
    } else {
        text_
    };
    let path = i.path.display().to_string();
    let cmd = |action: &str| {
        Command::new("ops.recover")
            .arg("journal", path.as_str())
            .arg("action", action)
    };
    let i18n = urfin_i18n::current();
    cx.state.banner = Some(urfin_core::Banner {
        level: urfin_core::Level::Warn,
        text: text_,
        actions: [
            (i18n.get("ops-recover-continue").into(), cmd("continue")),
            (i18n.get("ops-recover-clear").into(), cmd("clear")),
        ]
        .into_iter()
        .collect(),
    });
    cx.state.touch();
}

impl OpsHandler {
    /// `ops.multi_rename` (S5-23): без шаблона — черновик с полем; с шаблоном —
    /// превью (`UiEvent::Ext ops.multi_rename.preview`, без I/O — по листингу
    /// таба) или, с `apply`, job. Цели — выделение или строка курсора.
    fn multi_rename(&self, cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
        use crate::ops::multi_rename::{Problem, Target, plan};
        let a = &cmd.args;
        let Some(pattern) = a.str("pattern") else {
            draft(cx, cmd, false);
            return Ok(());
        };
        let t = cx.state.panel(cx.state.active).tab();
        if !t.loc.is_local() {
            return Err(CmdError::Rejected(
                text("ops-not-yet", &[("what", "vfs")]).into(),
            ));
        }
        let Some(tg) = t.targets() else {
            return Err(CmdError::Rejected(text("ops-nothing", &[]).into()));
        };
        let targets: Vec<Target> = tg
            .items
            .iter()
            .map(|e| Target {
                name: e.name.clone(),
                mtime: e.mtime,
                dir: e.kind.is_dir_like(),
            })
            .collect();
        let names: std::collections::HashSet<CompactString> = t
            .listing
            .entries
            .iter()
            .map(|e| crate::names::name_key(&e.name))
            .collect();
        let existing = |n: &str| names.contains(&crate::names::name_key(n));
        let spec = crate::api::RenameSpec {
            pattern: pattern.into(),
            // В имени файла `/` не бывает: шаблон с `/` — regex «найти/заменить».
            regex: a.bool("regex").unwrap_or_else(|| pattern.contains('/')),
        };
        let result = plan(&targets, &spec, &existing, &crate::sys::fs::local_ymd);
        if a.bool("apply").unwrap_or(false) {
            let p = result.map_err(|e| CmdError::Rejected(e.0.into()))?;
            if p.rows.is_empty() {
                return Err(CmdError::Rejected(text("ops-mr-nothing", &[]).into()));
            }
            if p.problems > 0 {
                let n = p.problems.to_string();
                cx.toast(Level::Warn, text("ops-mr-skipped", &[("n", &n)]));
            }
            crate::ops::multi_rename::start(&self.engine, cx, tg.base, &p);
            return Ok(());
        }
        // Превью: первые строки и счётчики.
        let mut args = Args::default();
        match result {
            Err(e) => args = args.with("error", e.0.as_str()),
            Ok(p) => {
                let rows: Vec<urfin_core::Value> = p
                    .rows
                    .iter()
                    .take(MR_PREVIEW_ROWS)
                    .map(|r| {
                        let mark = match r.problem {
                            None => "",
                            Some(Problem::Invalid) => "invalid",
                            Some(Problem::Duplicate) => "duplicate",
                            Some(Problem::Exists) => "exists",
                        };
                        urfin_core::Value::from(format!("{}\t{}\t{mark}", r.from, r.to))
                    })
                    .collect();
                args = args
                    .with("rows", rows)
                    .with("changed", i64::try_from(p.rows.len()).unwrap_or(i64::MAX))
                    .with("problems", i64::try_from(p.problems).unwrap_or(i64::MAX));
            }
        }
        cx.ui(UiEvent::Ext {
            id: CommandId::from_static("ops.multi_rename.preview"),
            args,
        });
        Ok(())
    }

    /// `ops.link {kind?, dest?}` (S5-22, Alt+F6): ссылка на элемент под курсором;
    /// без `dest` — черновик с путём другой панели и именем элемента.
    fn link(&self, cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
        let a = &cmd.args;
        let t = cx.state.panel(cx.state.active).tab();
        let Some(Row::Entry { entry, .. }) = t.current() else {
            return Err(CmdError::Rejected(text("ops-nothing", &[]).into()));
        };
        let other = cx.state.panel(cx.state.active.other()).tab();
        if !t.loc.is_local() || !other.loc.is_local() {
            return Err(CmdError::Rejected(
                text("ops-not-yet", &[("what", "vfs")]).into(),
            ));
        }
        let kind = match a.str("kind") {
            None => None,
            Some(k) => Some(crate::ops::link::Kind::parse(k).ok_or_else(|| {
                CmdError::Rejected(text("ops-link-kind-bad", &[("what", k)]).into())
            })?),
        };
        let src = t.loc.path.join(entry.name.as_str());
        let Some(dest) = a.str("dest") else {
            let init = other
                .loc
                .path
                .join(entry.name.as_str())
                .display()
                .to_string();
            draft_init(cx, cmd, &init);
            return Ok(());
        };
        let dest = parse_dest(&t.loc, dest).map_err(dest_error)?;
        let touch = [t.loc.clone(), other.loc.clone()];
        let link = crate::ops::link::Link {
            kind,
            src,
            dest: dest.path,
            touch,
        };
        crate::ops::link::start(&self.engine, cx, link);
        Ok(())
    }

    /// `ops.priority {id, prio}`, `job.priority {id, p}`, `ops.run_now {id}` (S5-19).
    fn job_control(&self, cmd: &Command) -> Result<(), CmdError> {
        let a = &cmd.args;
        let id = a
            .int("id")
            .and_then(|v| u64::try_from(v).ok())
            .map(urfin_core::JobId);
        let ok = id.is_some_and(|id| match cmd.id.as_str() {
            "ops.run_now" => self.engine.run_now(id),
            "job.priority" => {
                let p = a.int("p").and_then(|p| u8::try_from(p.clamp(0, 2)).ok());
                self.engine.set_priority(id, p.unwrap_or(1))
            }
            _ => {
                let p = match a.str("prio") {
                    Some("high") => 2,
                    Some("low") => 0,
                    _ => 1,
                };
                self.engine.set_priority(id, p)
            }
        });
        if !ok {
            return Err(CmdError::Rejected(text("ops-job-gone", &[]).into()));
        }
        Ok(())
    }

    /// «Продолжить» или «Очистить» прерванную job (S5 §6.3): журнал
    /// захватывается (занят — не трогаем), temp удаляются; «Продолжить» —
    /// тот же запрос заново, файлы, равные источнику, пропускаются.
    fn recover(&self, cx: &mut Cx<'_>, path: std::path::PathBuf, go: bool) {
        cx.state.banner = None;
        let (sink, small, dir, eng) = (
            cx.rt.sink(),
            u64::from(self.cfg.small_file),
            self.journal_dir.clone(),
            self.engine.clone(),
        );
        cx.rt.spawn(
            Lane::Bulk,
            "ops.journal.recover",
            cx.rt.token(),
            move |_| {
                let start = match crate::journal::take(&path) {
                    Ok(Some((f, i))) => {
                        let start = i.and_then(|i| {
                            // «Продолжить» оставляет недописанные с докачкой (S5-27).
                            i.remove_temps(go);
                            go.then(|| {
                                crate::ops::copy::recovered(&i.header, &i.resumable, small, dir)
                            })
                            .flatten()
                        });
                        drop(f);
                        if let Err(e) = std::fs::remove_file(&path) {
                            tracing::warn!(journal = %path.display(), error = %e, "journal remove");
                        }
                        start
                    }
                    Ok(None) => {
                        tracing::info!(journal = %path.display(), "journal is live, left alone");
                        None
                    }
                    Err(e) => {
                        tracing::warn!(journal = %path.display(), error = %e, "journal recover");
                        None
                    }
                };
                let m = urfin_core::msg("ops.journal.recovered", move |cx| {
                    if let Some(s) = start {
                        crate::ops::copy::start(&eng, cx, s);
                    }
                    next_banner(cx);
                });
                let _ = sink.send(m, &urfin_core::CancelToken::new());
            },
        );
    }

    /// Запрос по выделению активного таба (или элементу под курсором при `current`).
    fn request(&self, cx: &Cx<'_>, kind: OpKind, current: bool) -> Result<OpRequest, CmdError> {
        let t = cx.state.panel(cx.state.active).tab();
        let items: Vec<CompactString> = if current {
            match t.current() {
                Some(Row::Entry { entry, .. }) => vec![entry.name.clone()],
                _ => Vec::new(),
            }
        } else {
            t.targets()
                .map(|tg| tg.items.iter().map(|e| e.name.clone()).collect())
                .unwrap_or_default()
        };
        if items.is_empty() {
            return Err(CmdError::Rejected(text("ops-nothing", &[]).into()));
        }
        Ok(OpRequest {
            kind,
            base: t.loc.clone(),
            items,
            dest: None,
            opts: self.cfg.defaults.clone(),
        })
    }
}

/// Handler `file`: открытие и показ в Проводнике; буфер обмена — S5 позже.
#[derive(Debug, Default)]
pub struct FileHandler;

impl Handler for FileHandler {
    fn namespace(&self) -> &'static str {
        "file"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &FILE_COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let how = match cmd.id.as_str() {
            "file.open" if cmd.args.bool("elevated").unwrap_or(false) => crate::sys::Open::Elevated,
            "file.open" => crate::sys::Open::Default,
            "file.open_with" => crate::sys::Open::With,
            "file.reveal" => crate::sys::Open::Reveal,
            _ => return Err(not_yet(cmd)),
        };
        let t = cx.state.panel(cx.state.active).tab();
        let Some(Row::Entry { entry, .. }) = t.current() else {
            return Err(CmdError::Rejected(text("ops-nothing", &[]).into()));
        };
        if !t.loc.is_local() {
            // Архивы и сеть — через VFS (S8).
            return Err(not_yet(cmd));
        }
        let path = t.loc.path.join(entry.name.as_str());
        let name = entry.name.clone();
        let sink = cx.rt.sink();
        cx.rt
            .spawn(Lane::Interactive, "file.open", cx.rt.token(), move |tcx| {
                if let Err(e) = crate::sys::open(&path, how) {
                    tracing::warn!(target: "ops", error = %e, path = %path.display(), "file.open");
                    let msg = text(
                        "ops-open-failed",
                        &[("name", name.as_str()), ("error", &e.to_string())],
                    );
                    let _ = sink.send(
                        urfin_core::msg("ops.open_failed", move |cx| cx.toast(Level::Error, msg)),
                        &tcx.cancel,
                    );
                }
            });
        Ok(())
    }
}

#[cfg(test)]
mod attr_time_tests {
    use super::{parse_attrs, parse_time};
    use crate::sys::fs::{ATTR_ARCHIVE, ATTR_HIDDEN, ATTR_READONLY, ATTR_SYSTEM};

    #[test]
    fn attrs_syntax() {
        assert_eq!(
            parse_attrs(Some("+R -h"), None),
            Ok((ATTR_READONLY, ATTR_HIDDEN))
        );
        assert_eq!(
            parse_attrs(Some("rs"), Some("a")),
            Ok((ATTR_READONLY | ATTR_SYSTEM, ATTR_ARCHIVE))
        );
        assert_eq!(
            parse_attrs(Some("+r\u{2212}a"), None),
            Ok((ATTR_READONLY, ATTR_ARCHIVE))
        );
        assert_eq!(parse_attrs(Some("+x"), None), Err("x".into()));
    }

    #[test]
    fn time_syntax() {
        let t = |s: &str| {
            parse_time(s).map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs())
        };
        let day = t("2026-10-21").unwrap();
        assert_eq!(t("2026-10-21 00:00"), Some(day));
        assert_eq!(t("2026-10-21T00:00:30"), Some(day + 30));
        assert_eq!(t("2026-10-21 18:05").unwrap() - day, 18 * 3600 + 5 * 60);
        assert!(t("now").is_some());
        assert_eq!(t("21.10.2026"), None);
        assert_eq!(t("2026-10-21 25"), None);
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::api::{HashAlgo, Verify};

    #[test]
    fn verify_is_raised_to_policy_minimum() {
        let blake = Verify::Hash(HashAlgo::Blake3);
        assert_eq!(raise_verify(Verify::Off, None), Verify::Off);
        assert_eq!(
            raise_verify(Verify::Off, Some(MinVerify::Size)),
            Verify::Size
        );
        assert_eq!(
            raise_verify(Verify::Size, Some(MinVerify::None)),
            Verify::Size
        );
        assert_eq!(raise_verify(blake, Some(MinVerify::Size)), blake);
        assert_eq!(
            raise_verify(Verify::Size, Some(MinVerify::Hash)),
            Verify::Hash(HashAlgo::Xxh3_128)
        );
    }
}
