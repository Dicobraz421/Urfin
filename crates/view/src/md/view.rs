//! Режим документа у `ViewerDoc` (S7 §3.10): «Разметка» у Markdown при
//! `viewer.markdown = "document"` и файле ≤ 16 МБ. Модуль-потомок `viewer` (доступ к полям
//! документа), файл — в `md/`.
//!
//! Позиция — одна: `top` (байт исходника) = начало верхнего блока; `MdState::top` — его
//! индекс, `top_frac` — доля его высоты (от UI). Документ → Текст: Текст со строки начала
//! блока; Текст → документ: блок, содержащий `top`. Alt+F8 и Hex-поиск переключают в
//! Текст на той же позиции. F7 — поиск S7-09 по проекции ([`super::ProjSource`]).

use std::ops::Range;
use std::sync::Arc;

use rustc_hash::FxHashSet;
use urfin_core::{CancelToken, Runtime};

use super::ViewerDoc;
use crate::enc::Enc;
use crate::layout::Mode;
use crate::md::job::{MdJob, spawn_parse};
use crate::md::{BlockKind, DOC_MAX, MdDoc, ProjSource};
use crate::source::ByteSource;
use crate::viewer::Scroll;

/// Состояние документа Markdown у вьювера.
#[derive(Debug, Default)]
pub struct MdState {
    /// Последний разбор (`complete == false` — пока только начало большого файла).
    pub doc: Option<Arc<MdDoc>>,
    /// Идёт разбор (первый или после смены кодировки).
    pub parsing: bool,
    /// Ошибка чтения при разборе.
    pub failed: Option<String>,
    /// Верхний блок и прокрученная доля его высоты (0…1).
    pub top: u32,
    pub top_frac: f32,
    /// `<details>`, чьё состояние переключено относительно атрибута `open` (ключ —
    /// начало блока в исходнике: переживает дозапись документа).
    pub toggled: FxHashSet<u64>,
    /// Текущее совпадение F7 в проекции.
    pub hit: Option<Range<u32>>,
    /// Выделение мышью по проекции (Ctrl+C — видимый текст без разметки).
    pub sel: Option<Range<u32>>,
    /// Блок, который UI выводит на треть экрана (совпадение), и сбрасывает.
    pub reveal: Option<u32>,
    pass: u64,
    cancel: CancelToken,
    /// Кодировка разобранного текста.
    enc: Option<Enc>,
}

impl MdState {
    /// `<details>` раскрыт (с учётом клика).
    pub fn is_open(&self, doc: &MdDoc, b: u32) -> bool {
        let Some(blk) = doc.blocks.get(b as usize) else {
            return false;
        };
        match blk.kind {
            BlockKind::Details { open } => open != self.toggled.contains(&blk.src.start),
            _ => true,
        }
    }

    /// Блок виден: ни один предок-`<details>` не свёрнут (подпись `Summary` видна всегда).
    pub fn visible(&self, doc: &MdDoc, b: u32) -> bool {
        let mut child = b;
        for p in doc.ancestors(b) {
            if !self.is_open(doc, p) && doc.summary_of(p) != Some(child) {
                return false;
            }
            child = p;
        }
        true
    }

    /// Проекция как источник для поиска.
    pub fn proj_source(&self) -> Option<Arc<dyn ByteSource>> {
        let d = self.doc.as_ref()?;
        Some(Arc::new(ProjSource(d.clone())))
    }
}

/// Примерная высота блока в строках (PgUp/PgDn в документе; точная — у UI).
fn est_rows(doc: &MdDoc, b: u32, cols: u32) -> u32 {
    let blk = &doc.blocks[b as usize];
    let t = doc.block_text(b);
    let cols = cols.max(20) as usize;
    let rows = match blk.kind {
        BlockKind::Code { .. } => t.lines().count() + 1,
        BlockKind::Rule | BlockKind::Row { .. } => 1,
        BlockKind::Image(_) => 8,
        BlockKind::Heading(_) => 2,
        _ if t.is_empty() => 0,
        _ => t.lines().map(|l| l.chars().count() / cols + 1).sum(),
    };
    u32::try_from(rows).unwrap_or(u32::MAX)
}

impl ViewerDoc {
    /// Режим документа: «Разметка» у Markdown, `viewer.markdown = "document"`, ≤ 16 МБ.
    pub fn md_mode(&self) -> bool {
        self.md_document
            && self.mode == Mode::Markup
            && self.lang == Some("markdown")
            && self.is_ready()
            && self.len <= DOC_MAX
            // В области панели (быстрый просмотр) — до 4 МиБ, больше — Текст (S7 §4).
            && (self.placement == crate::Placement::Window || self.len <= super::QUICK_DOC_MAX)
    }

    /// Документ, если он разобран и режим — документ.
    pub fn md_doc(&self) -> Option<&Arc<MdDoc>> {
        if !self.md_mode() {
            return None;
        }
        self.md.as_ref()?.doc.as_ref()
    }

    /// Режим документа включён: разбор (если нет или кодировка другая) и блок по `top`.
    pub(crate) fn md_sync(&mut self, rt: &Runtime) {
        if !self.md_mode() || !self.len_final {
            return;
        }
        let enc = self.enc;
        let md = self.md.get_or_insert_with(MdState::default);
        if md.enc != Some(enc) {
            self.md_start(rt);
        }
        self.md_from_top();
    }

    fn md_start(&mut self, rt: &Runtime) {
        let Some(src) = self.src.clone() else {
            return;
        };
        let dir = self
            .loc
            .is_local()
            .then(|| self.loc.path.parent().map(std::path::Path::to_path_buf))
            .flatten();
        let image_max = self.md_image_max;
        let enc = self.enc;
        let cancel = self.cancel.child();
        let md = self.md.get_or_insert_with(MdState::default);
        md.cancel.cancel();
        md.cancel = cancel.clone();
        md.pass += 1;
        md.parsing = true;
        md.failed = None;
        md.enc = Some(enc);
        let job = MdJob {
            doc: self.id,
            pass: md.pass,
            src,
            enc,
            dir,
            image_max,
        };
        spawn_parse(rt, job, cancel);
        self.rev += 1;
    }

    /// Ответ разбора текущего поколения.
    pub(crate) fn md_apply(&mut self, pass: u64, res: Result<Arc<MdDoc>, String>, rt: &Runtime) {
        let Some(md) = self.md.as_mut() else {
            return;
        };
        if pass != md.pass {
            return;
        }
        match res {
            Ok(doc) => {
                md.parsing = !doc.complete;
                md.doc = Some(doc);
            }
            Err(e) => {
                md.parsing = false;
                md.failed = Some(e);
            }
        }
        self.md_from_top();
        self.md_goto_anchor();
        let complete = self
            .md
            .as_ref()
            .is_some_and(|m| m.doc.as_ref().is_some_and(|d| d.complete));
        if complete && self.md_mode() && std::mem::take(&mut self.search_queued) {
            self.run_search(rt);
        }
        self.rev += 1;
    }

    /// Верхний блок — по `top` (Текст → документ, переход, «назад», новый разбор).
    pub(crate) fn md_from_top(&mut self) {
        if !self.md_mode() {
            return;
        }
        let top = self.top;
        let Some(md) = self.md.as_mut() else {
            return;
        };
        let Some(doc) = &md.doc else {
            return;
        };
        // Верхний блок уже начинается с `top` (прокрутка документа) — его не трогаем.
        if doc
            .blocks
            .get(md.top as usize)
            .is_some_and(|b| b.src.start == top)
        {
            if let Some(f) = self.md_frac0.take() {
                md.top_frac = f;
            }
            return;
        }
        let b = doc.block_at_src(top).unwrap_or(0);
        if b != md.top {
            md.top = b;
            md.top_frac = 0.0;
        }
        if let Some(f) = self.md_frac0.take() {
            md.top_frac = f;
        }
    }

    /// Якорь из ссылки другого `.md`: верхний блок — его заголовок, как только он есть в
    /// разобранном (нет и в полном документе — остаётся позиция).
    fn md_goto_anchor(&mut self) {
        let Some(a) = self.md_anchor.as_deref() else {
            return;
        };
        let Some(md) = self.md.as_mut() else {
            return;
        };
        let Some(doc) = md.doc.clone() else {
            return;
        };
        match doc.anchor(a) {
            Some(b) => {
                md.top = b;
                md.top_frac = 0.0;
                md.reveal = None;
                self.md_frac0 = None;
                self.top = doc.blocks[b as usize].src.start;
                self.md_anchor = None;
            }
            None if doc.complete => self.md_anchor = None,
            None => {}
        }
    }

    /// Ссылка `файл.md#якорь` на этот же файл: прежняя позиция — в «назад», верхний блок —
    /// заголовок якоря (сразу или после разбора).
    pub(crate) fn md_jump_anchor(&mut self, a: String) {
        self.push_back();
        self.set_anchor(Some(a));
        self.md_goto_anchor();
        self.rev += 1;
    }

    /// Документ → Текст: `top` — начало верхнего блока, затем к началу строки.
    pub(crate) fn md_leave(&mut self) {
        if let Some(md) = &self.md
            && let Some(doc) = &md.doc
            && let Some(b) = doc.blocks.get(md.top as usize)
        {
            self.top = b.src.start.min(self.len.saturating_sub(1));
        }
        self.realign_top();
    }

    /// Alt+F8, Hex-поиск: из документа — в Текст на той же позиции. `true` — переключили.
    pub fn leave_doc(&mut self, rt: &Runtime) -> bool {
        if !self.md_mode() {
            return false;
        }
        self.set_mode(Mode::Text, rt)
    }

    /// Смена кодировки: документ разбирается заново (в режиме документа — сразу).
    pub(crate) fn md_reset(&mut self, rt: &Runtime) {
        if self.md_mode() {
            self.md_start(rt);
        } else if let Some(md) = &mut self.md {
            md.cancel.cancel();
            self.md = None;
        }
    }

    /// `InboxMsg::MdTop`: UI прокрутил список блоков. `false` — то же место.
    pub fn md_set_top(&mut self, block: u32, frac: f32) -> bool {
        let Some(md) = self.md.as_mut() else {
            return false;
        };
        let Some(doc) = &md.doc else {
            return false;
        };
        let Some(b) = doc.blocks.get(block as usize) else {
            return false;
        };
        let frac = frac.clamp(0.0, 1.0);
        if md.top == block && (md.top_frac - frac).abs() < f32::EPSILON {
            return false;
        }
        md.top = block;
        md.top_frac = frac;
        self.top = b.src.start;
        self.rev += 1;
        true
    }

    /// `InboxMsg::MdToggle`: клик по `<details>`. `false` — не `Details`.
    pub fn md_toggle(&mut self, block: u32) -> bool {
        let Some(md) = self.md.as_mut() else {
            return false;
        };
        let Some(doc) = &md.doc else {
            return false;
        };
        let Some(b) = doc.blocks.get(block as usize) else {
            return false;
        };
        if !matches!(b.kind, BlockKind::Details { .. }) {
            return false;
        }
        let key = b.src.start;
        if !md.toggled.remove(&key) {
            md.toggled.insert(key);
        }
        self.rev += 1;
        true
    }

    /// `InboxMsg::MdSelect`: выделение по проекции. `false` — то же.
    pub fn md_select(&mut self, sel: Option<Range<u32>>) -> bool {
        let Some(md) = self.md.as_mut() else {
            return false;
        };
        if md.sel == sel {
            return false;
        }
        md.sel = sel;
        self.rev += 1;
        true
    }

    /// Выделенный видимый текст (Ctrl+C в документе), весь — при `all` (Ctrl+A).
    pub fn md_copy_text(&self, all: bool) -> Option<&str> {
        let md = self.md.as_ref()?;
        let doc = md.doc.as_ref()?;
        if all {
            return Some(doc.text.as_str());
        }
        let s = md.sel.as_ref()?;
        let e = (s.end as usize).min(doc.text.len());
        let st = (s.start as usize).min(e);
        doc.text.get(st..e)
    }

    /// `viewer.scroll` в документе: строки — блоки, страница — блоки на высоту окна.
    pub(crate) fn md_scroll(&mut self, s: Scroll) -> bool {
        let (_, rows) = self.viewport();
        let cols = self.viewport().0;
        let Some(md) = self.md.as_mut() else {
            return false;
        };
        let Some(doc) = md.doc.clone() else {
            return false;
        };
        let n = u32::try_from(doc.blocks.len()).unwrap_or(u32::MAX);
        if n == 0 {
            return false;
        }
        let vis = |b: u32| md.visible(&doc, b);
        let step = |mut b: u32, down: bool| -> u32 {
            loop {
                let nb = if down {
                    if b + 1 >= n {
                        return b;
                    }
                    b + 1
                } else {
                    if b == 0 {
                        return b;
                    }
                    b - 1
                };
                b = nb;
                if vis(b) {
                    return b;
                }
            }
        };
        let page = |b: u32, down: bool| -> u32 {
            let mut acc = 0;
            let mut cur = b;
            while acc < rows {
                let next = step(cur, down);
                if next == cur {
                    break;
                }
                acc += est_rows(&doc, if down { cur } else { next }, cols).max(1);
                cur = next;
            }
            cur
        };
        let before = md.top;
        let had_frac = md.top_frac > 0.0;
        let mut t = md.top.min(n - 1);
        match s {
            Scroll::Up(k) => (0..k).for_each(|_| t = step(t, false)),
            Scroll::Down(k) => (0..k).for_each(|_| t = step(t, true)),
            Scroll::PageUp(k) => (0..k).for_each(|_| t = page(t, false)),
            Scroll::PageDown(k) => (0..k).for_each(|_| t = page(t, true)),
            Scroll::Home => t = 0,
            Scroll::End => t = page(n - 1, false).min(n - 1),
            Scroll::Left(_) | Scroll::Right(_) => {}
        }
        md.top = t;
        md.top_frac = 0.0;
        md.reveal = None;
        let changed = before != t || had_frac;
        if changed {
            self.top = doc.blocks[t as usize].src.start;
            self.rev += 1;
        }
        changed
    }

    /// Совпадение F7 в проекции: выделение, верхний блок, свёрнутые `<details>` над ним
    /// помечаются к раскрытию.
    pub(crate) fn md_hit(&mut self, r: Range<u64>) {
        let Some(md) = self.md.as_mut() else {
            return;
        };
        let Some(doc) = md.doc.clone() else {
            return;
        };
        let r =
            u32::try_from(r.start).unwrap_or(u32::MAX)..u32::try_from(r.end).unwrap_or(u32::MAX);
        let b = doc.block_at_text(r.start).unwrap_or(0);
        let mut child = b;
        for p in doc.ancestors(b).collect::<Vec<_>>() {
            if !md.is_open(&doc, p) && doc.summary_of(p) != Some(child) {
                let key = doc.blocks[p as usize].src.start;
                if !md.toggled.remove(&key) {
                    md.toggled.insert(key);
                }
            }
            child = p;
        }
        md.hit = Some(r.clone());
        md.sel = Some(r);
        md.top = b;
        md.top_frac = 0.0;
        md.reveal = Some(b);
        self.top = doc.blocks[b as usize].src.start;
    }

    /// С чего начинать поиск по проекции: от выделения или начала верхнего блока.
    pub(crate) fn md_search_from(&self, back: bool) -> u64 {
        let Some(md) = &self.md else {
            return 0;
        };
        if let Some(s) = md.sel.as_ref().or(md.hit.as_ref()) {
            return u64::from(if back { s.start } else { s.end });
        }
        md.doc
            .as_ref()
            .and_then(|d| d.blocks.get(md.top as usize))
            .map_or(0, |b| u64::from(b.text.start))
    }
}
