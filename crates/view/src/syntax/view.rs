//! Подсветка у `ViewerDoc` (S7 §5.7, ADR-0024): когда считать, приём, отрезки для UI.
//! Счёт — `syntax::job` в фоне; здесь — только состояние, без I/O.
//!
//! - Встроенные языки (JSON, TOML, INI, XML, YAML, Markdown) — **окно**: проход по файлу
//!   (точки) в выделенном потоке, окно — видимое ± запас (`win::margins`) на `Lane::Interactive` от
//!   ближайшей точки; любой размер. Пока проход не дошёл до окна — без цвета.
//! - Пакетные языки — полный разбор файла ≤ 16 МБ (как S7-21).

use std::ops::Range;
use std::sync::Arc;

use urfin_core::Runtime;

use super::{MARKUP_MAX, ViewerDoc};
use crate::layout::Mode;
use crate::syntax::Span;
use crate::syntax::job::{SynJob, SynState, WinJob, WinView, spawn, spawn_pass, spawn_window};
use crate::syntax::lex::{Feed, Lang};
use crate::syntax::marks::Marks;
use crate::syntax::win::{Window, margins};

/// Видимое длиннее (строки без переноса) — окно по началу.
const VIS_MAX: u64 = 4 << 20;

impl ViewerDoc {
    /// Язык с окном (ADR-0024): встроенный, кодировка без состояния.
    fn win_lang(&self) -> Option<(Lang, Feed)> {
        let lang = Lang::of(self.lang?)?;
        Some((lang, Feed::of(self.enc)?))
    }

    /// Подсветка нужна: «Разметка» (не Markdown-документ), файл дочитан, грамматика есть;
    /// пакетные языки — файл ≤ 16 МБ.
    pub fn syn_mode(&self) -> bool {
        self.mode == Mode::Markup
            && self.syntax_ok
            && self.is_ready()
            && self.len_final
            && (self.win_lang().is_some() || self.len <= MARKUP_MAX)
            && !self.md_mode()
    }

    /// Отрезки подсветки по байтам источника, если режим их рисует и счёт пришёл. У окна
    /// — только посчитанные байты (вне — без цвета).
    pub fn syntax(&self) -> Option<&Arc<[Span]>> {
        if !self.syn_mode() {
            return None;
        }
        let s = self.syn.as_ref()?;
        match &s.win {
            Some(w) => Some(&w.spans),
            None => s.spans.as_ref(),
        }
    }

    /// Проход по файлу ещё идёт, а окно его ждёт: доля пройденного (статус «Подсветка
    /// N %»).
    pub fn syntax_progress(&self) -> Option<f32> {
        let w = self.syn.as_ref()?.win.as_ref()?;
        (!w.done && w.waiting).then(|| {
            #[allow(clippy::cast_precision_loss, reason = "доля для статуса")]
            let p = w.frontier as f32 / self.len.max(1) as f32;
            p.clamp(0.0, 1.0)
        })
    }

    /// Режим подсветки включён: разбор (пакетные) или проход и окно (встроенные).
    pub(crate) fn syn_sync(&mut self, rt: &Runtime) {
        if !self.syn_mode() {
            return;
        }
        if let Some((lang, feed)) = self.win_lang() {
            self.win_sync(lang, feed, rt);
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
        s.win = None;
        let job = SynJob {
            doc: self.id,
            pass: s.pass,
            src,
            enc: self.enc,
            lang,
        };
        spawn(rt, job, cancel);
    }

    /// Окно: проход (если нет), окно видимого (если посчитанное его не покрывает).
    fn win_sync(&mut self, lang: Lang, feed: Feed, rt: &Runtime) {
        let Some(src) = self.src.clone() else {
            return;
        };
        let vis = self.syn_visible();
        let (len, enc, id) = (self.len, self.enc, self.id);
        let start = u64::from(enc.bom_len).min(len);
        let s = self.syn.get_or_insert_with(SynState::default);
        if s.enc != Some(enc)
            || s.win
                .as_ref()
                .is_none_or(|w| w.lang != lang || w.len != len)
        {
            s.cancel.cancel();
            s.cancel = self.cancel.child();
            s.pass += 1;
            s.enc = Some(enc);
            s.spans = None;
            s.win = Some(WinView::new(lang, feed, enc, start, len));
            let w = s.win.as_mut().expect("win");
            w.pass_on = true;
            spawn_pass(rt, id, s.pass, src.clone(), w, s.cancel.clone());
        }
        let pass = s.pass;
        let cancel = s.cancel.clone();
        let Some(w) = s.win.as_mut() else {
            return;
        };
        if w.win_busy {
            return;
        }
        let (margin, edge) = margins(lang);
        let need = vis.start.saturating_sub(edge)..vis.end.saturating_add(edge).min(len);
        if w.fresh && w.covered.start <= need.start && need.end <= w.covered.end {
            return;
        }
        let Some(marks) = w.marks.clone() else {
            w.waiting = true;
            return;
        };
        let range =
            vis.start.saturating_sub(margin).max(start)..vis.end.saturating_add(margin).min(len);
        if range.start > marks.frontier() && !marks.complete() {
            // Проход ещё не дошёл: окно — когда дойдёт.
            w.waiting = true;
            return;
        }
        w.waiting = false;
        w.win_busy = true;
        w.win_pass += 1;
        let job = WinJob {
            doc: id,
            pass,
            win_pass: w.win_pass,
            src,
            marks,
            range,
            lang,
            feed,
            enc,
            start,
        };
        spawn_window(rt, job, cancel);
    }

    /// Видимые байты (строки без переноса длиннее [`VIS_MAX`] — по началу).
    fn syn_visible(&self) -> Range<u64> {
        let v = self.visible_bytes();
        v.start..v.end.min(v.start.saturating_add(VIS_MAX))
    }

    /// Смена кодировки: разбор заново (в режиме подсветки — сразу), иначе — забыть.
    pub(crate) fn syn_reset(&mut self, rt: &Runtime) {
        if let Some(s) = self.syn.take() {
            s.cancel.cancel();
        }
        if self.syn_mode() {
            self.syn_sync(rt);
        }
    }

    /// Ответ задачи разбора (пакетные языки).
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

    /// Точки прохода: новые (`done` — до конца файла); окно, ждущее прохода, — сейчас.
    pub(crate) fn syn_marks(&mut self, pass: u64, marks: Arc<Marks>, done: bool, rt: &Runtime) {
        let Some(s) = self.syn.as_mut() else {
            return;
        };
        if pass != s.pass {
            return;
        }
        let Some(w) = s.win.as_mut() else {
            return;
        };
        w.frontier = marks.frontier();
        w.marks = Some(marks);
        w.done = done;
        if done {
            w.pass_on = false;
        }
        let refresh = w.waiting || (done && w.partial);
        if done && w.partial {
            w.fresh = false;
        }
        if refresh {
            self.syn_sync(rt);
            self.bump();
        }
    }

    /// Окно посчитано (`None` — проход ещё не дошёл).
    pub(crate) fn syn_window(
        &mut self,
        pass: u64,
        win_pass: u64,
        win: Option<Window>,
        rt: &Runtime,
    ) {
        let Some(s) = self.syn.as_mut() else {
            return;
        };
        if pass != s.pass {
            return;
        }
        let Some(w) = s.win.as_mut() else {
            return;
        };
        if win_pass != w.win_pass {
            return;
        }
        w.win_busy = false;
        match win {
            Some(win) => {
                w.spans = win.spans.into();
                w.covered = win.range;
                w.partial = win.partial;
                w.fresh = true;
            }
            None => w.waiting = true,
        }
        self.bump();
        self.syn_sync(rt);
    }
}
