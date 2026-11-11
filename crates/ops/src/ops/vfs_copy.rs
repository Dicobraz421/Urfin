//! Копирование и перенос через схему с записью — `Engine::Vfs` (F03 §4.1–4.2, F03-07):
//! закачка на сервер, скачивание, копия между серверами; тот же движок job, что у
//! локального копирования (очередь устройств, job-бар, немодальные конфликты).
//!
//! План (поток координатора): назначение по Far ([`resolve_dest`]); каталоги назначения
//! создаются до детей (слияние с существующим), каталог назначения листингуется один раз
//! (индекс имён), конфликт решается политикой до передачи данных. Перенос в пределах
//! одного сервера — `rename` элемента целиком.
//!
//! Воркер (до `channels` параллельно): данные — в `<имя>.urfin-part` рядом с целью,
//! затем `fsync@openssh.com` (если есть) и `rename` поверх (`posix-rename`, иначе
//! удаление и rename — решает backend). Докачка: `part` есть и не больше источника —
//! сверяется последний мебибайт (xxh3); совпало — дописываем с конца `part` («докачка с
//! N %»), иначе пишем заново. Обрыв связи посреди файла — «ждёт связи»: job ждёт
//! переподключения и продолжает с конца `part`; отключились совсем — ошибка элемента.
//! «Сохранить права и даты» — права (только у цели-сервера) и время изменения после
//! commit; каталоги — в Finalizer.
//!
//! Из архива (S8 §3.5, §6; S8-27): источник с `outer`, цель — только вне архива (отказ до
//! задачи — в handler). Данные — потоками `extract_plan` ([`super::vfs_extract`]), если
//! backend их даёт, иначе `open_read`; докачка — только у источника с `Caps::SEEK`.
//! Каждое имя назначения проверяется (`..`, `:`, зарезервированные — ошибка элемента),
//! ссылки из архива — ссылками только при `LinkMode::Preserve` и цели внутри
//! назначения, иначе файлом с текстом цели. Mark-of-the-Web файла архива
//! (`Zone.Identifier`) переносится на извлечённые файлы (`[arc] propagate_motw`).
//! «Без путей» — все файлы в один каталог; без имён в запросе — весь каталог источника.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use urfin_core::{
    Cancelled, Cx, Entry, EntryKind, JobId, JobSpec, JobSummary, Lane, Level, Location, Mask, Phase,
};
use urfin_vfs::VfsError;

use super::vfs_extract::{Extract, Outcome};
use crate::api::{
    ConflictPolicy, DestMode, ErrorPolicy, FailedItem, IoClass, Issue, IssueId, IssueKind,
    ItemMeta, ItemOp, LinkMode, OpKind, OpOptions, OpRequest, Preserve, Resolution, Verify,
};
use crate::dest::resolve_dest;
use crate::engine::{
    DirNode, ExecCx, ItemResult, JobEngine, Note, OpExec, Plan, PlanError, Resolved, Work,
};
use crate::remote::{End, Meta, err_text};
use urfin_vfs::Link;

/// Суффикс недописанного файла (F03 §4.2).
pub(crate) const PART: &str = ".urfin-part";
/// Сверяемый хвост `part` и источника при докачке.
const TAIL: u64 = 1 << 20;
/// Буфер передачи.
const BUF: usize = 256 << 10;
/// Шаг ожидания связи.
const LINK_POLL: Duration = Duration::from_millis(50);
/// Обрыв виден не сразу: столько ждём, что канал сочтут потерянным.
const LINK_GRACE: Duration = Duration::from_millis(500);
/// Ожиданий связи на один файл, не больше.
const LINK_WAITS: u32 = 20;

/// Как положить файл.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    Create,
    Replace,
}

/// Ссылка из архива (S8 §6).
#[derive(Clone, Debug)]
enum AsLink {
    /// Ссылкой: цель внутри назначения, `LinkMode::Preserve`.
    Make { target: PathBuf, dir: bool },
    /// Файлом с текстом цели.
    Text(Vec<u8>),
}

/// Работа воркера: файл или (перенос в пределах сервера) элемент целиком.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    src: Location,
    dst: Location,
    size: u64,
    mtime: i64,
    mode: Option<u32>,
    how: How,
    /// Перенос на том же сервере: `rename`.
    rename: bool,
    /// Номер файла в плане выборки из архива.
    slot: Option<u32>,
    link: Option<AsLink>,
}

/// Каталог: источник, назначение, время и права источника.
#[derive(Debug)]
pub(crate) struct DirData {
    src: Location,
    dst: Location,
    mtime: i64,
    mode: Option<u32>,
}

/// Запрос из handler.
pub(crate) struct Start {
    pub req: OpRequest,
    pub dest: Location,
    pub dest_raw: String,
    pub mask: Option<Mask>,
    pub src: End,
    pub dst: End,
    pub jobs: crate::config::DeviceJobs,
    /// «Без путей»: все файлы — прямо в назначение.
    pub flat: bool,
    /// Mark-of-the-Web архива — на извлечённые файлы (`[arc] propagate_motw`).
    pub motw: bool,
}

pub(crate) struct VfsTransfer {
    kind: OpKind,
    base: Location,
    items: Vec<CompactString>,
    dest: Location,
    dest_raw: String,
    opts: OpOptions,
    mask: Option<Mask>,
    src: End,
    dst: End,
    jobs: crate::config::DeviceJobs,
    /// Допуск времени «новее» и «одинаковые».
    tol: i64,
    flat: bool,
    motw: bool,
    workers: u16,
    /// `Zone.Identifier` файла архива (план).
    zone: OnceLock<Option<Vec<u8>>>,
    /// Потоки выборки из архива (план).
    extract: OnceLock<Extract<Item>>,
}

/// Обход плана.
struct Walk {
    /// Корень назначения: ссылки из архива — только внутрь него.
    root: Location,
    /// Источник в архиве: работы и закрытия каталогов — после плана выборки.
    buf: Option<Buffered>,
    /// «Без путей».
    flat: Option<Flat>,
}

/// Работа до постановки: каталог, работа, байт.
type Queued = (Option<Arc<DirNode<DirData>>>, Item, u64);

#[derive(Default)]
struct Buffered {
    works: Vec<Queued>,
    closes: Vec<Arc<DirNode<DirData>>>,
    /// Назначения, рядом с которыми есть `.urfin-part`: с `Caps::SEEK` — докачка, не поток.
    parts: HashSet<PathBuf>,
}

impl Walk {
    /// Рядом с `dst` лежит недописанный `part` (по индексу каталога назначения).
    fn note_part(&mut self, idx: &HashMap<CompactString, Meta>, end: &End, dst: &Location) {
        if let Some(b) = &mut self.buf
            && idx.contains_key(&end.key(&format!("{}{PART}", name_of(dst))))
        {
            b.parts.insert(dst.path.clone());
        }
    }
}

/// «Без путей»: индекс корня назначения и уже поставленные имена.
struct Flat {
    idx: HashMap<CompactString, Meta>,
    planned: HashSet<CompactString>,
}

/// Из handler (UI-поток, без I/O): job копирования или переноса через схему.
pub(crate) fn start(eng: &JobEngine, cx: &mut Cx<'_>, s: Start) -> JobId {
    let mv = s.req.kind == OpKind::Move;
    let title = super::copy::job_title(&s.req.items, &End::shown(&s.dest));
    let spec = JobSpec {
        kind: if mv { "move" } else { "copy" },
        title,
        lane: Lane::Bulk,
        priority: 1,
        owner_ns: "ops",
    };
    let channels = [&s.src, &s.dst]
        .iter()
        .filter_map(|e| e.channels())
        .min()
        .unwrap_or(1)
        .max(1);
    let workers = s
        .req
        .opts
        .workers
        .map_or(channels, |n| u16::from(n.get()).min(channels));
    let tol = s.src.time_tol(&s.req.base).max(s.dst.time_tol(&s.dest));
    let t = VfsTransfer {
        kind: s.req.kind,
        base: s.req.base,
        items: s.req.items,
        dest: s.dest,
        dest_raw: s.dest_raw,
        opts: s.req.opts,
        mask: s.mask,
        src: s.src,
        dst: s.dst,
        jobs: s.jobs,
        tol,
        flat: s.flat,
        motw: s.motw,
        workers,
        zone: OnceLock::new(),
        extract: OnceLock::new(),
    };
    eng.start(cx, spec, workers, t)
}

fn tr(key: &str, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

fn name_of(loc: &Location) -> String {
    loc.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `<имя>.urfin-part` рядом с `dst`.
pub(crate) fn part_of(end: &End, dst: &Location) -> Location {
    let parent = dst.parent().unwrap_or_else(|| dst.clone());
    end.join(&parent, &format!("{}{PART}", name_of(dst)))
}

/// Ошибка передачи.
enum Fail {
    Cancelled,
    Io(VfsError),
}

impl From<VfsError> for Fail {
    fn from(e: VfsError) -> Self {
        Fail::Io(e)
    }
}

impl From<Cancelled> for Fail {
    fn from(_: Cancelled) -> Self {
        Fail::Cancelled
    }
}

fn io_fail(e: io::Error) -> Fail {
    if e.kind() == io::ErrorKind::Interrupted {
        return Fail::Cancelled;
    }
    Fail::Io(VfsError::Io {
        path: PathBuf::new(),
        source: e,
    })
}

/// Ошибка, при которой стоит проверить связь (не «нет файла» и не «нет прав»).
fn maybe_net(e: &VfsError) -> bool {
    !matches!(
        e,
        VfsError::NotFound(_)
            | VfsError::Denied(_)
            | VfsError::NotDir(_)
            | VfsError::Cancelled
            | VfsError::NoWrite(_)
    )
}

fn class_of(e: &VfsError) -> IoClass {
    match e {
        VfsError::NotFound(_) => IoClass::NotFound,
        VfsError::Denied(_) => IoClass::Permission,
        VfsError::Io { source, .. } => match source.kind() {
            io::ErrorKind::NotConnected
            | io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe => IoClass::Network,
            io::ErrorKind::PermissionDenied => IoClass::Permission,
            io::ErrorKind::NotFound => IoClass::NotFound,
            _ => IoClass::Other,
        },
        _ => IoClass::Other,
    }
}

/// Прочитать `n` байт с позиции `at` (короче — конец файла).
fn read_at(r: &mut dyn urfin_vfs::VfsRead, at: u64, n: u64) -> io::Result<Vec<u8>> {
    r.seek(io::SeekFrom::Start(at))?;
    let mut out = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    r.take(n).read_to_end(&mut out)?;
    Ok(out)
}

impl VfsTransfer {
    fn op(&self) -> ItemOp {
        if self.kind == OpKind::Move {
            ItemOp::Move
        } else {
            ItemOp::Copy
        }
    }

    fn failed(&self, src: &Location, dst: Option<&Location>, e: &VfsError) -> FailedItem {
        let _ = dst;
        FailedItem {
            path: PathBuf::from(End::shown(src)),
            op: self.op(),
            error: err_text(e).into(),
            class: class_of(e),
            pair: None,
        }
    }

    fn exists_issue(&self, it: &Item, d: &Meta) -> Issue {
        Issue {
            id: IssueId(0),
            kind: IssueKind::Exists,
            src: Some(ItemMeta {
                name: name_of(&it.src).into(),
                size: it.size,
                mtime: it.mtime,
                attrs: 0,
            }),
            dst: Some(ItemMeta {
                name: name_of(&it.dst).into(),
                size: d.size,
                mtime: d.mtime,
                attrs: 0,
            }),
            op: self.op(),
            temp: None,
            error: None,
            path: None,
            code: None,
        }
    }

    /// Решение по существующему файлу назначения: `None` — пропустить, `Some(Err)` —
    /// вопрос.
    fn decide(&self, it: &Item, d: &Meta) -> Option<Result<How, ()>> {
        let (sm, dm) = (i128::from(it.mtime), i128::from(d.mtime));
        let tol = i128::from(self.tol);
        let same = it.size == d.size && (sm - dm).abs() <= tol;
        match self.opts.conflict {
            ConflictPolicy::Overwrite => Some(Ok(How::Replace)),
            ConflictPolicy::Skip => None,
            ConflictPolicy::OverwriteIfNewer => (sm > dm + tol).then_some(Ok(How::Replace)),
            ConflictPolicy::OverwriteIfDiffers => (!same).then_some(Ok(How::Replace)),
            ConflictPolicy::SkipIdentical => (!same).then_some(Err(())),
            // Новое имя и «старое — в .bak» у серверов — вопросом (F03-07: только
            // основные политики).
            ConflictPolicy::Ask
            | ConflictPolicy::RenameNew(_)
            | ConflictPolicy::RenameExisting(_) => Some(Err(())),
        }
    }

    /// Индекс каталога назначения: ключ имени → (вид, размер, время).
    fn index(&self, dir: &Location, cx: &ExecCx<'_>) -> HashMap<CompactString, Meta> {
        let Ok(list) = self.dst.list(dir, &cx.cancel_token()) else {
            return HashMap::new();
        };
        list.into_iter()
            .map(|e| {
                (
                    self.dst.key(&e.name),
                    Meta {
                        kind: e.kind,
                        size: e.size,
                        mtime: e.mtime,
                        mode: None,
                    },
                )
            })
            .collect()
    }

    /// Имя, которое нельзя создать в назначении (S8 §6: zip-slip, `:` и устройства на
    /// Windows): локально — правила NTFS, у сервера — разделитель и `.`/`..`.
    fn bad_name(&self, name: &str) -> bool {
        if self.dst.is_local() {
            crate::names::check_name(name).is_err()
        } else {
            name.is_empty() || name == "." || name == ".." || name.contains('/')
        }
    }

    fn bad_name_failed(&self, src: &Location) -> ItemResult {
        ItemResult::Failed(FailedItem {
            path: PathBuf::from(End::shown(src)),
            op: self.op(),
            error: tr("ops-issue-invalid-name", &[]).into(),
            class: IoClass::Other,
            pair: None,
        })
    }

    /// Работа — в очередь или (источник в архиве) в буфер до плана выборки.
    fn push(
        p: &mut Plan<'_, Self>,
        w: &mut Walk,
        parent: Option<&Arc<DirNode<DirData>>>,
        item: Item,
        bytes: u64,
    ) -> Result<(), Cancelled> {
        let Some(b) = &mut w.buf else {
            return p.push(parent, item, bytes);
        };
        b.works.push((parent.cloned(), item, bytes));
        if b.works.len().is_multiple_of(64) {
            p.cx().checkpoint()?;
        }
        Ok(())
    }

    fn close(p: &mut Plan<'_, Self>, w: &mut Walk, node: &Arc<DirNode<DirData>>) {
        match &mut w.buf {
            Some(b) => b.closes.push(Arc::clone(node)),
            None => p.close(node),
        }
    }

    /// Ссылка из архива: ссылкой, если можно, иначе файлом с текстом цели (S8 §6).
    fn as_link(&self, root: &Location, dst: &Location, target: PathBuf, dir: bool) -> AsLink {
        let inside = self.opts.links == LinkMode::Preserve
            && self.dst.is_local()
            && dst
                .parent()
                .is_some_and(|d| link_inside(&root.path, &d.path, &target));
        if inside {
            AsLink::Make { target, dir }
        } else {
            AsLink::Text(target.to_string_lossy().into_owned().into_bytes())
        }
    }

    /// Элемент источника `src` → `dst` (существующий в назначении — `existing`).
    #[allow(clippy::too_many_lines, reason = "шаг обхода: каталог и файл")]
    #[allow(
        clippy::too_many_arguments,
        reason = "шаг обхода: обход и пара адресов"
    )]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "адреса уходят в работу или узел каталога"
    )]
    fn put(
        &self,
        p: &mut Plan<'_, Self>,
        walk: &mut Walk,
        parent: Option<&Arc<DirNode<DirData>>>,
        src: Location,
        meta: Meta,
        dst: Location,
        existing: Option<Meta>,
    ) -> Result<(), Cancelled> {
        let mv_same = self.kind == OpKind::Move
            && walk.flat.is_none()
            && self.src.same_fs(&src, &self.dst, &dst);
        // Ссылка из архива (S8 §6): без цели — файл с данными записи.
        let mut link = None;
        if End::in_archive(&src) && matches!(meta.kind, EntryKind::LinkFile | EntryKind::LinkDir) {
            if self.opts.links == LinkMode::Skip {
                p.record(parent, ItemResult::Skipped);
                return Ok(());
            }
            if let Some(t) = self.src.link_target(&src) {
                link = Some(self.as_link(&walk.root, &dst, t, meta.kind == EntryKind::LinkDir));
            }
        }
        let item = |how| Item {
            src: src.clone(),
            dst: dst.clone(),
            size: meta.size,
            mtime: meta.mtime,
            mode: meta.mode,
            how,
            rename: mv_same,
            slot: None,
            link: link.clone(),
        };
        if meta.kind.is_dir_like() && link.is_none() {
            if walk.flat.is_none() && existing.is_some_and(|d| !d.kind.is_dir_like()) {
                let e = VfsError::Io {
                    path: PathBuf::new(),
                    source: io::ErrorKind::AlreadyExists.into(),
                };
                p.record(
                    parent,
                    ItemResult::Failed(self.failed(&src, Some(&dst), &e)),
                );
                return Ok(());
            }
            if mv_same && existing.is_none() {
                return Self::push(p, walk, parent, item(How::Create), 0);
            }
            // Ссылка на каталог на сервере — без захода внутрь (петли).
            if meta.kind == EntryKind::LinkDir && !self.src.is_local() {
                p.record(parent, ItemResult::Skipped);
                return Ok(());
            }
            let children = |p: &mut Plan<'_, Self>| self.src.list(&src, &p.cx().cancel_token());
            if walk.flat.is_some() {
                // «Без путей»: каталоги не создаются, файлы — в корень назначения.
                let list = match children(p) {
                    Ok(c) => c,
                    Err(VfsError::Cancelled) => return Err(Cancelled),
                    Err(e) => {
                        p.record(parent, ItemResult::Failed(self.failed(&src, None, &e)));
                        return Ok(());
                    }
                };
                for e in list {
                    if e.name.ends_with(PART) {
                        continue;
                    }
                    let s = self.src.join(&src, &e.name);
                    if self.bad_name(&e.name) {
                        p.record(parent, self.bad_name_failed(&s));
                        continue;
                    }
                    let m = self.meta_of(&src, &e);
                    let d = self.dst.join(&walk.root, &e.name);
                    let ex = walk
                        .flat
                        .as_ref()
                        .and_then(|f| f.idx.get(&self.dst.key(&e.name)).copied());
                    self.put(p, walk, parent, s, m, d, ex)?;
                }
                return Ok(());
            }
            if existing.is_none()
                && let Err(e) = self.dst.mkdir(&dst)
            {
                p.record(
                    parent,
                    ItemResult::Failed(self.failed(&src, Some(&dst), &e)),
                );
                return Ok(());
            }
            p.cx().touch(&dst.parent().unwrap_or_else(|| dst.clone()));
            let node = p.dir(
                parent,
                DirData {
                    src: src.clone(),
                    dst: dst.clone(),
                    mtime: meta.mtime,
                    mode: meta.mode,
                },
            )?;
            let list = match children(p) {
                Ok(c) => c,
                Err(VfsError::Cancelled) => return Err(Cancelled),
                Err(e) => {
                    p.record(Some(&node), ItemResult::Failed(self.failed(&src, None, &e)));
                    Self::close(p, walk, &node);
                    return Ok(());
                }
            };
            let idx = if existing.is_some() {
                self.index(&dst, &p.cx())
            } else {
                HashMap::new()
            };
            for e in list {
                if e.name.ends_with(PART) {
                    continue;
                }
                let s = self.src.join(&src, &e.name);
                if self.bad_name(&e.name) {
                    p.record(Some(&node), self.bad_name_failed(&s));
                    continue;
                }
                let m = self.meta_of(&src, &e);
                let d = self.dst.join(&dst, &e.name);
                let ex = idx.get(&self.dst.key(&e.name)).copied();
                walk.note_part(&idx, &self.dst, &d);
                self.put(p, walk, Some(&node), s, m, d, ex)?;
            }
            Self::close(p, walk, &node);
            return Ok(());
        }
        if let Some(m) = &self.mask
            && !m.matches(&name_of(&src))
        {
            return Ok(());
        }
        if let Some(f) = &mut walk.flat
            && !f.planned.insert(self.dst.key(&name_of(&dst)))
        {
            // Одноимённый файл из другой папки: остаётся первый (как `7z e -aos`).
            p.record(parent, ItemResult::Skipped);
            return Ok(());
        }
        let Some(d) = existing else {
            return Self::push(p, walk, parent, item(How::Create), meta.size);
        };
        if d.kind.is_dir_like() {
            let it = item(How::Create);
            let mut issue = self.exists_issue(&it, &d);
            issue.kind = IssueKind::TypeMismatch;
            return p.park(parent, it, meta.size, issue);
        }
        let it = item(How::Replace);
        match self.decide(&it, &d) {
            None => {
                p.record(parent, ItemResult::Skipped);
                Ok(())
            }
            Some(Ok(how)) => Self::push(p, walk, parent, Item { how, ..it }, meta.size),
            Some(Err(())) => {
                let issue = self.exists_issue(&it, &d);
                p.park(parent, it, meta.size, issue)
            }
        }
    }

    /// Буфер обхода — в очередь: файлы с данными — в план выборки (если backend его
    /// даёт), затем работы и закрытия каталогов.
    fn dispatch(&self, p: &mut Plan<'_, Self>, b: Buffered) -> Result<(), Cancelled> {
        let Buffered {
            mut works,
            closes,
            parts,
        } = b;
        let mut files = Vec::new();
        let mut at = Vec::new();
        for (k, (_, it, _)) in works.iter().enumerate() {
            let resume = parts.contains(&it.dst.path) && self.src.seekable(&it.src);
            if !it.rename && it.link.is_none() && !resume {
                files.push(it.src.clone());
                at.push(k);
            }
        }
        let parallel = u8::try_from(self.workers).unwrap_or(u8::MAX).max(1);
        if let Ok(n) = u32::try_from(files.len())
            && n > 0
            && let Some(streams) = self.src.extract_plan(&files, parallel)
        {
            tracing::info!(target: "ops", files = n, streams = streams.len(), "extract plan");
            let mut wanted = HashMap::with_capacity(files.len());
            for (i, &k) in (0..n).zip(&at) {
                works[k].1.slot = Some(i);
                wanted.insert(i, works[k].1.clone());
            }
            let _ = self.extract.set(Extract::new(streams, wanted));
        }
        for (parent, it, bytes) in works {
            p.push(parent.as_ref(), it, bytes)?;
        }
        for n in &closes {
            p.close(n);
        }
        Ok(())
    }

    /// Метаданные записи листинга источника (права — у сервера из `stat` не берём: для
    /// каталогов и файлов достаточно `Entry` + режим по требованию).
    fn meta_of(&self, dir: &Location, e: &Entry) -> Meta {
        // Права архива не переносим: цель — диск, а `stat` записи — лишний поиск по индексу.
        let mode = if self.src.is_local()
            || End::in_archive(dir)
            || !self.opts.preserve.contains(Preserve::TIMES)
        {
            None
        } else {
            self.src
                .stat(&self.src.join(dir, &e.name))
                .ok()
                .flatten()
                .and_then(|m| m.mode)
        };
        Meta {
            kind: e.kind,
            size: e.size,
            mtime: e.mtime,
            mode,
        }
    }

    /// Связь сторон элемента: худшая из двух.
    fn link(&self, it: &Item) -> Link {
        let a = self.src.link(&it.src);
        let b = self.dst.link(&it.dst);
        match (a, b) {
            (Link::Down, _) | (_, Link::Down) => Link::Down,
            (Link::Waiting, _) | (_, Link::Waiting) => Link::Waiting,
            _ => Link::Up,
        }
    }

    /// Ошибка из-за обрыва: ждём переподключения («ждёт связи»). `Ok(true)` — связь
    /// вернулась, повторить; `Ok(false)` — ошибка не сетевая или ждать нечего.
    fn wait_link(&self, it: &Item, cx: &ExecCx<'_>) -> Result<bool, Cancelled> {
        let t0 = Instant::now();
        loop {
            match self.link(it) {
                Link::Up if t0.elapsed() >= LINK_GRACE => return Ok(false),
                Link::Up => {}
                Link::Down => return Ok(false),
                Link::Waiting => break,
            }
            cx.checkpoint()?;
            std::thread::sleep(LINK_POLL);
        }
        let waiting = tr("ops-net-wait", &[]);
        cx.progress().set_current(&waiting);
        tracing::info!(target: "ops", src = %End::shown(&it.src), "waiting for connection");
        let r = loop {
            cx.checkpoint()?;
            match self.link(it) {
                Link::Up => break Ok(true),
                Link::Down => break Ok(false),
                Link::Waiting => std::thread::sleep(LINK_POLL),
            }
        };
        cx.progress().set_current("");
        r
    }

    /// Данные файла в `part`, с докачкой; `counted` — байт файла уже в прогрессе.
    fn transfer(
        &self,
        it: &Item,
        part: &Location,
        counted: &mut u64,
        cx: &ExecCx<'_>,
    ) -> Result<(), Fail> {
        let mut from = 0;
        // Докачка — только при произвольном доступе к источнику (S8 §3.5).
        if self.src.seekable(&it.src)
            && let Some(pm) = self.dst.stat(part)?
            && pm.size > 0
            && pm.size <= it.size
            && self.tail_matches(it, part, pm.size)?
        {
            from = pm.size;
        }
        // Прогресс — с начала докачки.
        let p = cx.progress();
        if from >= *counted {
            p.bytes_done.fetch_add(from - *counted, Ordering::Relaxed);
        } else {
            p.bytes_done.fetch_sub(*counted - from, Ordering::Relaxed);
        }
        *counted = from;
        if from > 0 {
            let pct = (u128::from(from) * 100 / u128::from(it.size.max(1))).to_string();
            p.set_phase(Phase::Recover);
            p.set_current(&tr("ops-resume-from", &[("pct", &pct)]));
            tracing::info!(target: "ops", file = %End::shown(&it.dst), from, "resume");
        }
        let mut r = self.src.open_read(&it.src)?;
        if from > 0 {
            r.seek(io::SeekFrom::Start(from)).map_err(io_fail)?;
        }
        let mut w = self.dst.open_write(part, (from > 0).then_some(from))?;
        let mut buf = vec![0u8; BUF];
        loop {
            cx.checkpoint()?;
            let n = r.read(&mut buf).map_err(io_fail)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n]).map_err(io_fail)?;
            cx.add_bytes(n as u64);
            *counted += n as u64;
        }
        drop(r);
        w.finish()?;
        if from > 0 {
            p.set_phase(Phase::Transfer);
            p.set_current("");
        }
        Ok(())
    }

    /// Последний мебибайт `part` и источника совпадает (xxh3).
    fn tail_matches(&self, it: &Item, part: &Location, size: u64) -> Result<bool, Fail> {
        let len = size.min(TAIL);
        let at = size - len;
        let mut src = self.src.open_read(&it.src)?;
        let mut dst = self.dst.open_read(part)?;
        let ours = read_at(src.as_mut(), at, len).map_err(io_fail)?;
        let theirs = read_at(dst.as_mut(), at, len).map_err(io_fail)?;
        let same = ours.len() == theirs.len()
            && xxhash_rust::xxh3::xxh3_64(&ours) == xxhash_rust::xxh3::xxh3_64(&theirs);
        if !same {
            tracing::info!(target: "ops", part = %End::shown(part), "foreign part, rewriting");
        }
        Ok(same)
    }

    /// Проверка после записи (S5 §4.9): размер — `stat`, хэш — чтение назад.
    fn verify(&self, it: &Item) -> Result<(), VfsError> {
        let bad = || VfsError::Io {
            path: PathBuf::new(),
            source: io::Error::other(tr("ops-verify-mismatch", &[])),
        };
        match self.opts.verify {
            Verify::Off => Ok(()),
            Verify::Size => match self.dst.stat(&it.dst)? {
                Some(m) if m.size == it.size => Ok(()),
                _ => Err(bad()),
            },
            Verify::Hash(_) => {
                let hash = |end: &End, l: &Location| -> Result<u128, VfsError> {
                    let mut r = end.open_read(l)?;
                    let mut h = xxhash_rust::xxh3::Xxh3::new();
                    let mut buf = vec![0u8; BUF];
                    loop {
                        let n = r
                            .read(&mut buf)
                            .map_err(|e| VfsError::from_io(&l.path, e))?;
                        if n == 0 {
                            break;
                        }
                        h.update(&buf[..n]);
                    }
                    Ok(h.digest128())
                };
                if hash(&self.src, &it.src)? == hash(&self.dst, &it.dst)? {
                    Ok(())
                } else {
                    Err(bad())
                }
            }
        }
    }

    /// Права (только у сервера-цели) и время после commit.
    fn preserve(&self, dst: &Location, mode: Option<u32>, mtime: i64) {
        if !self.opts.preserve.contains(Preserve::TIMES) {
            return;
        }
        let mode = if self.dst.is_local() { None } else { mode };
        let mtime = (mtime != i64::MIN).then_some(mtime);
        if let Err(e) = self.dst.set_meta(dst, mode.map(|m| m & 0o7777), mtime) {
            tracing::warn!(target: "ops", dst = %End::shown(dst), error = %e, "set times failed");
        }
    }

    /// Ошибка после ожиданий связи — по `ErrorPolicy`.
    fn on_error(&self, it: &Item, e: &VfsError, cx: &ExecCx<'_>) -> ItemResult {
        match self.opts.errors {
            ErrorPolicy::Ask => {
                let mut issue = self.exists_issue(
                    it,
                    &Meta {
                        kind: EntryKind::File,
                        size: 0,
                        mtime: i64::MIN,
                        mode: None,
                    },
                );
                issue.kind = IssueKind::Io(class_of(e));
                issue.dst = None;
                issue.error = Some(err_text(e).into());
                issue.path = Some(PathBuf::from(End::shown(&it.src)));
                ItemResult::Park(Box::new(issue), it.size)
            }
            ErrorPolicy::Skip => ItemResult::Failed(self.failed(&it.src, Some(&it.dst), e)),
            ErrorPolicy::Abort => {
                cx.abort(&format!("{}: {}", End::shown(&it.src), err_text(e)));
                ItemResult::Failed(self.failed(&it.src, Some(&it.dst), e))
            }
        }
    }

    fn touch_parent(cx: &ExecCx<'_>, l: &Location) {
        if let Some(p) = l.parent() {
            cx.touch(&p);
        }
    }

    /// Одна работа воркера.
    fn run(&self, it: &Item, cx: &ExecCx<'_>) -> ItemResult {
        if cx.checkpoint().is_err() {
            return ItemResult::Cancelled;
        }
        if it.rename {
            return match self.dst.rename(&it.src, &it.dst, it.how == How::Replace) {
                Ok(()) => {
                    Self::touch_parent(cx, &it.src);
                    Self::touch_parent(cx, &it.dst);
                    ItemResult::Done
                }
                Err(e) => self.on_error(it, &e, cx),
            };
        }
        if let Some(i) = it.slot
            && let Some(ex) = self.extract.get()
            && let Some(r) = ex.take(i, cx, &|it, r| self.write_streamed(it, r, cx))
        {
            return r;
        }
        if let Some(l) = &it.link {
            return self.put_link(it, l, cx);
        }
        let part = part_of(&self.dst, &it.dst);
        let mut counted = 0;
        let mut waits = 0;
        loop {
            let res = self.transfer(it, &part, &mut counted, cx).and_then(|()| {
                self.dst
                    .rename(&part, &it.dst, it.how == How::Replace)
                    .map_err(Fail::Io)
            });
            match res {
                Ok(()) => break,
                Err(Fail::Cancelled) => return ItemResult::Cancelled,
                Err(Fail::Io(e)) => {
                    let again = waits < LINK_WAITS
                        && maybe_net(&e)
                        && match self.wait_link(it, cx) {
                            Ok(up) => up,
                            Err(Cancelled) => return ItemResult::Cancelled,
                        };
                    if again {
                        waits += 1;
                        continue;
                    }
                    cx.progress()
                        .bytes_done
                        .fetch_sub(counted, Ordering::Relaxed);
                    cx.progress().set_current("");
                    return self.on_error(it, &e, cx);
                }
            }
        }
        self.commit(it, cx)
    }

    /// Файл на месте: проверка, Mark-of-the-Web, права и время, источник переноса.
    fn commit(&self, it: &Item, cx: &ExecCx<'_>) -> ItemResult {
        if it.link.is_none()
            && let Err(e) = self.verify(it)
        {
            return ItemResult::Failed(self.failed(&it.src, Some(&it.dst), &e));
        }
        // Поток `Zone.Identifier` меняет время файла: до `preserve`.
        self.mark_web(&it.dst);
        self.preserve(&it.dst, it.mode, it.mtime);
        Self::touch_parent(cx, &it.dst);
        if self.kind == OpKind::Move {
            if let Err(e) = self.src.remove(&it.src, false) {
                return ItemResult::Failed(self.failed(&it.src, None, &e));
            }
            Self::touch_parent(cx, &it.src);
        }
        ItemResult::Done
    }

    /// Данные из `r` — в `part`, затем на место; `counted` — байт в прогрессе.
    fn write_data(
        &self,
        it: &Item,
        r: &mut dyn Read,
        counted: &mut u64,
        cx: &ExecCx<'_>,
    ) -> Result<(), Fail> {
        let part = part_of(&self.dst, &it.dst);
        let mut w = self.dst.open_write(&part, None)?;
        let mut buf = vec![0u8; BUF];
        loop {
            cx.checkpoint()?;
            let n = r.read(&mut buf).map_err(io_fail)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n]).map_err(io_fail)?;
            cx.add_bytes(n as u64);
            *counted += n as u64;
        }
        w.finish()?;
        self.dst.rename(&part, &it.dst, it.how == How::Replace)?;
        Ok(())
    }

    /// Файл из потока выборки (S8 §3.5). Не вышло — обычным путём: там повтор, ожидание
    /// связи и политика ошибок.
    fn write_streamed(&self, it: &Item, r: &mut dyn Read, cx: &ExecCx<'_>) -> Outcome {
        let mut counted = 0;
        match self.write_data(it, r, &mut counted, cx) {
            Ok(()) => Outcome::Done(self.commit(it, cx)),
            Err(Fail::Cancelled) => Outcome::Done(ItemResult::Cancelled),
            Err(Fail::Io(e)) => {
                cx.progress()
                    .bytes_done
                    .fetch_sub(counted, Ordering::Relaxed);
                let _ = self.dst.remove(&part_of(&self.dst, &it.dst), false);
                tracing::info!(target: "ops", src = %End::shown(&it.src), error = %e, "extract stream file failed, reading it alone");
                Outcome::Fallback
            }
        }
    }

    /// Ссылка из архива: ссылкой или файлом с текстом цели (S8 §6).
    fn put_link(&self, it: &Item, l: &AsLink, cx: &ExecCx<'_>) -> ItemResult {
        let text = match l {
            AsLink::Make { target, dir } => {
                let os = crate::names::os_path(&it.dst.path);
                if it.how == How::Replace {
                    let _ = std::fs::remove_file(&os);
                }
                match make_link(target, &os, *dir) {
                    Ok(()) => {
                        Self::touch_parent(cx, &it.dst);
                        return ItemResult::Done;
                    }
                    Err(e) => {
                        tracing::info!(target: "ops", dst = %End::shown(&it.dst), error = %e, "symlink failed, writing target as text");
                        target.to_string_lossy().into_owned().into_bytes()
                    }
                }
            }
            AsLink::Text(t) => t.clone(),
        };
        let mut counted = 0;
        match self.write_data(it, &mut text.as_slice(), &mut counted, cx) {
            Ok(()) => self.commit(it, cx),
            Err(Fail::Cancelled) => ItemResult::Cancelled,
            Err(Fail::Io(e)) => {
                cx.progress()
                    .bytes_done
                    .fetch_sub(counted, Ordering::Relaxed);
                self.on_error(it, &e, cx)
            }
        }
    }

    /// `Zone.Identifier` файла архива — на извлечённый файл (как Проводник).
    fn mark_web(&self, dst: &Location) {
        let Some(Some(zone)) = self.zone.get() else {
            return;
        };
        if !self.dst.is_local() {
            return;
        }
        let mut os = crate::names::os_path(&dst.path).into_os_string();
        os.push(ZONE);
        if let Err(e) = std::fs::write(&os, zone) {
            tracing::debug!(target: "ops", dst = %dst.path.display(), error = %e, "zone identifier not written");
        }
    }
}

/// Поток Mark-of-the-Web.
const ZONE: &str = ":Zone.Identifier";

/// `Zone.Identifier` файла `file`; нет потока — `None`.
fn read_zone(file: &Path) -> Option<Vec<u8>> {
    let mut os = crate::names::os_path(file).into_os_string();
    os.push(ZONE);
    std::fs::read(os).ok()
}

/// Цель ссылки `target` из каталога `dir` лексически остаётся внутри `root`: без корня,
/// диска и `:` (S8 §6).
fn link_inside(root: &Path, dir: &Path, target: &Path) -> bool {
    use std::path::Component;
    let text = target.to_string_lossy().replace('/', "\\");
    let mut cur: Vec<Component<'_>> = dir.components().collect();
    for c in Path::new(&text).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if cur.pop().is_none() {
                    return false;
                }
            }
            Component::Normal(n) if !n.to_string_lossy().contains(':') => cur.push(c),
            _ => return false,
        }
    }
    cur.iter().collect::<PathBuf>().starts_with(root)
}

/// Символьная ссылка `at` → `target`.
fn make_link(target: &Path, at: &Path, dir: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        let t = PathBuf::from(target.to_string_lossy().replace('/', "\\"));
        if dir {
            std::os::windows::fs::symlink_dir(t, at)
        } else {
            std::os::windows::fs::symlink_file(t, at)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        std::os::unix::fs::symlink(target, at)
    }
}

impl OpExec for VfsTransfer {
    type Item = Item;
    type Dir = DirData;

    #[allow(clippy::too_many_lines, reason = "план: назначение, устройства, обход")]
    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        let fail = |at: &Location, e: &VfsError| {
            PlanError::Failed(format!("{}: {}", End::shown(at), err_text(e)).into())
        };
        // Без имён — весь каталог источника (корень архива для `arc.extract`).
        let whole = self.items.is_empty();
        let items: Vec<CompactString> = if whole {
            match self.src.list(&self.base, &p.cx().cancel_token()) {
                Ok(v) => v.into_iter().map(|e| e.name).collect(),
                Err(VfsError::Cancelled) => return Err(PlanError::Cancelled),
                Err(e) => return Err(fail(&self.base, &e)),
            }
        } else {
            self.items.clone()
        };
        let d = self
            .dst
            .stat(&self.dest)
            .map_err(|e| fail(&self.dest, &e))?;
        let mode = if whole || self.flat {
            if d.is_some_and(|m| !m.kind.is_dir_like()) {
                let shown = End::shown(&self.dest);
                return Err(PlanError::Failed(
                    tr("ops-dest-file", &[("path", &shown)]).into(),
                ));
            }
            DestMode::Into(self.dest.clone())
        } else {
            let req = OpRequest {
                kind: self.kind.clone(),
                base: self.base.clone(),
                items: items.clone(),
                dest: Some(self.dest.clone()),
                opts: self.opts.clone(),
            };
            resolve_dest(&req, &self.dest, &self.dest_raw, d.map(|m| m.kind))
                .map_err(|e| PlanError::Failed(e.to_string().into()))?
        };
        let root = match &mode {
            DestMode::Into(l) => l.clone(),
            DestMode::RenameTo(l) => l.parent().unwrap_or_else(|| l.clone()),
        };
        // Очередь устройств: сервер — полоса «Сеть» (F03 §4.1); архив — диск его файла.
        let lim = |e: &End, at: &Location| {
            if e.is_local() || End::in_archive(at) {
                self.jobs.ssd.max(1)
            } else {
                self.jobs.net.max(1)
            }
        };
        let mut slots: Vec<crate::engine::Slot> = Vec::with_capacity(2);
        for (e, at) in [(&self.src, &self.base), (&self.dst, &root)] {
            let k = e.device(at);
            match slots.iter_mut().find(|s| s.0 == k) {
                Some(s) => s.1 = s.1.min(lim(e, at)),
                None => slots.push((k, lim(e, at))),
            }
        }
        p.device_slot(&slots)?;
        if let DestMode::Into(l) = &mode
            && d.is_none()
        {
            self.dst.mkdir(l).map_err(|e| fail(&self.dest, &e))?;
        }
        let zone = if self.motw && self.dst.is_local() {
            crate::remote::archive_file(&self.base).and_then(|f| read_zone(&f.path))
        } else {
            None
        };
        let _ = self.zone.set(zone);
        let idx = self.index(&root, &p.cx());
        let mut w = Walk {
            root: root.clone(),
            buf: End::in_archive(&self.base).then(Buffered::default),
            flat: self.flat.then(|| Flat {
                idx: idx.clone(),
                planned: HashSet::new(),
            }),
        };
        for name in &items {
            let src = self.src.join(&self.base, name);
            let meta = match self.src.stat(&src) {
                Ok(Some(m)) => m,
                Ok(None) => {
                    let e = VfsError::NotFound(src.path.clone());
                    p.record(None, ItemResult::Failed(self.failed(&src, None, &e)));
                    continue;
                }
                Err(e) => {
                    p.record(None, ItemResult::Failed(self.failed(&src, None, &e)));
                    continue;
                }
            };
            let dst = match &mode {
                DestMode::Into(l) => self.dst.join(l, name),
                DestMode::RenameTo(l) => l.clone(),
            };
            if self.bad_name(&name_of(&dst)) {
                p.record(None, self.bad_name_failed(&src));
                continue;
            }
            if self.src.same_fs(&src, &self.dst, &dst) && End::shown(&src) == End::shown(&dst) {
                let e = VfsError::Unsupported("same file");
                p.record(None, ItemResult::Failed(self.failed(&src, Some(&dst), &e)));
                continue;
            }
            let ex = idx.get(&self.dst.key(&name_of(&dst))).copied();
            w.note_part(&idx, &self.dst, &dst);
            self.put(p, &mut w, None, src, meta, dst, ex)?;
        }
        if let Some(b) = w.buf.take() {
            self.dispatch(p, b)?;
        }
        Ok(())
    }

    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult {
        self.run(&work.item, cx)
    }

    fn finalize_dir(&self, dir: &DirNode<DirData>, cx: &ExecCx<'_>) -> ItemResult {
        let d = &dir.data;
        self.preserve(&d.dst, d.mode, d.mtime);
        if self.kind == OpKind::Move && dir.clean() {
            if let Err(e) = self.src.remove(&d.src, true) {
                return ItemResult::Failed(self.failed(&d.src, None, &e));
            }
            Self::touch_parent(cx, &d.src);
        }
        ItemResult::Done
    }

    fn resolve(&self, mut it: Item, issue: &Issue, r: &Resolution) -> Resolved<Item> {
        if let IssueKind::Io(c) = issue.kind {
            if *r == Resolution::Retry {
                return Resolved::Run(it);
            }
            let e = VfsError::Io {
                path: PathBuf::new(),
                source: io::Error::other(issue.error.as_deref().unwrap_or_default().to_owned()),
            };
            let mut f = self.failed(&it.src, Some(&it.dst), &e);
            f.class = c;
            return Resolved::Done(ItemResult::Failed(f));
        }
        if issue.kind != IssueKind::Exists {
            return Resolved::Done(ItemResult::Skipped);
        }
        let newer = match (&issue.src, &issue.dst) {
            (Some(s), Some(d)) => i128::from(s.mtime) > i128::from(d.mtime) + i128::from(self.tol),
            _ => false,
        };
        match r {
            Resolution::Overwrite => it.how = How::Replace,
            Resolution::OverwriteIfNewer if newer => it.how = How::Replace,
            _ => return Resolved::Done(ItemResult::Skipped),
        }
        Resolved::Run(it)
    }

    fn finished(
        &self,
        r: &crate::api::Report,
        outcome: &Result<JobSummary, String>,
    ) -> Option<Note> {
        let s = match outcome {
            Err(e) if e == "cancelled" => return None,
            Err(e) => return Some((Level::Error, tr("ops-copy-job-failed", &[("error", e)]))),
            Ok(s) => s,
        };
        if s.errors > 0 {
            let first = r
                .failed
                .first()
                .map(|f| format!("{}: {}", f.path.display(), f.error))
                .unwrap_or_default();
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
    use super::*;

    #[test]
    fn part_is_sibling() {
        let dst = Location::local(std::env::temp_dir().join("a.bin"));
        let p = part_of(&End::Local, &dst);
        assert_eq!(p.path, std::env::temp_dir().join("a.bin.urfin-part"));
    }

    #[test]
    fn net_errors_are_classified() {
        let e = VfsError::Io {
            path: PathBuf::new(),
            source: io::ErrorKind::NotConnected.into(),
        };
        assert_eq!(class_of(&e), IoClass::Network);
        assert!(maybe_net(&e));
        assert!(!maybe_net(&VfsError::Denied(PathBuf::new())));
    }

    /// Ссылка из архива — ссылкой, только если цель лексически внутри назначения (S8 §6).
    #[test]
    fn link_target_must_stay_inside() {
        let root = std::env::temp_dir().join("out");
        let dir = root.join("a");
        assert!(link_inside(&root, &dir, Path::new("b.txt")));
        assert!(link_inside(&root, &dir, Path::new("./c/../b.txt")));
        assert!(link_inside(&root, &dir, Path::new("../b.txt")));
        assert!(!link_inside(&root, &dir, Path::new("../../x")));
        assert!(!link_inside(&root, &dir, Path::new("/etc/passwd")));
        assert!(!link_inside(&root, &dir, &std::env::temp_dir()));
        assert!(!link_inside(&root, &dir, Path::new("x:y")));
    }
}
