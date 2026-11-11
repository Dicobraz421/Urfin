//! `arc.extract` (Shift+F2, S8 §3.5, §3.7): строка распаковки — черновик
//! `UiEvent::Ext { id: "arc.draft" }`, подтверждение — `ops.copy` с опциями S8-27.
//!
//! - Без `confirmed`: выделенное внутри архива (активная панель в архиве) или выделенные
//!   архивы в панели без входа. Заголовки читает задача `Lane::Interactive`; итог —
//!   `arc.draft { stage: "open", … }` (размер, имя подпапки, признак пароля).
//! - `confirmed=true`: `dest`, `subdir`, `conflict`, `paths`, `queue`, `password`. Пароль
//!   проверяется чтением самой маленькой зашифрованной записи (не в UI-потоке) и кладётся
//!   в [`crate::ArchiveIndex::ctx`] до `ops.copy`; неверный — `arc.draft { stage:
//!   "wrong", left }`, три попытки. Запуск — по `ops.copy` на архив, затем `arc.draft {
//!   stage: "close" }`.
//! - `cancel=true` (Esc) — черновик забыт, ничего не запускается.
//!
//! Пароль не попадает ни в логи, ни в состояние: из аргумента команды — сразу в
//! `Zeroizing`, дальше только в `OpenCtx` индекса.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use urfin_core::{
    Args, Attrs, CancelToken, CmdError, Command, CommandId, Cx, EntryKind, Lane, Level, Location,
    UiEvent, msg,
};
use urfin_vfs::{VfsError, VfsRegistry};
use zeroize::Zeroizing;

use crate::avfs::root_of;
use crate::cache::ArcCache;
use crate::detect::{self, ArcKind};
use crate::index::{ArchiveIndex, NodeId, ROOT};

/// Схемы архивов (`ArcKind::scheme`).
const SCHEMES: [&str; 4] = ["zip", "tar", "7z", "rar"];

/// Попыток пароля (S8 §3.7).
pub(crate) const TRIES: u8 = 3;

/// Байт зашифрованной записи, которые читает проверка пароля: запись не больше —
/// целиком (CRC и подпись AES), больше — начало (распаковщик ломается на чужом ключе).
const PROBE_FULL: u64 = 4 << 20;
const PROBE_HEAD: u64 = 256 << 10;

/// Черновик распаковки в `AppState.ext` (один на окно).
#[derive(Default)]
pub(crate) struct Drafts {
    seq: u64,
    cur: Option<Draft>,
}

struct Draft {
    seq: u64,
    /// Внутри архива: каталог панели (выделенное в нём берёт `ops.copy`).
    inside: Option<Location>,
    /// Архивы; пусто, пока заголовки читаются.
    arcs: Vec<Found>,
    ready: bool,
    left: u8,
    checking: bool,
}

/// Архив черновика.
#[derive(Clone, Debug)]
struct Found {
    file: Location,
    scheme: &'static str,
    /// Имя подпапки: имя файла без расширения архива.
    stem: String,
    /// Зашифрованная запись для проверки пароля.
    probe: Option<Location>,
}

/// Что выбрано при Shift+F2.
enum Want {
    /// Выделенное в каталоге `dir` архива `file`.
    Inside {
        file: Location,
        scheme: &'static str,
        dir: Location,
        names: Vec<String>,
    },
    /// Архивы в панели: файл и вид по имени.
    Files(Vec<(Location, ArcKind)>),
}

/// Итог чтения заголовков.
struct Scanned {
    arcs: Vec<Found>,
    size: u64,
    /// Размер — файлов архива (сжатый tar без индекса), а не распакованного.
    estimate: bool,
    encrypted: bool,
    cipher: Option<&'static str>,
}

fn drafts<'a>(cx: &'a mut Cx<'_>) -> &'a mut Drafts {
    if cx.state.ext.get::<Drafts>().is_none() {
        cx.state.ext.insert(Drafts::default());
    }
    cx.state.ext.get_mut().expect("drafts are inserted above")
}

fn tr(key: &str) -> String {
    urfin_i18n::current().get(key)
}

fn rejected(key: &str) -> CmdError {
    CmdError::Rejected(tr(key).into())
}

fn draft_event(args: Args) -> UiEvent {
    UiEvent::Ext {
        id: CommandId::from_static("arc.draft"),
        args,
    }
}

fn stage(s: &str) -> Args {
    Args::default().with("stage", s)
}

fn as_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Имя файла адреса.
fn name_of(l: &Location) -> String {
    l.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Имя подпапки: без расширения архива (`urfin-0.4.2.tar.gz` → `urfin-0.4.2`); своё
/// расширение (`.cbz`) — без последнего.
fn stem_of(name: &str) -> String {
    let cut = detect::ext_in(name, detect::archive_exts())
        .map(|e| name.len() - e.len() - 1)
        .or_else(|| name.rfind('.').filter(|&i| i > 0));
    cut.map_or(name, |i| &name[..i]).to_owned()
}

/// Сжатый поток без оглавления: индекс — полная распаковка, для черновика не читаем.
fn streamed(k: ArcKind) -> bool {
    !matches!(
        k,
        ArcKind::Zip | ArcKind::SevenZ | ArcKind::Rar | ArcKind::Tar
    )
}

/// Назначение по умолчанию — другая панель, с разделителем на конце (как у F5).
fn default_dest(l: &Location) -> String {
    if !l.is_local() {
        return l.to_uri();
    }
    let mut s = l.path.display().to_string();
    if !s.ends_with(['\\', '/']) {
        s.push('\\');
    }
    s
}

/// Назначение архива: `dest` и подпапка (с разделителем — «внутрь», каталог создаётся).
fn compose(dest: &str, sub: Option<&str>) -> String {
    let d = dest.trim();
    let Some(sub) = sub.filter(|s| !s.is_empty()) else {
        return d.to_owned();
    };
    let sep = if d.contains("://") || d.starts_with('/') {
        '/'
    } else {
        '\\'
    };
    let mut s = d.to_owned();
    if !s.ends_with(['\\', '/']) {
        s.push(sep);
    }
    s.push_str(sub);
    s.push(sep);
    s
}

/// Индекс архива `file`: из кэша или построением (поток задачи, не UI).
fn index_of(
    reg: &VfsRegistry,
    cache: &ArcCache,
    scheme: &'static str,
    file: &Location,
) -> Result<Arc<ArchiveIndex>, VfsError> {
    let root = root_of(scheme, file.clone());
    reg.get(&root)?.stat(&root, false)?;
    cache
        .latest(file)
        .ok_or_else(|| VfsError::NotFound(file.path.clone()))
}

fn entry_at(scheme: &'static str, file: &Location, idx: &ArchiveIndex, id: NodeId) -> Location {
    Location {
        backend: scheme.into(),
        authority: None,
        path: PathBuf::from(idx.path_of(id)),
        outer: Some(Arc::new(file.clone())),
    }
}

/// Байт файлов в `ids` (с поддеревьями) и самая маленькая зашифрованная запись среди них.
fn sum(idx: &ArchiveIndex, ids: &[NodeId]) -> (u64, Option<NodeId>) {
    let mut size = 0;
    let mut probe: Option<(u64, NodeId)> = None;
    for &id in ids {
        let (s, _) = idx.subtree(id, |f| {
            let n = idx.node(f);
            if n.attrs.has(Attrs::ENCRYPTED) && probe.is_none_or(|(p, _)| n.size < p) {
                probe = Some((n.size, f));
            }
        });
        size += s;
    }
    (size, probe.map(|(_, id)| id))
}

/// Чтение заголовков для черновика (`Lane::Interactive`).
fn scan(reg: &VfsRegistry, cache: &ArcCache, want: Want) -> Result<Scanned, VfsError> {
    let mut out = Scanned {
        arcs: Vec::new(),
        size: 0,
        estimate: false,
        encrypted: false,
        cipher: None,
    };
    let add = |out: &mut Scanned, idx: &ArchiveIndex, found: Found, ids: &[NodeId]| {
        let (size, probe) = sum(idx, ids);
        out.size += size;
        let probe = probe.map(|id| entry_at(found.scheme, &found.file, idx, id));
        if probe.is_some() {
            out.encrypted = true;
            out.cipher = out.cipher.or(idx.cipher);
        }
        out.arcs.push(Found { probe, ..found });
    };
    match want {
        Want::Inside {
            file,
            scheme,
            dir,
            names,
        } => {
            let idx = index_of(reg, cache, scheme, &file)?;
            let d = idx
                .lookup(&dir.path)
                .map(|d| idx.follow(d))
                .ok_or_else(|| VfsError::NotFound(dir.path.clone()))?;
            let ids: Vec<NodeId> = names.iter().filter_map(|n| idx.child(d, n)).collect();
            let found = Found {
                stem: stem_of(&name_of(&file)),
                file,
                scheme,
                probe: None,
            };
            add(&mut out, &idx, found, &ids);
        }
        Want::Files(files) => {
            for (file, kind) in files {
                let found = Found {
                    stem: stem_of(&name_of(&file)),
                    file,
                    scheme: kind.scheme(),
                    probe: None,
                };
                if streamed(kind) && cache.latest(&found.file).is_none() {
                    let m = reg.get(&found.file)?.stat(&found.file, true)?;
                    out.size += m.size;
                    out.estimate = true;
                    out.arcs.push(found);
                    continue;
                }
                let idx = index_of(reg, cache, found.scheme, &found.file)?;
                add(&mut out, &idx, found, &[ROOT]);
            }
        }
    }
    Ok(out)
}

/// Пароль подходит к записи `probe`.
fn password_fits(reg: &VfsRegistry, probe: &Location, size: u64) -> Result<bool, VfsError> {
    // Целиком — с запасом в байт: проверка CRC и подписи AES — на конце потока.
    let take = if size <= PROBE_FULL {
        size + 1
    } else {
        PROBE_HEAD
    };
    let fits = |e: &std::io::Error| {
        !matches!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidData
        )
    };
    let mut r = match reg.get(probe)?.open_read(probe) {
        Ok(r) => r,
        Err(VfsError::Io { source, .. }) if !fits(&source) => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut sink = Vec::new();
    match r.by_ref().take(take).read_to_end(&mut sink) {
        Ok(_) => Ok(true),
        Err(e) if !fits(&e) => Ok(false),
        Err(e) => Err(VfsError::from_io(probe.path.clone(), e)),
    }
}

/// Пароль — в индексы зашифрованных архивов; не подошёл — убирается.
fn try_password(
    reg: &VfsRegistry,
    cache: &ArcCache,
    arcs: &[Found],
    pw: &str,
) -> Result<bool, VfsError> {
    let mut set = Vec::new();
    for a in arcs {
        let Some(probe) = &a.probe else {
            continue;
        };
        let idx = index_of(reg, cache, a.scheme, &a.file)?;
        idx.ctx().set_password(Some(pw));
        set.push(Arc::clone(&idx));
        let size = idx.lookup(&probe.path).map_or(0, |id| idx.node(id).size);
        let ok = password_fits(reg, probe, size);
        if !matches!(ok, Ok(true)) {
            for i in &set {
                i.ctx().set_password(None);
            }
            return ok;
        }
    }
    Ok(true)
}

/// `arc.extract` без подтверждения: что распаковывать, и чтение заголовков задачей.
pub(crate) fn start(
    cx: &mut Cx<'_>,
    reg: &Arc<VfsRegistry>,
    cache: &Arc<ArcCache>,
) -> Result<(), CmdError> {
    let tab = cx.state.active_tab();
    let targets = tab.targets().ok_or_else(|| rejected("ops-nothing"))?;
    let in_arc = tab.loc.outer.as_deref().and_then(|outer| {
        let scheme = SCHEMES.into_iter().find(|s| *s == tab.loc.backend)?;
        Some((outer.clone(), scheme))
    });
    let want = if let Some((file, scheme)) = in_arc {
        Want::Inside {
            file,
            scheme,
            dir: tab.loc.clone(),
            names: targets.items.iter().map(|e| e.name.to_string()).collect(),
        }
    } else {
        let files: Vec<(Location, ArcKind)> = targets
            .items
            .iter()
            .filter(|e| !e.kind.is_dir_like() && e.kind != EntryKind::LinkBroken)
            .filter_map(|e| {
                let kind = detect::by_name(&e.name)?;
                let file = reg.get(&tab.loc).ok()?.resolve(&tab.loc, e).ok()?;
                reg.get(&root_of(kind.scheme(), file.clone())).ok()?;
                Some((file, kind))
            })
            .collect();
        if files.is_empty() {
            return Err(rejected("arc-error-format"));
        }
        Want::Files(files)
    };
    let inside = matches!(want, Want::Inside { .. }).then(|| tab.loc.clone());
    let dest = default_dest(&cx.state.panel(cx.state.active.other()).tab().loc);
    let d = drafts(cx);
    d.seq += 1;
    let seq = d.seq;
    d.cur = Some(Draft {
        seq,
        inside,
        arcs: Vec::new(),
        ready: false,
        left: TRIES,
        checking: false,
    });
    let (reg, cache) = (Arc::clone(reg), Arc::clone(cache));
    cx.rt
        .spawn(Lane::Interactive, "arc.draft", cx.rt.token(), move |tcx| {
            let res = scan(&reg, &cache, want);
            let done = msg("arc.draft", move |cx| opened(cx, seq, res, &dest));
            let _ = tcx.sink.send(done, &tcx.cancel);
        });
    Ok(())
}

/// Заголовки прочитаны: черновик в строку.
fn opened(cx: &mut Cx<'_>, seq: u64, res: Result<Scanned, VfsError>, dest: &str) {
    let d = drafts(cx);
    let Some(inside) = d
        .cur
        .as_ref()
        .filter(|c| c.seq == seq)
        .map(|c| c.inside.is_some())
    else {
        return;
    };
    let s = match res {
        Ok(s) => s,
        Err(e) => {
            d.cur = None;
            cx.toast(Level::Error, e.to_string());
            return;
        }
    };
    let Some(first) = s.arcs.first() else {
        d.cur = None;
        cx.toast(Level::Warn, tr("arc-error-format"));
        return;
    };
    let mut args = stage("open")
        .with("name", name_of(&first.file))
        .with("subdir", first.stem.as_str())
        .with("count", as_i64(s.arcs.len() as u64))
        .with("size", as_i64(s.size))
        .with("estimate", s.estimate)
        .with("dest", dest)
        .with("encrypted", s.encrypted)
        .with("names_encrypted", false);
    if inside {
        args = args.with("items", cx_items(cx));
    }
    if s.encrypted {
        args = args
            .with("cipher", s.cipher.unwrap_or("ZipCrypto"))
            .with("left", i64::from(TRIES));
    }
    let d = drafts(cx);
    if let Some(cur) = d.cur.as_mut() {
        cur.arcs = s.arcs;
        cur.ready = true;
    }
    cx.ui(draft_event(args));
}

/// Выделено в активной панели (внутри архива).
fn cx_items(cx: &Cx<'_>) -> i64 {
    cx.state
        .active_tab()
        .targets()
        .map_or(0, |t| as_i64(t.items.len() as u64))
}

/// `arc.extract confirmed=true`: пароль (если нужен) и запуск.
pub(crate) fn confirm(
    cx: &mut Cx<'_>,
    a: &Args,
    reg: &Arc<VfsRegistry>,
    cache: &Arc<ArcCache>,
) -> Result<(), CmdError> {
    let dest = a
        .str("dest")
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| rejected("ops-dest-empty"))?
        .to_owned();
    let d = drafts(cx);
    let Some(cur) = d.cur.as_mut().filter(|c| c.ready) else {
        return Err(rejected("ops-nothing"));
    };
    if cur.checking {
        cx.keep_rev();
        return Ok(());
    }
    let opts = Launch::of(a, dest);
    if !cur.arcs.iter().any(|f| f.probe.is_some()) {
        return launch(cx, &opts);
    }
    let pw = Zeroizing::new(a.str("password").unwrap_or_default().to_owned());
    if pw.is_empty() {
        let left = cur.left;
        cx.ui(draft_event(stage("wrong").with("left", i64::from(left))));
        return Ok(());
    }
    cur.checking = true;
    let (seq, arcs) = (cur.seq, cur.arcs.clone());
    let (reg, cache) = (Arc::clone(reg), Arc::clone(cache));
    cx.rt.spawn(
        Lane::Interactive,
        "arc.password",
        CancelToken::new(),
        move |tcx| {
            let ok = try_password(&reg, &cache, &arcs, &pw);
            drop(pw);
            let done = msg("arc.password", move |cx| checked(cx, seq, ok, &opts));
            let _ = tcx.sink.send(done, &tcx.cancel);
        },
    );
    Ok(())
}

/// Пароль проверен.
fn checked(cx: &mut Cx<'_>, seq: u64, ok: Result<bool, VfsError>, opts: &Launch) {
    let d = drafts(cx);
    let Some(cur) = d.cur.as_mut().filter(|c| c.seq == seq) else {
        return;
    };
    cur.checking = false;
    match ok {
        Ok(true) => {
            if let Err(CmdError::Rejected(t)) = launch(cx, opts) {
                cx.toast(Level::Warn, t.to_string());
            }
        }
        Ok(false) => {
            cur.left = cur.left.saturating_sub(1);
            let left = cur.left;
            tracing::info!(target: "arc", left, "wrong archive password");
            if left == 0 {
                d.cur = None;
                cx.toast(Level::Warn, tr("arc-pw-exhausted"));
                cx.ui(draft_event(stage("close")));
            } else {
                cx.ui(draft_event(stage("wrong").with("left", i64::from(left))));
            }
        }
        Err(e) => {
            d.cur = None;
            cx.toast(Level::Error, e.to_string());
            cx.ui(draft_event(stage("close")));
        }
    }
}

/// Опции строки → аргументы `ops.copy`.
#[derive(Clone)]
struct Launch {
    dest: String,
    subdir: bool,
    conflict: Option<String>,
    paths: Option<bool>,
    queue: Option<String>,
}

impl Launch {
    fn of(a: &Args, dest: String) -> Launch {
        Launch {
            dest,
            subdir: a.bool("subdir").unwrap_or(true),
            conflict: a.str("conflict").map(str::to_owned),
            paths: a.bool("paths"),
            queue: a.str("queue").map(str::to_owned),
        }
    }

    fn command(&self, sub: &str) -> Command {
        let mut c =
            Command::new("ops.copy").arg("dest", compose(&self.dest, self.subdir.then_some(sub)));
        if let Some(v) = &self.conflict {
            c = c.arg("conflict", v.as_str());
        }
        if let Some(v) = self.paths {
            c = c.arg("paths", v);
        }
        if let Some(v) = &self.queue {
            c = c.arg("queue", v.as_str());
        }
        c
    }
}

/// `ops.copy` на архив (внутри архива — одна, выделенное берёт ops); строка закрывается.
fn launch(cx: &mut Cx<'_>, opts: &Launch) -> Result<(), CmdError> {
    let Some(cur) = drafts(cx).cur.take() else {
        return Err(rejected("ops-nothing"));
    };
    let mut cmds = Vec::with_capacity(cur.arcs.len());
    if let Some(dir) = &cur.inside {
        if cx.state.active_tab().loc != *dir {
            return Err(rejected("ops-nothing"));
        }
        if let Some(f) = cur.arcs.first() {
            cmds.push(opts.command(&f.stem));
        }
    } else {
        for f in &cur.arcs {
            let src = root_of(f.scheme, f.file.clone()).to_uri();
            cmds.push(opts.command(&f.stem).arg("src", src));
        }
    }
    tracing::info!(target: "arc", jobs = cmds.len(), inside = cur.inside.is_some(), "extract");
    for c in cmds {
        cx.dispatch(c);
    }
    cx.ui(draft_event(stage("close")));
    Ok(())
}

/// Esc: черновик забыт.
pub(crate) fn cancel(cx: &mut Cx<'_>) {
    drafts(cx).cur = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_and_destinations() {
        assert_eq!(stem_of("urfin-0.4.2.tar.gz"), "urfin-0.4.2");
        assert_eq!(stem_of("backup_1019.7z"), "backup_1019");
        assert_eq!(stem_of("Comic.CBZ"), "Comic");
        assert_eq!(stem_of("noext"), "noext");
        assert_eq!(compose(r"C:\Dev\urfin\", Some("a")), r"C:\Dev\urfin\a\");
        assert_eq!(compose(r"C:\Dev\urfin", Some("a")), r"C:\Dev\urfin\a\");
        assert_eq!(compose(r"C:\Dev\urfin\", None), r"C:\Dev\urfin\");
        assert_eq!(compose("sftp://srv/var", Some("a")), "sftp://srv/var/a/");
        assert_eq!(default_dest(&Location::local(r"C:\Dev")), r"C:\Dev\");
    }
}
