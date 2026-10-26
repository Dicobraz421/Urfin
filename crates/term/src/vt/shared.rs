//! Общее состояние сеанса (S6 §1.2): эмулятор под `FairMutex`, флаг «есть
//! непрорисованное», размер. Пишет reader, читает UI (снапшот), `stop_sync` по
//! таймауту делает writer.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use crossbeam_channel::Sender;

use crate::session::{SessionEvent, WriterCmd};
use crate::sys::conpty::PtySize;

/// Размер сетки для `Term` (`Dimensions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize(pub PtySize);

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows.max(1))
    }

    fn columns(&self) -> usize {
        usize::from(self.0.cols.max(1))
    }
}

/// Слушатель эмулятора: ответы терминала (DSR, DA) — во writer, остальное —
/// событиями сеанса. Вызывается под блокировкой, поэтому только неблокирующие
/// отправки в неограниченные каналы.
#[derive(Debug)]
pub struct VtListener {
    writer: Sender<WriterCmd>,
    events: Sender<SessionEvent>,
}

impl VtListener {
    pub(crate) fn new(writer: Sender<WriterCmd>, events: Sender<SessionEvent>) -> VtListener {
        VtListener { writer, events }
    }
}

impl EventListener for VtListener {
    fn send_event(&self, event: Event) {
        let ev = match event {
            Event::PtyWrite(s) => {
                let _ = self.writer.send(WriterCmd::Bytes(s.into_bytes()));
                return;
            }
            Event::Title(t) => SessionEvent::Title(Some(t)),
            Event::ResetTitle => SessionEvent::Title(None),
            Event::ClipboardStore(_, text) => SessionEvent::Clipboard(text),
            Event::Bell => SessionEvent::Bell,
            // Цвета палитры и размер в пикселях знает рендер (S6-06); остальное
            // (Wakeup, курсор мыши, мигание) UI берёт из снапшота.
            _ => return,
        };
        let _ = self.events.send(ev);
    }
}

/// Эмулятор и парсер: `Processor` хранится рядом с `Term` ради DEC 2026.
pub struct Vt {
    pub term: Term<VtListener>,
    pub processor: Processor,
}

impl std::fmt::Debug for Vt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vt").finish_non_exhaustive()
    }
}

/// Состояние сеанса, общее для reader, writer и UI.
pub struct TermShared {
    pub vt: FairMutex<Vt>,
    /// Есть непрорисованные изменения; UI сбрасывает при снапшоте.
    pub dirty: AtomicBool,
    /// `cols << 16 | rows`.
    pub size: AtomicU32,
    /// Растёт при resize и reset (ключ кэшей рендера).
    pub epoch: AtomicU64,
    /// `ConPTY` включил win32-input-mode (`CSI ?9001h`, S6-20); ведёт reader.
    pub win32_input: AtomicBool,
}

impl std::fmt::Debug for TermShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermShared")
            .field("dirty", &self.dirty)
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

/// Настройки эмулятора: скроллбэк и `term.osc52` (`off` | `copy`; вставку из
/// буфера по OSC 52 не даём).
pub fn term_config(scrollback: u32, osc52: &str) -> Config {
    Config {
        scrolling_history: scrollback as usize,
        osc52: if osc52 == "off" {
            Osc52::Disabled
        } else {
            Osc52::OnlyCopy
        },
        ..Config::default()
    }
}

fn pack(s: PtySize) -> u32 {
    u32::from(s.cols) << 16 | u32::from(s.rows)
}

impl TermShared {
    pub fn new(size: PtySize, scrollback: u32, osc52: &str, listener: VtListener) -> TermShared {
        let config = term_config(scrollback, osc52);
        TermShared {
            vt: FairMutex::new(Vt {
                term: Term::new(config, &GridSize(size), listener),
                processor: Processor::new(),
            }),
            dirty: AtomicBool::new(true),
            size: AtomicU32::new(pack(size)),
            epoch: AtomicU64::new(0),
            win32_input: AtomicBool::new(false),
        }
    }

    pub fn size(&self) -> PtySize {
        let v = self.size.load(Ordering::Acquire);
        PtySize {
            cols: (v >> 16) as u16,
            rows: v as u16,
        }
    }

    /// Новый размер сетки (reflow в эмуляторе). PTY меняет writer.
    pub fn resize(&self, size: PtySize) {
        self.vt.lock().term.resize(GridSize(size));
        self.size.store(pack(size), Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.dirty.store(true, Ordering::Release);
    }

    /// Новый предел скроллбэка (фоновый сеанс — меньше, S6 §1.2): лишняя
    /// история отбрасывается сразу.
    pub fn set_scrollback(&self, scrollback: u32, osc52: &str) {
        self.vt
            .lock()
            .term
            .set_options(term_config(scrollback, osc52));
    }

    /// Свой текст в сетку, как будто его вывел процесс (строка «процесс завершён»).
    pub fn inject(&self, bytes: &[u8]) {
        {
            let mut vt = self.vt.lock();
            let vt = &mut *vt;
            vt.processor.advance(&mut vt.term, bytes);
        }
        self.dirty.store(true, Ordering::Release);
    }

    /// Режимы терминала для кодировщика ввода.
    pub fn input_modes(&self) -> crate::vt::input::Modes {
        let vt = self.vt.lock();
        let m = vt.term.mode();
        crate::vt::input::Modes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            focus_events: m.contains(TermMode::FOCUS_IN_OUT),
            sgr_mouse: m.contains(TermMode::SGR_MOUSE),
            mouse_report: m.intersects(TermMode::MOUSE_MODE),
            mouse_drag: m.contains(TermMode::MOUSE_DRAG),
            mouse_motion: m.contains(TermMode::MOUSE_MOTION),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            alt_scroll: m.contains(TermMode::ALTERNATE_SCROLL),
            win32_input: self.win32_input.load(Ordering::Acquire),
        }
    }

    /// Включён альтернативный экран (vim, mc, less): вид «экран» (S6 §3.2).
    pub fn alt_screen(&self) -> bool {
        self.vt.lock().term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Отметить изменения; `true` — флаг поднят этим вызовом (нужен poke).
    pub fn mark_dirty(&self) -> bool {
        !self.dirty.swap(true, Ordering::AcqRel)
    }

    /// Для UI: были ли изменения с прошлого кадра (флаг сбрасывается).
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }
}
