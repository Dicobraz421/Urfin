//! Follow (tail -f) — `viewer.follow` (Shift+F), S7 §3.8. Карточка S7-12.
//!
//! Источник — всегда `File` с `FILE_SHARE_READ | WRITE | DELETE`: Owned и Mapped при
//! включении заменяются (задача `view.follow.open` на Interactive lane), у `File` рост
//! включается на месте (`ByteSource::set_follow`). Поток `view.follow.<doc>`
//! (`spawn_dedicated`) раз в `follow_interval_ms` делает `refresh_len()` по своему handle,
//! без watcher S4:
//! - прирост → [`FollowEvent::Grew`]: индексатор дочитывает сам (источник не финальный),
//!   `top` прилипает к концу, если прилипание не снято прокруткой вверх;
//! - длина меньше → [`FollowEvent::Truncated`]: страницы и индекс заново, позиция — в
//!   конец, тост;
//! - по пути другой файл (`file_id`), а свой не растёт → ротация: при
//!   `viewer.follow_reopen` поток открывает файл по пути и шлёт [`FollowEvent::Rotated`]
//!   с новым источником, дальше следит за ним.
//!
//! Простой — 4 `stat` и 4 открытия по пути в секунду, без чтения данных. Данные для чипа
//! «Слежение» в шапке — [`ViewerDoc::follow`] и [`ViewerDoc::follow_stick`]; рисует UI.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use urfin_core::{CancelToken, Cx, Lane, Level, Location, Msg, Runtime};
use urfin_vfs::{VfsError, VfsRegistry};

use super::{Scroll, ViewerDoc};
use crate::cache::{PAGE, Page};
use crate::index::{IndexMode, Indexer, Sep, spawn_index};
use crate::source::{ByteSource, FileSource, SourceKind, SourceOpts, open_source, read_full};
use crate::{DocId, ViewState};

/// Шаг опроса не короче и не длиннее (конфиг — `follow_interval_ms`).
const INTERVAL_MIN: Duration = Duration::from_millis(50);
const INTERVAL_MAX: Duration = Duration::from_secs(5);

/// Что увидел поток опроса.
#[derive(Debug)]
pub enum FollowEvent {
    /// Файл вырос до этой длины; хвост (не больше двух страниц) уже прочитан потоком —
    /// новые строки видны без лишнего круга loader'а.
    Grew(u64, Vec<(u64, Page)>),
    /// Файл стал короче (усечён писателем): новая длина.
    Truncated(u64),
    /// Файл по пути заменён (ротация); новый источник уже открыт.
    Rotated(Arc<dyn ByteSource>),
}

/// Почему follow не включился.
#[derive(Debug, PartialEq, Eq)]
pub enum FollowError {
    /// Не локальный файл (архив, SFTP): follow — только по handle локального файла.
    NotLocal,
}

/// Параметры follow из `[viewer]`.
#[derive(Copy, Clone, Debug)]
pub struct FollowCfg {
    pub interval: Duration,
    /// `viewer.follow_reopen`: после ротации открыть файл по пути заново.
    pub reopen: bool,
}

impl FollowCfg {
    pub fn new(interval_ms: u32, reopen: bool) -> FollowCfg {
        FollowCfg {
            interval: Duration::from_millis(u64::from(interval_ms))
                .clamp(INTERVAL_MIN, INTERVAL_MAX),
            reopen,
        }
    }
}

/// Состояние опроса: длина и id файла, за которым следим. Логика шага — без потока
/// (тесты).
#[derive(Debug)]
pub(crate) struct Watch {
    pub len: u64,
    pub id: Option<(u64, u128)>,
    /// Ротация замечена, а `follow_reopen` выключен: по пути больше не смотрим.
    pub ignore_path: bool,
}

/// Итог шага.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Same,
    Grew(u64),
    Truncated(u64),
    /// По пути другой файл; свой при этом не вырос.
    Rotated,
}

impl Watch {
    /// Шаг: `len` — длина по своему handle, `path_id` — id файла по пути (`None` — файла
    /// по пути нет: удалён, ещё не создан заново). Рост важнее ротации: писатель ещё
    /// дописывает старый файл.
    pub fn step(&mut self, len: u64, path_id: impl FnOnce() -> Option<(u64, u128)>) -> Step {
        if len > self.len {
            self.len = len;
            return Step::Grew(len);
        }
        if len < self.len {
            self.len = len;
            return Step::Truncated(len);
        }
        if self.ignore_path || self.id.is_none() {
            return Step::Same;
        }
        match path_id() {
            Some(id) if Some(id) != self.id => Step::Rotated,
            _ => Step::Same,
        }
    }
}

/// Id файла по пути (открытие на чтение, совместный доступ — как у std: все режимы).
fn path_id(path: &Path) -> Option<(u64, u128)> {
    let f = File::open(path).ok()?;
    crate::sys::file_id(&f).ok()
}

/// Страницы хвоста после роста `old → len`: от страницы прежнего конца, но не больше двух
/// последних.
fn tail_pages(src: &dyn ByteSource, old: u64, len: u64) -> Vec<(u64, Page)> {
    if len == 0 {
        return Vec::new();
    }
    let last = (len - 1) / PAGE;
    let first = (old / PAGE).max(last.saturating_sub(1)).min(last);
    let mut out = Vec::with_capacity(2);
    for no in first..=last {
        let off = no * PAGE;
        let mut buf = vec![0u8; (len - off).min(PAGE) as usize];
        match read_full(src, off, &mut buf) {
            Ok(n) => {
                buf.truncate(n);
                out.push((no, Page::Owned(buf.into())));
            }
            Err(e) => {
                tracing::debug!(error = %e, "follow: tail read failed");
                break;
            }
        }
    }
    out
}

/// Поток опроса документа `doc` поколения `gen_`.
fn spawn_follow(
    rt: &Runtime,
    doc: DocId,
    gen_: u32,
    src: Arc<dyn ByteSource>,
    path: PathBuf,
    cfg: FollowCfg,
    cancel: CancelToken,
) {
    rt.spawn_dedicated(format!("view.follow.{}", doc.0), cancel, move |cx| {
        let _span = tracing::debug_span!("view.follow", doc = doc.0).entered();
        let mut src = src;
        let mut w = Watch {
            len: src.len(),
            id: src.file_id(),
            ignore_path: false,
        };
        let send = |ev: FollowEvent| {
            let msg = FollowMsg { doc, gen_, ev };
            let _ = cx.sink.send(Box::new(msg), &cx.cancel);
        };
        loop {
            std::thread::sleep(cfg.interval);
            if cx.cancel.is_cancelled() {
                return;
            }
            let len = match src.refresh_len() {
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!(error = %e, "follow: stat failed");
                    continue;
                }
            };
            let old = w.len;
            match w.step(len, || path_id(&path)) {
                Step::Same => {}
                Step::Grew(n) => send(FollowEvent::Grew(n, tail_pages(&*src, old, n))),
                Step::Truncated(n) => send(FollowEvent::Truncated(n)),
                Step::Rotated if !cfg.reopen => w.ignore_path = true,
                Step::Rotated => match File::open(&path) {
                    Ok(f) => {
                        let next: Arc<dyn ByteSource> =
                            Arc::new(FileSource::new(f, path.clone(), true));
                        w = Watch {
                            len: next.len(),
                            id: next.file_id(),
                            ignore_path: false,
                        };
                        src = next.clone();
                        send(FollowEvent::Rotated(next));
                    }
                    Err(e) => tracing::debug!(error = %e, "follow: reopen failed"),
                },
            }
        }
    });
}

/// Событие потока → документ.
struct FollowMsg {
    doc: DocId,
    gen_: u32,
    ev: FollowEvent,
}

impl std::fmt::Debug for FollowMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FollowMsg")
            .field("doc", &self.doc)
            .field("gen", &self.gen_)
            .finish_non_exhaustive()
    }
}

impl Msg for FollowMsg {
    fn name(&self) -> &'static str {
        "view.follow"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(d) = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.viewer_mut(self.doc))
        else {
            return;
        };
        if let Some(key) = d.on_follow(self.gen_, self.ev, rt) {
            cx.toast(Level::Info, urfin_i18n::current().get(key));
        }
    }
}

/// Источник `File` с ростом открыт (документ был Owned или Mapped).
struct FollowOpenedMsg {
    doc: DocId,
    gen_: u32,
    res: Result<Arc<dyn ByteSource>, String>,
}

impl std::fmt::Debug for FollowOpenedMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FollowOpenedMsg")
            .field("doc", &self.doc)
            .field("ok", &self.res.is_ok())
            .finish_non_exhaustive()
    }
}

impl Msg for FollowOpenedMsg {
    fn name(&self) -> &'static str {
        "view.follow.opened"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let cfg = FollowCfg::new(view.viewer.follow_interval_ms, view.viewer.follow_reopen);
        let Some(d) = view.viewer_mut(self.doc) else {
            return;
        };
        if !d.follow || self.gen_ != d.follow_gen {
            return;
        }
        match self.res {
            Ok(src) => d.follow_attach(src, cfg, rt),
            Err(e) => {
                tracing::warn!(error = %e, "follow: open failed");
                d.follow_stop();
                let mut args = urfin_i18n::FluentArgs::new();
                args.set("id", "viewer.follow");
                args.set("error", e);
                let text = urfin_i18n::current().fmt("core-command-failed", &args);
                cx.toast(Level::Error, text);
            }
        }
    }
}

/// Задача `view.follow.open`: тот же файл источником `File` с ростом.
fn spawn_reopen(
    rt: &Runtime,
    doc: DocId,
    gen_: u32,
    reg: Arc<VfsRegistry>,
    loc: Location,
    cancel: CancelToken,
) {
    rt.spawn(Lane::Interactive, "view.follow.open", cancel, move |cx| {
        let opts = SourceOpts {
            follow: true,
            ..SourceOpts::default()
        };
        let res = match open_source(&reg, &loc, &opts, &cx.cancel) {
            Err(VfsError::Cancelled) => return,
            Err(e) => Err(e.to_string()),
            Ok(o) => Ok(o.src),
        };
        let msg = FollowOpenedMsg { doc, gen_, res };
        let _ = cx.sink.send(Box::new(msg), &cx.cancel);
    });
}

impl ViewerDoc {
    /// `viewer.follow` (Shift+F): включает или выключает слежение. Включение — в конец и
    /// прилипание к концу; документ Markdown переходит в Текст (документ ждёт конца
    /// файла). `Ok(true)` — состояние изменилось.
    pub(crate) fn toggle_follow(
        &mut self,
        cfg: FollowCfg,
        reg: &Arc<VfsRegistry>,
        rt: &Runtime,
    ) -> Result<bool, FollowError> {
        if self.follow {
            self.follow_stop();
            return Ok(true);
        }
        if !self.is_ready() {
            return Ok(false);
        }
        if !self.loc.is_local() {
            return Err(FollowError::NotLocal);
        }
        self.leave_doc(rt);
        self.follow = true;
        self.follow_stick = true;
        self.follow_gen = self.follow_gen.wrapping_add(1);
        self.follow_cancel.cancel();
        self.follow_cancel = self.cancel.child();
        let src = self
            .src
            .clone()
            .filter(|s| s.kind() == SourceKind::File && s.set_follow(true));
        if let Some(src) = src {
            self.sync_len();
            self.resume_index(rt);
            self.start_watch(src, cfg, rt);
        } else {
            let cancel = self.follow_cancel.clone();
            spawn_reopen(
                rt,
                self.id,
                self.follow_gen,
                reg.clone(),
                self.loc.clone(),
                cancel,
            );
        }
        self.tail(rt);
        self.rev += 1;
        Ok(true)
    }

    /// Слежение выключено: поток останавливается, источник снова финальный — индексатор
    /// дочитывает и завершается.
    pub(crate) fn follow_stop(&mut self) {
        self.follow = false;
        self.follow_stick = false;
        self.follow_cancel.cancel();
        if let Some(src) = &self.src {
            src.set_follow(false);
        }
        self.sync_len();
        self.rev += 1;
    }

    /// Источник `File` с ростом вместо Owned / Mapped (тот же файл): страницы заново,
    /// индекс — с места, если файл не стал короче.
    fn follow_attach(&mut self, src: Arc<dyn ByteSource>, cfg: FollowCfg, rt: &Runtime) {
        let keep_index = self.index.done && src.len() >= self.index.upto;
        self.swap_source(src.clone());
        if keep_index {
            self.resume_index(rt);
        } else {
            self.reset_index(rt);
        }
        self.start_watch(src, cfg, rt);
        self.tail(rt);
        self.rev += 1;
    }

    /// Новый источник документа: кэш страниц над ним (ответы прежнего loader'а не
    /// кладутся).
    fn swap_source(&mut self, src: Arc<dyn ByteSource>) {
        self.len = src.len();
        self.len_final = src.is_final();
        self.kind = Some(src.kind());
        self.pages.replace_source(src.clone());
        self.src = Some(src);
    }

    fn start_watch(&mut self, src: Arc<dyn ByteSource>, cfg: FollowCfg, rt: &Runtime) {
        spawn_follow(
            rt,
            self.id,
            self.follow_gen,
            src,
            self.loc.path.clone(),
            cfg,
            self.follow_cancel.clone(),
        );
    }

    /// Индексатор закончил (файл был финальным), а источник снова растёт: новый поток с
    /// того места, докуда дошёл индекс. Идущий индексатор дочитывает прирост сам.
    fn resume_index(&mut self, rt: &Runtime) {
        if !self.index_on || !self.index.done {
            return;
        }
        let Some(src) = self.src.clone() else {
            return;
        };
        self.index_cancel.cancel();
        self.index_cancel = self.cancel.child();
        self.index_epoch += 1;
        self.index.done = false;
        let ix = Indexer::resume(Sep::of(&self.enc, self.eol), &self.index);
        let mode = IndexMode::of(src.kind(), None);
        spawn_index(
            rt,
            self.id,
            self.index_epoch,
            src,
            ix,
            mode,
            self.index_cancel.clone(),
        );
    }

    /// Последняя строка — внизу экрана (документ Markdown — последний блок).
    fn tail(&mut self, rt: &Runtime) {
        if self.md_mode() {
            self.md_scroll(Scroll::End);
            return;
        }
        self.reveal = None;
        self.pending = Some(Scroll::End);
        self.settle(rt);
    }

    /// Прокрутка пользователя при слежении: вверх — прилипание снято, End — снова.
    pub(crate) fn follow_scrolled(&mut self, s: Scroll) {
        if !self.follow {
            return;
        }
        match s {
            Scroll::Up(_) | Scroll::PageUp(_) | Scroll::Home => self.follow_stick = false,
            Scroll::End => self.follow_stick = true,
            _ => {}
        }
    }

    /// Событие потока опроса. Возвращает ключ тоста (усечение, ротация; не в быстром
    /// просмотре).
    pub(crate) fn on_follow(
        &mut self,
        gen_: u32,
        ev: FollowEvent,
        rt: &Runtime,
    ) -> Option<&'static str> {
        if !self.follow || gen_ != self.follow_gen {
            return None;
        }
        let toast = match ev {
            FollowEvent::Grew(_, pages) => {
                self.sync_len();
                // Страница прежнего конца (неполная) снята `sync_len`; хвост — свежий.
                let len = self.len;
                self.pages.put(
                    pages
                        .into_iter()
                        .filter(|(no, _)| no * PAGE < len)
                        .collect(),
                );
                if self.follow_stick {
                    self.tail(rt);
                } else {
                    self.settle(rt);
                }
                None
            }
            FollowEvent::Truncated(_) => {
                // Писатель мог усечь и записать заново: прежние страницы не годятся.
                if let Some(src) = self.src.clone() {
                    self.swap_source(src);
                }
                self.after_replace(rt);
                Some("ui-view-truncated")
            }
            FollowEvent::Rotated(src) => {
                self.swap_source(src);
                self.after_replace(rt);
                Some("ui-view-rotated")
            }
        };
        self.rev += 1;
        toast.filter(|_| !self.quick)
    }

    /// Содержимое заменено (усечение, ротация): выделение, совпадения и индекс — заново,
    /// позиция — в конец, прилипание включено.
    fn after_replace(&mut self, rt: &Runtime) {
        self.sel = None;
        self.goto_wait = None;
        if let Some(s) = &mut self.search {
            s.cancel();
            s.hit = None;
            s.marks = Arc::from(Vec::new());
            s.count = crate::search::CountState::default();
        }
        self.reset_index(rt);
        self.top = 0;
        self.top_sub = 0;
        self.realign = false;
        self.follow_stick = true;
        self.tail(rt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_grows_truncates_and_rotates() {
        let id = Some((1, 7));
        let mut w = Watch {
            len: 10,
            id,
            ignore_path: false,
        };
        assert_eq!(w.step(10, || id), Step::Same);
        assert_eq!(w.step(15, || Some((1, 8))), Step::Grew(15), "рост важнее");
        assert_eq!(w.step(4, || id), Step::Truncated(4));
        assert_eq!(w.step(4, || None), Step::Same, "файла по пути нет");
        assert_eq!(w.step(4, || Some((1, 8))), Step::Rotated);
        w.ignore_path = true;
        assert_eq!(w.step(4, || Some((1, 8))), Step::Same);
    }

    #[test]
    fn interval_is_clamped() {
        assert_eq!(FollowCfg::new(0, true).interval, INTERVAL_MIN);
        assert_eq!(
            FollowCfg::new(250, true).interval,
            Duration::from_millis(250)
        );
        assert_eq!(FollowCfg::new(u32::MAX, true).interval, INTERVAL_MAX);
    }
}
