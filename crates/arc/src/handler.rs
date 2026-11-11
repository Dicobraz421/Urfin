//! Handler `arc` и резолвер Enter (S8 §3.1). Живёт в UI-потоке: сигнатуру читает задача
//! `Lane::Interactive`, сводки архивов — события [`ArcCache`] в `ArcModel`. Ставит вопрос
//! пароля потокам VFS ([`crate::password::UiAsk`]); `arc.test` — job проверки,
//! `arc.commands` — меню `UiEvent::Ext` (S8-35).

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use urfin_core::arc::ArcModel;
use urfin_core::{
    ArgSpec, Args, Attrs, CancelToken, CmdError, Command, CommandId, CommandMeta, Cx, Entry,
    EntryKind, Handler, JobHandle, JobSpec, JobSummary, Lane, Level, Location, OpenResolver, Phase,
    Row, UiEvent, Value, ValueKind, msg,
};
use urfin_vfs::{VfsError, VfsRegistry};

use crate::avfs::root_of;
use crate::cache::{ArcCache, ArcEvent};
use crate::config::ArcCfg;
use crate::detect::{self, ArcKind, HEAD_LEN};
use crate::extract::{SCHEMES, entry_at, index_of};
use crate::format::no_password;
use crate::index::ROOT;
use crate::password::{self, JobAsk, UiAsk};

const fn meta(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "arc",
        args,
        repeatable: false,
    }
}

const fn arg(name: &'static str, kind: ValueKind) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required: false,
    }
}

/// `arc.extract`: без `confirmed` — черновик строки (`arc.draft`); с ним — опции строки
/// (`dest`, «В подпапку» `subdir`, «Если файл есть» `conflict`, «Сохранять пути» `paths`,
/// «В фоне» `queue=bg`, `password`); `cancel` — Esc.
const EXTRACT_ARGS: [ArgSpec; 8] = [
    arg("confirmed", ValueKind::Bool),
    arg("cancel", ValueKind::Bool),
    arg("dest", ValueKind::Str),
    arg("subdir", ValueKind::Bool),
    arg("conflict", ValueKind::Str),
    arg("paths", ValueKind::Bool),
    arg("queue", ValueKind::Str),
    arg("password", ValueKind::Str),
];

/// Команды `arc.*` (S8 §3.1, §3.5, §3.6). Упаковка — M5b (S8-40).
pub static COMMANDS: [CommandMeta; 5] = [
    meta("arc.enter", "Enter archive", &[]),
    meta("arc.extract", "Extract", &EXTRACT_ARGS),
    meta("arc.add", "Pack", &[]),
    meta("arc.commands", "Archive commands", &[]),
    meta("arc.test", "Test archive", &[]),
];

/// Глубина цепочки `outer` у адреса (не больше 8, `LocError::TooDeep`).
fn depth(loc: &Location) -> usize {
    std::iter::successors(Some(loc), |l| l.outer.as_deref()).count()
}

/// Есть backend схемы.
fn has_scheme(reg: &VfsRegistry, scheme: &str) -> bool {
    reg.get(&Location {
        backend: scheme.into(),
        ..Location::local("")
    })
    .is_ok()
}

/// Настоящий адрес файла строки `e` каталога `dir` (у `search:` — файл источника).
fn file_of(reg: &VfsRegistry, dir: &Location, e: &Entry) -> Option<Location> {
    let file = reg.get(dir).ok()?.resolve(dir, e).ok()?;
    (depth(&file) < 8).then_some(file)
}

fn go(scheme: &str, file: Location) -> Command {
    Command::new("panel.go").arg("path", root_of(scheme, file))
}

fn tr(key: &str) -> String {
    urfin_i18n::current().get(key)
}

/// Enter по файлу архива: расширение из `[arc] enter_exts`. Известный вид — сразу в
/// корень архива; своё расширение (`cbz`) — `arc.enter`, формат по сигнатуре.
struct EnterResolver {
    exts: Vec<String>,
    reg: Arc<VfsRegistry>,
}

impl OpenResolver for EnterResolver {
    fn resolve(&self, dir: &Location, e: &Entry, _args: &Args) -> Option<Command> {
        if e.kind.is_dir_like() {
            return None;
        }
        let ext = detect::ext_in(&e.name, self.exts.iter().map(String::as_str))?;
        let Some(kind) = detect::by_name(&format!("x.{}", ext.trim_start_matches('.'))) else {
            return Some(Command::new("arc.enter"));
        };
        if !has_scheme(&self.reg, kind.scheme()) {
            // Формат ещё не подключён: файл откроет ОС.
            return None;
        }
        Some(go(kind.scheme(), file_of(&self.reg, dir, e)?))
    }
}

/// Резолвер Enter: регистрируется первым (`Core::add_open_resolver`).
pub fn open_resolver(cfg: &ArcCfg, reg: Arc<VfsRegistry>) -> Arc<dyn OpenResolver> {
    Arc::new(EnterResolver {
        exts: cfg.enter_exts.clone(),
        reg,
    })
}

/// Handler `arc`.
pub struct ArcHandler {
    reg: Arc<VfsRegistry>,
    cache: Arc<ArcCache>,
    installed: bool,
}

impl std::fmt::Debug for ArcHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArcHandler")
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

fn model<'a>(cx: &'a mut Cx<'_>) -> &'a mut ArcModel {
    if cx.state.ext.get::<ArcModel>().is_none() {
        cx.state.ext.insert(ArcModel::default());
    }
    cx.state.ext.get_mut().expect("model is inserted above")
}

/// Первые байты файла.
fn head_of(reg: &VfsRegistry, file: &Location) -> Result<Vec<u8>, String> {
    let mut r = reg
        .get(file)
        .and_then(|v| v.open_read(file))
        .map_err(|e| e.to_string())?;
    let mut head = Vec::with_capacity(HEAD_LEN);
    r.by_ref()
        .take(HEAD_LEN as u64)
        .read_to_end(&mut head)
        .map_err(|e| e.to_string())?;
    Ok(head)
}

impl ArcHandler {
    pub fn new(reg: Arc<VfsRegistry>, cache: Arc<ArcCache>) -> ArcHandler {
        ArcHandler {
            reg,
            cache,
            installed: false,
        }
    }

    /// `arc.enter` (Ctrl+Shift+PgDn): вход в файл под курсором по сигнатуре — и в
    /// `.docx`, и в архив с чужим расширением.
    fn enter(&self, cx: &mut Cx<'_>) {
        let tab = cx.state.active_tab();
        let file = match tab.current() {
            Some(Row::Entry { entry, .. }) if !entry.kind.is_dir_like() => {
                file_of(&self.reg, &tab.loc, entry)
            }
            _ => None,
        };
        let Some(file) = file else {
            cx.keep_rev();
            return;
        };
        let reg = Arc::clone(&self.reg);
        cx.rt
            .spawn(Lane::Interactive, "arc.detect", cx.rt.token(), move |tcx| {
                let name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let found: Result<Option<ArcKind>, String> = head_of(&reg, &file)
                    .map(|h| detect::detect(&name, &h).filter(|k| has_scheme(&reg, k.scheme())));
                let done = msg("arc.enter", move |cx| match found {
                    Ok(Some(kind)) => cx.dispatch(go(kind.scheme(), file)),
                    Ok(None) => cx.toast(Level::Warn, tr("arc-error-format")),
                    Err(e) => cx.toast(Level::Error, e),
                });
                let _ = tcx.sink.send(done, &tcx.cancel);
            });
    }

    /// `arc.test`: job на архив, поток — выделенный; пароль — `JobHandle::ask`.
    fn test(&self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (found, _) = archives(cx, &self.reg);
        if found.is_empty() {
            return Err(CmdError::Rejected(tr("arc-error-format").into()));
        }
        for (file, scheme) in found {
            let job = cx.state.jobs.start(
                JobSpec {
                    kind: "arc.test",
                    title: format!("{} · {}", tr("arc-job-test"), name_of(&file)),
                    lane: Lane::Bulk,
                    priority: 0,
                    owner_ns: "arc",
                },
                cx.rt.sink(),
            );
            let (reg, cache) = (Arc::clone(&self.reg), Arc::clone(&self.cache));
            cx.rt
                .spawn_dedicated("arc-test".into(), cx.rt.token(), move |tcx| {
                    let _span = tracing::info_span!("arc.test").entered();
                    let job = Arc::new(job);
                    let ask: Arc<dyn password::Ask> = Arc::new(JobAsk(Arc::clone(&job)));
                    let res = password::scoped(ask, || {
                        test_archive(&reg, &cache, scheme, &file, &job, &tcx.cancel)
                    });
                    let Ok(job) = Arc::try_unwrap(job) else {
                        return;
                    };
                    job.finish(match &res {
                        Ok(t) => Ok(JobSummary {
                            items: t.count,
                            bytes: t.bytes,
                            skipped: 0,
                            errors: t.bad.len() as u64,
                        }),
                        Err(e) => Err(e.to_string()),
                    });
                    tracing::info!(
                        target: "arc",
                        files = res.as_ref().map_or(0, |t| t.count),
                        broken = res.as_ref().map_or(0, |t| t.bad.len()),
                        "test"
                    );
                    let done = msg("arc.tested", move |cx| tested(cx, &file, res));
                    let _ = tcx.sink.send(done, &CancelToken::new());
                });
        }
        Ok(())
    }

    /// `arc.commands`: меню команд архива — `UiEvent::Ext { id: "arc.commands" }` с
    /// `commands` (id по порядку, [`MENU`]), `name` (первый архив), `count`, `inside`.
    fn menu(&self, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let (found, inside) = archives(cx, &self.reg);
        let Some((first, _)) = found.first() else {
            return Err(CmdError::Rejected(tr("arc-error-format").into()));
        };
        let commands: Vec<Value> = MENU.iter().map(|c| Value::Str((*c).into())).collect();
        let args = Args::default()
            .with("commands", Value::List(commands.into()))
            .with("name", name_of(first))
            .with("count", as_i64(found.len()))
            .with("inside", inside);
        cx.ui(UiEvent::Ext {
            id: CommandId::from_static("arc.commands"),
            args,
        });
        Ok(())
    }

    /// Сводки из кэша — в `ArcModel`.
    fn sync(&self, cx: &mut Cx<'_>) {
        let events = self.cache.take_events();
        if events.is_empty() {
            return;
        }
        let m = model(cx);
        for e in events {
            match e {
                ArcEvent::Set(at, s) => m.set(at, s),
                ArcEvent::Remove(at) => {
                    m.remove(&at);
                }
            }
        }
        cx.state.touch();
    }
}

/// Пунктов меню `arc.commands`, по порядку.
pub const MENU: [&str; 2] = ["arc.test", "arc.extract"];

/// Имён битых файлов в тосте `arc.test` — не больше.
const TOAST_NAMES: usize = 5;

/// Архивы под рукой: внутри архива — он сам (`inside`), иначе выделенные архивы панели
/// (без выделения — под курсором) подключённых схем.
fn archives(cx: &Cx<'_>, reg: &VfsRegistry) -> (Vec<(Location, &'static str)>, bool) {
    let tab = cx.state.active_tab();
    if let Some(outer) = tab.loc.outer.as_deref()
        && let Some(scheme) = SCHEMES.into_iter().find(|s| *s == tab.loc.backend)
    {
        return (vec![(outer.clone(), scheme)], true);
    }
    let Some(t) = tab.targets() else {
        return (Vec::new(), false);
    };
    let found = t
        .items
        .iter()
        .filter(|e| !e.kind.is_dir_like() && e.kind != EntryKind::LinkBroken)
        .filter_map(|e| {
            let kind = detect::by_name(&e.name)?;
            if !has_scheme(reg, kind.scheme()) {
                return None;
            }
            Some((file_of(reg, &tab.loc, e)?, kind.scheme()))
        })
        .collect();
    (found, false)
}

fn name_of(l: &Location) -> String {
    l.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn as_i64(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Итог проверки архива.
struct Tested {
    count: u64,
    bytes: u64,
    /// Битые файлы: пути внутри архива без `/` в начале.
    bad: Vec<String>,
}

/// Читает поток до конца в никуда; байты — в прогресс задачи.
fn drain(r: &mut dyn Read, job: &JobHandle) -> std::io::Result<()> {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        job.progress()
            .bytes_done
            .fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// `arc.test`: полная распаковка в никуда — CRC и подписи проверяют форматы при чтении.
/// Порядок — `extract_plan` (solid читается одним проходом); файл, который поток не
/// отдал или не дочитал, — битый.
fn test_archive(
    reg: &VfsRegistry,
    cache: &ArcCache,
    scheme: &'static str,
    file: &Location,
    job: &JobHandle,
    cancel: &CancelToken,
) -> Result<Tested, VfsError> {
    let idx = index_of(reg, cache, scheme, file)?;
    let mut ids = Vec::new();
    idx.subtree(ROOT, |f| {
        if idx.node(f).kind == EntryKind::File {
            ids.push(f);
        }
    });
    let bytes: u64 = ids.iter().map(|&id| idx.node(id).size).sum();
    let p = job.progress();
    p.items_total.store(ids.len() as u64, Ordering::Relaxed);
    p.bytes_total.store(bytes, Ordering::Relaxed);
    p.total_final.store(true, Ordering::Relaxed);
    p.set_phase(Phase::Verify);
    let locs: Vec<Location> = ids
        .iter()
        .map(|&id| entry_at(scheme, file, &idx, id))
        .collect();
    let vfs = reg.get(&root_of(scheme, file.clone()))?;
    let halted = || cancel.is_cancelled() || job.checkpoint().is_err();
    // План спрашивает пароль зашифрованных записей (`JobHandle::ask`, [`JobAsk`]).
    let plan = vfs.extract_plan(&locs, 1);
    let encrypted = ids
        .iter()
        .any(|&id| idx.node(id).attrs.has(Attrs::ENCRYPTED));
    if encrypted && !idx.ctx().has_password() {
        return Err(if halted() {
            VfsError::Cancelled
        } else {
            no_password(file)
        });
    }
    let mut ok = vec![false; locs.len()];
    let mut check = |i: usize, r: Result<&mut dyn Read, VfsError>| {
        p.items_done.fetch_add(1, Ordering::Relaxed);
        p.set_current(&idx.path_of(ids[i]));
        let read =
            r.and_then(|r| drain(r, job).map_err(|e| VfsError::from_io(file.path.clone(), e)));
        match read {
            Ok(()) => ok[i] = true,
            Err(e) => tracing::debug!(target: "arc", error = %e, "test: broken entry"),
        }
    };
    if let Some(streams) = plan {
        for mut s in streams {
            loop {
                if halted() {
                    return Err(VfsError::Cancelled);
                }
                match s.next(cancel) {
                    Ok(Some((i, mut r))) => check(i as usize, Ok(&mut *r)),
                    Ok(None) => break,
                    Err(VfsError::Cancelled) => return Err(VfsError::Cancelled),
                    Err(e) => {
                        tracing::debug!(target: "arc", error = %e, "test: stream broke");
                        break;
                    }
                }
            }
        }
    } else {
        for (i, l) in locs.iter().enumerate() {
            if halted() {
                return Err(VfsError::Cancelled);
            }
            match vfs.open_read(l) {
                Ok(mut r) => check(i, Ok(&mut *r)),
                Err(e) => check(i, Err(e)),
            }
        }
    }
    let bad = ids
        .iter()
        .zip(&ok)
        .filter(|(_, ok)| !**ok)
        .map(|(&id, _)| idx.path_of(id).trim_start_matches('/').to_owned())
        .collect();
    Ok(Tested {
        count: ids.len() as u64,
        bytes,
        bad,
    })
}

/// Итог `arc.test`: тост и `UiEvent::Ext { id: "arc.tested" }` с именами битых файлов
/// (`name`, `count`, `errors`, `bad`).
fn tested(cx: &mut Cx<'_>, file: &Location, res: Result<Tested, VfsError>) {
    let t = match res {
        Ok(t) => t,
        Err(VfsError::Cancelled) => return,
        Err(e) => {
            cx.toast(Level::Error, e.to_string());
            return;
        }
    };
    let i18n = urfin_i18n::current();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", t.count);
    if t.bad.is_empty() {
        cx.toast(Level::Info, i18n.fmt("arc-test-ok", &a));
    } else {
        a.set("errors", t.bad.len() as u64);
        let mut text = i18n.fmt("arc-test-failed", &a);
        text.push_str(": ");
        text.push_str(&t.bad[..t.bad.len().min(TOAST_NAMES)].join(", "));
        if t.bad.len() > TOAST_NAMES {
            text.push_str(", …");
        }
        cx.toast(Level::Warn, text);
    }
    let bad: Vec<Value> = t
        .bad
        .iter()
        .map(|n| Value::Str(n.as_str().into()))
        .collect();
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("arc.tested"),
        args: Args::default()
            .with("name", name_of(file))
            .with("count", i64::try_from(t.count).unwrap_or(i64::MAX))
            .with("errors", as_i64(bad.len()))
            .with("bad", Value::List(bad.into())),
    });
}

fn not_yet(cmd: &Command) -> CmdError {
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("what", cmd.id.as_str().to_owned());
    CmdError::Rejected(urfin_i18n::current().fmt("ops-not-yet", &args).into())
}

impl Handler for ArcHandler {
    fn namespace(&self) -> &'static str {
        "arc"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "arc.enter" => {
                self.enter(cx);
                Ok(())
            }
            "arc.extract" if cmd.args.bool("cancel") == Some(true) => {
                crate::extract::cancel(cx);
                Ok(())
            }
            "arc.extract" if cmd.args.bool("confirmed") == Some(true) => {
                crate::extract::confirm(cx, &cmd.args, &self.reg, &self.cache)
            }
            "arc.extract" => crate::extract::start(cx, &self.reg, &self.cache),
            "arc.test" => self.test(cx),
            "arc.commands" => self.menu(cx),
            _ => Err(not_yet(cmd)),
        }
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if !std::mem::replace(&mut self.installed, true) {
            self.cache
                .set_asker(Some(Arc::new(UiAsk::here(cx.rt.sink()))));
            model(cx);
            cx.state.touch();
        }
        self.sync(cx);
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
