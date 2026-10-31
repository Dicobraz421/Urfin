//! Документ вьювера: открытие, детект, режимы, кодировки, позиция (S7 §3.1, §3.2, §3.4).
//! Карточка S7-08; поиск документа — S7-09 ([`crate::search`]).
//!
//! Открытие (§3.1): handler кладёт `ViewerDoc` в состоянии `Opening` и ставит задачу
//! `view.open` на `Lane::Interactive`: `open_source` → первые 64 КБ → `detect` → язык →
//! `OpenedMsg`. Первая страница кэша — выборка детекта: первый экран рисуется без второго
//! чтения. Spool (поток без seek): насос копирования — `spawn_dedicated`, детект ждёт
//! первые 64 КБ в отдельном потоке (`ProbedMsg`).
//!
//! UI-поток здесь файл не читает: только кэш страниц (раскладка для выравнивания позиции).

use std::io;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use encoding_rs::Encoding;
use smallvec::SmallVec;
use urfin_core::{CancelToken, Cx, Lane, Level, Location, Msg, Runtime, TabId};
use urfin_vfs::{VfsError, VfsRegistry};

use crate::cache::{PAGE, Page, PageCache, Prio};
use crate::config::{HexCols, ViewerCfg, WrapCfg};
use crate::detect::{Detected, Eol, detect, detect_eol};
use crate::enc::Enc;
use crate::history::ViewMemo;
use crate::index::{IndexDelta, IndexMode, Indexer, LineIndex, Sep, spawn_index};
use crate::layout::{LayoutSpec, Mode, WrapKind, hex_cols_auto, line_start_before};
use crate::search::job::{SearchEvent, spawn_count, spawn_find};
use crate::search::{SearchReq, SearchState, compile};
use crate::source::{ByteSource, SourceKind, SourceOpts, open_source, read_full};
use crate::{DocId, Placement, ViewState};

#[path = "viewport.rs"]
mod viewport;
pub use viewport::Scroll;

#[path = "jump.rs"]
mod jump;
pub use jump::{BACK_MAX, BOOKMARKS, GotoWait};

#[path = "md/view.rs"]
mod md_view;
pub use md_view::MdState;

#[path = "select.rs"]
mod select;
pub use select::decode_range;
pub(crate) use select::{copy, select_all};

#[path = "quick.rs"]
mod quick;
pub(crate) use quick::on_pump as quick_pump;
pub use quick::{
    QUICK_DOC_MAX, QUICK_READS, QuickNote, QuickNoteKind, QuickTarget, line_count, panel_shows,
    quick_target, visible_lines,
};
pub(crate) use quick::{dock, quick};

/// Выборка детекта и первая страница.
pub const SAMPLE: usize = PAGE as usize;
/// Разметка — для файлов не больше (S7 §3.4, §5.7).
pub const MARKUP_MAX: u64 = 16 << 20;
/// Spool: детект ждёт выборку не дольше.
const SPOOL_WAIT: Duration = Duration::from_millis(500);

/// Состояние открытия.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenState {
    /// Источник ещё не открыт: экран рисует вкладку, шапку и пустое поле.
    Opening,
    Ready,
}

/// Документ вьювера (S7 §2).
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые признаки документа"
)]
#[derive(Debug)]
pub struct ViewerDoc {
    pub id: DocId,
    pub loc: Location,
    pub placement: Placement,
    /// Быстрый просмотр (Ctrl+Q): без индекса и поиска, пока нет фокуса.
    pub quick: bool,
    pub state: OpenState,
    /// Источник; `None` до открытия. UI его не читает — только `pages`.
    pub src: Option<Arc<dyn ByteSource>>,
    pub kind: Option<SourceKind>,
    /// Длина документа (растёт у Spool) и окончательна ли она.
    pub len: u64,
    pub len_final: bool,
    /// Итог детекта при открытии (`utf32` — тост «не поддерживается»).
    pub detected: Option<Detected>,
    pub enc: Enc,
    pub eol: Eol,
    pub mode: Mode,
    /// Последний текстовый режим (F4 — Hex ↔ он).
    pub text_mode: Mode,
    /// Перенос; `None` — выключен.
    pub wrap: Option<WrapKind>,
    /// Вид переноса для F2 (`off` ↔ он) и `viewer.wrap_mode`.
    pub wrap_kind: WrapKind,
    /// Язык подсветки (имя gpui-kit) — режим «Разметка».
    pub lang: Option<&'static str>,
    /// Начало верхней строки (в Hex рисуется с `top` вниз до `hex_cols`, см. [`Self::pos`]).
    pub top: u64,
    /// Подстрока верхней строки при переносе.
    pub top_sub: u32,
    /// Колонка горизонтальной прокрутки (без переноса).
    pub hscroll: u32,
    /// `top` ждёт выравнивания к началу строки: нужных страниц не было в кэше.
    pub realign: bool,
    /// Найденное совпадение, которое нужно вывести на треть высоты экрана (S7 §3.6):
    /// handler ставит позицию в `settle`, когда страницы на месте, и сбрасывает поле.
    pub reveal: Option<u64>,
    /// Видимая область от UI (`InboxMsg::Viewport`): ширина окна текста в колонках и
    /// строк на экране; 0 — ещё не пришла (тогда 80×25).
    pub view_cols: u32,
    pub view_rows: u32,
    pub pages: PageCache,
    pub index: LineIndex,
    pub search: Option<SearchState>,
    pub sel: Option<Range<u64>>,
    /// Позиции до переходов (`viewer.back`), последняя — сверху.
    pub back: SmallVec<[u64; BACK_MAX]>,
    /// Закладки 0–9: позиция `top`.
    pub bookmarks: [Option<u64>; BOOKMARKS],
    /// Переход на строку ждёт индекса или страниц (§3.3): прогресс в статусе.
    pub goto_wait: Option<GotoWait>,
    /// Ждавший переход состоялся: handler закрывает строку перехода.
    goto_done: bool,
    pub max_line: u32,
    pub tab: u8,
    pub hex_cols: HexCols,
    /// Токен документа: loader, индексатор, поиск; отменяется при закрытии.
    pub cancel: CancelToken,
    pub rev: u64,
    /// Начало файла (выборка детекта): BOM и EOL при смене кодировки.
    sample: Arc<[u8]>,
    /// Запись истории, ждущая открытия.
    memo: Option<ViewMemo>,
    budget: usize,
    index_on: bool,
    index_epoch: u32,
    index_cancel: CancelToken,
    pgen: u64,
    /// Прокрутка, ждущая страниц (`viewer.scroll` через непрочитанное).
    pending: Option<Scroll>,
    /// Поиск задан до открытия источника: запустить после детекта (под его кодировку).
    search_queued: bool,
    /// Markdown-документ (S7 §3.10): разбор, верхний блок, `<details>`, поиск F7.
    pub md: Option<MdState>,
    /// `viewer.markdown = "document"`.
    md_document: bool,
    /// `viewer.md_image_max_mb` в байтах.
    md_image_max: u64,
    /// Доля верхнего блока из истории — до первого разбора.
    md_frac0: Option<f32>,
    /// Якорь из ссылки другого `.md` (`файл.md#якорь`): верхний блок после разбора.
    md_anchor: Option<String>,
    /// Таб панели, из которого открыт файл (`viewer.next_file`, `viewer.reveal`).
    pub source: Option<TabId>,
    /// Файлы до перехода по ссылке `.md` в этом же экране (`viewer.back` — к ним).
    pub files_back: Vec<ViewMemo>,
}

impl ViewerDoc {
    /// Документ в состоянии `Opening`; `cancel` — токен документа.
    pub fn new(
        id: DocId,
        loc: Location,
        placement: Placement,
        quick: bool,
        cfg: &ViewerCfg,
        cancel: CancelToken,
    ) -> ViewerDoc {
        let budget = if quick {
            1 << 20
        } else {
            (cfg.cache_mb.max(1) as usize) << 20
        };
        let (wrap, wrap_kind) = match cfg.wrap {
            WrapCfg::Off => (None, WrapKind::Words),
            WrapCfg::Words => (Some(WrapKind::Words), WrapKind::Words),
            WrapCfg::Chars => (Some(WrapKind::Chars), WrapKind::Chars),
        };
        let index_cancel = cancel.child();
        ViewerDoc {
            id,
            loc,
            placement,
            quick,
            state: OpenState::Opening,
            src: None,
            kind: None,
            len: 0,
            len_final: false,
            detected: None,
            enc: Enc::utf8(),
            eol: Eol::default(),
            mode: Mode::Text,
            text_mode: Mode::Text,
            wrap,
            wrap_kind,
            lang: None,
            top: 0,
            top_sub: 0,
            hscroll: 0,
            realign: false,
            reveal: None,
            view_cols: 0,
            view_rows: 0,
            pages: PageCache::new(budget, 0, false),
            index: LineIndex::default(),
            search: None,
            sel: None,
            back: SmallVec::new(),
            bookmarks: [None; BOOKMARKS],
            goto_wait: None,
            goto_done: false,
            max_line: cfg.max_line,
            tab: cfg.tab_size,
            hex_cols: cfg.hex_cols,
            cancel,
            rev: 0,
            sample: Arc::from(Vec::new()),
            memo: None,
            budget,
            index_on: false,
            index_epoch: 0,
            index_cancel,
            pgen: 0,
            pending: None,
            search_queued: false,
            md: None,
            md_document: cfg.markdown == crate::config::MdView::Document,
            md_image_max: u64::from(cfg.md_image_max_mb) << 20,
            md_frac0: None,
            md_anchor: None,
            source: None,
            files_back: Vec::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state == OpenState::Ready
    }

    /// Режим «Разметка» доступен: язык известен и файл не больше 16 МБ.
    pub fn markup_ok(&self) -> bool {
        self.lang.is_some() && self.len <= MARKUP_MAX
    }

    /// Раскладка для окна шириной `width_cols` колонок.
    pub fn layout(&self, width_cols: u32) -> LayoutSpec {
        LayoutSpec {
            mode: self.mode,
            wrap: self.wrap,
            width_cols,
            tab: self.tab,
            max_line: self.max_line,
            hex_cols: match self.hex_cols {
                HexCols::Auto => hex_cols_auto(width_cols),
                HexCols::Fixed(n) => n,
            },
            enc: self.enc,
            eol: self.eol,
        }
    }

    /// Позиция верхней строки для `rows_down`: в Hex — `top` вниз до `hex_cols`.
    pub fn pos(&self, s: &LayoutSpec) -> (u64, u32) {
        if s.mode == Mode::Hex {
            let c = u64::from(s.hex_cols.max(1));
            (self.top / c * c, 0)
        } else {
            (self.top, self.top_sub)
        }
    }

    /// Запись истории (позиция, кодировка, если выбрана вручную, режим, перенос).
    pub fn memo(&self) -> ViewMemo {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "доля 0…1 в десятитысячных"
        )]
        let frac = match (&self.md, self.md_mode()) {
            (Some(m), true) => (m.top_frac.clamp(0.0, 1.0) * 10_000.0) as u16,
            _ => 0,
        };
        ViewMemo {
            loc: self.loc.clone(),
            top: self.top,
            hscroll: self.hscroll,
            enc: (!self.enc.auto).then(|| self.enc.enc.name().into()),
            mode: self.mode,
            wrap: self.wrap,
            frac,
            kind: crate::history::HistKind::View,
            ts: 0,
        }
    }

    /// Запись истории, с которой открыть (до `OpenedMsg`): соседний файл, «назад» по
    /// ссылкам. `None` — с начала, по детекту.
    pub(crate) fn set_memo(&mut self, m: Option<ViewMemo>) {
        self.memo = m;
    }

    /// Якорь `#…` для документа Markdown: верхним станет его заголовок.
    pub(crate) fn set_anchor(&mut self, a: Option<String>) {
        self.md_anchor = a.filter(|a| !a.is_empty());
    }

    /// Перечитанный `[viewer]` (`app.reload_config`): Markdown — документ или исходник,
    /// предел картинок, таб, колонки Hex. Позиция сохраняется. `false` — для документа
    /// ничего не изменилось.
    pub(crate) fn apply_cfg(&mut self, cfg: &ViewerCfg, rt: &Runtime) -> bool {
        let document = cfg.markdown == crate::config::MdView::Document;
        let image_max = u64::from(cfg.md_image_max_mb) << 20;
        if document == self.md_document
            && image_max == self.md_image_max
            && cfg.tab_size == self.tab
            && cfg.hex_cols == self.hex_cols
        {
            return false;
        }
        let was_doc = self.md_mode();
        if was_doc && !document {
            // Документ → исходник: со строки начала верхнего блока.
            self.md_leave();
        }
        let reparse = image_max != self.md_image_max;
        self.md_document = document;
        self.md_image_max = image_max;
        self.tab = cfg.tab_size;
        self.hex_cols = cfg.hex_cols;
        let now_doc = self.md_mode();
        if reparse || (was_doc && !now_doc) {
            // Новый разбор (картинки сверяются с пределом) или документ больше не нужен.
            self.md_reset(rt);
        }
        if now_doc {
            self.md_sync(rt);
        }
        if was_doc != now_doc {
            self.retarget_search();
        }
        if self.is_ready() {
            self.settle(rt);
        }
        self.bump();
        true
    }

    fn bump(&mut self) {
        self.rev += 1;
    }

    // ------------------------------------------------------------ открытие

    /// Источник открыт: кэш над ним; детект — `apply_probe`.
    fn attach(&mut self, src: Arc<dyn ByteSource>) {
        self.len = src.len();
        self.len_final = src.is_final();
        self.kind = Some(src.kind());
        self.pages = PageCache::with_source(self.budget, src.clone());
        self.src = Some(src);
    }

    /// Выборка и детект: кодировка, режим (§3.1 п. 4), история, первая страница, индекс.
    fn apply_probe(&mut self, p: Probe, rt: &Runtime) {
        self.sync_len();
        let det = p.detected;
        self.detected = Some(det);
        self.lang = p.lang;
        if !p.sample.is_empty() && (p.sample.len() == SAMPLE || p.sample.len() as u64 == self.len) {
            self.pages.seed(0, Page::Owned(p.sample.clone()));
        }
        self.sample = p.sample;
        self.enc = det.enc;
        self.eol = det.eol;
        self.mode = if det.binary {
            Mode::Hex
        } else if self.markup_ok() {
            Mode::Markup
        } else {
            Mode::Text
        };
        self.text_mode = if self.mode == Mode::Hex {
            Mode::Text
        } else {
            self.mode
        };
        if let Some(m) = self.memo.take()
            && (m.top < self.len || m.top == 0)
        {
            if let Some(e) = m
                .enc
                .as_deref()
                .and_then(|l| Encoding::for_label(l.as_bytes()))
            {
                self.enc = self.with_bom(Enc::new(e));
                self.eol = self.eol_for(&self.enc);
            }
            self.mode = match m.mode {
                Mode::Markup if !self.markup_ok() => Mode::Text,
                x => x,
            };
            if self.mode != Mode::Hex {
                self.text_mode = self.mode;
            }
            self.wrap = m.wrap;
            if let Some(k) = m.wrap {
                self.wrap_kind = k;
            }
            self.top = m.top;
            self.hscroll = m.hscroll;
            if m.frac > 0 {
                self.md_frac0 = Some(f32::from(m.frac) / 10_000.0);
            }
            if self.mode != Mode::Hex {
                self.realign_top();
            }
        }
        self.state = OpenState::Ready;
        self.start_index(rt);
        // Markdown-документ: разбор на Interactive lane (S7 §3.10).
        self.md_sync(rt);
        // Прогрев позиции: первая страница уже в кэше, восстановленная — грузится.
        self.settle(rt);
        // В документе поиск ждёт разбора (`md_apply`).
        let md_wait = self.md_mode() && self.md_doc().is_none_or(|d| !d.complete);
        if !md_wait
            && std::mem::take(&mut self.search_queued)
            && let Some(s) = &mut self.search
        {
            s.matcher = None;
            let back = s.req.back;
            self.search_again(back, rt);
        }
        self.bump();
    }

    /// Длина источника изменилась (Spool): атомики, без I/O.
    pub(crate) fn sync_len(&mut self) {
        let Some(src) = &self.src else {
            return;
        };
        let fin = src.is_final();
        let len = src.len();
        if len != self.len || fin != self.len_final {
            self.len = len;
            self.len_final = fin;
            self.pages.set_len(len, fin);
            self.bump();
        }
    }

    /// После страниц loader'а: длина, отложенное выравнивание, прокрутка и совпадение,
    /// видимые страницы. `failed` — чтение не удалось: те же страницы заново не просим.
    pub(crate) fn after_pages(&mut self, rt: &Runtime, failed: bool) {
        self.sync_len();
        // Spool дописан до конца — документ Markdown можно разбирать.
        self.md_sync(rt);
        if self.realign {
            self.realign_top();
        }
        if failed {
            self.pages.pump(rt, self.id, &self.cancel);
        } else {
            self.settle(rt);
        }
    }

    // ------------------------------------------------------------ индекс

    fn start_index(&mut self, rt: &Runtime) {
        if self.quick || self.index_on || self.mode == Mode::Hex || !self.is_ready() {
            return;
        }
        let Some(src) = self.src.clone() else {
            return;
        };
        self.index_on = true;
        let ix = Indexer::new(Sep::of(&self.enc, self.eol));
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

    /// Индекс заново (разделитель сменился: UTF-16 ↔ байтовые, `Cr` ↔ `Lf`).
    fn reset_index(&mut self, rt: &Runtime) {
        self.index_cancel.cancel();
        self.index_cancel = self.cancel.child();
        self.index_epoch += 1;
        self.index = LineIndex::default();
        self.index_on = false;
        self.start_index(rt);
    }

    /// Дельта индексатора текущего поколения; ждущий переход на строку — заново.
    pub(crate) fn on_index(&mut self, epoch: u32, delta: IndexDelta, rt: &Runtime) {
        if epoch != self.index_epoch {
            return;
        }
        self.index.apply(delta);
        self.sync_len();
        if self.goto_wait.is_some() {
            self.settle(rt);
        }
        self.bump();
    }

    /// Номер поколения индекса (тесты, UI-отладка).
    pub fn index_epoch(&self) -> u32 {
        self.index_epoch
    }

    // ------------------------------------------------------------ позиция

    /// `top` → начало строки по кэшу; страниц нет — до единицы кода и позже (`realign`).
    fn realign_top(&mut self) {
        self.top_sub = 0;
        if self.mode == Mode::Hex {
            self.realign = false;
            return;
        }
        let spec = self.layout(80);
        if let Ok(s) = line_start_before(&self.pages, self.top, &spec) {
            self.top = s;
            self.realign = false;
        } else {
            let u = u64::from(self.enc.unit());
            self.top = self.top / u * u;
            self.realign = true;
            let lo = self.top.saturating_sub(u64::from(self.max_line) + 8);
            self.pages.want(lo..self.top + 1, Prio::Visible);
        }
    }

    // ------------------------------------------------------------ режимы

    /// Режим; позиция сохраняется: в Hex `top` не меняется (рисуется с `top` вниз до
    /// `hex_cols`), из Hex — к началу строки. `false` — режим тот же или недоступен.
    pub fn set_mode(&mut self, m: Mode, rt: &Runtime) -> bool {
        if m == self.mode || (m == Mode::Markup && !self.markup_ok()) {
            return false;
        }
        let from_hex = self.mode == Mode::Hex;
        let was_doc = self.md_mode();
        if was_doc {
            // Документ → Текст / Hex: со строки начала верхнего блока.
            self.md_leave();
        }
        self.mode = m;
        if self.md_mode() {
            self.md_sync(rt);
        }
        if was_doc != self.md_mode() {
            self.retarget_search();
        }
        if m == Mode::Hex {
            self.top_sub = 0;
        } else {
            self.text_mode = m;
            if from_hex {
                self.realign_top();
            }
            self.start_index(rt);
        }
        self.bump();
        true
    }

    /// F4: Hex ↔ последний текстовый режим.
    pub fn toggle_hex(&mut self, rt: &Runtime) -> bool {
        let to = if self.mode == Mode::Hex {
            self.text_mode
        } else {
            Mode::Hex
        };
        self.set_mode(to, rt)
    }

    /// F2: перенос `off` ↔ последний вид.
    pub fn toggle_wrap(&mut self) {
        self.wrap = match self.wrap {
            Some(_) => None,
            None => Some(self.wrap_kind),
        };
        self.top_sub = 0;
        self.bump();
    }

    /// По словам ↔ по символам; при выключенном переносе — только запоминает вид.
    pub fn toggle_wrap_kind(&mut self) {
        self.wrap_kind = match self.wrap_kind {
            WrapKind::Words => WrapKind::Chars,
            WrapKind::Chars => WrapKind::Words,
        };
        if self.wrap.is_some() {
            self.wrap = Some(self.wrap_kind);
        }
        self.top_sub = 0;
        self.bump();
    }

    // ------------------------------------------------------------ кодировки

    fn at_eof(&self) -> bool {
        self.len_final && self.sample.len() as u64 >= self.len
    }

    /// `enc` с длиной BOM этого файла.
    fn with_bom(&self, mut enc: Enc) -> Enc {
        enc.bom_len = bom_len(enc.enc, &self.sample);
        enc
    }

    fn eol_for(&self, enc: &Enc) -> Eol {
        match self.detected {
            Some(d) if d.enc.enc == enc.enc => d.eol,
            _ => {
                let body = self.sample.get(enc.bom_len as usize..).unwrap_or(&[]);
                detect_eol(body, enc, self.at_eof())
            }
        }
    }

    /// Кодировка по имени WHATWG или `auto` (детект).
    pub fn resolve(&self, label: &str) -> Option<Enc> {
        if label.eq_ignore_ascii_case("auto") {
            return Some(self.detected.map_or(Enc::utf8(), |d| d.enc));
        }
        Encoding::for_label(label.trim().as_bytes()).map(|e| self.with_bom(Enc::new(e)))
    }

    /// Смена кодировки. Индекс сбрасывается, только если сменился разделитель строк
    /// (UTF-16 ↔ байтовые, LE ↔ BE): 1251 ↔ 866 индекс сохраняют.
    pub fn set_enc(&mut self, enc: Enc, rt: &Runtime) -> bool {
        if enc == self.enc {
            return false;
        }
        let old = Sep::of(&self.enc, self.eol);
        self.eol = self.eol_for(&enc);
        self.enc = enc;
        if Sep::of(&self.enc, self.eol) != old {
            self.reset_index(rt);
            self.realign_top();
        }
        if let Some(s) = &mut self.search {
            // Образец компилируется под кодировку: следующий поиск — заново.
            s.cancel();
            s.matcher = None;
            s.marks = Arc::from(Vec::new());
            s.count = crate::search::CountState::default();
        }
        self.md_reset(rt);
        self.bump();
        true
    }

    /// `viewer.codepage cp=…`: `None` — неизвестное имя.
    pub fn set_codepage(&mut self, label: &str, rt: &Runtime) -> Option<bool> {
        let enc = self.resolve(label)?;
        Some(self.set_enc(enc, rt))
    }

    /// F8: следующая в кольце кодировка, отличная от текущей.
    pub fn next_codepage(&mut self, ring: &[impl AsRef<str>], rt: &Runtime) -> bool {
        let Some(label) = ring_next(ring, &self.enc, |l| self.resolve(l)) else {
            return false;
        };
        self.set_codepage(&label, rt).unwrap_or(false)
    }

    // ------------------------------------------------------------ поиск

    /// Новый поиск от выделения (или `top`): вперёд — от его конца, назад — от начала.
    pub fn search(&mut self, req: SearchReq, rt: &Runtime) {
        if matches!(req.pat, crate::search::Pattern::Hex(_)) {
            // Hex-поиск в документе — в Тексте на той же позиции (S7 §3.10).
            self.leave_doc(rt);
        }
        if let Some(s) = &mut self.search {
            s.cancel();
        }
        self.pgen += 1;
        let mut st = SearchState::new(req, self.pgen);
        st.proj = self.md_mode();
        match compile(&st.req.pat, self.search_enc(st.proj)) {
            Ok(m) => st.matcher = Some(Arc::new(m)),
            Err(e) => st.error = Some(e),
        }
        self.search = Some(st);
        self.run_search(rt);
    }

    /// Образец не разобран (hex): ошибка — в строке поиска, прежний поиск остановлен.
    pub fn search_failed(&mut self, req: SearchReq, err: crate::search::SearchError, rt: &Runtime) {
        if matches!(req.pat, crate::search::Pattern::Hex(_)) {
            self.leave_doc(rt);
        }
        if let Some(s) = &mut self.search {
            s.cancel();
        }
        self.pgen += 1;
        let mut st = SearchState::new(req, self.pgen);
        st.error = Some(err);
        self.search = Some(st);
        self.bump();
    }

    /// Shift+F7 / Alt+F7: тот же образец вперёд или назад. `false` — поиска не было.
    pub fn search_again(&mut self, back: bool, rt: &Runtime) -> bool {
        let Some(hex) = self
            .search
            .as_ref()
            .map(|s| matches!(s.req.pat, crate::search::Pattern::Hex(_)))
        else {
            return false;
        };
        if hex {
            self.leave_doc(rt);
        }
        self.retarget_search();
        let enc = self.search_enc(self.md_mode());
        let Some(s) = &mut self.search else {
            return false;
        };
        s.req.back = back;
        if s.matcher.is_none() && s.error.is_none() {
            match compile(&s.req.pat, enc) {
                Ok(m) => s.matcher = Some(Arc::new(m)),
                Err(e) => s.error = Some(e),
            }
        }
        self.run_search(rt);
        true
    }

    /// Esc в строке поиска: идущий поиск отменяется (метки и итог остаются).
    pub fn search_cancel(&mut self) {
        if let Some(s) = &mut self.search {
            s.cancel.cancel();
            s.running = false;
            self.bump();
        }
    }

    /// Кодировка образца: проекция документа — UTF-8, иначе — кодировка файла.
    fn search_enc(&self, proj: bool) -> Enc {
        if proj { Enc::utf8() } else { self.enc }
    }

    /// Поиск сменил пространство (документ ↔ Текст): итог и метки сбрасываются, образец
    /// компилируется заново.
    fn retarget_search(&mut self) {
        let proj = self.md_mode();
        if let Some(s) = &mut self.search
            && s.proj != proj
        {
            s.cancel();
            s.proj = proj;
            s.matcher = None;
            s.hit = None;
            s.marks = Arc::from(Vec::new());
            s.count = crate::search::CountState::default();
            self.rev += 1;
        }
    }

    fn run_search(&mut self, rt: &Runtime) {
        if self.src.is_none() || !self.is_ready() {
            // Документ ещё открывается: поиск — после детекта.
            self.search_queued = self.search.is_some();
            return;
        }
        let proj = self.search.as_ref().is_some_and(|s| s.proj);
        let src = if proj {
            // F7 в документе — по проекции, когда документ разобран целиком.
            let ready = self
                .md
                .as_ref()
                .filter(|m| m.doc.as_ref().is_some_and(|d| d.complete));
            let Some(m) = ready else {
                self.search_queued = self.search.is_some();
                return;
            };
            m.proj_source()
        } else {
            self.src.clone()
        };
        let md_from = (self.md_search_from(false), self.md_search_from(true));
        let (Some(src), Some(s)) = (src, self.search.as_mut()) else {
            return;
        };
        let Some(m) = s.matcher.clone() else {
            self.rev += 1;
            return;
        };
        s.cancel.cancel();
        s.cancel = self.cancel.child();
        s.seq += 1;
        let from = match (&self.sel, s.req.back) {
            (_, false) if proj => md_from.0,
            (_, true) if proj => md_from.1,
            (Some(r), false) => r.end,
            (Some(r), true) => r.start,
            (None, _) => self.top,
        };
        s.from = from;
        s.scanned.store(0, Ordering::Relaxed);
        s.total = src.len();
        s.running = true;
        s.not_found = false;
        s.wrapped = false;
        s.failed = None;
        spawn_find(
            rt,
            self.id,
            s.seq,
            src,
            m,
            from,
            &s.req,
            s.cancel.clone(),
            s.scanned.clone(),
        );
        self.rev += 1;
    }

    /// Ответ задачи поиска.
    pub(crate) fn on_search(&mut self, seq: u64, ev: SearchEvent, rt: &Runtime, limit: usize) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        match ev {
            SearchEvent::Counted(c) => {
                if seq != s.pgen {
                    return;
                }
                s.count.n = c.starts.len();
                s.count.more = c.more;
                s.count.done = true;
                s.counting = false;
                s.marks = Arc::from(c.starts);
            }
            SearchEvent::Found(h) => {
                if seq != s.seq {
                    return;
                }
                s.running = false;
                match h {
                    None => s.not_found = true,
                    Some(h) => {
                        s.hit = Some(h.range.clone());
                        s.wrapped = h.wrapped;
                        let proj = s.proj;
                        let start_count = !s.count.done && !s.counting && !self.quick;
                        let src = if proj {
                            self.md.as_ref().and_then(MdState::proj_source)
                        } else {
                            self.src.clone()
                        };
                        if start_count && let (Some(src), Some(m)) = (src, s.matcher.clone()) {
                            s.counting = true;
                            s.count_cancel = self.cancel.child();
                            spawn_count(rt, self.id, s.pgen, src, m, limit, s.count_cancel.clone());
                        }
                        if proj {
                            // Совпадение в проекции → (блок, отрезок) документа.
                            self.md_hit(h.range);
                        } else {
                            self.sel = Some(h.range.clone());
                            self.reveal = Some(h.range.start);
                            self.top = h.range.start;
                            self.realign_top();
                        }
                        self.settle(rt);
                    }
                }
            }
            SearchEvent::Failed(e) => {
                if seq != s.seq {
                    return;
                }
                s.running = false;
                s.failed = Some(e);
            }
        }
        self.bump();
    }

    /// Закрытие: всё фоновое документа останавливается.
    pub(crate) fn close(&mut self) {
        self.goto_wait = None;
        if let Some(s) = &mut self.search {
            s.cancel();
        }
        self.cancel.cancel();
    }
}

/// Длина BOM кодировки `enc` в начале `head`.
pub fn bom_len(enc: &'static Encoding, head: &[u8]) -> u8 {
    match Encoding::for_bom(head) {
        Some((e, n)) if e == enc => n as u8,
        _ => 0,
    }
}

/// Следующая метка кольца после текущей кодировки, дающая другую кодировку.
fn ring_next(
    ring: &[impl AsRef<str>],
    cur: &Enc,
    resolve: impl Fn(&str) -> Option<Enc>,
) -> Option<String> {
    if ring.is_empty() {
        return None;
    }
    let at = ring.iter().position(|l| {
        let l = l.as_ref();
        if l.eq_ignore_ascii_case("auto") {
            cur.auto
        } else {
            !cur.auto && resolve(l).is_some_and(|e| e.enc == cur.enc)
        }
    });
    let n = ring.len();
    let start = at.map_or(0, |i| i + 1);
    (0..n)
        .map(|k| ring[(start + k) % n].as_ref())
        .find(|l| resolve(l).is_some_and(|e| e.enc != cur.enc))
        .or_else(|| Some(ring[start % n].as_ref()))
        .map(str::to_owned)
}

/// Группы «мешочка» кодировок (`screen-viewerenc.png`): после «Авто» — Юникод, кириллица,
/// западная, остальные кодировки `encoding_rs`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CpGroup {
    Unicode,
    Cyrillic,
    Western,
    Other,
}

/// Кодировки «мешочка» по группам, в порядке показа.
pub fn codepage_menu() -> [(CpGroup, &'static [&'static Encoding]); 4] {
    use encoding_rs as e;
    static UNICODE: [&Encoding; 3] = [e::UTF_8, e::UTF_16LE, e::UTF_16BE];
    static CYRILLIC: [&Encoding; 6] = [
        e::WINDOWS_1251,
        e::IBM866,
        e::KOI8_R,
        e::KOI8_U,
        e::ISO_8859_5,
        e::X_MAC_CYRILLIC,
    ];
    static WESTERN: [&Encoding; 4] = [e::WINDOWS_1252, e::ISO_8859_15, e::MACINTOSH, e::ISO_8859_2];
    static OTHER: [&Encoding; 26] = [
        e::WINDOWS_1250,
        e::WINDOWS_1253,
        e::WINDOWS_1254,
        e::WINDOWS_1255,
        e::WINDOWS_1256,
        e::WINDOWS_1257,
        e::WINDOWS_1258,
        e::WINDOWS_874,
        e::ISO_8859_3,
        e::ISO_8859_4,
        e::ISO_8859_6,
        e::ISO_8859_7,
        e::ISO_8859_8,
        e::ISO_8859_8_I,
        e::ISO_8859_10,
        e::ISO_8859_13,
        e::ISO_8859_14,
        e::ISO_8859_16,
        e::SHIFT_JIS,
        e::EUC_JP,
        e::ISO_2022_JP,
        e::GBK,
        e::GB18030,
        e::BIG5,
        e::EUC_KR,
        e::X_USER_DEFINED,
    ];
    [
        (CpGroup::Unicode, &UNICODE),
        (CpGroup::Cyrillic, &CYRILLIC),
        (CpGroup::Western, &WESTERN),
        (CpGroup::Other, &OTHER),
    ]
}

// ---------------------------------------------------------------- задачи

/// Выборка начала файла, детект и язык.
#[derive(Debug)]
pub(crate) struct Probe {
    sample: Arc<[u8]>,
    detected: Detected,
    lang: Option<&'static str>,
}

fn probe(src: &dyn ByteSource, name: &str, tld: Option<&[u8]>) -> io::Result<Probe> {
    let n = src.len().min(SAMPLE as u64) as usize;
    let mut buf = vec![0u8; n];
    let got = read_full(src, 0, &mut buf)?;
    buf.truncate(got);
    let at_eof = src.is_final() && got as u64 >= src.len();
    let detected = detect(&buf, at_eof, tld);
    let lang = if detected.binary {
        None
    } else {
        crate::lang::detect(name, &buf)
    };
    Ok(Probe {
        sample: buf.into(),
        detected,
        lang,
    })
}

/// Что нужно задаче открытия.
#[derive(Debug)]
pub(crate) struct OpenJob {
    pub doc: DocId,
    pub loc: Location,
    pub opts: SourceOpts,
    pub tld: Option<&'static [u8]>,
    pub reg: Arc<VfsRegistry>,
}

impl OpenJob {
    fn name(&self) -> String {
        self.loc
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Задача `view.open` на `Lane::Interactive`.
pub(crate) fn spawn_open(rt: &Runtime, job: OpenJob, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "view.open", cancel, move |cx| {
        let _span = tracing::debug_span!("view.open").entered();
        let res = match open_source(&job.reg, &job.loc, &job.opts, &cx.cancel) {
            Err(VfsError::Cancelled) => return,
            Err(e) => Err(e.to_string()),
            Ok(o) if o.pump.is_some() => Ok((o, None)),
            Ok(o) => match probe(&*o.src, &job.name(), job.tld) {
                Ok(p) => Ok((o, Some(p))),
                Err(e) => Err(e.to_string()),
            },
        };
        let msg = OpenedMsg {
            doc: job.doc,
            name: job.name(),
            tld: job.tld,
            res,
        };
        let _ = cx.sink.send(Box::new(msg), &cx.cancel);
    });
}

/// Spool: ждёт первые 64 КБ (или конец потока, или [`SPOOL_WAIT`]) и делает детект.
fn spawn_probe(
    rt: &Runtime,
    doc: DocId,
    src: Arc<dyn ByteSource>,
    name: String,
    tld: Option<&'static [u8]>,
    cancel: CancelToken,
) {
    rt.spawn_dedicated(format!("view.probe.{}", doc.0), cancel, move |cx| {
        let t = Instant::now();
        while !src.is_final() && src.len() < SAMPLE as u64 && t.elapsed() < SPOOL_WAIT {
            if cx.cancel.is_cancelled() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let res = probe(&*src, &name, tld).map_err(|e| e.to_string());
        let _ = cx.sink.send(Box::new(ProbedMsg { doc, res }), &cx.cancel);
    });
}

/// Источник открыт (с детектом, кроме Spool).
pub(crate) struct OpenedMsg {
    doc: DocId,
    name: String,
    tld: Option<&'static [u8]>,
    res: Result<(crate::source::Opened, Option<Probe>), String>,
}

impl std::fmt::Debug for OpenedMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedMsg")
            .field("doc", &self.doc)
            .field("ok", &self.res.is_ok())
            .finish_non_exhaustive()
    }
}

impl Msg for OpenedMsg {
    fn name(&self) -> &'static str {
        "view.opened"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let OpenedMsg {
            doc,
            name,
            tld,
            res,
        } = *self;
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let Some(d) = view.viewer_mut(doc) else {
            // Документ закрыли раньше: источник и насос Spool уходят здесь.
            return;
        };
        match res {
            Err(e) => {
                fail_open(cx, doc, &e);
            }
            Ok((opened, probe)) => {
                let src = opened.src.clone();
                d.attach(opened.src);
                if let Some(pump) = opened.pump {
                    rt.spawn_dedicated(format!("view.spool.{}", doc.0), d.cancel.clone(), |_| {
                        pump.run();
                    });
                }
                if let Some(p) = probe {
                    d.apply_probe(p, rt);
                    utf32_toast(cx, doc);
                } else {
                    let cancel = d.cancel.clone();
                    spawn_probe(rt, doc, src, name, tld, cancel);
                }
            }
        }
    }
}

/// UTF-32 не поддерживается — показан Hex (S7-08): тост, кроме быстрого просмотра.
fn utf32_toast(cx: &mut Cx<'_>, doc: DocId) {
    let utf32 = cx
        .state
        .ext
        .get::<ViewState>()
        .and_then(|v| v.viewer(doc))
        .is_some_and(|d| !d.quick && d.detected.is_some_and(|x| x.utf32));
    if utf32 {
        cx.toast(Level::Info, urfin_i18n::current().get("ui-view-utf32"));
    }
}

/// Детект Spool готов.
#[derive(Debug)]
pub(crate) struct ProbedMsg {
    doc: DocId,
    res: Result<Probe, String>,
}

impl Msg for ProbedMsg {
    fn name(&self) -> &'static str {
        "view.probed"
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
            Ok(p) => {
                d.apply_probe(p, rt);
                utf32_toast(cx, self.doc);
            }
            Err(e) => fail_open(cx, self.doc, &e),
        }
    }
}

/// Открыть не удалось: тост, документ закрывается.
fn fail_open(cx: &mut Cx<'_>, doc: DocId, err: &str) {
    tracing::warn!(error = %err, "viewer open failed");
    if let Some(view) = cx.state.ext.get_mut::<ViewState>()
        && let Some(crate::Doc::Viewer(mut d)) = view.remove(doc)
    {
        d.close();
    }
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("id", "viewer.open");
    args.set("error", err.to_owned());
    let text = urfin_i18n::current().fmt("core-command-failed", &args);
    cx.toast(Level::Error, text);
    crate::handler::refocus(cx);
}

/// Открывает документ: `Opening` в `ViewState`, задача `view.open`. Возвращает id.
#[allow(clippy::too_many_arguments, reason = "параметры открытия из команды")]
pub(crate) fn open(
    view: &mut ViewState,
    rt: &Runtime,
    reg: &Arc<VfsRegistry>,
    loc: Location,
    placement: Placement,
    quick: bool,
    spool_dir: Option<std::path::PathBuf>,
    tld: Option<&'static [u8]>,
) -> DocId {
    let id = view.alloc_id();
    let cancel = rt.token().child();
    let mut d = ViewerDoc::new(
        id,
        loc.clone(),
        placement,
        quick,
        &view.viewer,
        cancel.clone(),
    );
    if view.viewer.remember_pos {
        d.memo = view.history.get(&loc).cloned();
    }
    let opts = SourceOpts {
        follow: false,
        mmap: if view.viewer.mmap {
            crate::source::MmapPolicy::Auto
        } else {
            crate::source::MmapPolicy::Off
        },
        volume: None,
        spool_dir,
    };
    view.insert(crate::Doc::Viewer(d));
    let job = OpenJob {
        doc: id,
        loc,
        opts,
        tld,
        reg: reg.clone(),
    };
    spawn_open(rt, job, cancel);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_skips_same_encoding() {
        let ring = ["auto", "utf-8", "windows-1251", "ibm866"];
        let res = |l: &str| {
            if l == "auto" {
                Some(Enc {
                    auto: true,
                    ..Enc::utf8()
                })
            } else {
                Encoding::for_label(l.as_bytes()).map(Enc::new)
            }
        };
        let auto_utf8 = Enc {
            auto: true,
            ..Enc::utf8()
        };
        assert_eq!(
            ring_next(&ring, &auto_utf8, res).as_deref(),
            Some("windows-1251")
        );
        let cp1251 = Enc::new(encoding_rs::WINDOWS_1251);
        assert_eq!(ring_next(&ring, &cp1251, res).as_deref(), Some("ibm866"));
        let cp866 = Enc::new(encoding_rs::IBM866);
        assert_eq!(ring_next(&ring, &cp866, res).as_deref(), Some("auto"));
        let koi = Enc::new(encoding_rs::KOI8_R);
        assert_eq!(ring_next(&ring, &koi, res).as_deref(), Some("auto"));
    }

    #[test]
    fn bom_lengths() {
        assert_eq!(bom_len(encoding_rs::UTF_8, b"\xEF\xBB\xBFx"), 3);
        assert_eq!(bom_len(encoding_rs::UTF_16LE, b"\xFF\xFEx\0"), 2);
        assert_eq!(bom_len(encoding_rs::UTF_16BE, b"\xFF\xFEx\0"), 0);
        assert_eq!(bom_len(encoding_rs::WINDOWS_1251, b"\xEF\xBB\xBF"), 0);
    }

    #[test]
    fn menu_has_every_group() {
        let m = codepage_menu();
        assert_eq!(m[0].1[0], encoding_rs::UTF_8);
        assert!(m[1].1.contains(&encoding_rs::IBM866));
        assert!(!m[3].1.contains(&encoding_rs::REPLACEMENT));
    }
}
