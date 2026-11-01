//! Подсветка у `ViewerDoc` (S7 §5.7): когда разбирать, приём отрезков, отрезки для UI.
//! Разбор — `syntax::job` в фоне; здесь — только состояние, без I/O.

use std::sync::Arc;

use urfin_core::Runtime;

use super::{MARKUP_MAX, ViewerDoc};
use crate::layout::Mode;
use crate::syntax::Span;
use crate::syntax::job::{SynJob, SynState, spawn};

impl ViewerDoc {
    /// Подсветка нужна: «Разметка» (не Markdown-документ), файл ≤ 16 МБ и дочитан,
    /// грамматика есть.
    pub fn syn_mode(&self) -> bool {
        self.mode == Mode::Markup
            && self.syntax_ok
            && self.is_ready()
            && self.len_final
            && self.len <= MARKUP_MAX
            && !self.md_mode()
    }

    /// Отрезки подсветки по байтам источника, если режим их рисует и разбор пришёл.
    pub fn syntax(&self) -> Option<&Arc<[Span]>> {
        if !self.syn_mode() {
            return None;
        }
        self.syn.as_ref()?.spans.as_ref()
    }

    /// Режим подсветки включён: разбор, если его нет или кодировка другая.
    pub(crate) fn syn_sync(&mut self, rt: &Runtime) {
        if !self.syn_mode() {
            return;
        }
        if self.syn.as_ref().is_none_or(|s| s.enc != Some(self.enc)) {
            self.syn_start(rt);
        }
    }

    fn syn_start(&mut self, rt: &Runtime) {
        let (Some(src), Some(lang)) = (self.src.clone(), self.lang) else {
            return;
        };
        let cancel = self.cancel.child();
        let s = self.syn.get_or_insert_with(SynState::default);
        s.cancel.cancel();
        s.cancel = cancel.clone();
        s.pass += 1;
        s.enc = Some(self.enc);
        s.spans = None;
        s.complete = false;
        let job = SynJob {
            doc: self.id,
            pass: s.pass,
            src,
            enc: self.enc,
            lang,
        };
        spawn(rt, job, cancel);
    }

    /// Смена кодировки: разбор заново (в режиме подсветки — сразу), иначе — забыть.
    pub(crate) fn syn_reset(&mut self, rt: &Runtime) {
        if self.syn_mode() {
            self.syn_start(rt);
        } else if let Some(s) = self.syn.take() {
            s.cancel.cancel();
        }
    }

    /// Ответ задачи разбора.
    pub(crate) fn syn_apply(&mut self, pass: u64, spans: Arc<[Span]>, complete: bool) {
        let Some(s) = self.syn.as_mut() else {
            return;
        };
        if pass != s.pass {
            return;
        }
        s.spans = Some(spans);
        s.complete = complete;
        self.bump();
    }
}
