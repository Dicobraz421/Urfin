//! Предпросмотр рядом (S7 §5.9, карточка S7-24): документ Markdown по тексту редактора.
//!
//! Правка документ не перестраивает: `settle` только отодвигает момент разбора на
//! [`PAUSE`]; разбор — задача `Lane::Interactive` по снимку текста ([`TextSnap`], O(1)),
//! новый отменяет прежний. Задача сравнивает итог с прошлым документом ([`Reuse`]): список
//! UI заменяет только изменившиеся блоки, высоты остальных остаются.
//!
//! Синхронная прокрутка — по смещениям блоков (`MdDoc::top_at`, `MdDoc::off_at`). Ведёт
//! одна сторона ([`Lead`]): та, которую пользователь прокрутил последней. Текст ведёт
//! после ввода, движения, колеса и полосы; документ — после колеса над предпросмотром
//! (`InboxMsg::MdLead`). Ведомая сторона только догоняет и лидерство не перехватывает:
//! эха нет.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use urfin_core::{CancelToken, Cx, Lane, Msg, Runtime};

use super::input::{Act, MdAct, missing_range};
use super::piece::TextSnap;
use super::{EdState, EditorDoc};
use crate::enc::Enc;
use crate::md::job::parse_all;
use crate::md::resolve::{RealFs, Resolver};
use crate::md::{BlockKind, DOC_MAX, MdDoc};
use crate::{DocId, MdState, ViewState};

/// Пауза в правке перед разбором.
pub const PAUSE: Duration = Duration::from_millis(150);

/// Какая сторона ведёт прокрутку.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Lead {
    #[default]
    Text,
    Doc,
}

/// Новый документ относительно прежнего: блоки `..head` и последние `tail` совпали.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Reuse {
    /// Адрес прежнего документа (`Arc::as_ptr`).
    pub from: usize,
    pub head: u32,
    pub tail: u32,
}

/// Предпросмотр документа редактора.
#[derive(Debug, Default)]
pub struct Preview {
    /// Разобранный документ и его позиция; `doc` — последний готовый, пока идёт разбор.
    pub md: MdState,
    pub lead: Lead,
    /// Что совпало с прошлым документом.
    pub reuse: Option<Reuse>,
    /// Правок текста в последнем начатом разборе; `None` — разбора ещё не было.
    seen: Option<u64>,
    /// Правок текста в документе `md.doc`.
    parsed: Option<u64>,
    /// Когда разбирать (пауза в правке).
    due: Option<Instant>,
    /// Верх текста, с которым документ уже совпадает.
    top: u64,
    /// Верх текста ждёт страниц (документ ведёт).
    want_top: Option<u64>,
}

impl Preview {
    /// Разбор не ждёт и не идёт: снимок сцены можно делать.
    pub fn idle(&self) -> bool {
        self.due.is_none() && !self.md.parsing
    }
}

/// Текст документа в байтах снимка; отмена — `None`.
fn read_all(snap: &TextSnap, cancel: &CancelToken) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = vec![0u8; usize::try_from(snap.len()).unwrap_or(0)];
    let n = snap.read_at(0, &mut buf)?;
    buf.truncate(n);
    Ok((!cancel.is_cancelled()).then_some(buf))
}

/// Что нужно задаче разбора.
#[derive(Debug)]
struct Job {
    doc: DocId,
    pass: u64,
    edits: u64,
    snap: TextSnap,
    enc: Enc,
    dir: Option<PathBuf>,
    image_max: u64,
    prev: Option<Arc<MdDoc>>,
}

fn spawn(rt: &Runtime, job: Job, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "edit.preview", cancel, move |cx| {
        let _span = tracing::debug_span!("edit.preview").entered();
        let bytes = match read_all(&job.snap, &cx.cancel) {
            Ok(Some(b)) => b,
            Ok(None) => return,
            Err(e) => {
                let msg = PreviewMsg {
                    doc: job.doc,
                    pass: job.pass,
                    edits: job.edits,
                    res: Err(e.to_string()),
                };
                let _ = cx.sink.send(Box::new(msg), &cx.cancel);
                return;
            }
        };
        let r = Resolver {
            dir: job.dir.clone(),
            image_max: job.image_max,
            fs: &RealFs,
        };
        let Ok(doc) = parse_all(&bytes, job.enc, &r, &cx.cancel) else {
            return;
        };
        let reuse = job.prev.as_ref().map(|p| {
            let (head, tail) = crate::md::same_ends(p, &doc);
            Reuse {
                from: Arc::as_ptr(p) as usize,
                head,
                tail,
            }
        });
        let msg = PreviewMsg {
            doc: job.doc,
            pass: job.pass,
            edits: job.edits,
            res: Ok((Arc::new(doc), reuse)),
        };
        let _ = cx.sink.send(Box::new(msg), &cx.cancel);
    });
}

/// Ответ разбора → предпросмотр документа.
#[derive(Debug)]
struct PreviewMsg {
    doc: DocId,
    pass: u64,
    edits: u64,
    res: Result<(Arc<MdDoc>, Option<Reuse>), String>,
}

impl Msg for PreviewMsg {
    fn name(&self) -> &'static str {
        "edit.preview"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(d) = view.editor_mut(self.doc) {
            d.preview_apply(self.pass, self.edits, self.res);
        }
    }
}

/// Паузы вышли — разбор; пока ждём — будильник окна.
pub(crate) fn tick(cx: &mut Cx<'_>) {
    let rt = cx.rt;
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let image_max = u64::from(view.viewer.md_image_max_mb) << 20;
    let now = Instant::now();
    let mut wait: Option<Duration> = None;
    for d in view.editors_mut() {
        if let Some(left) = d.preview_tick(rt, image_max, now) {
            wait = Some(wait.map_or(left, |w| w.min(left)));
        }
    }
    if let Some(w) = wait {
        cx.rt.waker().wake_after(w);
    }
}

/// `<details>` документа по порядку.
fn details(doc: &MdDoc) -> Vec<u64> {
    doc.blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Details { .. }))
        .map(|b| b.src.start)
        .collect()
}

impl EditorDoc {
    /// Файл влезает в предпросмотр.
    pub fn preview_fits(&self) -> bool {
        self.engine.as_deref().is_none_or(|e| e.len() <= DOC_MAX)
    }

    fn preview_wanted(&self) -> bool {
        self.preview_on
            && self.lang == Some("markdown")
            && self.preview_fits()
            && matches!(
                self.state,
                EdState::Ready | EdState::Saving(_) | EdState::Replacing
            )
    }

    /// Новый текст с самого начала (перечитывание, смена кодировки).
    pub(crate) fn preview_drop(&mut self) {
        if let Some(p) = self.preview.take() {
            p.md.stop();
        }
    }

    /// Разобранный документ — по текущему тексту (иначе смещения блоков устарели).
    pub(crate) fn preview_fresh(&self) -> bool {
        let edits = self.engine.as_deref().map(|e| e.text.edits());
        self.preview.as_deref().is_some_and(|p| p.parsed == edits)
    }

    /// Правка, движение, ввод: ведёт текст (флажок в предпросмотре — не прокрутка).
    pub(crate) fn preview_text_leads(&mut self, a: &Act) {
        if !matches!(a, Act::Md(MdAct::Check(_))) {
            self.preview_lead(Lead::Text);
        }
    }

    pub(crate) fn preview_lead(&mut self, lead: Lead) {
        if let Some(p) = self.preview.as_deref_mut() {
            p.lead = lead;
            p.md.sel = None;
            if lead == Lead::Text {
                p.want_top = None;
            }
        }
    }

    /// После действия над документом (`settle`): включить или снять предпросмотр,
    /// отодвинуть разбор, догнать верх текста.
    pub(crate) fn preview_sync(&mut self, rt: &Runtime) {
        if !self.preview_wanted() {
            if self.preview.is_some() {
                self.preview_drop();
                self.rev += 1;
            }
            return;
        }
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        let p = self.preview.get_or_insert_with(Box::default);
        let edits = e.text.edits();
        let first = p.seen.is_none();
        if p.seen != Some(edits) {
            p.due = Some(Instant::now() + if first { Duration::ZERO } else { PAUSE });
            p.seen = Some(edits);
            rt.waker().wake();
        }
        if let Some(off) = p.want_top {
            match e.scroll_to(off) {
                Ok(_) => p.want_top = None,
                Err(m) => {
                    e.need = Some(missing_range(&m));
                }
            }
            p.top = e.top;
        }
        if p.lead == Lead::Text && p.top != e.top {
            p.top = e.top;
            Self::top_from_text(p);
        }
    }

    /// Верхний блок — по верху текста.
    fn top_from_text(p: &mut Preview) {
        let top = p.top;
        let md = &mut p.md;
        let Some(doc) = md.doc.clone() else {
            return;
        };
        let (mut b, frac) = doc.top_at(top);
        let mut f = frac;
        // Внутри свёрнутого `<details>` — сам он.
        while !md.visible(&doc, b) {
            let Some(up) = doc.blocks[b as usize].parent else {
                break;
            };
            b = up;
            f = 0.0;
        }
        md.top = b;
        md.top_frac = f;
    }

    fn preview_tick(&mut self, rt: &Runtime, image_max: u64, now: Instant) -> Option<Duration> {
        let due = self.preview.as_deref()?.due?;
        if now < due {
            return Some(due - now);
        }
        self.preview_parse(rt, image_max);
        None
    }

    /// Разбор снимка: прежний отменяется.
    fn preview_parse(&mut self, rt: &Runtime, image_max: u64) {
        let loc = self.shown_loc();
        let dir = loc
            .is_local()
            .then(|| loc.path.parent().map(std::path::Path::to_path_buf))
            .flatten();
        let cancel = self.cancel.child();
        let (Some(e), Some(p)) = (self.engine.as_deref(), self.preview.as_deref_mut()) else {
            return;
        };
        p.due = None;
        let edits = e.text.edits();
        p.seen = Some(edits);
        let pass = p.md.begin(cancel.clone(), e.enc);
        let job = Job {
            doc: self.id,
            pass,
            edits,
            snap: e.text.snap(),
            enc: e.enc,
            dir,
            image_max,
            prev: p.md.doc.clone(),
        };
        spawn(rt, job, cancel);
    }

    /// Ответ разбора текущего поколения.
    fn preview_apply(
        &mut self,
        pass: u64,
        edits: u64,
        res: Result<(Arc<MdDoc>, Option<Reuse>), String>,
    ) {
        let Some(p) = self.preview.as_deref_mut() else {
            return;
        };
        if pass != p.md.pass() {
            return;
        }
        p.md.parsing = false;
        match res {
            Ok((doc, reuse)) => {
                // Раскрытые `<details>` — по порядку: смещения сдвинулись правкой выше.
                if let Some(old) = p.md.doc.as_deref()
                    && !p.md.toggled.is_empty()
                {
                    let (a, b) = (details(old), details(&doc));
                    if a.len() == b.len() {
                        let map = |k: &u64| a.binary_search(k).ok().map(|i| b[i]);
                        p.md.toggled = p.md.toggled.iter().filter_map(map).collect();
                    }
                }
                p.md.doc = Some(doc);
                p.md.failed = None;
                p.md.sel = None;
                p.reuse = reuse;
                p.parsed = Some(edits);
            }
            Err(e) => p.md.failed = Some(e),
        }
        Self::top_from_text(p);
        self.rev += 1;
    }

    /// `InboxMsg::MdTop`: список прокручен. Ведёт документ — текст идёт за ним.
    pub(crate) fn preview_set_top(&mut self, block: u32, frac: f32, rt: &Runtime) -> bool {
        let Some(p) = self.preview.as_deref_mut() else {
            return false;
        };
        let Some(doc) = p.md.doc.clone() else {
            return false;
        };
        if doc.blocks.get(block as usize).is_none() {
            return false;
        }
        let frac = frac.clamp(0.0, 1.0);
        let same = p.md.top == block && (p.md.top_frac - frac).abs() < f32::EPSILON;
        p.md.top = block;
        p.md.top_frac = frac;
        if p.lead == Lead::Doc && !same {
            p.want_top = Some(doc.off_at(block, frac));
            self.settle(rt);
        }
        self.rev += 1;
        !same
    }

    /// `InboxMsg::MdToggle`: клик по `<details>`.
    pub(crate) fn preview_toggle(&mut self, block: u32) -> bool {
        let Some(p) = self.preview.as_deref_mut() else {
            return false;
        };
        let changed = p.md.toggle(block);
        if changed {
            self.rev += 1;
        }
        changed
    }

    /// `InboxMsg::MdSelect`: выделение в предпросмотре.
    pub(crate) fn preview_select(&mut self, sel: Option<std::ops::Range<u32>>) -> bool {
        let Some(p) = self.preview.as_deref_mut() else {
            return false;
        };
        if p.md.sel == sel {
            return false;
        }
        p.md.sel = sel;
        self.rev += 1;
        true
    }
}
