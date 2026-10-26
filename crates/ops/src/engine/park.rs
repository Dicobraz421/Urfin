//! Немодальные проблемы (S5 §5, S5-07): работа с проблемой паркуется, воркеры
//! идут дальше. Спрашивает отдельный поток `ops-ask-N` — по одной проблеме,
//! ждёт ответа только он. Ответ «для всех» записывается в таблицу `decided` и
//! применяется ко всем запаркованным того же вида без новых вопросов; к новым
//! проблемам этого вида — сразу, без парковки. Разобранные работы идут в
//! приоритетную очередь воркеров.
//!
//! Запаркованная работа не закрывает ребёнка своего каталога: каталог не
//! финализируется (при переносе — не удаляется), пока на неё не ответили.
//! Воркеры не выходят, пока есть запаркованные или разобранные работы.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use compact_str::CompactString;
use crossbeam_channel::{Receiver, Sender};
use smallvec::SmallVec;
use urfin_core::{Anchor, Attrs, Choice, Entry, EntryKind, PromptDetail, PromptSpec, Reply};

use super::job::{Event, POLL, Shared};
use super::{ItemResult, OpExec, Work};
use crate::api::{IoClass, Issue, IssueId, IssueKind, ItemMeta, Resolution};

/// Итог ответа для одной работы.
#[derive(Debug)]
pub(crate) enum Resolved<I> {
    /// Выполнить (изменённую) работу.
    Run(I),
    /// Итог без работы: пропуск или ошибка.
    Done(ItemResult),
}

/// Запаркованная работа.
pub(super) struct Parked<E: OpExec> {
    pub(super) issue: Issue,
    pub(super) work: Work<E>,
    /// Байт в `bytes_total`, которые не перенесутся, если работу пропустят.
    pub(super) bytes: u64,
}

struct St<E: OpExec> {
    queue: VecDeque<Parked<E>>,
    decided: HashMap<IssueKind, Resolution>,
    /// Задано вопросов (номер следующего — `asked + 1`).
    asked: u64,
    /// Воркеры вышли: поток вопросов завершается.
    closed: bool,
    next_id: u32,
}

/// Парковка job.
pub(super) struct Park<E: OpExec> {
    st: Mutex<St<E>>,
    cv: Condvar,
    /// Запаркованных и разобранных, но ещё не взятых воркером работ.
    outstanding: AtomicU64,
    /// Поток вопросов ждёт ответа пользователя.
    asking: AtomicBool,
    retry_tx: Sender<Work<E>>,
    pub(super) retry_rx: Receiver<Work<E>>,
}

impl<E: OpExec> Park<E> {
    pub(super) fn new() -> Park<E> {
        let (retry_tx, retry_rx) = crossbeam_channel::unbounded();
        Park {
            st: Mutex::new(St {
                queue: VecDeque::new(),
                decided: HashMap::new(),
                asked: 0,
                closed: false,
                next_id: 0,
            }),
            cv: Condvar::new(),
            outstanding: AtomicU64::new(0),
            asking: AtomicBool::new(false),
            retry_tx,
            retry_rx,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, St<E>> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ответ «для всех» на проблемы такого вида.
    pub(super) fn decided(&self, kind: IssueKind) -> Option<Resolution> {
        self.lock().decided.get(&kind).cloned()
    }

    /// Воркерам нельзя выходить: есть запаркованные или разобранные работы.
    pub(super) fn busy(&self) -> bool {
        self.outstanding.load(Ordering::SeqCst) > 0
    }

    /// Открыт вопрос пользователю.
    pub(super) fn asking(&self) -> bool {
        self.asking.load(Ordering::SeqCst)
    }

    /// Воркер взял разобранную работу и закончил с ней.
    pub(super) fn retry_done(&self) {
        self.outstanding.fetch_sub(1, Ordering::SeqCst);
    }

    /// Воркеры вышли: поток вопросов заканчивает, оставшееся — [`Park::drain`].
    pub(super) fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    /// Неразобранные работы (отмена job).
    pub(super) fn drain(&self) -> Vec<Parked<E>> {
        self.lock().queue.drain(..).collect()
    }
}

/// Запарковать работу (её ребёнок у каталога уже учтён). Есть ответ «для всех» —
/// применяется сразу.
pub(super) fn put<E: OpExec>(sh: &Shared<E>, work: Work<E>, mut issue: Issue, bytes: u64) {
    let park = &sh.park;
    let mut st = park.lock();
    if let Some(r) = st.decided.get(&issue.kind).cloned() {
        drop(st);
        apply(sh, Parked { issue, work, bytes }, &r);
        return;
    }
    st.next_id += 1;
    issue.id = IssueId(st.next_id);
    park.outstanding.fetch_add(1, Ordering::SeqCst);
    st.queue.push_back(Parked { issue, work, bytes });
    drop(st);
    sh.ctl.job.progress().parked.fetch_add(1, Ordering::Relaxed);
    park.cv.notify_all();
}

/// Ответ для одной работы: в приоритетную очередь воркеров или итог сразу.
fn apply<E: OpExec>(sh: &Shared<E>, p: Parked<E>, r: &Resolution) {
    let Parked { issue, work, bytes } = p;
    let Work { item, parent } = work;
    match sh.exec.resolve(item, &issue, r) {
        Resolved::Run(item) => {
            sh.park.outstanding.fetch_add(1, Ordering::SeqCst);
            // Неограниченная очередь: воркеры её не наполняют, отправка не ждёт.
            let _ = sh.park.retry_tx.send(Work { item, parent });
        }
        Resolved::Done(res) => settle(sh, parent.as_ref(), res, bytes),
    }
}

/// Итог запаркованной работы без воркера: учёт и закрытие ребёнка каталога.
pub(super) fn settle<E: OpExec>(
    sh: &Shared<E>,
    parent: Option<&Arc<super::DirNode<E::Dir>>>,
    res: ItemResult,
    bytes: u64,
) {
    let p = sh.ctl.job.progress();
    if !matches!(res, ItemResult::Done) {
        p.bytes_total.fetch_sub(bytes, Ordering::Relaxed);
    }
    if !sh.ctl.record(res, parent) {
        return;
    }
    p.items_done.fetch_add(1, Ordering::Relaxed);
    if let Some(d) = parent
        && d.complete_child()
    {
        let _ = sh.events.send(Event::DirReady(Arc::clone(d)));
    }
}

/// Поток вопросов: первая запаркованная проблема → вопрос → ответ.
pub(super) fn ask_loop<E: OpExec>(sh: &Shared<E>) {
    let _span = sh.ctl.span.enter();
    let park = &sh.park;
    loop {
        let (first, index, total) = {
            let mut st = park.lock();
            loop {
                if sh.ctl.stopped() {
                    return;
                }
                if let Some(p) = st.queue.pop_front() {
                    st.asked += 1;
                    let total = st.asked + st.queue.len() as u64;
                    break (p, st.asked, total);
                }
                if st.closed {
                    return;
                }
                st = park
                    .cv
                    .wait_timeout(st, POLL)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        };
        let progress = sh.ctl.job.progress();
        let spec = prompt(&first.issue, index, total);
        park.asking.store(true, Ordering::SeqCst);
        // Остановку движка `ask` не видит: координатор отменяет job, только если
        // вопрос открыт, — поэтому проверка после флага (одна из сторон увидит другую).
        if sh.ctl.stopped() {
            park.asking.store(false, Ordering::SeqCst);
            park.lock().queue.push_front(first);
            return;
        }
        let asked = sh.ctl.job.ask(spec);
        park.asking.store(false, Ordering::SeqCst);
        let Ok(reply) = asked else {
            // Отмена job: работа остаётся до `drain` (удалить temp).
            park.lock().queue.push_front(first);
            return;
        };
        let r = resolution(&reply);
        if r == Resolution::Abort {
            park.lock().queue.push_front(first);
            sh.ctl.job.cancel_token().cancel();
            return;
        }
        let kind = first.issue.kind;
        let mut batch = vec![first];
        // Нехватка места — одна причина на всех запаркованных: ответ (обычно
        // «Повторить» после освобождения) — им всем, но не в `decided`: кончится
        // место снова — спросим снова.
        if reply.all || kind == IssueKind::LowSpace {
            let mut st = park.lock();
            if kind != IssueKind::LowSpace {
                st.decided.insert(kind, r.clone());
            }
            let (same, rest): (VecDeque<_>, VecDeque<_>) =
                st.queue.drain(..).partition(|p| p.issue.kind == kind);
            st.queue = rest;
            batch.extend(same);
        }
        for p in batch {
            progress.parked.fetch_sub(1, Ordering::Relaxed);
            apply(sh, p, &r);
            park.outstanding.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Варианты ответа (id — [`resolution`]).
const OVERWRITE: &str = "overwrite";
const SKIP: &str = "skip";
const RENAME: &str = "rename";
const NEWER: &str = "newer";
const CANCEL: &str = "cancel";
const RETRY: &str = "retry";
const DELETE: &str = "delete";
const FIX: &str = "fix";
const JUNCTION: &str = "junction";

fn resolution(r: &Reply) -> Resolution {
    match r.choice.as_str() {
        OVERWRITE => Resolution::Overwrite,
        // «Исправить имя» — тоже новое имя (`fix_name`).
        RENAME | FIX => Resolution::RenameNew,
        NEWER => Resolution::OverwriteIfNewer,
        CANCEL => Resolution::Abort,
        RETRY => Resolution::Retry,
        DELETE => Resolution::DeletePermanently,
        JUNCTION => Resolution::Junction,
        _ => Resolution::Skip,
    }
}

fn tr(key: &str, args: &[(&str, String)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, v.clone());
    }
    urfin_i18n::current().fmt(key, &a)
}

fn choice(id: &str, key: &str, k: char) -> Choice {
    Choice {
        id: id.into(),
        label: CompactString::from(urfin_i18n::current().get(key)),
        key: k,
    }
}

/// Вопрос по проблеме (S5 §2.2, таблица вариантов). По умолчанию — «Пропустить»:
/// вопрос появляется сам, и Enter, нажатый для панели, не должен ничего заменить.
pub(super) fn prompt(issue: &Issue, index: u64, total: u64) -> PromptSpec {
    let name = issue
        .src
        .as_ref()
        .or(issue.dst.as_ref())
        .map_or_else(String::new, |m| m.name.to_string());
    let mut choices: SmallVec<[Choice; 4]> = SmallVec::new();
    if issue.op == crate::api::ItemOp::Link {
        return link_prompt(issue, name, index, total);
    }
    if let IssueKind::Io(_) | IssueKind::LowSpace = issue.kind {
        return error_prompt(issue, name, index, total);
    }
    if issue.kind == IssueKind::NoTrash {
        return no_trash_prompt(issue, name, index, total);
    }
    if let IssueKind::TooLarge | IssueKind::InvalidName = issue.kind {
        return name_prompt(issue, name, index, total);
    }
    let (title, cmp) = if issue.kind == IssueKind::Exists {
        choices.push(choice(OVERWRITE, "ops-choice-overwrite", 'r'));
        choices.push(choice(SKIP, "ops-choice-skip", 's'));
        choices.push(choice(RENAME, "ops-choice-rename", 'n'));
        choices.push(choice(NEWER, "ops-choice-newer", 'w'));
        let cmp = match (&issue.src, &issue.dst) {
            (Some(s), Some(d)) => Some(compare(s, d)),
            _ => None,
        };
        ("ops-conflict-exists", cmp)
    } else {
        choices.push(choice(SKIP, "ops-choice-skip", 's'));
        ("ops-conflict-other", None)
    };
    choices.push(choice(CANCEL, "ops-choice-cancel", 'c'));
    let default = choices
        .iter()
        .position(|c| c.id == SKIP)
        .and_then(|i| u8::try_from(i).ok())
        .unwrap_or(0);
    let mut args = vec![
        ("index", index.to_string()),
        ("total", total.to_string()),
        ("name", name),
    ];
    let key = match cmp {
        Some(c) => {
            args.push(("cmp", c));
            "ops-conflict-body"
        }
        None => "ops-conflict-body-short",
    };
    let body = tr(key, &args);
    let detail = match (&issue.src, &issue.dst) {
        (Some(s), Some(d)) => Some(PromptDetail::Conflict {
            src: entry(s),
            dst: entry(d),
            index,
            total,
        }),
        _ => None,
    };
    PromptSpec {
        anchor: Anchor::JobBar,
        title: urfin_i18n::current().get(title),
        body,
        choices,
        default,
        input: None,
        allow_all: true,
        detail,
        secret: false,
    }
}

/// Тело вопроса об ошибке: первая строка — путь (или имя), вторая — кто держит файл
/// (№69, только «занят»), причина без точки с маленькой буквы и код ОС; при нескольких
/// проблемах — «k из N».
fn error_body(issue: &Issue, name: String, index: u64, total: u64) -> String {
    let path = issue
        .path
        .as_ref()
        .map_or(name, |p| crate::ops::copy::shown(p));
    let mut parts: Vec<String> = Vec::with_capacity(4);
    let mut held = false;
    if issue.kind == IssueKind::Io(IoClass::Sharing)
        && let Some(p) = issue.path.as_deref()
    {
        for h in crate::sys::rm::holders(p).into_iter().take(2) {
            parts.push(tr(
                "ops-issue-holder",
                &[("exe", h.exe), ("pid", h.pid.to_string())],
            ));
            held = true;
        }
    }
    // Известно, кто держит файл, — длинная фраза ОС «файл занят» не нужна.
    if let Some(e) = issue
        .error
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty() && !held)
    {
        let e = e.trim_end_matches('.');
        let mut c = e.chars();
        let lower: String = c
            .next()
            .map(|f| f.to_lowercase().chain(c).collect())
            .unwrap_or_default();
        parts.push(lower);
    }
    if let Some(code) = issue.code {
        parts.push(tr("ops-issue-code", &[("code", code.to_string())]));
    }
    if total > 1 {
        parts.push(tr(
            "ops-issue-of",
            &[("index", index.to_string()), ("total", total.to_string())],
        ));
    }
    if parts.is_empty() {
        path
    } else {
        format!("{path}\n{}", parts.join(" · "))
    }
}

/// Вопрос по ошибке после автоповторов (S5-11): Повторить, Пропустить, Отмена;
/// «для всех» — по классу ошибки (для нехватки места — нет).
fn error_prompt(issue: &Issue, name: String, index: u64, total: u64) -> PromptSpec {
    let title = match issue.kind {
        IssueKind::LowSpace => "ops-issue-low-space",
        IssueKind::Io(IoClass::Permission) => "ops-issue-permission",
        IssueKind::Io(IoClass::Sharing) => "ops-issue-sharing",
        IssueKind::Io(IoClass::Busy) => "ops-issue-busy",
        IssueKind::Io(IoClass::Network) => "ops-issue-network",
        IssueKind::Io(IoClass::NotFound) => "ops-issue-not-found",
        _ => "ops-issue-other",
    };
    let choices: SmallVec<[Choice; 4]> = [
        choice(RETRY, "ops-choice-retry", 'r'),
        choice(SKIP, "ops-choice-skip", 's'),
        choice(CANCEL, "ops-choice-cancel", 'c'),
    ]
    .into_iter()
    .collect();
    let body = error_body(issue, name, index, total);
    PromptSpec {
        anchor: Anchor::JobBar,
        title: urfin_i18n::current().get(title),
        body,
        choices,
        default: 0,
        input: None,
        allow_all: issue.kind != IssueKind::LowSpace,
        detail: None,
        secret: false,
    }
}

/// В корзину нельзя (удалилось бы насовсем): «Удалить насовсем», «Пропустить»
/// (по умолчанию), «Отмена»; «для всех» — один ответ на все такие.
fn no_trash_prompt(issue: &Issue, name: String, index: u64, total: u64) -> PromptSpec {
    let choices: SmallVec<[Choice; 4]> = [
        choice(DELETE, "ops-choice-delete-permanently", 'd'),
        choice(SKIP, "ops-choice-skip", 's'),
        choice(CANCEL, "ops-choice-cancel", 'c'),
    ]
    .into_iter()
    .collect();
    let body = tr(
        "ops-issue-body",
        &[
            ("index", index.to_string()),
            ("total", total.to_string()),
            ("name", name),
            (
                "error",
                issue.error.as_deref().unwrap_or_default().to_owned(),
            ),
        ],
    );
    PromptSpec {
        anchor: Anchor::JobBar,
        title: urfin_i18n::current().get("ops-issue-no-trash"),
        body,
        choices,
        default: 1,
        input: None,
        allow_all: true,
        detail: None,
        secret: false,
    }
}

/// Файл не положить на ФС назначения: больше предела (FAT32) — «Пропустить»,
/// «Отмена»; недопустимое имя — ещё «Исправить имя» (`fix_name`). «Для всех».
fn name_prompt(issue: &Issue, name: String, index: u64, total: u64) -> PromptSpec {
    let mut choices: SmallVec<[Choice; 4]> = SmallVec::new();
    let title = if issue.kind == IssueKind::InvalidName {
        choices.push(choice(FIX, "ops-choice-fix-name", 'f'));
        "ops-issue-invalid-name"
    } else {
        "ops-issue-too-large"
    };
    choices.push(choice(SKIP, "ops-choice-skip", 's'));
    choices.push(choice(CANCEL, "ops-choice-cancel", 'c'));
    let detail = match issue.kind {
        IssueKind::TooLarge => urfin_i18n::current().get("ops-too-large-why"),
        _ => issue.error.as_deref().unwrap_or_default().to_owned(),
    };
    let body = tr(
        "ops-issue-body",
        &[
            ("index", index.to_string()),
            ("total", total.to_string()),
            ("name", name),
            ("error", detail),
        ],
    );
    PromptSpec {
        anchor: Anchor::JobBar,
        title: urfin_i18n::current().get(title),
        body,
        choices,
        default: 0,
        input: None,
        allow_all: true,
        detail: None,
        secret: false,
    }
}

/// Symlink без прав (режим разработчика выключен): для каталога — junction.
fn link_prompt(issue: &Issue, name: String, index: u64, total: u64) -> PromptSpec {
    let choices: SmallVec<[Choice; 4]> = [
        choice(JUNCTION, "ops-choice-junction", 'j'),
        choice(CANCEL, "ops-choice-cancel", 'c'),
    ]
    .into_iter()
    .collect();
    let body = tr(
        "ops-issue-body",
        &[
            ("index", index.to_string()),
            ("total", total.to_string()),
            ("name", name),
            (
                "error",
                issue.error.as_deref().unwrap_or_default().to_owned(),
            ),
        ],
    );
    PromptSpec {
        anchor: Anchor::JobBar,
        title: urfin_i18n::current().get("ops-issue-no-symlink"),
        body,
        choices,
        default: 0,
        input: None,
        allow_all: false,
        detail: None,
        secret: false,
    }
}

fn entry(m: &ItemMeta) -> Entry {
    Entry {
        ctime: i64::MIN,
        atime: i64::MIN,
        name: m.name.clone(),
        size: m.size,
        mtime: m.mtime,
        attrs: Attrs::default(),
        kind: EntryKind::File,
    }
}

/// «новее, больше на 2,1 МБ»: источник относительно существующего.
fn compare(s: &ItemMeta, d: &ItemMeta) -> String {
    let time = match s.mtime.cmp(&d.mtime) {
        _ if s.mtime == i64::MIN || d.mtime == i64::MIN => None,
        std::cmp::Ordering::Greater => Some(tr("ops-conflict-newer", &[])),
        std::cmp::Ordering::Less => Some(tr("ops-conflict-older", &[])),
        std::cmp::Ordering::Equal => Some(tr("ops-conflict-same-time", &[])),
    };
    let size = match s.size.cmp(&d.size) {
        std::cmp::Ordering::Greater => tr(
            "ops-conflict-larger",
            &[("size", urfin_i18n::fmt::bytes(s.size - d.size))],
        ),
        std::cmp::Ordering::Less => tr(
            "ops-conflict-smaller",
            &[("size", urfin_i18n::fmt::bytes(d.size - s.size))],
        ),
        std::cmp::Ordering::Equal => tr("ops-conflict-same-size", &[]),
    };
    match time {
        Some(t) => format!("{t}, {size}"),
        None => size,
    }
}
