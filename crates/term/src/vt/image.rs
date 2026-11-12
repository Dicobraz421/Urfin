//! Полное состояние эмулятора (F03 §3.6): ключевой кадр записи сеанса. Перемотка обязана
//! дать тот же эмулятор, что проигрывание с начала, поэтому кадр хранит не экран, а всё,
//! от чего зависит дальнейший вывод: сетки с атрибутами и признаками переноса, оба
//! курсора с пером и таблицами символов, режимы, область DECSTBM, табуляции, палитру
//! OSC, вид курсора, активную таблицу символов, символ для REP (`CSI b`).
//!
//! `alacritty Term` не клонируется и часть полей не отдаёт. Они снимаются так:
//! - область прокрутки и табуляции — пробой через публичный `Handler` на самом эмуляторе
//!   (движения курсора, клетки возвращаются на место), см. `region` и `tab_stops`;
//! - активная таблица и символ REP живут в разборе — их ведёт [`Tracker`] своим
//!   `vte::Parser` по тем же байтам;
//! - неактивная сетка видна только через `swap_alt`. Основной экран за альтернативным
//!   снимается один раз, при входе (`Base`): вывод до него не доходит, а изменения от
//!   resize воспроизводятся теми же resize при восстановлении.
//!
//! Посреди последовательности и внутри DEC 2026 кадр не снимается ([`Tracker::capture`]):
//! незавершённый разбор в кадр не переносится, ключ берётся на следующей границе.
//!
//! Не восстанавливаются: заголовок и его стек, стек режимов клавиатуры Kitty (на вывод не
//! влияют). Два следа смены размера тоже вне кадра, клетки экрана в кадре точные:
//! - стоп табуляции в самой последней колонке — без расширения окна не виден, остаётся
//!   значение по умолчанию;
//! - служебное у строк `alacritty` (граница занятых клеток `Row::occ`, резерв сетки): после
//!   смены размера он оставляет на экране фон прежней очистки цветным пером и после
//!   `CSI 2J`. Из кадра следующая очистка убирает этот фон полностью.
//!
//! Всё это — для прохода по записи: путь вывода и снимка экрана здесь не задействован.

use std::mem;
use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::grid::{Cursor, Dimensions, Grid, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::color::COUNT;
use alacritty_terminal::vte::ansi::{
    CharsetIndex, CursorStyle, Handler, Mode, NamedMode, NamedPrivateMode, PrivateMode, Rgb,
    TabulationClearMode, Timeout,
};
use alacritty_terminal::vte::{Params, Parser, Perform};

use super::shared::{TermShared, Vt, VtListener};
use crate::sys::pty::PtySize;

/// Шаг табуляции по умолчанию (`TabStops::new` в `alacritty_terminal`).
const TAB_EVERY: usize = 8;

/// Режимы DEC, которые восстанавливаются один к одному.
const PRIVATE_MODES: [(TermMode, NamedPrivateMode); 13] = [
    (TermMode::SHOW_CURSOR, NamedPrivateMode::ShowCursor),
    (TermMode::APP_CURSOR, NamedPrivateMode::CursorKeys),
    (TermMode::LINE_WRAP, NamedPrivateMode::LineWrap),
    (TermMode::ORIGIN, NamedPrivateMode::Origin),
    (TermMode::BRACKETED_PASTE, NamedPrivateMode::BracketedPaste),
    (TermMode::FOCUS_IN_OUT, NamedPrivateMode::ReportFocusInOut),
    (
        TermMode::ALTERNATE_SCROLL,
        NamedPrivateMode::AlternateScroll,
    ),
    (TermMode::URGENCY_HINTS, NamedPrivateMode::UrgencyHints),
    (
        TermMode::MOUSE_REPORT_CLICK,
        NamedPrivateMode::ReportMouseClicks,
    ),
    (
        TermMode::MOUSE_DRAG,
        NamedPrivateMode::ReportCellMouseMotion,
    ),
    (
        TermMode::MOUSE_MOTION,
        NamedPrivateMode::ReportAllMouseMotion,
    ),
    (TermMode::SGR_MOUSE, NamedPrivateMode::SgrMouse),
    (TermMode::UTF8_MOUSE, NamedPrivateMode::Utf8Mouse),
];

/// Сетка: клетки кусками одного начертания и оба курсора.
#[derive(Clone, Debug)]
struct Screen {
    /// Образцы клеток; символ образца — пробел.
    pens: Box<[Cell]>,
    /// (образец, символов): куски подряд по всем строкам.
    runs: Box<[(u32, u32)]>,
    /// Конец кусков каждой строки в `runs`; хвост строки — пустые клетки.
    rows: Box<[u32]>,
    text: Box<str>,
    cursor: Cursor<Cell>,
    saved: Cursor<Cell>,
}

impl Screen {
    fn of(g: &Grid<Cell>) -> Screen {
        let blank = Cell::default();
        let mut pens: Vec<Cell> = Vec::new();
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut rows = Vec::with_capacity(g.screen_lines());
        let mut text = String::new();
        for l in 0..g.screen_lines() {
            let row = &g[line(l)];
            let used = (0..g.columns())
                .rev()
                .find(|&c| row[Column(c)] != blank)
                .map_or(0, |c| c + 1);
            for c in 0..used {
                let cell = &row[Column(c)];
                text.push(cell.c);
                let mut pen = cell.clone();
                pen.c = ' ';
                match runs.last_mut() {
                    Some((p, n)) if c > 0 && pens[*p as usize] == pen => *n += 1,
                    _ => {
                        // Ближние образцы: типичный экран — несколько цветов на строку.
                        let found = pens.iter().rev().take(32).position(|p| *p == pen);
                        let i = found.map_or(pens.len(), |back| pens.len() - 1 - back);
                        if found.is_none() {
                            pens.push(pen);
                        }
                        runs.push((i as u32, 1));
                    }
                }
            }
            rows.push(runs.len() as u32);
        }
        Screen {
            pens: pens.into(),
            runs: runs.into(),
            rows: rows.into(),
            text: text.into(),
            cursor: g.cursor.clone(),
            saved: g.saved_cursor.clone(),
        }
    }

    /// Клетки и курсоры в сетку того же размера с любым содержимым.
    fn write(&self, g: &mut Grid<Cell>) {
        let cols = g.columns();
        let mut chars = self.text.chars();
        let mut from = 0;
        for (l, &end) in self.rows.iter().enumerate() {
            let mut row = Vec::with_capacity(cols);
            for &(pen, n) in &self.runs[from..end as usize] {
                for _ in 0..n {
                    let mut cell = self.pens[pen as usize].clone();
                    cell.c = chars.next().unwrap_or(' ');
                    row.push(cell);
                }
            }
            row.resize_with(cols, Cell::default);
            g[line(l)] = Row::from_vec(row, cols);
            from = end as usize;
        }
        g.cursor = self.cursor.clone();
        g.saved_cursor = self.saved.clone();
    }

    fn bytes(&self) -> usize {
        mem::size_of::<Screen>()
            + self.pens.len() * mem::size_of::<Cell>()
            + self.runs.len() * mem::size_of::<(u32, u32)>()
            + self.rows.len() * mem::size_of::<u32>()
            + self.text.len()
    }
}

fn line(l: usize) -> Line {
    Line(i32::try_from(l).unwrap_or(i32::MAX))
}

/// Основной экран при входе на альтернативный: размер и сетка.
#[derive(Clone, Debug)]
struct Base {
    size: PtySize,
    screen: Screen,
}

/// Ключевой кадр: всё состояние эмулятора.
#[derive(Clone, Debug)]
pub struct Image {
    /// Активная сетка.
    screen: Screen,
    /// На альтернативном экране — основной при входе и resize после входа.
    base: Option<Arc<Base>>,
    resizes: Box<[PtySize]>,
    /// Сохранённый курсор альтернативной сетки, пока она не активна: следующий её сеанс
    /// видит его по DECRC.
    alt_saved: Cursor<Cell>,
    size: PtySize,
    mode: TermMode,
    /// Область прокрутки, `start..end` в строках экрана.
    region: (i32, i32),
    /// Стопы табуляции.
    tabs: Box<[u16]>,
    /// Цвета палитры, заданные OSC.
    colors: Box<[(u16, Rgb)]>,
    style: CursorStyle,
    /// Активна G1 (SO).
    shift: bool,
    /// Последний выведенный символ: его повторяет `CSI b`.
    last: Option<char>,
}

impl Image {
    /// Размер нового эмулятора для [`Image::restore`].
    pub fn start(&self) -> PtySize {
        self.base.as_ref().map_or(self.size, |b| b.size)
    }

    /// Кадр снят на альтернативном экране.
    pub fn alt(&self) -> bool {
        self.base.is_some()
    }

    /// Память кадра, байт; основной экран общий у кадров сеанса альтернативного экрана и
    /// сюда не входит.
    pub fn bytes(&self) -> usize {
        mem::size_of::<Image>()
            + self.screen.bytes()
            + self.tabs.len() * mem::size_of::<u16>()
            + self.resizes.len() * mem::size_of::<PtySize>()
            + self.colors.len() * mem::size_of::<(u16, Rgb)>()
    }

    /// Восстанавливает состояние в новый эмулятор размера [`Image::start`].
    pub fn restore(&self, t: &TermShared) {
        {
            let mut vt = t.vt.lock();
            let vt = &mut *vt;
            if let Some(c) = self.last {
                let mut utf = [0; 4];
                vt.processor
                    .advance(&mut vt.term, c.encode_utf8(&mut utf).as_bytes());
                // REP помнит разбор, а не сетка: символ остаётся, вывод — нет.
                vt.term.reset_state();
            }
            let term = &mut vt.term;
            if let Some(b) = &self.base {
                b.screen.write(term.grid_mut());
                term.swap_alt();
            } else {
                term.swap_alt();
                term.grid_mut().saved_cursor = self.alt_saved.clone();
                term.swap_alt();
            }
        }
        for &s in &self.resizes {
            t.resize(s);
        }
        let mut vt = t.vt.lock();
        let term = &mut vt.term;
        for (bit, m) in PRIVATE_MODES {
            match (term.mode().contains(bit), self.mode.contains(bit)) {
                (false, true) => term.set_private_mode(PrivateMode::Named(m)),
                (true, false) => term.unset_private_mode(PrivateMode::Named(m)),
                _ => {}
            }
        }
        for (bit, m) in [
            (TermMode::INSERT, NamedMode::Insert),
            (TermMode::LINE_FEED_NEW_LINE, NamedMode::LineFeedNewLine),
        ] {
            if self.mode.contains(bit) {
                term.set_mode(Mode::Named(m));
            }
        }
        if self.mode.contains(TermMode::APP_KEYPAD) {
            term.set_keypad_application_mode();
        }
        set_tab_stops(term, &self.tabs);
        set_region(term, self.region);
        for &(i, c) in &self.colors {
            term.set_color(usize::from(i), c);
        }
        term.set_cursor_style(Some(self.style));
        if self.shift {
            term.set_active_charset(CharsetIndex::G1);
        }
        self.screen.write(term.grid_mut());
        drop(vt);
        t.mark_dirty();
    }
}

/// Проба на живом эмуляторе: курсор возвращается на место.
fn keeping_cursor<R>(term: &mut Term<VtListener>, f: impl FnOnce(&mut Term<VtListener>) -> R) -> R {
    let cursor = term.grid().cursor.clone();
    let r = f(term);
    term.grid_mut().cursor = cursor;
    r
}

/// Область прокрутки: в режиме ORIGIN `goto` ограничен ею.
fn region(term: &mut Term<VtListener>) -> (i32, i32) {
    let rows = i32::try_from(term.screen_lines()).unwrap_or(i32::MAX);
    keeping_cursor(term, |term| {
        let origin = term.mode().contains(TermMode::ORIGIN);
        if !origin {
            term.set_private_mode(PrivateMode::Named(NamedPrivateMode::Origin));
        }
        term.goto(0, 0);
        let top = term.grid().cursor.point.line.0;
        term.goto(rows, 0);
        let bottom = term.grid().cursor.point.line.0 + 1;
        if !origin {
            term.unset_private_mode(PrivateMode::Named(NamedPrivateMode::Origin));
        }
        if top == rows - 1 && is_empty_region(term, rows) {
            return (rows, rows);
        }
        (top, bottom)
    })
}

/// Нижняя строка или пустая область (`CSI 30;40r` на 24 строках): `goto` их не различает.
/// RI в верху области прокручивает её, вне области — нет; строка возвращается на место.
fn is_empty_region(term: &mut Term<VtListener>, rows: i32) -> bool {
    let last = Line(rows - 1);
    let row = term.grid()[last].clone();
    term.grid_mut()[last][Column(0)].c = 'x';
    term.grid_mut().cursor.point.line = last;
    term.reverse_index();
    let empty = term.grid()[last][Column(0)].c == 'x';
    term.grid_mut()[last] = row;
    empty
}

fn set_region(term: &mut Term<VtListener>, (top, bottom): (i32, i32)) {
    let rows = i32::try_from(term.screen_lines()).unwrap_or(i32::MAX);
    if (top, bottom) == (0, rows) {
        return;
    }
    let (top, bottom) = (top.unsigned_abs() as usize, bottom.unsigned_abs() as usize);
    // Пустая область получается только обрезкой по экрану — так и задаётся.
    term.set_scrolling_region(top + 1, Some(bottom.max(top + 2)));
}

/// Стопы табуляции, кроме последней колонки: шагом назад от неё (CBT не зависит от
/// режимов и клеток не трогает).
fn tab_stops(term: &mut Term<VtListener>) -> Box<[u16]> {
    keeping_cursor(term, |term| {
        let mut stops = Vec::new();
        let mut col = term.last_column();
        term.grid_mut().cursor.point.column = col;
        loop {
            term.move_backward_tabs(1);
            let now = term.grid().cursor.point.column;
            if now == col {
                break;
            }
            col = now;
            stops.push(col.0 as u16);
        }
        stops.into()
    })
}

fn set_tab_stops(term: &mut Term<VtListener>, stops: &[u16]) {
    keeping_cursor(term, |term| {
        for c in 0..term.columns() - 1 {
            let on = stops.contains(&(c as u16));
            if on != c.is_multiple_of(TAB_EVERY) {
                term.grid_mut().cursor.point.column = Column(c);
                if on {
                    term.set_horizontal_tabstop();
                } else {
                    term.clear_tabs(TabulationClearMode::Current);
                }
            }
        }
    });
}

fn settled(vt: &Vt) -> bool {
    vt.processor.sync_bytes_count() == 0 && !vt.processor.sync_timeout().pending_timeout()
}

/// Основной экран за активным альтернативным. Обратный `swap_alt` чистит альтернативную
/// сетку (она возвращается копией) и приравнивает сохранённый курсор основной к её
/// курсору: при входе они и так равны, а снимок делается до любого resize.
fn primary_behind_alt(term: &mut Term<VtListener>) -> Screen {
    let alt = term.grid().clone();
    term.swap_alt();
    let screen = Screen::of(term.grid());
    term.swap_alt();
    *term.grid_mut() = alt;
    screen
}

/// Сохранённый курсор неактивной альтернативной сетки. Вход на неё красит её пером курсора
/// и портит сохранённый курсор основной: перо на время чистое, курсоры возвращаются.
/// Содержимое сетки не важно: следующий вход очистит её сам.
fn saved_of_alt(term: &mut Term<VtListener>) -> Cursor<Cell> {
    let cursor = mem::take(&mut term.grid_mut().cursor);
    let saved = term.grid().saved_cursor.clone();
    term.swap_alt();
    let alt = term.grid().saved_cursor.clone();
    term.swap_alt();
    let g = term.grid_mut();
    g.cursor = cursor;
    g.saved_cursor = saved;
    alt
}

/// Что видно только в разборе потока.
#[derive(Debug, Default)]
struct Probe {
    last: Option<char>,
    shift: bool,
    /// Был выход с альтернативного экрана (DECRST 1049 или RIS).
    left_alt: bool,
    /// Байт завершил последовательность или вывел символ.
    done: bool,
}

impl Perform for Probe {
    fn print(&mut self, c: char) {
        self.last = Some(c);
        self.done = true;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0e => self.shift = true,
            0x0f => self.shift = false,
            _ => {}
        }
    }

    fn unhook(&mut self) {
        self.done = true;
    }

    fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {
        self.done = true;
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if !ignore && action == 'l' && intermediates == b"?" && params.iter().any(|p| p[0] == 1049)
        {
            self.left_alt = true;
        }
        self.done = true;
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _: bool, byte: u8) {
        if byte == b'c' && intermediates.is_empty() {
            self.shift = false;
            self.left_alt = true;
        }
        self.done = true;
    }
}

/// Ведёт поток записи: вывод идёт через него, а [`Tracker::capture`] снимает кадр.
pub struct Tracker {
    parser: Parser,
    probe: Probe,
    /// Разбор на границе последовательностей.
    ground: bool,
    /// Основной экран за активным альтернативным и resize после снятия.
    base: Option<Arc<Base>>,
    resizes: Vec<PtySize>,
    alt: bool,
}

impl std::fmt::Debug for Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracker")
            .field("ground", &self.ground)
            .field("alt", &self.alt)
            .finish_non_exhaustive()
    }
}

impl Default for Tracker {
    fn default() -> Tracker {
        Tracker {
            parser: Parser::new(),
            probe: Probe::default(),
            ground: true,
            base: None,
            resizes: Vec::new(),
            alt: false,
        }
    }
}

impl Tracker {
    /// Вывод в эмулятор. Границы вызовов — границы событий: только на них снимаются кадры.
    pub fn feed(&mut self, t: &TermShared, bytes: &[u8]) {
        t.inject(bytes);
        self.parse(bytes);
        let mut vt = t.vt.lock();
        let vt = &mut *vt;
        let alt = vt.term.mode().contains(TermMode::ALT_SCREEN);
        // Внутри DEC 2026 выход ещё в буфере: дождаться применения.
        let left = settled(vt) && mem::take(&mut self.probe.left_alt);
        if alt && (!self.alt || left) {
            let size = PtySize {
                cols: vt.term.columns() as u16,
                rows: vt.term.screen_lines() as u16,
            };
            let screen = primary_behind_alt(&mut vt.term);
            self.base = Some(Arc::new(Base { size, screen }));
            self.resizes.clear();
        } else if !alt {
            self.base = None;
            self.resizes.clear();
        }
        self.alt = alt;
    }

    /// Новый размер эмулятора.
    pub fn resize(&mut self, t: &TermShared, size: PtySize) {
        if self.base.is_some() {
            self.resizes.push(size);
        }
        t.resize(size);
    }

    /// Кадр текущего состояния. `None` — разбор посреди последовательности или внутри
    /// DEC 2026: такое состояние кадром не передать.
    pub fn capture(&self, t: &TermShared) -> Option<Image> {
        if !self.ground {
            return None;
        }
        let mut vt = t.vt.lock();
        if !settled(&vt) {
            return None;
        }
        let term = &mut vt.term;
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        if alt != self.base.is_some() {
            return None;
        }
        let colors: Vec<(u16, Rgb)> = (0..COUNT)
            .filter_map(|i| term.colors()[i].map(|c| (i as u16, c)))
            .collect();
        Some(Image {
            screen: Screen::of(term.grid()),
            base: self.base.clone(),
            resizes: self.resizes.as_slice().into(),
            alt_saved: if alt {
                Cursor::default()
            } else {
                saved_of_alt(term)
            },
            size: PtySize {
                cols: term.columns() as u16,
                rows: term.screen_lines() as u16,
            },
            mode: *term.mode(),
            region: region(term),
            tabs: tab_stops(term),
            colors: colors.into(),
            style: term.cursor_style(),
            shift: self.probe.shift,
            last: self.probe.last,
        })
    }

    /// Разбор идёт целиком, на границу смотрит только конец: последний байт выше C0
    /// завершил последовательность, если разбор откликнулся на него, а управляющие за ним
    /// положения не меняют, кроме ESC, CAN/SUB и конца строки OSC.
    fn parse(&mut self, bytes: &[u8]) {
        let tail = bytes.iter().rposition(|&b| b >= 0x20).map_or(0, |i| i + 1);
        if tail > 0 {
            self.parser.advance(&mut self.probe, &bytes[..tail - 1]);
            self.probe.done = false;
            self.parser.advance(&mut self.probe, &bytes[tail - 1..tail]);
            self.ground = self.probe.done;
        }
        for b in &bytes[tail..] {
            self.probe.done = false;
            self.parser
                .advance(&mut self.probe, std::slice::from_ref(b));
            match b {
                0x1b => self.ground = false,
                0x18 | 0x1a => self.ground = true,
                _ => self.ground |= self.probe.done,
            }
        }
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
