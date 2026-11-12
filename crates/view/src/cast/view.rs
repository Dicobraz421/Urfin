//! Запись в документе вьювера (F03 §3.6): детект при открытии, проход по файлу, смена
//! источника между режимами, команды плеера. `src` документа: в «Записи» и «Тексте» —
//! вывод без VT (по нему же идёт поиск F7), в Hex — сам файл.

use std::io;
use std::ops::Range;
use std::sync::Arc;

use urfin_core::{CancelToken, Cx, Level, Msg, Runtime};
use urfin_term::vt::grid::Defaults;

use super::ViewerDoc;
use crate::cast::player::{self, Cmd, FrameMsg};
use crate::cast::scan::{CastIndex, scan};
use crate::cast::{CastState, Replay, header};
use crate::enc::Enc;
use crate::layout::Mode;
use crate::source::{ByteSource, SpoolSource, spool_dir};
use crate::{DocId, ViewState};

/// Цвета терминала до первого кадра UI.
const COLORS0: Defaults = Defaults {
    fg: [0x1a, 0x1a, 0x1a],
    bg: [0xf3, 0xf3, 0xf3],
    cursor: [0x1a, 0x1a, 0x1a],
};

/// Проход закончен.
#[derive(Debug)]
struct ScannedMsg {
    doc: DocId,
    res: Result<(Arc<CastIndex>, Arc<dyn ByteSource>), String>,
}

impl Msg for ScannedMsg {
    fn name(&self) -> &'static str {
        "view.cast.scanned"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let Some(d) = view.viewer_mut(self.doc) else {
            return;
        };
        match self.res {
            Ok((index, text)) => d.cast_ready(index, text, rt),
            Err(e) => {
                tracing::error!(error = %e, "cast scan failed");
                d.cast_failed(rt);
                let mut a = urfin_i18n::FluentArgs::new();
                a.set("error", e);
                let text = urfin_i18n::current().fmt("ui-cast-failed", &a);
                cx.toast(Level::Error, text);
            }
        }
    }
}

/// Проход по записи на отдельном потоке.
fn spawn_scan(rt: &Runtime, doc: DocId, st: &CastState, dir: std::path::PathBuf, c: CancelToken) {
    let src = st.file.clone();
    let h = st.header.clone();
    let done = st.scanned.clone();
    rt.spawn_dedicated(format!("view.cast.scan.{}", doc.0), c, move |cx| {
        let _span = tracing::debug_span!("view.cast.scan").entered();
        let run = || -> io::Result<Option<(CastIndex, Arc<dyn ByteSource>)>> {
            let (spool, mut out) = SpoolSource::create(&dir, cx.cancel.child())?;
            let ix = scan(&src, &h, &mut |b| out.write(b), &cx.cancel, &done)?;
            out.finish();
            Ok(ix.map(|ix| (ix, Arc::new(spool) as Arc<dyn ByteSource>)))
        };
        let res = match run() {
            Ok(None) => return,
            Ok(Some((ix, text))) => Ok((Arc::new(ix), text)),
            Err(e) => Err(e.to_string()),
        };
        let _ = cx.sink.send(Box::new(ScannedMsg { doc, res }), &cx.cancel);
    });
}

fn is_cast(d: &ViewerDoc) -> bool {
    d.loc
        .file_name()
        .and_then(|n| std::path::Path::new(n).extension())
        .is_some_and(|e| e.eq_ignore_ascii_case("cast"))
}

impl ViewerDoc {
    pub fn cast(&self) -> Option<&CastState> {
        self.cast.as_deref()
    }

    /// Файл — запись asciicast v2, открытая на экран: режим `Cast`. Быстрый просмотр и
    /// поток без seek (архив) остаются текстом.
    pub(super) fn cast_detect(&mut self) -> bool {
        if self.quick || !is_cast(self) {
            return false;
        }
        let (Some(src), Some(h)) = (self.src.clone(), header(&self.sample)) else {
            return false;
        };
        if !src.is_final() {
            return false;
        }
        self.cast = Some(Box::new(CastState::new(h, src)));
        self.mode = Mode::Cast;
        self.text_mode = Mode::Cast;
        self.enc = Enc::utf8();
        self.lang = None;
        self.syntax_ok = false;
        self.memo = None;
        true
    }

    /// Запуск прохода по записи.
    pub(super) fn cast_scan(&mut self, rt: &Runtime) {
        let Some(st) = self.cast.as_deref() else {
            return;
        };
        let dir = self
            .spool_dir
            .clone()
            .unwrap_or_else(|| spool_dir(&std::env::temp_dir()));
        spawn_scan(rt, self.id, st, dir, self.cancel.child());
    }

    /// Текст без VT ещё не готов: «Текст» и поиск ждут.
    pub(super) fn cast_waits(&self) -> bool {
        self.mode != Mode::Hex && self.cast().is_some_and(|c| c.text.is_none())
    }

    fn cast_ready(&mut self, index: Arc<CastIndex>, text: Arc<dyn ByteSource>, rt: &Runtime) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = self.cancel.child();
        let Some(c) = self.cast.as_deref_mut() else {
            return;
        };
        let replay = Replay::new(c.file.clone(), &c.header, index.clone());
        player::spawn(rt, self.id, replay, rx, c.colors.unwrap_or(COLORS0), cancel);
        c.tx = Some(tx);
        c.index = Some(index);
        c.text = Some(text.clone());
        if self.mode != Mode::Hex {
            self.use_source(text, rt);
        }
        if std::mem::take(&mut self.search_queued) && self.search.is_some() {
            self.search_again(false, rt);
        }
        self.bump();
    }

    /// Запись не разобрать: обычный текст файла.
    fn cast_failed(&mut self, rt: &Runtime) {
        self.cast = None;
        if self.mode == Mode::Cast {
            self.mode = Mode::Text;
        }
        self.text_mode = Mode::Text;
        self.start_index(rt);
        self.settle(rt);
        self.bump();
    }

    /// Другой источник документа: позиция с начала, индекс строк заново, итог поиска
    /// сброшен (смещения другие).
    fn use_source(&mut self, src: Arc<dyn ByteSource>, rt: &Runtime) {
        self.pages.replace_source(src.clone());
        self.len = src.len();
        self.len_final = src.is_final();
        self.kind = Some(src.kind());
        self.src = Some(src);
        self.top = 0;
        self.top_sub = 0;
        self.hscroll = 0;
        self.sel = None;
        self.realign = false;
        self.reveal = None;
        self.pending = None;
        self.reset_index(rt);
        if let Some(s) = &mut self.search {
            s.cancel();
            s.hit = None;
            s.marks = Arc::from(Vec::new());
            s.count = crate::search::CountState::default();
        }
        if let Some(c) = self.cast.as_deref_mut() {
            c.ticks = Arc::from(Vec::new());
        }
        self.settle(rt);
    }

    /// Запись / Текст / Hex документа записи. `false` — режим тот же или недоступен.
    pub(super) fn cast_mode(&mut self, m: Mode, rt: &Runtime) -> bool {
        let Some(c) = self.cast.as_deref() else {
            return false;
        };
        if m == self.mode || m == Mode::Markup || (m != Mode::Hex && c.text.is_none()) {
            return false;
        }
        let was_hex = self.mode == Mode::Hex;
        let src = match m {
            Mode::Hex => Some(c.file.clone()),
            _ if was_hex => c.text.clone(),
            _ => None,
        };
        if m != Mode::Cast {
            self.cast_play(false);
        }
        self.mode = m;
        if m != Mode::Hex {
            self.text_mode = m;
        }
        if let Some(s) = src {
            self.use_source(s, rt);
        } else {
            self.start_index(rt);
            self.settle(rt);
        }
        self.bump();
        true
    }

    /// Пуск или пауза. На конце записи пуск — с начала.
    pub(crate) fn cast_toggle(&mut self) -> bool {
        let Some(c) = self.cast.as_deref() else {
            return false;
        };
        let on = !c.playing;
        if on && c.index.is_some() && c.ms >= c.duration_ms() {
            self.cast_seek(0);
        }
        self.cast_play(on);
        true
    }

    fn cast_play(&mut self, on: bool) {
        if let Some(c) = self.cast.as_deref_mut()
            && c.playing != on
            && c.index.is_some()
        {
            c.playing = on;
            c.send(Cmd::Play(on));
            self.rev += 1;
        }
    }

    /// Плеер к моменту `ms`.
    pub(crate) fn cast_seek(&mut self, ms: u64) -> bool {
        let Some(c) = self.cast.as_deref_mut().filter(|c| c.index.is_some()) else {
            return false;
        };
        let ms = ms.min(c.duration_ms());
        c.seq += 1;
        c.ms = ms;
        c.send(Cmd::Seek { ms, seq: c.seq });
        self.rev += 1;
        true
    }

    /// ← → и шаг команды: `s` секунд от текущего места. Поиск дальше пойдёт отсюда.
    pub(crate) fn cast_seek_by(&mut self, s: i64) -> bool {
        let Some(ms) = self.cast().map(|c| c.ms) else {
            return false;
        };
        self.sel = None;
        self.cast_seek(ms.saturating_add_signed(s.saturating_mul(1000)))
    }

    /// Ползунок.
    pub(crate) fn cast_seek_to(&mut self, ms: u64) -> bool {
        self.sel = None;
        self.cast_seek(ms)
    }

    pub(crate) fn cast_speed(&mut self, n: u8) -> bool {
        let Some(c) = self.cast.as_deref_mut().filter(|c| c.speed != n) else {
            return false;
        };
        c.speed = n;
        c.send(Cmd::Speed(n));
        self.rev += 1;
        true
    }

    /// Цвета терминала сменились с темой.
    pub(crate) fn cast_colors(&mut self, d: Defaults) -> bool {
        let Some(c) = self.cast.as_deref_mut() else {
            return false;
        };
        let changed = c.colors_changed(d);
        if changed {
            c.frame = None;
            c.send(Cmd::Colors(d));
            self.rev += 1;
        }
        changed
    }

    pub(crate) fn on_cast_frame(&mut self, m: FrameMsg) {
        if let Some(c) = self.cast.as_deref_mut() {
            c.seen = m.seq;
            // Перемотка в пути: положение уже у ползунка, старый кадр его не сдвигает.
            if c.seq == m.seq {
                c.ms = m.ms;
                c.playing = m.playing;
            }
            if m.frame.is_some() {
                c.frame = m.frame;
            }
            self.rev += 1;
        }
    }

    /// Откуда искать в «Записи»: без выделения — от места плеера.
    pub(super) fn cast_search_from(&self) -> Option<u64> {
        if self.mode != Mode::Cast || self.sel.is_some() {
            return None;
        }
        let c = self.cast()?;
        if c.ms == 0 {
            return Some(0);
        }
        Some(c.index.as_ref()?.text_at(c.ms))
    }

    /// Совпадение в «Записи»: плеер — к моменту, когда оно уже на экране.
    pub(super) fn cast_hit(&mut self, r: &Range<u64>) {
        if self.mode != Mode::Cast {
            return;
        }
        let ms = self.cast().and_then(|c| {
            let last = r.end.saturating_sub(1).max(r.start);
            Some(c.index.as_ref()?.time_of(last))
        });
        if let Some(ms) = ms {
            self.cast_seek(ms);
        }
    }

    /// Счёт готов: метки на шкале.
    pub(super) fn cast_marks(&mut self) {
        let marks = self.search.as_ref().map(|s| s.marks.clone());
        if let (Some(c), Some(marks)) = (self.cast.as_deref_mut(), marks)
            && let Some(ix) = c.index.clone()
        {
            c.ticks = marks.iter().map(|&m| ix.frac(ix.time_of(m))).collect();
        }
    }
}
