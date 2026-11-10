//! Handler `arc` и резолвер Enter (S8 §3.1). Живёт в UI-потоке: сигнатуру читает задача
//! `Lane::Interactive`, сводки архивов — события [`ArcCache`] в `ArcModel`.

use std::io::Read;
use std::sync::Arc;

use urfin_core::arc::ArcModel;
use urfin_core::{
    ArgSpec, Args, CmdError, Command, CommandMeta, Cx, Entry, Handler, Lane, Level, Location,
    OpenResolver, Row, msg,
};
use urfin_vfs::VfsRegistry;

use crate::avfs::root_of;
use crate::cache::{ArcCache, ArcEvent};
use crate::config::ArcCfg;
use crate::detect::{self, ArcKind, HEAD_LEN};

const fn meta(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "arc",
        args,
        repeatable: false,
    }
}

/// Команды `arc.*` (S8 §3.1, §3.5, §3.6). Кроме `arc.enter` — пока заглушки: распаковка —
/// S8-28, меню и проверка — S8-35/36, упаковка — M5b (S8-40).
pub static COMMANDS: [CommandMeta; 5] = [
    meta("arc.enter", "Enter archive", &[]),
    meta("arc.extract", "Extract", &[]),
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
            _ => Err(not_yet(cmd)),
        }
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if !std::mem::replace(&mut self.installed, true) {
            model(cx);
            cx.state.touch();
        }
        self.sync(cx);
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
