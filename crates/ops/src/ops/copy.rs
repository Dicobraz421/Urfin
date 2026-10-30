//! Копирование (S5-06) и простой перенос (S5-08 MVP) на движке job (S5-04).
//!
//! План (поток координатора): назначение по Far ([`resolve_dest`]) — с
//! обращением к ФС здесь, не в UI-потоке; том и ФС назначения; обход
//! источников [`Walker`] — каталоги назначения создаются до своих детей
//! (`DirPolicy::Merge` — в существующий), существующий каталог назначения
//! листингуется один раз (индекс имён), конфликт решается политикой до
//! копирования данных.
//!
//! Воркер: файл — `CopyFileExW` (данные, потоки ADS, атрибуты, время изменения —
//! как Проводник, S0 №13), прогресс по чанкам, пауза и отмена внутри файла.
//! Commit (S5 §6.1): новый файл меньше `small_file` — сразу под своим именем
//! (время изменения система ставит после данных), крупный и любая перезапись —
//! во временный `.<имя>.fnv~<метка>` рядом и атомарным rename. Каталог —
//! время и атрибуты в Finalizer, после всех детей.
//!
//! Перенос: тот же том — rename каждого элемента (каталог целиком; в
//! существующий — по детям, пустой источник удаляется в Finalizer); другой
//! том — копия и удаление источника файла сразу после его commit, когда копия
//! сброшена на диск и её размер равен источнику (S0 №12, S5-08): прерванный
//! перенос оставляет каждый файл целым хотя бы в одном месте.
//!
//! Ошибки (S5-11): занятый, недоступный носитель и сеть повторяются сами
//! (0,2 / 1 / 4 с), затем — по `ErrorPolicy`: вопрос (Повторить, Пропустить,
//! «для всех» — по классу ошибки), пропуск в отчёт или остановка job. Упавшие
//! пары «источник → назначение» — в отчёте; `ops.retry` запускает job только из
//! них ([`start_retry`]).
//!
//! `ConflictPolicy::Ask` (по умолчанию): файл с занятым именем паркуется, вопрос
//! задаётся без модального окна, остальное копируется дальше (S5-07, движок —
//! `engine::park`). Имя заняли после плана — данные уже во временном файле,
//! «Заменить» — rename без повторной копии. `DirPolicy::Ask` — слияние, как в
//! Far: вопрос задаётся по файлам внутри.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use compact_str::CompactString;
use smallvec::SmallVec;
use urfin_core::{
    Attrs, Cancelled, Cx, Entry, EntryKind, JobId, JobSpec, JobSummary, Lane, Level, Location, Mask,
};
use urfin_vfs::{ListOpts, LocalFs, Vfs};

use crate::api::{
    ConflictPolicy, DestMode, DirPolicy, ErrorPolicy, FailedItem, IoClass, Issue, IssueId,
    IssueKind, ItemMeta, ItemOp, LinkMode, OpKind, OpOptions, OpRequest, Preserve, RenameScheme,
    Report, Resolution,
};
use crate::dest::{DestError, resolve_dest};
use crate::engine::walker::{Enter, Visit, Walker, loc};
use crate::engine::{
    DirNode, ExecCx, ItemResult, JobEngine, Note, OpExec, Plan, PlanError, Resolved, Retry, Work,
};
use crate::journal::{self, Journal, Record};
use crate::sys::fs as sys;

/// Потоков воркеров на job: столько нужно `Nvme` (S5 §3.3); работают из них
/// столько, сколько даёт носитель ([`crate::strategy::workers_for`]), остальные
/// ждут в [`Gate`].
const WORKERS: u16 = 16;
/// Выход с прерыванием задач (вопрос при выходе, S5 §6.4): прерванные job
/// оставляют журнал для «Продолжить».
pub(crate) static QUITTING: AtomicBool = AtomicBool::new(false);
/// Раз в столько тиков (500 мс) — счётчики в журнал (5 с).
const COUNTERS_EVERY: u64 = 10;
/// Паузы автоповтора транзиентных ошибок (S5 §2.2, S5-11).
const RETRY_WAITS: [Duration; 3] = [
    Duration::from_millis(200),
    Duration::from_secs(1),
    Duration::from_secs(4),
];
/// Точность времени FAT/exFAT: сравнение «новее/отличается» с допуском.
const FAT_TOLERANCE_NS: i64 = 2_000_000_000;

/// Копия или перенос.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Copy,
    Move,
}

/// Что известно о существующем элементе назначения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dst {
    kind: EntryKind,
    size: u64,
    /// нс от эпохи Unix; `i64::MIN` — неизвестно.
    mtime: i64,
}

/// Как положить элемент.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    /// Назначения нет (эксклюзивное создание).
    Create,
    /// Заменить существующее (через временный файл, атомарно).
    Replace,
    /// Существующее — в `имя.bak~N`, затем создать.
    Backup,
}

/// Решение по конфликту.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Put(How),
    Skip,
    /// Нужен вопрос: работа паркуется (S5-07).
    Ask,
    /// Файл ↔ каталог.
    Mismatch,
    /// Положить под свободным именем.
    NewName,
}

/// Работа воркера: один файл, ссылка или (перенос в пределах тома) каталог целиком.
#[derive(Debug)]
pub(crate) struct Item {
    src: PathBuf,
    dst: PathBuf,
    kind: EntryKind,
    size: u64,
    how: How,
    /// Перенос в пределах тома: rename вместо копии.
    rename: bool,
    /// Данные уже во временном файле (имя заняли после плана): только commit.
    temp: Option<PathBuf>,
}

/// Каталог для Finalizer.
#[derive(Debug)]
pub(crate) struct DirData {
    /// `None` — корень назначения.
    src: Option<PathBuf>,
    dst: PathBuf,
    mtime: i64,
    /// `FILE_ATTRIBUTE_*` источника.
    attrs: u32,
    /// Создан этой job: ставим время и атрибуты источника.
    created: bool,
    /// Перенос: удалить пустой источник.
    remove_src: bool,
    touch: SmallVec<[Location; 2]>,
}

/// Решения плана, нужные воркерам.
#[derive(Debug, Default)]
struct Info {
    tol_ns: i64,
    /// Перенос rename-ом (тот же том).
    rename: bool,
    /// Наибольший файл на ФС назначения (FAT32 — 4 ГБ − 1), `None` — без предела.
    max_file: Option<u64>,
    /// Назначение — FAT/exFAT: имена с хвостовой точкой и зарезервированные туда
    /// не положить (на NTFS verbatim-путь их создаёт — копируем как есть).
    fat: bool,
    /// Корень назначения: свободное место (S5-20).
    root: PathBuf,
    /// Назначение — сеть или USB: крупные файлы — с докачкой (S5-27).
    resumable_dest: bool,
    /// Тот же том `ReFS` (Dev Drive): клон блоков вместо копии (S5-15); размер кластера.
    clone_cluster: Option<u32>,
}

/// Докачка — для файлов от гигабайта (S5-27).
const RESUME_MIN: u64 = 1 << 30;

/// Наибольший файл FAT32.
const FAT32_MAX_FILE: u64 = 0xFFFF_FFFF;
/// Запас места сверх оставшегося объёма: max(1 % тома, 256 МБ) (S5 §4.12).
const SPACE_FLOOR: u64 = 256 << 20;

/// Ограничение одновременных копий (HDD — 1, USB — 2; 0 — без ограничения).
#[derive(Debug, Default)]
struct Gate {
    max: AtomicUsize,
    used: Mutex<usize>,
    cv: Condvar,
}

struct GateGuard<'a>(Option<&'a Gate>);

impl Gate {
    fn enter(&self, cx: &ExecCx<'_>) -> Result<GateGuard<'_>, Cancelled> {
        let max = self.max.load(Ordering::Relaxed);
        if max == 0 {
            return Ok(GateGuard(None));
        }
        loop {
            {
                let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
                if *used < max {
                    *used += 1;
                    return Ok(GateGuard(Some(self)));
                }
                let _ = self
                    .cv
                    .wait_timeout(used, Duration::from_millis(10))
                    .unwrap_or_else(PoisonError::into_inner);
            }
            cx.checkpoint()?;
        }
    }
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        if let Some(g) = self.0 {
            *g.used.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
            g.cv.notify_one();
        }
    }
}

/// Операция копирования или переноса.
#[derive(Debug)]
pub(crate) struct Transfer {
    mode: Mode,
    base: Location,
    items: Vec<CompactString>,
    dest: Location,
    /// Текст назначения (завершающий разделитель — «создать каталог»).
    dest_raw: String,
    opts: OpOptions,
    mask: Option<Mask>,
    small: u64,
    /// Повтор (`ops.retry`): пары «источник → назначение» вместо `items`.
    pairs: Vec<(PathBuf, PathBuf)>,
    /// Метка временных файлов job.
    tag: String,
    /// AIMD работающих воркеров и прошлый замер (байты, файлы, время).
    aimd: Mutex<Option<(crate::engine::aimd::Aimd, u64, u64, std::time::Instant)>>,
    /// Каталог журналов (`None` — без журнала: повтор, тесты).
    journal_dir: Option<PathBuf>,
    /// Корни копии «источник → назначение» (план): цели junction внутри дерева
    /// переписываются на копию (S5-22).
    roots: Mutex<Vec<(PathBuf, PathBuf)>>,
    strategy: crate::strategy::StrategyTable,
    journal: OnceLock<Journal>,
    ticks: AtomicU64,
    info: OnceLock<Info>,
    gate: Gate,
    /// Тест: перед commit временного файла (гонка за имя).
    #[cfg(test)]
    before_commit: Option<fn(&Path)>,
    /// Тест: свободное место и размер тома вместо ОС.
    #[cfg(test)]
    free_space: Option<fn() -> (u64, u64)>,
    /// Тест: носитель обоих томов вместо ОС.
    #[cfg(test)]
    media: Option<urfin_vfs::volume::Media>,
    /// Тест: сразу после commit (порча копии для проверки).
    #[cfg(test)]
    after_commit: Option<fn(&Path)>,
}

/// Параметры запуска.
#[derive(Debug)]
pub(crate) struct Start {
    pub req: OpRequest,
    pub dest: Location,
    pub dest_raw: String,
    pub mask: Option<Mask>,
    pub small_file: u64,
    pub journal_dir: Option<PathBuf>,
    /// Движки по размеру файла (`[ops.strategy]`, S5-14).
    pub strategy: crate::strategy::StrategyTable,
    /// «Продолжить» (S5-27): метка прерванной job — её временные файлы с докачкой
    /// дописываются, а не копируются заново.
    pub resume_tag: Option<String>,
}

/// «Продолжить» прерванную job (S5 §6.3): тот же запрос; файлы, равные
/// источнику (размер и время), пропускаются, отличающиеся — наши недописанные —
/// копируются заново без вопросов.
pub(crate) fn recovered(
    h: &journal::Header,
    resumable: &[PathBuf],
    small_file: u64,
    journal_dir: Option<PathBuf>,
) -> Option<Start> {
    let mut req = h.req.clone();
    req.opts.conflict = ConflictPolicy::OverwriteIfDiffers;
    let mask = match req.opts.filter.as_deref() {
        None | Some("") => None,
        Some(m) => Some(Mask::parse(m).ok()?),
    };
    Some(Start {
        req,
        dest: Location::local(&h.dest),
        dest_raw: h.dest_raw.clone(),
        mask,
        small_file,
        journal_dir,
        strategy: crate::strategy::StrategyTable::default(),
        resume_tag: resumable.iter().find_map(|p| tag_of(p)),
    })
}

/// Метка job по имени временного файла (`.имя.fnv~<метка>`, [`temp_path`]).
fn tag_of(p: &Path) -> Option<String> {
    let name = p.file_name()?.to_str()?;
    let (_, tag) = name.rsplit_once(".fnv~")?;
    (!tag.is_empty()).then(|| tag.to_owned())
}

/// Как повторить упавшее: режим и опции исходной job (хранится с отчётом).
#[derive(Debug)]
pub(crate) struct RetrySpec {
    mode: Mode,
    opts: OpOptions,
    mask: Option<Mask>,
    small: u64,
}

/// `ops.retry` (UI-поток): job только из упавших пар отчёта. `None` — повторять
/// нечего.
pub(crate) fn start_retry(
    eng: &JobEngine,
    cx: &mut Cx<'_>,
    report: &Report,
    retry: &Retry,
) -> Option<JobId> {
    let spec = retry.downcast_ref::<RetrySpec>()?;
    let pairs: Vec<(PathBuf, PathBuf)> = report
        .failed
        .iter()
        .filter_map(|f| f.pair.clone())
        .collect();
    let (first_src, first_dst) = pairs.first()?.clone();
    let names: Vec<CompactString> = pairs
        .iter()
        .map(|(s, _)| {
            s.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref()
                .into()
        })
        .collect();
    let spec_job = JobSpec {
        kind: if spec.mode == Mode::Move {
            "move"
        } else {
            "copy"
        },
        title: job_title(&names, &shown(first_dst.parent().unwrap_or(&first_dst))),
        lane: Lane::Bulk,
        priority: prio(spec.opts.priority),
        owner_ns: "ops",
    };
    let dir = |p: &Path| Location::local(shown(p.parent().unwrap_or(p)));
    let t = Transfer {
        mode: spec.mode,
        base: dir(&first_src),
        items: Vec::new(),
        dest: dir(&first_dst),
        dest_raw: String::new(),
        opts: spec.opts.clone(),
        mask: spec.mask.clone(),
        small: spec.small,
        pairs,
        tag: new_tag(),
        aimd: Mutex::default(),
        journal_dir: None,
        roots: Mutex::default(),
        strategy: crate::strategy::StrategyTable::default(),
        journal: OnceLock::new(),
        ticks: AtomicU64::new(0),
        info: OnceLock::new(),
        gate: Gate::default(),
        #[cfg(test)]
        before_commit: None,
        #[cfg(test)]
        free_space: None,
        #[cfg(test)]
        media: None,
        #[cfg(test)]
        after_commit: None,
    };
    let workers = spec.opts.workers.map_or(WORKERS, |n| u16::from(n.get()));
    Some(eng.start(cx, spec_job, workers, t))
}

/// Из handler (UI-поток, без I/O): job копирования или переноса.
pub(crate) fn start(eng: &JobEngine, cx: &mut Cx<'_>, s: Start) -> JobId {
    let mode = if s.req.kind == OpKind::Move {
        Mode::Move
    } else {
        Mode::Copy
    };
    let dest = s.req.dest.as_ref().map_or_else(String::new, |d| {
        if d.is_local() {
            d.path.display().to_string()
        } else {
            d.to_uri()
        }
    });
    let title = job_title(&s.req.items, &dest);
    let spec = JobSpec {
        kind: if mode == Mode::Move { "move" } else { "copy" },
        title,
        lane: Lane::Bulk,
        priority: prio(s.req.opts.priority),
        owner_ns: "ops",
    };
    let workers = s.req.opts.workers.map_or(WORKERS, |n| u16::from(n.get()));
    let t = Transfer::new(mode, s);
    eng.start(cx, spec, workers, t)
}

/// Приоритет job в очереди устройства: больше — раньше.
fn prio(p: crate::api::Priority) -> u8 {
    match p {
        crate::api::Priority::Low => 0,
        crate::api::Priority::Normal => 1,
        crate::api::Priority::High => 2,
    }
}

/// Название задачи для job-бара (D-06): «что → куда»; вид («Копирование») пишет UI.
fn job_title(items: &[CompactString], dest: &str) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("n", items.len());
    a.set(
        "name",
        items.first().map_or_else(String::new, ToString::to_string),
    );
    a.set("dest", dest.to_owned());
    urfin_i18n::current().fmt("ops-job-transfer", &a)
}

fn tr(key: &str, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

fn tr_n(key: &str, n: u64, args: &[(&str, &str)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("n", n);
    for (k, v) in args {
        a.set(*k, (*v).to_owned());
    }
    urfin_i18n::current().fmt(key, &a)
}

/// Метка временных файлов: процесс и номер job в процессе (база 36).
fn new_tag() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{}{}", base36(u64::from(std::process::id())), base36(n))
}

fn base36(mut n: u64) -> String {
    const D: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(D[usize::try_from(n % 36).unwrap_or(0)]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

impl Transfer {
    fn new(mode: Mode, s: Start) -> Transfer {
        Transfer {
            mode,
            base: s.req.base,
            items: s.req.items,
            dest: s.dest,
            dest_raw: s.dest_raw,
            opts: s.req.opts,
            mask: s.mask,
            small: s.small_file,
            pairs: Vec::new(),
            tag: s.resume_tag.unwrap_or_else(new_tag),
            aimd: Mutex::default(),
            journal_dir: s.journal_dir,
            roots: Mutex::default(),
            strategy: s.strategy,
            journal: OnceLock::new(),
            ticks: AtomicU64::new(0),
            info: OnceLock::new(),
            gate: Gate::default(),
            #[cfg(test)]
            before_commit: None,
            #[cfg(test)]
            free_space: None,
            #[cfg(test)]
            media: None,
            #[cfg(test)]
            after_commit: None,
        }
    }

    fn info(&self) -> &Info {
        self.info.get_or_init(Info::default)
    }

    /// Копировать с докачкой (S5-27): файл не меньше [`RESUME_MIN`], назначение — сеть
    /// или USB.
    fn resumable(&self, it: &Item) -> bool {
        it.size >= RESUME_MIN && self.info().resumable_dest
    }

    fn op(&self) -> ItemOp {
        match self.mode {
            Mode::Copy => ItemOp::Copy,
            Mode::Move => ItemOp::Move,
        }
    }

    fn fail(&self, path: &Path, e: &io::Error) -> ItemResult {
        ItemResult::Failed(self.failed(path, &io_text(e), class(e)))
    }

    fn failed(&self, path: &Path, error: &str, class: IoClass) -> FailedItem {
        FailedItem {
            path: PathBuf::from(shown(path)),
            op: self.op(),
            error: error.into(),
            class,
            pair: None,
        }
    }

    /// Ошибка работы, которую можно повторить (`ops.retry`).
    fn failed_item(&self, it: &Item, error: &str, class: IoClass) -> FailedItem {
        FailedItem {
            pair: Some((it.src.clone(), it.dst.clone())),
            ..self.failed(&it.src, error, class)
        }
    }

    /// Ошибка после автоповторов — по `ErrorPolicy` (S5-11).
    fn on_error(&self, it: &Item, e: &io::Error, cx: &ExecCx<'_>) -> ItemResult {
        let (text, c) = (io_text(e), class(e));
        match self.opts.errors {
            ErrorPolicy::Ask => {
                let kind = if low_space(e) {
                    IssueKind::LowSpace
                } else {
                    IssueKind::Io(c)
                };
                let name = it.src.file_name().unwrap_or_default().to_string_lossy();
                let mut issue = self.issue(&name, it.size, mtime_of(&it.src), &it.dst, None, None);
                issue.kind = kind;
                issue.dst = None;
                issue.error = Some(text.into());
                issue.path = Some(it.src.clone());
                issue.code = e.raw_os_error();
                let bytes = if it.rename { 0 } else { it.size };
                ItemResult::Park(Box::new(issue), bytes)
            }
            ErrorPolicy::Skip => ItemResult::Failed(self.failed_item(it, &text, c)),
            ErrorPolicy::Abort => {
                cx.abort(&format!("{}: {text}", shown(&it.src)));
                ItemResult::Failed(self.failed_item(it, &text, c))
            }
        }
    }

    /// Решение по существующему `dst`.
    #[allow(clippy::match_same_arms, reason = "строка на политику")]
    fn decide(&self, src_size: u64, src_mtime: i64, src_dir: bool, dst: Option<Dst>) -> Decision {
        let Some(d) = dst else {
            return Decision::Put(How::Create);
        };
        if d.kind.is_dir_like() != src_dir {
            return Decision::Mismatch;
        }
        let tol = i128::from(self.info().tol_ns);
        let (sm, dm) = (i128::from(src_mtime), i128::from(d.mtime));
        let same = src_size == d.size && (sm - dm).abs() <= tol;
        match self.opts.conflict {
            ConflictPolicy::Ask => Decision::Ask,
            ConflictPolicy::Overwrite => Decision::Put(How::Replace),
            ConflictPolicy::Skip => Decision::Skip,
            ConflictPolicy::OverwriteIfNewer if sm > dm + tol => Decision::Put(How::Replace),
            ConflictPolicy::OverwriteIfNewer => Decision::Skip,
            ConflictPolicy::OverwriteIfDiffers if same => Decision::Skip,
            ConflictPolicy::OverwriteIfDiffers => Decision::Put(How::Replace),
            ConflictPolicy::SkipIdentical if same => Decision::Skip,
            ConflictPolicy::SkipIdentical => Decision::Ask,
            ConflictPolicy::RenameNew(_) => Decision::NewName,
            ConflictPolicy::RenameExisting(_) => Decision::Put(How::Backup),
        }
    }

    fn scheme(&self) -> RenameScheme {
        match self.opts.conflict {
            ConflictPolicy::RenameNew(s) | ConflictPolicy::RenameExisting(s) => s,
            _ => RenameScheme::Counter,
        }
    }

    // --- воркер ---

    /// Одна работа: положить, на гонке (имя заняли после плана) — решить заново.
    fn run(&self, it: &Item, cx: &ExecCx<'_>) -> ItemResult {
        let Ok(_gate) = self.gate.enter(cx) else {
            return ItemResult::Cancelled;
        };
        let mut how = it.how;
        let mut dst = it.dst.clone();
        let mut rename = it.rename;
        let mut temp = it.temp.clone();
        if !it.rename
            && temp.is_none()
            && it.kind == EntryKind::File
            && let Some(issue) = self.space_issue(it, cx)
        {
            // Места не хватит на остаток job: новые файлы ждут ответа (S5 §4.12).
            return ItemResult::Park(Box::new(issue), it.size);
        }
        let mut waits = RETRY_WAITS.iter();
        // Конфликты (до 4 попыток) и автоповторы (до 3).
        for _ in 0..8 {
            let r = if rename {
                rename_one(&it.src, &dst, how, self.scheme())
            } else {
                self.copy_one(it, &dst, how, &mut temp, cx)
            };
            match r {
                Ok(()) => return self.committed(it, &dst, rename, cx),
                Err(e) if e.raw_os_error() == Some(sys::ERR_ABORTED) => {
                    return ItemResult::Cancelled;
                }
                Err(e)
                    if rename
                        && e.raw_os_error() == Some(sys::ERR_NOT_SAME_DEVICE)
                        && !it.kind.is_dir_like() =>
                {
                    // Точка монтирования внутри дерева: файл — копией.
                    rename = false;
                }
                Err(e)
                    if e.kind() == io::ErrorKind::AlreadyExists
                        && how == How::Create
                        && it.kind != EntryKind::Dir =>
                {
                    let d = stat_dst(&dst);
                    match self.decide(it.size, mtime_of(&it.src), false, d) {
                        Decision::Put(h) => how = h,
                        Decision::Skip => {
                            drop_temp(&mut temp, it.kind);
                            return ItemResult::Skipped;
                        }
                        Decision::Ask => {
                            // Данные во временном файле остаются: «Заменить» — rename.
                            let bytes = if temp.is_some() || rename { 0 } else { it.size };
                            let name = it.src.file_name().unwrap_or_default().to_string_lossy();
                            let issue =
                                self.issue(&name, it.size, mtime_of(&it.src), &dst, d, temp.take());
                            return ItemResult::Park(Box::new(issue), bytes);
                        }
                        Decision::Mismatch => {
                            drop_temp(&mut temp, it.kind);
                            let msg = tr("ops-copy-type", &[]);
                            return ItemResult::Failed(self.failed(&dst, &msg, IoClass::Other));
                        }
                        Decision::NewName => {
                            let Some(free) = free_name(&dst, self.scheme(), false, false, None)
                            else {
                                drop_temp(&mut temp, it.kind);
                                return self.fail(&dst, &e);
                            };
                            dst = free;
                        }
                    }
                }
                Err(e) => {
                    drop_temp(&mut temp, it.kind);
                    if class(&e).transient()
                        && let Some(d) = waits.next()
                    {
                        tracing::debug!(src = %it.src.display(), error = %e, wait = ?d, "retry");
                        if pause(*d, cx).is_err() {
                            return ItemResult::Cancelled;
                        }
                        continue;
                    }
                    return self.on_error(it, &e, cx);
                }
            }
        }
        drop_temp(&mut temp, it.kind);
        let e = io::Error::from(io::ErrorKind::AlreadyExists);
        self.fail(&dst, &e)
    }

    /// Копия файла или ссылки по правилу commit §6.1. `temp` — данные уже во
    /// временном файле (тогда только commit); занятое имя при commit оставляет
    /// временный файл в `temp` для вопроса.
    fn copy_one(
        &self,
        it: &Item,
        dst: &Path,
        how: How,
        temp: &mut Option<PathBuf>,
        cx: &ExecCx<'_>,
    ) -> io::Result<()> {
        if how == How::Backup {
            let Some(bak) = free_name(dst, self.scheme(), true, false, None) else {
                return Err(io::ErrorKind::AlreadyExists.into());
            };
            sys::move_file(dst, &bak, false)?;
        }
        let staged = if let Some(t) = temp {
            t.clone()
        } else {
            let link = is_link(it.kind);
            let direct = how != How::Replace && (link || it.size < self.small);
            if direct {
                return self.data(it, dst, true, cx);
            }
            let fresh = temp_path(dst, &self.tag);
            // Докачка (S5-27): крупный файл на сеть или USB; недописанный временный
            // файл прерванной job с той же меткой — дописывается.
            let resume = self.resumable(it);
            if !resume {
                match std::fs::remove_file(&fresh) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => {
                        // Осталась от прерванной job с той же меткой и не удаляется.
                        return Err(e);
                    }
                    _ => {}
                }
            }
            if let Some(j) = self.journal.get() {
                j.append_now(&Record::Temp(fresh.clone()));
                if resume {
                    j.append_now(&Record::Resumable(fresh.clone()));
                }
            }
            self.data(it, &fresh, false, cx)?;
            *temp = Some(fresh.clone());
            fresh
        };
        #[cfg(test)]
        if let Some(f) = self.before_commit {
            f(dst);
        }
        match sys::move_file(&staged, dst, how == How::Replace) {
            Ok(()) => {
                if let Some(j) = self.journal.get() {
                    j.append(&Record::TempDone(staged));
                }
                *temp = None;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            Err(e) => {
                drop_temp(temp, it.kind);
                Err(e)
            }
        }
    }

    /// Файл на месте: проверка копии (S5-21), при переносе между томами — удаление
    /// источника.
    fn committed(&self, it: &Item, dst: &Path, rename: bool, cx: &ExecCx<'_>) -> ItemResult {
        #[cfg(test)]
        if let Some(f) = self.after_commit {
            f(dst);
        }
        if !rename
            && !is_link(it.kind)
            && matches!(self.opts.verify, crate::api::Verify::Hash(_))
            && let Err(e) = verify(&it.src, dst)
        {
            // Копия не та: убрать её; источник не трогаем (и при переносе).
            let _ = remove_src(dst, it.kind);
            return self.on_error(it, &e, cx);
        }
        if self.mode == Mode::Move && !rename {
            return self.finish_move(it, dst);
        }
        ItemResult::Done
    }

    /// Межтомный перенос файла после commit копии: копия на диске (fsync) и того
    /// же размера, что источник сейчас, — тогда источник удаляется (S0 №12).
    fn finish_move(&self, it: &Item, dst: &Path) -> ItemResult {
        if !is_link(it.kind) {
            let want = std::fs::symlink_metadata(&it.src).map_or(it.size, |m| m.len());
            match durable(dst) {
                Ok(got) if got == want => {}
                Ok(got) => {
                    let (got, want) = (got.to_string(), want.to_string());
                    let msg = tr("ops-move-size", &[("got", &got), ("want", &want)]);
                    return ItemResult::Failed(self.failed(dst, &msg, IoClass::Other));
                }
                Err(e) => {
                    let msg = tr("ops-move-flush", &[("error", &io_text(&e))]);
                    return ItemResult::Failed(self.failed(dst, &msg, class(&e)));
                }
            }
        }
        if let Err(e) = remove_src(&it.src, it.kind) {
            let msg = tr("ops-move-src-left", &[("error", &io_text(&e))]);
            return ItemResult::Failed(self.failed(&it.src, &msg, class(&e)));
        }
        ItemResult::Done
    }

    /// Места меньше, чем нужно на остаток job с запасом, — `LowSpace` до записи.
    fn space_issue(&self, it: &Item, cx: &ExecCx<'_>) -> Option<Issue> {
        let root = &self.info().root;
        if root.as_os_str().is_empty() {
            return None;
        }
        #[cfg(test)]
        let space = self
            .free_space
            .map_or_else(|| sys::free_space(root).ok(), |f| Some(f()));
        #[cfg(not(test))]
        let space = sys::free_space(root).ok();
        let (free, total) = space?;
        let p = cx.progress();
        let left = p
            .bytes_total
            .load(Ordering::Relaxed)
            .saturating_sub(p.bytes_done.load(Ordering::Relaxed))
            .max(it.size);
        let need = left.saturating_add((total / 100).max(SPACE_FLOOR));
        if free >= need {
            return None;
        }
        let name = it.src.file_name().unwrap_or_default().to_string_lossy();
        let mut issue = self.issue(&name, it.size, mtime_of(&it.src), &it.dst, None, None);
        issue.kind = IssueKind::LowSpace;
        issue.dst = None;
        let (need, free) = (urfin_i18n::fmt::bytes(need), urfin_i18n::fmt::bytes(free));
        issue.error = Some(tr("ops-low-space-detail", &[("need", &need), ("free", &free)]).into());
        Some(issue)
    }

    /// Цель junction `link` внутри одного из копируемых каталогов → то же место в
    /// копии; `None` — не junction или цель снаружи (копируется как есть).
    fn junction_inside(&self, link: &Path) -> Option<PathBuf> {
        let target = sys::junction_target(link)?;
        let roots = self.roots.lock().unwrap_or_else(PoisonError::into_inner);
        roots.iter().find_map(|(src, dst)| {
            let src = PathBuf::from(shown(src));
            let rest = strip_prefix_ci(&target, &src)?;
            Some(PathBuf::from(shown(dst)).join(rest))
        })
    }

    /// Замер AIMD (S5-19): пропускная способность с прошлого тика → число
    /// работающих воркеров в [`Gate`].
    fn aimd_step(&self, cx: &ExecCx<'_>) {
        let mut g = self.aimd.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((a, bytes0, items0, t0)) = g.as_mut() else {
            return;
        };
        let p = cx.progress();
        let (bytes, items) = (
            p.bytes_done.load(Ordering::Relaxed),
            p.items_done.load(Ordering::Relaxed),
        );
        let dt = t0.elapsed().as_secs_f64();
        if dt <= 0.0 {
            return;
        }
        let work = bytes.saturating_sub(*bytes0)
            + items.saturating_sub(*items0) * crate::engine::aimd::ITEM_WEIGHT;
        #[allow(clippy::cast_precision_loss, reason = "скорость")]
        let rate = work as f64 / dt;
        (*bytes0, *items0, *t0) = (bytes, items, std::time::Instant::now());
        let n = a.step(rate);
        if self.gate.max.swap(n, Ordering::Relaxed) != n {
            tracing::debug!(workers = n, rate, "aimd");
            self.gate.cv.notify_all();
        }
    }

    /// Проблема «имя занято» для вопроса.
    fn issue(
        &self,
        name: &str,
        size: u64,
        mtime: i64,
        dst: &Path,
        d: Option<Dst>,
        temp: Option<PathBuf>,
    ) -> Issue {
        let d = d.unwrap_or(Dst {
            kind: EntryKind::File,
            size: 0,
            mtime: i64::MIN,
        });
        let dst_name = dst.file_name().unwrap_or_default().to_string_lossy();
        Issue {
            id: IssueId(0),
            kind: IssueKind::Exists,
            src: Some(ItemMeta {
                name: name.into(),
                size,
                mtime,
                attrs: 0,
            }),
            dst: Some(ItemMeta {
                name: dst_name.as_ref().into(),
                size: d.size,
                mtime: d.mtime,
                attrs: 0,
            }),
            op: self.op(),
            temp,
            error: None,
            path: None,
            code: None,
        }
    }

    /// Данные файла (или сама ссылка) в `to`.
    fn data(&self, it: &Item, to: &Path, excl: bool, cx: &ExecCx<'_>) -> io::Result<()> {
        if is_link(it.kind) {
            let dir = it.kind == EntryKind::LinkDir
                || sys::attrs(&it.src).is_ok_and(|a| a & ATTR_DIRECTORY != 0);
            // Junction внутрь копируемого дерева — на то же место в копии (S5-22).
            if dir && let Some(t) = self.junction_inside(&it.src) {
                return sys::make_junction(to, &t);
            }
            return sys::copy_link(&it.src, to, dir);
        }
        if it.size >= self.small {
            cx.progress().set_current(&shown(&it.src));
        } else if excl && self.strategy.small == crate::api::Engine::Small {
            // Свой путь мелкого файла (S5-14); не годится (ссылка, EA, облако) — системный.
            let mut buf = SMALL_BUF.take();
            let r = sys::copy_small(&it.src, to, &mut buf);
            buf.clear();
            SMALL_BUF.set(buf);
            if r? {
                cx.add_bytes(it.size);
                return Ok(());
            }
        }
        // Тот же том `ReFS` (S5-15): клон блоков; не вышло — обычная копия. Проверка
        // хэшем клону не нужна (S5 §4.1) — клон только без неё.
        #[cfg(windows)]
        if let Some(cluster) = self.info().clone_cluster
            && it.size >= self.small
            && !matches!(self.opts.verify, crate::api::Verify::Hash(_))
            && crate::sys::refs_clone::clone_file(&it.src, to, cluster)?
        {
            cx.add_bytes(it.size);
            return Ok(());
        }
        let mut last = 0u64;
        let flags = sys::CopyFlags {
            excl,
            symlink: false,
            no_buffering: it.size >= self.strategy.large,
            // Разреженный (образы ВМ, базы): только данные — проверка атрибута
            // для заметных файлов.
            sparse: it.size >= self.small
                && sys::attrs(&it.src).is_ok_and(|a| a & ATTR_SPARSE != 0),
            restartable: !excl && self.resumable(it),
        };
        let r = sys::copy_file(&it.src, to, flags, &mut |done| {
            if done > last {
                cx.add_bytes(done - last);
                last = done;
            }
            cx.checkpoint().is_ok()
        });
        if let Err(e) = r {
            // Недописанное не считается: повтор посчитает файл заново.
            cx.progress().bytes_done.fetch_sub(last, Ordering::Relaxed);
            return Err(e);
        }
        if it.size > last {
            cx.add_bytes(it.size - last);
        }
        Ok(())
    }
}

thread_local! {
    /// Буфер своего пути мелких файлов: один на воркер.
    static SMALL_BUF: std::cell::Cell<Vec<u8>> = const { std::cell::Cell::new(Vec::new()) };
}

/// Сбросить файл на диск (`FlushFileBuffers`) и вернуть его размер. Read-only
/// (атрибут скопирован с источника) на время снимается: сброс требует записи.
fn durable(p: &Path) -> io::Result<u64> {
    let sync = || -> io::Result<u64> {
        let f = std::fs::OpenOptions::new().write(true).open(p)?;
        f.sync_all()?;
        Ok(f.metadata()?.len())
    };
    match sync() {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            let a = sys::attrs(p)?;
            if a & sys::ATTR_READONLY == 0 {
                return Err(e);
            }
            sys::set_attrs(p, a & !sys::ATTR_READONLY)?;
            let r = sync();
            sys::set_attrs(p, a)?;
            r
        }
        r => r,
    }
}

/// Проверка копии (S5 §4.9, S5-21): xxh3-128 источника (обычно ещё в кэше после
/// копии) и копии, перечитанной мимо кэша, — с диска, а не из памяти. Не совпало —
/// ошибка «контрольная сумма не совпала».
fn verify(src: &Path, dst: &Path) -> io::Result<()> {
    let hash = |p: &Path, uncached: bool| -> io::Result<u128> {
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        sys::read_chunks(p, uncached, &mut |b| h.update(b))?;
        Ok(h.digest128())
    };
    if hash(src, false)? == hash(dst, true)? {
        return Ok(());
    }
    Err(io::Error::other(tr("ops-verify-mismatch", &[])))
}

/// `path` внутри `base` (без учёта регистра, по компонентам) → остаток.
fn strip_prefix_ci(path: &Path, base: &Path) -> Option<PathBuf> {
    let mut p = path.components();
    for b in base.components() {
        let c = p.next()?;
        if !c.as_os_str().eq_ignore_ascii_case(b.as_os_str()) {
            return None;
        }
    }
    Some(p.as_path().to_path_buf())
}

/// Удалить временный файл, если он есть.
fn drop_temp(temp: &mut Option<PathBuf>, kind: EntryKind) {
    if let Some(t) = temp.take() {
        let _ = remove_src(&t, kind);
    }
}

/// Перенос rename-ом (тот же том).
fn rename_one(src: &Path, dst: &Path, how: How, scheme: RenameScheme) -> io::Result<()> {
    match how {
        How::Create => sys::move_file(src, dst, false),
        How::Replace => sys::move_file(src, dst, true),
        How::Backup => {
            let Some(bak) = free_name(dst, scheme, true, false, None) else {
                return Err(io::ErrorKind::AlreadyExists.into());
            };
            sys::move_file(dst, &bak, false)?;
            sys::move_file(src, dst, false)
        }
    }
}

const ATTR_DIRECTORY: u32 = 0x10;
/// `FILE_ATTRIBUTE_SPARSE_FILE`.
const ATTR_SPARSE: u32 = 0x200;

fn is_link(k: EntryKind) -> bool {
    matches!(
        k,
        EntryKind::LinkFile | EntryKind::LinkDir | EntryKind::LinkBroken
    )
}

/// Удалить источник после копии (перенос между томами); read-only снимается.
fn remove_src(p: &Path, kind: EntryKind) -> io::Result<()> {
    let dir = kind == EntryKind::LinkDir
        || (kind == EntryKind::LinkBroken && sys::attrs(p).is_ok_and(|a| a & ATTR_DIRECTORY != 0));
    let rm = || {
        if dir {
            std::fs::remove_dir(p)
        } else {
            std::fs::remove_file(p)
        }
    };
    match rm() {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            let a = sys::attrs(p)?;
            if a & sys::ATTR_READONLY == 0 {
                return Err(e);
            }
            sys::set_attrs(p, a & !sys::ATTR_READONLY & !ATTR_DIRECTORY)?;
            rm()
        }
        r => r,
    }
}

/// Временное имя рядом с назначением (S5 §6.1).
fn temp_path(dst: &Path, tag: &str) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(dst.file_name().unwrap_or_default());
    name.push(".fnv~");
    name.push(tag);
    dst.with_file_name(name)
}

/// Свободное имя рядом с `dst`: `имя (2).ext` или (`backup`) `имя.bak~1`.
/// `taken` — индекс каталога назначения (кроме ФС).
fn free_name(
    dst: &Path,
    scheme: RenameScheme,
    backup: bool,
    dir: bool,
    taken: Option<&dyn Fn(&str) -> bool>,
) -> Option<PathBuf> {
    let name = dst.file_name()?.to_string_lossy().into_owned();
    let stamp = if scheme == RenameScheme::Timestamp {
        sys::local_stamp()
    } else {
        String::new()
    };
    let (stem, ext) = if dir || backup {
        (name.as_str(), "")
    } else {
        split_ext(&name)
    };
    let first = u32::from(!backup) + 1;
    for n in first..10_000 {
        let cand = match (scheme, backup) {
            (RenameScheme::Counter, false) => format!("{stem} ({n}){ext}"),
            (RenameScheme::Counter, true) => format!("{stem}.bak~{n}"),
            (RenameScheme::Timestamp, false) if n == first => format!("{stem} ({stamp}){ext}"),
            (RenameScheme::Timestamp, false) => format!("{stem} ({stamp}) ({n}){ext}"),
            (RenameScheme::Timestamp, true) if n == first => format!("{stem}.bak~{stamp}"),
            (RenameScheme::Timestamp, true) => format!("{stem}.bak~{stamp}-{n}"),
        };
        if taken.is_some_and(|t| t(&cand)) {
            continue;
        }
        let p = dst.with_file_name(&cand);
        if std::fs::symlink_metadata(&p).is_err() {
            return Some(p);
        }
    }
    None
}

/// `report.final.pdf` → (`report.final`, `.pdf`); `.gitignore` → без расширения.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

fn nanos(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}

fn time_of(ns: i64) -> Option<SystemTime> {
    if ns == i64::MIN {
        return None;
    }
    let d = Duration::from_nanos(ns.unsigned_abs());
    if ns >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(d)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(d)
    }
}

fn mtime_of(p: &Path) -> i64 {
    std::fs::symlink_metadata(p)
        .and_then(|m| m.modified())
        .map_or(i64::MIN, nanos)
}

/// Существующий элемент назначения (без разыменования ссылки).
fn stat_dst(p: &Path) -> Option<Dst> {
    let m = std::fs::symlink_metadata(p).ok()?;
    let ft = m.file_type();
    let kind = if ft.is_symlink() {
        if sys::attrs(p).is_ok_and(|a| a & ATTR_DIRECTORY != 0) {
            EntryKind::LinkDir
        } else {
            EntryKind::LinkFile
        }
    } else if ft.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Some(Dst {
        kind,
        size: m.len(),
        mtime: m.modified().map_or(i64::MIN, nanos),
    })
}

/// Путь для показа: без `\\?\`.
pub(crate) fn shown(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        s.into_owned()
    }
}

/// Verbatim-путь (`\\?\C:\…`): имена с хвостовой точкой и пробелом, длинные пути.
pub(crate) fn verbatim(p: &Path) -> PathBuf {
    crate::names::os_path(p)
}

/// Ключ имени для сравнения на ФС без учёта регистра (NTFS, S5-03).
fn name_key(name: &str) -> CompactString {
    crate::names::name_key(name)
}

fn same_path(a: &Path, b: &Path) -> bool {
    name_key(&a.to_string_lossy()) == name_key(&b.to_string_lossy())
}

/// `inner` лежит внутри `outer`.
fn inside(inner: &Path, outer: &Path) -> bool {
    let (i, mut o) = (
        name_key(&inner.to_string_lossy()),
        name_key(&outer.to_string_lossy()),
    );
    if !o.ends_with('\\') {
        o.push('\\');
    }
    i.starts_with(o.as_str())
}

/// Текст ошибки ОС без «(os error N)».
fn io_text(e: &io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// Класс ошибки по коду Win32 (S5-11): занятость и сеть повторяются сами.
fn class(e: &io::Error) -> IoClass {
    match e.raw_os_error() {
        // SHARING_VIOLATION, LOCK_VIOLATION, USER_MAPPED_FILE.
        Some(32 | 33 | 1224) => IoClass::Sharing,
        // NOT_READY, BUSY, BUSY_DRIVE, DEVICE_IN_USE, DEVICE_NOT_AVAILABLE.
        Some(21 | 170 | 142 | 2404 | 4319) => IoClass::Busy,
        // BAD_NETPATH, UNEXP_NET_ERR, NETNAME_DELETED, BAD_NET_NAME, SEM_TIMEOUT,
        // NETWORK_UNREACHABLE, HOST_UNREACHABLE, CONNECTION_ABORTED, NETWORK_BUSY.
        Some(53 | 59 | 64 | 67 | 121 | 1231 | 1232 | 1236 | 54) => IoClass::Network,
        // ACCESS_DENIED, NETWORK_ACCESS_DENIED, PRIVILEGE_NOT_HELD.
        Some(5 | 65 | 1314) => IoClass::Permission,
        // FILE_NOT_FOUND, PATH_NOT_FOUND, BAD_NETPATH уже выше.
        Some(2 | 3) => IoClass::NotFound,
        _ => match e.kind() {
            io::ErrorKind::PermissionDenied => IoClass::Permission,
            io::ErrorKind::NotFound => IoClass::NotFound,
            _ => IoClass::Other,
        },
    }
}

/// Нет места (`DISK_FULL`, `HANDLE_DISK_FULL`): отдельный вопрос, без «для всех».
fn low_space(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(112 | 39))
}

/// Пауза автоповтора с отменой и паузой job.
fn pause(d: Duration, cx: &ExecCx<'_>) -> Result<(), Cancelled> {
    let end = std::time::Instant::now() + d;
    while std::time::Instant::now() < end {
        cx.checkpoint()?;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// Атрибуты записи → `FILE_ATTRIBUTE_*` для каталога назначения.
fn win_attrs(a: Attrs) -> u32 {
    let mut out = 0;
    for (bit, fa) in [
        (Attrs::READONLY, sys::ATTR_READONLY),
        (Attrs::HIDDEN, sys::ATTR_HIDDEN),
        (Attrs::SYSTEM, sys::ATTR_SYSTEM),
        (Attrs::ARCHIVE, sys::ATTR_ARCHIVE),
    ] {
        if a.has(bit) {
            out |= fa;
        }
    }
    out
}

// --- план ---

/// Существующие имена каталога назначения.
#[derive(Debug)]
enum Index {
    /// Каталог создан этой job: конфликтов нет.
    New,
    /// Прочитан один раз.
    Known(HashMap<CompactString, Dst>),
    /// Не прочитан (ошибка) или один элемент под новым именем: stat по месту.
    Stat,
}

/// Каталог назначения для детей обхода.
#[derive(Debug)]
pub(crate) struct Ctx {
    dst: PathBuf,
    index: Index,
}

impl Ctx {
    fn existing(&self, dst: &Path, name: &str) -> Option<Dst> {
        match &self.index {
            Index::New => None,
            Index::Known(m) => m.get(&name_key(name)).copied(),
            Index::Stat => stat_dst(dst),
        }
    }

    fn taken(&self, name: &str) -> bool {
        matches!(&self.index, Index::Known(m) if m.contains_key(&name_key(name)))
    }
}

struct Planner<'a> {
    t: &'a Transfer,
    fs: LocalFs,
}

impl Planner<'_> {
    /// Листинг существующего каталога назначения (один раз, S5 §0).
    fn index(&self, dir: &Path) -> Index {
        let mut map = HashMap::new();
        let opts = ListOpts {
            resolve_links: false,
            first_batch_hint: 256,
        };
        let mut batch = Vec::new();
        let r = self.fs.list(&loc(dir), &opts).and_then(|mut l| {
            let never = urfin_core::CancelToken::new();
            loop {
                batch.clear();
                let more = l.next_batch(&mut batch, &never)?;
                for e in &batch {
                    map.insert(
                        name_key(&e.name),
                        Dst {
                            kind: e.kind,
                            size: e.size,
                            mtime: e.mtime,
                        },
                    );
                }
                if !more {
                    return Ok(());
                }
            }
        });
        match r {
            Ok(()) => Index::Known(map),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "dest index, stat per item");
                Index::Stat
            }
        }
    }

    fn fail(
        &self,
        p: &mut Plan<'_, Transfer>,
        dir: &Arc<DirNode<DirData>>,
        path: &Path,
        e: &io::Error,
    ) {
        p.record(Some(dir), self.t.fail(path, e));
    }

    fn fail_text(
        &self,
        p: &mut Plan<'_, Transfer>,
        dir: &Arc<DirNode<DirData>>,
        path: &Path,
        key: &str,
    ) {
        let f = self.t.failed(path, &tr(key, &[]), IoClass::Other);
        p.record(Some(dir), ItemResult::Failed(f));
    }

    /// Файл или ссылка в `dst`.
    fn put_file(
        &mut self,
        p: &mut Plan<'_, Transfer>,
        parent: &Arc<DirNode<DirData>>,
        ctx: &Ctx,
        src: PathBuf,
        e: &Entry,
        mut dst: PathBuf,
    ) -> Result<(), PlanError> {
        let rename = self.t.info().rename;
        if let Some(kind) = self.t.precheck(e, &dst, rename) {
            let mut issue = self.t.issue(&e.name, e.size, e.mtime, &dst, None, None);
            issue.kind = kind;
            issue.dst = None;
            if kind == IssueKind::InvalidName {
                let fixed =
                    crate::names::fix_name(&dst.file_name().unwrap_or_default().to_string_lossy());
                issue.error = Some(tr("ops-invalid-name-fix", &[("name", &fixed)]).into());
            }
            let item = Item {
                src,
                dst,
                kind: e.kind,
                size: e.size,
                how: How::Create,
                rename,
                temp: None,
            };
            p.park(Some(parent), item, if rename { 0 } else { e.size }, issue)?;
            return Ok(());
        }
        let how = match ctx.existing(&dst, &e.name) {
            None => How::Create,
            // Ссылка-каталог поверх каталога — тоже «файл ↔ каталог».
            Some(d) => match self.t.decide(e.size, e.mtime, false, Some(d)) {
                Decision::Put(h) => h,
                Decision::Skip => {
                    p.record(Some(parent), ItemResult::Skipped);
                    return Ok(());
                }
                Decision::Ask => {
                    let issue = self.t.issue(&e.name, e.size, e.mtime, &dst, Some(d), None);
                    let bytes = if rename { 0 } else { e.size };
                    let item = Item {
                        src,
                        dst,
                        kind: e.kind,
                        size: e.size,
                        how: How::Create,
                        rename,
                        temp: None,
                    };
                    p.park(Some(parent), item, bytes, issue)?;
                    return Ok(());
                }
                Decision::Mismatch => {
                    self.fail_text(p, parent, &dst, "ops-copy-type");
                    return Ok(());
                }
                Decision::NewName => {
                    let taken = |n: &str| ctx.taken(n);
                    let Some(free) = free_name(&dst, self.t.scheme(), false, false, Some(&taken))
                    else {
                        self.fail_text(p, parent, &dst, "ops-copy-no-name");
                        return Ok(());
                    };
                    dst = free;
                    How::Create
                }
            },
        };
        let bytes = if rename { 0 } else { e.size };
        let item = Item {
            src,
            dst,
            kind: e.kind,
            size: e.size,
            how,
            rename,
            temp: None,
        };
        p.push(Some(parent), item, bytes)?;
        Ok(())
    }

    /// Каталог в `dst`: создать (или войти в существующий) и обойти; при
    /// переносе в пределах тома в несуществующее — rename целиком.
    fn put_dir(
        &mut self,
        p: &mut Plan<'_, Transfer>,
        parent: &Arc<DirNode<DirData>>,
        ctx: &Ctx,
        src: PathBuf,
        e: &Entry,
        dst: PathBuf,
    ) -> Result<Enter<DirData, Ctx>, PlanError> {
        let t = self.t;
        let mv = t.mode == Mode::Move;
        let (created, index) = match ctx.existing(&dst, &e.name) {
            // С маской — по файлам: целиком перенёсся бы и не подходящий под неё.
            None if mv && t.info().rename && t.mask.is_none() => {
                let item = Item {
                    src,
                    dst,
                    kind: EntryKind::Dir,
                    size: 0,
                    how: How::Create,
                    rename: true,
                    temp: None,
                };
                p.push(Some(parent), item, 0)?;
                return Ok(None);
            }
            None => match std::fs::create_dir(&dst) {
                Ok(()) => (true, Index::New),
                // Появился после индекса: слияние.
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => (false, self.index(&dst)),
                Err(err) => {
                    self.fail(p, parent, &dst, &err);
                    return Ok(None);
                }
            },
            Some(d) if d.kind.is_dir_like() => match t.opts.dirs {
                DirPolicy::Skip => {
                    p.record(Some(parent), ItemResult::Skipped);
                    return Ok(None);
                }
                // TODO S5-07: `DirPolicy::Ask` — вопрос; пока — слияние.
                DirPolicy::Merge | DirPolicy::Ask => (false, self.index(&dst)),
            },
            Some(_) => {
                self.fail_text(p, parent, &dst, "ops-copy-type");
                return Ok(None);
            }
        };
        let mut touch = SmallVec::new();
        touch.push(Location::local(shown(&dst)));
        if mv {
            touch.push(Location::local(shown(&src)));
        }
        let data = DirData {
            src: Some(src),
            dst: dst.clone(),
            mtime: e.mtime,
            attrs: win_attrs(e.attrs),
            created,
            remove_src: mv,
            touch,
        };
        let node = p.dir(Some(parent), data)?;
        Ok(Some((node, Ctx { dst, index })))
    }
}

impl Visit<Transfer> for Planner<'_> {
    type Ctx = Ctx;

    fn file(
        &mut self,
        p: &mut Plan<'_, Transfer>,
        dir: &Arc<DirNode<DirData>>,
        ctx: &Ctx,
        src: PathBuf,
        e: &Entry,
    ) -> Result<(), PlanError> {
        let dst = ctx.dst.join(Walker::os_name(e));
        self.put_file(p, dir, ctx, src, e, dst)
    }

    fn dir(
        &mut self,
        p: &mut Plan<'_, Transfer>,
        parent: &Arc<DirNode<DirData>>,
        ctx: &Ctx,
        src: PathBuf,
        e: &Entry,
    ) -> Result<Enter<DirData, Ctx>, PlanError> {
        let dst = ctx.dst.join(Walker::os_name(e));
        self.put_dir(p, parent, ctx, src, e, dst)
    }

    fn list_failed(
        &mut self,
        p: &mut Plan<'_, Transfer>,
        dir: &Arc<DirNode<DirData>>,
        src: &Path,
        err: &urfin_vfs::VfsError,
    ) {
        let f = self.t.failed(src, &err.to_string(), IoClass::Other);
        p.record(Some(dir), ItemResult::Failed(f));
    }
}

fn dest_text(e: &DestError) -> String {
    match e {
        DestError::Empty => tr("ops-dest-empty", &[]),
        DestError::NotADirectory(p) => tr("ops-dest-file", &[("path", p)]),
    }
}

impl OpExec for Transfer {
    type Item = Item;
    type Dir = DirData;

    #[allow(clippy::too_many_lines, reason = "план: назначение, корень, элементы")]
    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        if !self.pairs.is_empty() {
            return self.plan_pairs(p);
        }
        let base = verbatim(&self.base.path);
        let dest = verbatim(&self.dest.path);
        let kind = std::fs::metadata(&dest).ok().map(|m| {
            if m.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            }
        });
        let stub = OpRequest {
            kind: OpKind::Copy,
            base: self.base.clone(),
            items: self.items.clone(),
            dest: None,
            opts: OpOptions::default(),
        };
        let mode = resolve_dest(&stub, &self.dest, &self.dest_raw, kind)
            .map_err(|e| PlanError::Failed(dest_text(&e).into()))?;
        let (root, rename_to) = match mode {
            DestMode::Into(_) => (dest, None),
            DestMode::RenameTo(_) => {
                let parent = dest
                    .parent()
                    .map_or_else(|| dest.clone(), Path::to_path_buf);
                (parent, Some(dest))
            }
        };
        let existed = root.is_dir();
        if !existed {
            std::fs::create_dir_all(&root).map_err(|e| {
                let msg = tr(
                    "ops-dest-mkdir-failed",
                    &[("path", &shown(&root)), ("error", &io_text(&e))],
                );
                PlanError::Failed(msg.into())
            })?;
        }
        let slots = self.setup(&base, &root);
        p.device_slot(&slots)?;
        self.open_journal(&root);

        let mut touch = SmallVec::new();
        touch.push(Location::local(shown(&root)));
        if self.mode == Mode::Move {
            touch.push(self.base.clone());
        }
        let root_node = p.dir(
            None,
            DirData {
                src: None,
                dst: root.clone(),
                mtime: i64::MIN,
                attrs: 0,
                created: false,
                remove_src: false,
                touch,
            },
        )?;
        let mut pl = Planner {
            t: self,
            fs: LocalFs::new(),
        };
        let ctx = Ctx {
            dst: root.clone(),
            index: match (&rename_to, existed) {
                (Some(_), _) => Index::Stat,
                (None, true) => pl.index(&root),
                (None, false) => Index::New,
            },
        };
        let mut walker = Walker::new(self.mask.clone(), self.opts.links, p.cx().cancel_token());
        for name in &self.items {
            let src = base.join(name.as_str());
            let e = match pl.fs.stat_entry(&loc(&base), name, Attrs::default()) {
                Ok(e) => e,
                Err(err) => {
                    let f = self.failed(&src, &err.to_string(), IoClass::NotFound);
                    p.record(Some(&root_node), ItemResult::Failed(f));
                    continue;
                }
            };
            let target = rename_to
                .clone()
                .unwrap_or_else(|| root.join(Walker::os_name(&e)));
            let descend = e.kind == EntryKind::Dir
                || (e.kind == EntryKind::LinkDir && self.opts.links == LinkMode::Follow);
            if descend {
                self.roots
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((src.clone(), target.clone()));
            }
            if same_path(&src, &target) {
                // Перенос со сменой только регистра — rename; иначе «сам в себя».
                if self.mode == Mode::Move && src != target {
                    let item = Item {
                        src,
                        dst: target,
                        kind: e.kind,
                        size: 0,
                        how: How::Create,
                        rename: true,
                        temp: None,
                    };
                    p.push(Some(&root_node), item, 0)?;
                } else {
                    pl.fail_text(p, &root_node, &src, "ops-copy-same");
                }
                continue;
            }
            if descend && inside(&target, &src) {
                pl.fail_text(p, &root_node, &src, "ops-copy-into-self");
                continue;
            }
            if descend {
                if let Some((node, c)) = pl.put_dir(p, &root_node, &ctx, src.clone(), &e, target)? {
                    walker.walk(p, &mut pl, src, node, c)?;
                }
            } else if self.mask.as_ref().is_none_or(|m| m.matches(&e.name)) {
                pl.put_file(p, &root_node, &ctx, src, &e, target)?;
            }
        }
        p.close(&root_node);
        tracing::debug!(stats = ?walker.stats, "walk done");
        Ok(())
    }

    fn exec(&self, work: &Work<Self>, cx: &ExecCx<'_>) -> ItemResult {
        self.run(&work.item, cx)
    }

    fn finalize_dir(&self, dir: &DirNode<DirData>, cx: &ExecCx<'_>) -> ItemResult {
        let d = &dir.data;
        for l in &d.touch {
            cx.touch(l);
        }
        if d.created {
            if self.opts.preserve.contains(Preserve::TIMES)
                && let Some(t) = time_of(d.mtime)
                && let Err(e) = sys::set_dir_mtime(&d.dst, t)
            {
                tracing::warn!(dir = %d.dst.display(), error = %e, "dir mtime");
            }
            if self.opts.preserve.contains(Preserve::ATTRS)
                && d.attrs != 0
                && let Err(e) = sys::set_attrs(&d.dst, d.attrs)
            {
                tracing::warn!(dir = %d.dst.display(), error = %e, "dir attrs");
            }
        }
        if d.remove_src
            && dir.clean()
            && let Some(src) = &d.src
        {
            match remove_src(src, EntryKind::LinkDir) {
                Ok(()) => {}
                // Остались файлы не по маске — источник не пуст, это не ошибка.
                Err(e) if e.raw_os_error() == Some(145) => {}
                Err(e) => return self.fail(src, &e),
            }
        }
        ItemResult::Done
    }

    fn resolve(&self, mut it: Item, issue: &Issue, r: &Resolution) -> Resolved<Item> {
        if issue.kind == IssueKind::InvalidName {
            if *r != Resolution::RenameNew {
                return Resolved::Done(ItemResult::Skipped);
            }
            let fixed =
                crate::names::fix_name(&it.dst.file_name().unwrap_or_default().to_string_lossy());
            it.dst.set_file_name(fixed);
            return Resolved::Run(it);
        }
        if issue.kind == IssueKind::TooLarge {
            return Resolved::Done(ItemResult::Skipped);
        }
        if let IssueKind::Io(_) | IssueKind::LowSpace = issue.kind {
            if *r == Resolution::Retry {
                return Resolved::Run(it);
            }
            let c = match issue.kind {
                IssueKind::Io(c) => c,
                _ => IoClass::Other,
            };
            let text = issue.error.as_deref().unwrap_or_default();
            return Resolved::Done(ItemResult::Failed(self.failed_item(&it, text, c)));
        }
        it.temp.clone_from(&issue.temp);
        let newer = match (&issue.src, &issue.dst) {
            (Some(s), Some(d)) => {
                i128::from(s.mtime) > i128::from(d.mtime) + i128::from(self.info().tol_ns)
            }
            _ => false,
        };
        match r {
            Resolution::Overwrite => it.how = How::Replace,
            Resolution::OverwriteIfNewer if newer => it.how = How::Replace,
            Resolution::RenameExisting => it.how = How::Backup,
            Resolution::RenameNew => {
                let Some(free) = free_name(&it.dst, self.scheme(), false, false, None) else {
                    let f = self.failed(&it.dst, &tr("ops-copy-no-name", &[]), IoClass::Other);
                    self.discard(it, issue);
                    return Resolved::Done(ItemResult::Failed(f));
                };
                it.dst = free;
                it.how = How::Create;
            }
            _ => {
                self.discard(it, issue);
                return Resolved::Done(ItemResult::Skipped);
            }
        }
        Resolved::Run(it)
    }

    fn retry(&self) -> Option<Retry> {
        Some(Arc::new(RetrySpec {
            mode: self.mode,
            opts: self.opts.clone(),
            mask: self.mask.clone(),
            small: self.small,
        }))
    }

    fn discard(&self, it: Item, issue: &Issue) {
        if let Some(t) = &issue.temp {
            let _ = remove_src(t, it.kind);
        }
    }

    fn tick(&self, cx: &ExecCx<'_>) {
        self.aimd_step(cx);
        let Some(j) = self.journal.get() else {
            return;
        };
        if self.ticks.fetch_add(1, Ordering::Relaxed) % COUNTERS_EVERY == COUNTERS_EVERY - 1 {
            let p = cx.progress();
            j.append(&Record::Counters {
                items: p.items_done.load(Ordering::Relaxed),
                bytes: p.bytes_done.load(Ordering::Relaxed),
                items_total: p.items_total.load(Ordering::Relaxed),
                bytes_total: p.bytes_total.load(Ordering::Relaxed),
            });
        }
        j.flush();
    }

    fn finished(&self, r: &Report, outcome: &Result<JobSummary, String>) -> Option<Note> {
        if let Some(j) = self.journal.get() {
            match outcome {
                Err(e) if e == "cancelled" && QUITTING.load(Ordering::Relaxed) => j.keep(),
                _ => j.finish(r.summary.items, r.summary.bytes),
            }
        }
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
            return Some((
                Level::Error,
                tr_n("ops-copy-errors", s.errors, &[("first", &first)]),
            ));
        }
        if s.skipped > 0 {
            return Some((Level::Info, tr_n("ops-copy-skipped", s.skipped, &[])));
        }
        None
    }
}

impl Transfer {
    /// Предпроверки файла в плане (S5-20, S5 §4.11–4.12): больше предела ФС
    /// назначения (FAT32) — `TooLarge`; имя, которое на Windows не создать или не
    /// открыть, — `InvalidName`. Перенос rename-ом на том же томе — без проверок.
    fn precheck(&self, e: &Entry, dst: &Path, rename: bool) -> Option<IssueKind> {
        if rename {
            return None;
        }
        if let Some(max) = self.info().max_file
            && e.size > max
        {
            return Some(IssueKind::TooLarge);
        }
        let name = dst.file_name().unwrap_or_default().to_string_lossy();
        match crate::names::check_name(&name) {
            Ok(()) => None,
            Err(
                crate::names::NameIssue::Reserved | crate::names::NameIssue::TrailingDotOrSpace,
            ) if !self.info().fat => None,
            Err(_) => Some(IssueKind::InvalidName),
        }
    }

    /// Журнал job (S5-12): заголовок с запросом, корень назначения.
    fn open_journal(&self, root: &Path) {
        let Some(dir) = &self.journal_dir else {
            return;
        };
        let header = journal::Header {
            v: journal::VERSION,
            req: OpRequest {
                kind: if self.mode == Mode::Move {
                    OpKind::Move
                } else {
                    OpKind::Copy
                },
                base: self.base.clone(),
                items: self.items.clone(),
                dest: None,
                opts: self.opts.clone(),
            },
            dest: self.dest.path.clone(),
            dest_raw: self.dest_raw.clone(),
            started: nanos(std::time::SystemTime::now()) / 1_000_000_000,
            pid: std::process::id(),
        };
        match Journal::create(dir, header) {
            Ok(j) => {
                j.append(&Record::Root(root.to_path_buf()));
                let _ = self.journal.set(j);
            }
            Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "journal not created"),
        }
    }

    /// План повтора (`ops.retry`): каждая упавшая пара заново — файл или каталог
    /// целиком в своё назначение; каталог назначения создаётся при нужде.
    fn plan_pairs(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        let (src0, dst0) = &self.pairs[0];
        let parent = |x: &Path| {
            x.parent()
                .map_or_else(|| x.to_path_buf(), Path::to_path_buf)
        };
        let slots = self.setup(&parent(src0), &parent(dst0));
        p.device_slot(&slots)?;
        let mut touch: SmallVec<[Location; 2]> = SmallVec::new();
        for (s, d) in &self.pairs {
            for l in [
                Location::local(shown(&parent(d))),
                Location::local(shown(&parent(s))),
            ] {
                if (self.mode == Mode::Move || l.path != parent(s)) && !touch.contains(&l) {
                    touch.push(l);
                }
            }
        }
        let root = p.dir(
            None,
            DirData {
                src: None,
                dst: parent(dst0),
                mtime: i64::MIN,
                attrs: 0,
                created: false,
                remove_src: false,
                touch,
            },
        )?;
        let mut pl = Planner {
            t: self,
            fs: LocalFs::new(),
        };
        let mut walker = Walker::new(self.mask.clone(), self.opts.links, p.cx().cancel_token());
        for (src, dst) in &self.pairs {
            let (sp, dp) = (parent(src), parent(dst));
            if let Err(e) = std::fs::create_dir_all(&dp) {
                pl.fail(p, &root, &dp, &e);
                continue;
            }
            let name = src.file_name().unwrap_or_default().to_string_lossy();
            let e = match pl.fs.stat_entry(&loc(&sp), &name, Attrs::default()) {
                Ok(e) => e,
                Err(err) => {
                    let f = self.failed(src, &err.to_string(), IoClass::NotFound);
                    p.record(Some(&root), ItemResult::Failed(f));
                    continue;
                }
            };
            let ctx = Ctx {
                dst: dp,
                index: Index::Stat,
            };
            if e.kind == EntryKind::Dir {
                if let Some((node, c)) = pl.put_dir(p, &root, &ctx, src.clone(), &e, dst.clone())? {
                    walker.walk(p, &mut pl, src.clone(), node, c)?;
                }
            } else {
                pl.put_file(p, &root, &ctx, src.clone(), &e, dst.clone())?;
            }
        }
        p.close(&root);
        Ok(())
    }

    /// Том и ФС: перенос rename-ом, допуск времени, число работающих воркеров по
    /// носителю (S5 §3.3); слоты устройств источника и назначения (S5-19).
    fn setup(&self, base: &Path, root: &Path) -> SmallVec<[crate::engine::Slot; 2]> {
        use urfin_vfs::volume::{FsKind, Media};
        let vs = urfin_vfs::volume::of(base).ok();
        let vd = urfin_vfs::volume::of(root).ok();
        let same = matches!((&vs, &vd), (Some(a), Some(b)) if a.id == b.id);
        let tol = match vd.as_ref().map(|v| v.fs) {
            Some(FsKind::Fat32 | FsKind::ExFat) => FAT_TOLERANCE_NS,
            _ => 0,
        };
        #[allow(unused_mut, reason = "подмена носителя в тестах")]
        let mut media = [&vs, &vd].map(|v| v.as_ref().map_or(Media::Unknown, |v| v.media));
        #[cfg(test)]
        if let Some(m) = self.media {
            media = [m, m];
        }
        let n = media
            .iter()
            .map(|m| crate::strategy::workers_for(*m))
            .min()
            .unwrap_or(1);
        self.gate.max.store(n, Ordering::Relaxed);
        let hdd = media.contains(&Media::Hdd);
        *self.aimd.lock().unwrap_or_else(PoisonError::into_inner) = Some((
            crate::engine::aimd::Aimd::new(n, usize::from(WORKERS), hdd),
            0,
            0,
            std::time::Instant::now(),
        ));
        // Устройство: номер диска; нет — номер тома (сеть — по корню).
        let key = |p: &Path, v: &Option<urfin_vfs::volume::VolumeInfo>| {
            urfin_vfs::volume::device_key(p).unwrap_or_else(|| v.as_ref().map_or(0, |v| v.id))
        };
        let mut slots: SmallVec<[crate::engine::Slot; 2]> = SmallVec::new();
        for (k, m) in [(key(base, &vs), media[0]), (key(root, &vd), media[1])] {
            let lim = urfin_vfs::volume::jobs_for(&self.strategy.jobs, m);
            match slots.iter_mut().find(|s| s.0 == k) {
                Some(s) => s.1 = s.1.min(lim),
                None => slots.push((k, lim)),
            }
        }
        let max_file =
            matches!(vd.as_ref().map(|v| v.fs), Some(FsKind::Fat32)).then_some(FAT32_MAX_FILE);
        tracing::debug!(
            same_volume = same,
            tol,
            workers = n,
            ?slots,
            "transfer setup"
        );
        let _ = self.info.set(Info {
            tol_ns: tol,
            rename: same && self.mode == Mode::Move,
            max_file,
            fat: matches!(
                vd.as_ref().map(|v| v.fs),
                Some(FsKind::Fat32 | FsKind::ExFat)
            ),
            root: root.to_path_buf(),
            resumable_dest: matches!(media[1], Media::Net | Media::Usb),
            clone_cluster: vd
                .as_ref()
                .filter(|v| {
                    same && v.fs == FsKind::Refs && v.caps.has(urfin_vfs::volume::VolCaps::CLONE)
                })
                .map(|v| v.cluster),
        });
        slots
    }
}

#[cfg(test)]
#[path = "copy_tests.rs"]
mod tests;
