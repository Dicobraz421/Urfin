//! Групповое переименование (S5 §4.14, S5-23).
//!
//! Шаблон в стиле Total Commander / Far: `[N]` имя без расширения, `[N2-5]`,
//! `[N3]`, `[N2-]` — символы имени (с 1), `[E]` расширение (и `[E1-2]`…),
//! `[C]` / `[C:start:step:width]` счётчик, `[Y]` `[M]` `[D]` дата изменения
//! (местная), `[U]` / `[L]` / `[n]` — дальше прописными / строчными / как есть,
//! `[[` — сама `[`. Пустое расширение съедает точку перед `[E]`. Режим regex:
//! `найти/заменить` (по последней `/`; в имени `/` не бывает), `$1` — группы.
//!
//! План (`from → to`) строится без I/O по именам каталога — для превью на
//! каждое нажатие (10k — меньше 50 мс). Проверки: недопустимое имя, дубли среди
//! новых имён, занятое имя вне набора. Применение — job, одно переименование
//! на элемент: цепочка (`a → b`, `b → c`) идёт с хвоста, цикл (`a → b`,
//! `b → a`) — через одно временное имя на цикл (S0 №93).
//!
//! Откат (S5-29): переименованные строки job пишет в журнал ([`UndoLog`]);
//! `ops.multi_rename.undo` применяет их наоборот тем же движком (цепочки и циклы —
//! так же). Что не вернулось (имя занято, файла нет) — остаётся в журнале: откат
//! можно повторить.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use compact_str::CompactString;
use urfin_core::{Cx, JobId, JobSpec, JobSummary, Lane, Level, Location};

use crate::api::{FailedItem, IoClass, ItemOp, RenameSpec, Report};
use crate::engine::{DirNode, ExecCx, ItemResult, JobEngine, Note, OpExec, Plan, PlanError, Work};
use crate::journal::RenameLog;
use crate::names::{check_name, name_key};

/// Элемент для переименования.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: CompactString,
    /// Наносекунды от эпохи Unix; `i64::MIN` — неизвестно.
    pub mtime: i64,
    pub dir: bool,
}

/// Что не так с новым именем.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Пустое или недопустимое на Windows.
    Invalid,
    /// Такое же новое имя у другого элемента.
    Duplicate,
    /// Имя занято элементом вне набора.
    Exists,
}

/// Строка плана.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub from: CompactString,
    pub to: CompactString,
    pub problem: Option<Problem>,
}

/// План: только изменяющиеся имена.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenamePlan {
    pub rows: Vec<Row>,
    pub problems: usize,
}

/// Ошибка шаблона (текст — для строки превью).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PatternError(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(String),
    Name(usize, Option<usize>),
    Ext(usize, Option<usize>),
    Counter { start: i64, step: i64, width: usize },
    Year,
    Month,
    Day,
    Upper,
    Lower,
    Plain,
}

fn bad(what: &str) -> PatternError {
    PatternError(what.to_owned())
}

/// `N` / `N3` / `N2-5` / `N2-` → (с, по).
fn range(rest: &str) -> Result<(usize, Option<usize>), PatternError> {
    if rest.is_empty() {
        return Ok((1, None));
    }
    let num = |s: &str| s.parse::<usize>().ok().filter(|n| *n > 0);
    match rest.split_once('-') {
        None => {
            let n = num(rest).ok_or_else(|| bad(rest))?;
            Ok((n, Some(n)))
        }
        Some((a, "")) => Ok((num(a).ok_or_else(|| bad(a))?, None)),
        Some((a, b)) => {
            let (a, b) = (num(a).ok_or_else(|| bad(a))?, num(b).ok_or_else(|| bad(b))?);
            if b < a {
                return Err(bad(rest));
            }
            Ok((a, Some(b)))
        }
    }
}

fn parse(pattern: &str) -> Result<Vec<Tok>, PatternError> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut rest = pattern;
    while let Some(i) = rest.find('[') {
        lit.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('[') {
            lit.push('[');
            rest = r;
            continue;
        }
        let Some(end) = rest.find(']') else {
            return Err(bad(&format!("[{rest}")));
        };
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        if !lit.is_empty() {
            out.push(Tok::Lit(std::mem::take(&mut lit)));
        }
        let mut chars = tag.chars();
        let tok = match (chars.next(), chars.as_str()) {
            (Some('N'), r) => {
                let (a, b) = range(r)?;
                Tok::Name(a, b)
            }
            (Some('E'), r) => {
                let (a, b) = range(r)?;
                Tok::Ext(a, b)
            }
            (Some('C'), r) => {
                let mut parts = r.split(':').skip(usize::from(r.starts_with(':')));
                let mut next = |d: i64| -> Result<i64, PatternError> {
                    match parts.next() {
                        None | Some("") => Ok(d),
                        Some(p) => p.parse().map_err(|_| bad(p)),
                    }
                };
                let (start, step, width) = (next(1)?, next(1)?, next(1)?);
                Tok::Counter {
                    start,
                    step,
                    width: usize::try_from(width.clamp(1, 12)).unwrap_or(1),
                }
            }
            (Some('Y'), "") => Tok::Year,
            (Some('M'), "") => Tok::Month,
            (Some('D'), "") => Tok::Day,
            (Some('U'), "") => Tok::Upper,
            (Some('L'), "") => Tok::Lower,
            (Some('n'), "") => Tok::Plain,
            _ => return Err(bad(&format!("[{tag}]"))),
        };
        out.push(tok);
    }
    lit.push_str(rest);
    if !lit.is_empty() {
        out.push(Tok::Lit(lit));
    }
    Ok(out)
}

/// `report.final.pdf` → (`report.final`, `pdf`); каталог и `.gitignore` — без расширения.
fn split(name: &str, dir: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && !dir => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn slice(s: &str, from: usize, to: Option<usize>) -> String {
    let n = s.chars().count();
    let to = to.unwrap_or(n).min(n);
    s.chars()
        .skip(from - 1)
        .take(to.saturating_sub(from - 1))
        .collect()
}

#[derive(Clone, Copy)]
enum Case {
    Plain,
    Upper,
    Lower,
}

fn push(out: &mut String, s: &str, case: Case) {
    match case {
        Case::Plain => out.push_str(s),
        Case::Upper => out.extend(s.chars().flat_map(char::to_uppercase)),
        Case::Lower => out.extend(s.chars().flat_map(char::to_lowercase)),
    }
}

/// Новое имя `target` (номер `index` в наборе — для счётчика).
fn apply(
    toks: &[Tok],
    target: &Target,
    index: usize,
    date: &dyn Fn(i64) -> Option<(u16, u8, u8)>,
) -> String {
    let (stem, ext) = split(&target.name, target.dir);
    let mut out = String::with_capacity(target.name.len() + 8);
    let mut case = Case::Plain;
    for (k, tok) in toks.iter().enumerate() {
        match tok {
            Tok::Lit(s) => push(&mut out, s, case),
            Tok::Name(from, to) => push(&mut out, &slice(stem, *from, *to), case),
            Tok::Ext(from, to) => {
                let part = slice(ext, *from, *to);
                if part.is_empty()
                    && out.ends_with('.')
                    && matches!(toks.get(k.wrapping_sub(1)), Some(Tok::Lit(_)))
                {
                    out.pop();
                }
                push(&mut out, &part, case);
            }
            Tok::Counter { start, step, width } => {
                let n = start + step * i64::try_from(index).unwrap_or(i64::MAX);
                let _ = write!(out, "{n:0width$}");
            }
            Tok::Year | Tok::Month | Tok::Day => {
                if let Some((y, m, d)) = date(target.mtime) {
                    let _ = match tok {
                        Tok::Year => write!(out, "{y:04}"),
                        Tok::Month => write!(out, "{m:02}"),
                        _ => write!(out, "{d:02}"),
                    };
                }
            }
            Tok::Upper => case = Case::Upper,
            Tok::Lower => case = Case::Lower,
            Tok::Plain => case = Case::Plain,
        }
    }
    out
}

/// План по шаблону или regex; `existing` — есть ли имя в каталоге (по листингу).
pub fn plan(
    targets: &[Target],
    spec: &RenameSpec,
    existing: &dyn Fn(&str) -> bool,
    date: &dyn Fn(i64) -> Option<(u16, u8, u8)>,
) -> Result<RenamePlan, PatternError> {
    let mut news: Vec<CompactString> = Vec::with_capacity(targets.len());
    if spec.regex {
        let (find, repl) = spec
            .pattern
            .rsplit_once('/')
            .ok_or_else(|| bad(&urfin_i18n::current().get("ops-mr-regex-form")))?;
        let re = regex::Regex::new(find).map_err(|e| bad(&e.to_string()))?;
        for t in targets {
            news.push(re.replace_all(&t.name, repl).as_ref().into());
        }
    } else {
        let toks = parse(&spec.pattern)?;
        for (i, t) in targets.iter().enumerate() {
            news.push(apply(&toks, t, i, date).into());
        }
    }
    let from_keys: std::collections::HashSet<CompactString> =
        targets.iter().map(|t| name_key(&t.name)).collect();
    let mut counts: std::collections::HashMap<CompactString, usize> =
        std::collections::HashMap::with_capacity(news.len());
    for n in &news {
        *counts.entry(name_key(n)).or_default() += 1;
    }
    let mut out = RenamePlan::default();
    for (t, to) in targets.iter().zip(news) {
        if to == t.name {
            continue;
        }
        let key = name_key(&to);
        let problem = if check_name(&to).is_err() {
            Some(Problem::Invalid)
        } else if counts.get(&key).copied().unwrap_or(0) > 1 {
            Some(Problem::Duplicate)
        } else if key != name_key(&t.name) && !from_keys.contains(&key) && existing(&to) {
            Some(Problem::Exists)
        } else {
            None
        };
        out.problems += usize::from(problem.is_some());
        out.rows.push(Row {
            from: t.name.clone(),
            to,
            problem,
        });
    }
    Ok(out)
}

// --- применение ---

/// Последнее применённое переименование для отката (S5-29): в памяти и, если у
/// handler есть каталог журналов, в файле (переживает перезапуск).
#[derive(Debug, Default)]
pub(crate) struct UndoLog {
    mem: Mutex<Option<RenameLog>>,
    dir: Option<PathBuf>,
}

impl UndoLog {
    pub(crate) fn new(dir: Option<PathBuf>) -> Arc<UndoLog> {
        Arc::new(UndoLog {
            mem: Mutex::default(),
            dir,
        })
    }

    /// Запомнить план (`None` — откатывать больше нечего). Пишет файл: не из UI-потока.
    fn store(&self, log: Option<RenameLog>) {
        if let Some(dir) = &self.dir {
            match &log {
                Some(l) => {
                    if let Err(e) = crate::journal::save_rename(dir, l) {
                        tracing::warn!(target: "ops", dir = %dir.display(), error = %e, "rename log write");
                    }
                }
                None => crate::journal::clear_rename(dir),
            }
        }
        *self.mem.lock().unwrap_or_else(PoisonError::into_inner) = log;
    }

    /// План для отката: из памяти, после перезапуска — из файла. Не из UI-потока.
    pub(crate) fn load(&self) -> Option<RenameLog> {
        let mem = self
            .mem
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        mem.or_else(|| crate::journal::load_rename(self.dir.as_deref()?))
    }
}

/// Job: строки плана без проблем.
#[derive(Debug)]
pub(crate) struct MultiRename {
    base: Location,
    rows: Vec<(CompactString, CompactString)>,
    /// Куда записать переименованное — для отката.
    undo: Option<Arc<UndoLog>>,
    /// Это откат: строки — «стало → было».
    reverting: bool,
}

fn spec() -> JobSpec {
    JobSpec {
        kind: "rename",
        title: urfin_i18n::current().get("ops-job-multi-rename"),
        lane: Lane::Bulk,
        priority: 1,
        owner_ns: "ops",
    }
}

/// Из handler (UI-поток): job применения плана.
pub(crate) fn start(
    eng: &JobEngine,
    cx: &mut Cx<'_>,
    base: Location,
    plan: &RenamePlan,
    undo: Arc<UndoLog>,
) -> JobId {
    let rows: Vec<_> = plan
        .rows
        .iter()
        .filter(|r| r.problem.is_none())
        .map(|r| (r.from.clone(), r.to.clone()))
        .collect();
    let mr = MultiRename {
        base,
        rows,
        undo: Some(undo),
        reverting: false,
    };
    eng.start(cx, spec(), 1, mr)
}

/// `ops.multi_rename.undo` (UI-поток): job отката плана `log` — те же строки наоборот.
pub(crate) fn start_undo(
    eng: &JobEngine,
    cx: &mut Cx<'_>,
    log: &RenameLog,
    undo: Arc<UndoLog>,
) -> JobId {
    eng.start(cx, spec(), 1, MultiRename::revert(log, undo))
}

fn failed(path: &Path, e: &std::io::Error) -> ItemResult {
    blocked(path, e.to_string().into())
}

fn blocked(path: &Path, error: std::sync::Arc<str>) -> ItemResult {
    ItemResult::Failed(FailedItem {
        path: path.to_path_buf(),
        op: ItemOp::Rename,
        error,
        class: IoClass::Other,
        pair: None,
    })
}

/// Порядок применения. Ребро `i → succ`: новое имя `i` — старое имя `succ`,
/// `succ` переименовывается раньше. Новые имена различны (дубли отсеял план),
/// поэтому у строки не больше одного предшественника, и граф распадается на
/// цепочки и циклы.
struct Order {
    pred: Vec<Option<usize>>,
    /// Концы цепочек (новое имя ничьё): с них обход по `pred`.
    tails: Vec<usize>,
    /// По строке на цикл.
    cycles: Vec<usize>,
}

impl Order {
    fn new(rows: &[(CompactString, CompactString)]) -> Order {
        let n = rows.len();
        let from: std::collections::HashMap<CompactString, usize> = rows
            .iter()
            .enumerate()
            .map(|(i, (f, _))| (name_key(f), i))
            .collect();
        let mut succ = vec![None; n];
        let mut pred = vec![None; n];
        for (i, (f, t)) in rows.iter().enumerate() {
            let key = name_key(t);
            // Смена регистра — своё же имя. Второй претендент на имя
            // (дубль мимо плана) — хвост: упадёт на занятом имени.
            if key != name_key(f)
                && let Some(&j) = from.get(&key)
                && pred[j].is_none()
            {
                succ[i] = Some(j);
                pred[j] = Some(i);
            }
        }
        let tails: Vec<usize> = (0..n).filter(|&i| succ[i].is_none()).collect();
        let mut seen = vec![false; n];
        for &t in &tails {
            let mut cur = Some(t);
            while let Some(i) = cur {
                seen[i] = true;
                cur = pred[i];
            }
        }
        let mut cycles = Vec::new();
        for s in 0..n {
            if seen[s] {
                continue;
            }
            cycles.push(s);
            let mut i = s;
            while !seen[i] {
                seen[i] = true;
                i = succ[i].unwrap_or(s);
            }
        }
        Order {
            pred,
            tails,
            cycles,
        }
    }

    /// Строки цикла после `s` в порядке применения (по `pred`).
    fn rest(&self, s: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.pred[s], |&i| self.pred[i]).take_while(move |&i| i != s)
    }
}

impl MultiRename {
    /// Job отката: строки журнала наоборот.
    fn revert(log: &RenameLog, undo: Arc<UndoLog>) -> MultiRename {
        MultiRename {
            base: log.base.clone(),
            rows: log
                .rows
                .iter()
                .map(|(from, to)| (to.as_str().into(), from.as_str().into()))
                .collect(),
            undo: Some(undo),
            reverting: true,
        }
    }

    /// Итог в журнал отката. Применение: переименованные строки (ни одной — прежний
    /// план остаётся). Откат: строки, которые не вернулись, — в исходном направлении;
    /// вернулось всё — журнал пуст.
    fn remember(&self, renamed: &[bool]) {
        let Some(undo) = &self.undo else {
            return;
        };
        let rows: Vec<(String, String)> = self
            .rows
            .iter()
            .zip(renamed)
            .filter(|(_, done)| **done != self.reverting)
            .map(|((f, t), _)| {
                if self.reverting {
                    (t.to_string(), f.to_string())
                } else {
                    (f.to_string(), t.to_string())
                }
            })
            .collect();
        if rows.is_empty() && !self.reverting {
            return;
        }
        tracing::info!(target: "ops", rows = rows.len(), reverting = self.reverting, "multi-rename log");
        undo.store((!rows.is_empty()).then(|| RenameLog {
            v: crate::journal::VERSION,
            base: self.base.clone(),
            rows,
        }));
    }

    fn dir(&self) -> PathBuf {
        crate::names::os_path(&self.base.path)
    }

    fn paths(&self, i: usize) -> (PathBuf, PathBuf) {
        let dir = self.dir();
        let (f, t) = &self.rows[i];
        (dir.join(f.as_str()), dir.join(t.as_str()))
    }

    fn one(from: &Path, to: &Path, case_only: bool) -> std::io::Result<()> {
        crate::sys::rename(from, to, case_only)
    }

    /// Причина для строк цикла, которые не тронуты или откачены.
    fn stopped_at(&self, i: usize) -> std::sync::Arc<str> {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("name", self.rows[i].0.as_str());
        urfin_i18n::current().fmt("ops-mr-cycle-blocked", &a).into()
    }

    /// Цикл: `s` — во временное имя, остальные по `pred` на освободившиеся
    /// места, временное — в новое имя `s`. Сбой или отмена — откат всего цикла:
    /// имена прежние, под временным именем файл не остаётся. Итоги строк
    /// пишутся в конце цикла; `renamed` — строки, оставшиеся с новым именем.
    fn cycle(
        &self,
        p: &mut Plan<'_, Self>,
        order: &Order,
        s: usize,
        tag: &str,
        renamed: &mut [bool],
    ) -> Result<(), PlanError> {
        let (from, to) = self.paths(s);
        let tmp = self.dir().join(format!(".fnv~mr{tag}-{s}"));
        if let Err(err) = Self::one(&from, &tmp, false) {
            p.record(None, failed(&from, &err));
            for i in order.rest(s) {
                p.record(None, blocked(&self.paths(i).0, self.stopped_at(s)));
            }
            return Ok(());
        }
        let mut done = Vec::new();
        // Сломавшая цикл строка (`None` — сама `s`) и ошибка.
        let mut fail = None;
        for i in order.rest(s) {
            if let Err(c) = p.cx().checkpoint() {
                let (back, home) = self.undo(&done, &tmp, &from);
                for (&i, ok) in done.iter().zip(back) {
                    renamed[i] = !ok;
                }
                if let Err(err) = home {
                    tracing::warn!(target: "ops", tmp = %tmp.display(), error = %err, "multi-rename cancel: temp name left");
                }
                return Err(c.into());
            }
            let (src, dst) = self.paths(i);
            match Self::one(&src, &dst, false) {
                Ok(()) => done.push(i),
                Err(err) => {
                    fail = Some((Some(i), err));
                    break;
                }
            }
        }
        let (bad, err) = match fail {
            Some(f) => f,
            None => match Self::one(&tmp, &to, false) {
                Ok(()) => {
                    renamed[s] = true;
                    for &i in &done {
                        renamed[i] = true;
                    }
                    for _ in 0..=done.len() {
                        p.record(None, ItemResult::Done);
                    }
                    return Ok(());
                }
                Err(err) => (None, err),
            },
        };
        let why = self.stopped_at(bad.unwrap_or(s));
        let (back, home) = self.undo(&done, &tmp, &from);
        p.record(
            None,
            match home {
                // Не вернулся на место — показать, где он.
                Err(home_err) => failed(&tmp, &home_err),
                Ok(()) if bad.is_none() => failed(&from, &err),
                Ok(()) => blocked(&from, why.clone()),
            },
        );
        for (&i, ok) in done.iter().zip(back) {
            // Не откатился — остался с новым именем: переименован.
            renamed[i] = !ok;
            p.record(
                None,
                if ok {
                    blocked(&self.paths(i).0, why.clone())
                } else {
                    ItemResult::Done
                },
            );
        }
        for i in order.rest(s).skip(done.len()) {
            let src = self.paths(i).0;
            p.record(
                None,
                if Some(i) == bad {
                    failed(&src, &err)
                } else {
                    blocked(&src, why.clone())
                },
            );
        }
        Ok(())
    }

    /// Все строки: цепочки, затем циклы. `renamed` — что получило новое имя.
    fn apply(&self, p: &mut Plan<'_, Self>, renamed: &mut [bool]) -> Result<(), PlanError> {
        let order = Order::new(&self.rows);
        // Цепочки — с хвоста: цель каждого шага освободил предыдущий.
        for &tail in &order.tails {
            let mut cur = Some(tail);
            while let Some(i) = cur {
                p.cx().checkpoint()?;
                let (from, to) = self.paths(i);
                let (f, t) = &self.rows[i];
                let r = Self::one(&from, &to, name_key(f) == name_key(t));
                renamed[i] = r.is_ok();
                p.record(
                    None,
                    r.map_or_else(|e| failed(&from, &e), |()| ItemResult::Done),
                );
                cur = order.pred[i];
            }
        }
        let tag = format!("{:x}", std::process::id());
        for &s in &order.cycles {
            p.cx().checkpoint()?;
            self.cycle(p, &order, s, &tag, renamed)?;
        }
        Ok(())
    }

    /// Откат цикла: сделанное — назад с конца, временное — на старое место.
    /// По строке `done` — откатилась ли; итог возврата временного.
    fn undo(&self, done: &[usize], tmp: &Path, from: &Path) -> (Vec<bool>, std::io::Result<()>) {
        let mut back = vec![true; done.len()];
        for (k, &i) in done.iter().enumerate().rev() {
            let (f, t) = self.paths(i);
            if let Err(e) = Self::one(&t, &f, false) {
                tracing::warn!(target: "ops", from = %t.display(), error = %e, "multi-rename undo");
                back[k] = false;
            }
        }
        (back, Self::one(tmp, from, false))
    }
}

impl OpExec for MultiRename {
    type Item = ();
    type Dir = ();

    fn plan(&self, p: &mut Plan<'_, Self>) -> Result<(), PlanError> {
        {
            let cx = p.cx();
            let pr = cx.progress();
            pr.items_total
                .store(self.rows.len() as u64, std::sync::atomic::Ordering::Relaxed);
            pr.total_final
                .store(true, std::sync::atomic::Ordering::Relaxed);
            pr.set_phase(urfin_core::Phase::Transfer);
            // И при отмене часть имён уже другая.
            cx.touch(&self.base);
        }
        let mut renamed = vec![false; self.rows.len()];
        let r = self.apply(p, &mut renamed);
        // И при отмене: что переименовано, то и откатывать.
        self.remember(&renamed);
        r
    }

    fn exec(&self, _work: &Work<Self>, _cx: &ExecCx<'_>) -> ItemResult {
        ItemResult::Done
    }

    fn finalize_dir(&self, _dir: &DirNode<()>, _cx: &ExecCx<'_>) -> ItemResult {
        ItemResult::Done
    }

    fn finished(&self, r: &Report, outcome: &Result<JobSummary, String>) -> Option<Note> {
        let s = outcome.as_ref().ok()?;
        let f = r.failed.first()?;
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("n", s.errors);
        a.set("first", format!("{}: {}", f.path.display(), f.error));
        Some((
            Level::Error,
            urfin_i18n::current().fmt("ops-copy-errors", &a),
        ))
    }
}

#[cfg(test)]
#[path = "multi_rename_tests.rs"]
mod tests;
