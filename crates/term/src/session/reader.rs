//! Поток reader (S6 §1.1): блокирующее чтение PTY → пре-сканер → эмулятор под
//! блокировкой (не больше одного чтения, ≤ 64 КБ) → `dirty` и `poke`.
//!
//! Незавершённый UTF-8 в конце чтения переносится в следующее: vte 0.15.0
//! теряет ASCII после символа, разрезанного между `advance`, если следом идёт
//! ещё один незавершённый (`advance_partial_utf8` возвращает `valid_bytes`
//! вместо длины первого символа).

use std::io::{ErrorKind, Read};
use std::time::Instant;

use alacritty_terminal::grid::Dimensions;
use crossbeam_channel::{Receiver, Sender};

use super::scan::{ModeWatch, Piece, Scanner};
use super::{Mark, SessionEvent, SessionSink, WriterCmd};
use crate::vt::shared::TermShared;

/// Чтение за раз (S6 §1.1).
pub const READ_BUF: usize = 64 * 1024;

/// Длина начала `b`, не обрывающегося внутри символа UTF-8 (хвост ≤ 3 байт).
pub(crate) fn utf8_complete(b: &[u8]) -> usize {
    let n = b.len();
    for back in 1..=n.min(3) {
        let c = b[n - back];
        if c & 0xC0 == 0x80 {
            continue; // продолжение — искать ведущий
        }
        let need = match c {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return n, // ASCII или мусор — резать нечего
        };
        return if back < need { n - back } else { n };
    }
    n
}

struct Ctx<'a> {
    shared: &'a TermShared,
    listener_events: &'a Receiver<SessionEvent>,
    events: Vec<SessionEvent>,
    modes: ModeWatch,
}

impl Ctx<'_> {
    /// Байты в эмулятор; `(есть что показать, срок DEC 2026)`.
    fn process(&mut self, scanner: &mut Scanner, bytes: &[u8]) -> (bool, Option<Instant>) {
        if let Some(on) = self.modes.feed(bytes) {
            self.shared
                .win32_input
                .store(on, std::sync::atomic::Ordering::Release);
        }
        let mut vt = self.shared.vt.lock();
        let vt = &mut *vt;
        let (term, processor) = (&mut vt.term, &mut vt.processor);
        let (events, listener) = (&mut self.events, self.listener_events);
        scanner.feed(bytes, &mut |p| match p {
            Piece::Bytes(b) => {
                processor.advance(term, b);
                // События эмулятора — сразу: порядок с OSC как в потоке.
                events.extend(listener.try_iter());
            }
            Piece::Event(event) => {
                let at = Mark {
                    line: term.grid().cursor.point.line.0,
                    history: u32::try_from(term.grid().history_size()).unwrap_or(u32::MAX),
                    col: u16::try_from(term.grid().cursor.point.column.0).unwrap_or(u16::MAX),
                };
                events.push(SessionEvent::Osc { event, at });
            }
        });
        // Всё прочитанное ушло в буфер DEC 2026 — показывать нечего.
        let shown = processor.sync_bytes_count() < bytes.len();
        (shown, processor.sync_timeout().sync_timeout())
    }

    fn flush(&mut self, sink: &dyn SessionSink) {
        // Ещё события — от `stop_sync` во writer.
        self.events.extend(self.listener_events.try_iter());
        for ev in self.events.drain(..) {
            sink.event(ev);
        }
    }
}

/// Читать до EOF или ошибки.
pub(crate) fn run(
    mut src: impl Read,
    shared: &TermShared,
    mut scanner: Scanner,
    listener_events: &Receiver<SessionEvent>,
    writer: &Sender<WriterCmd>,
    sink: &dyn SessionSink,
) {
    let mut cx = Ctx {
        shared,
        listener_events,
        events: Vec::new(),
        modes: ModeWatch::default(),
    };
    let mut buf = vec![0u8; READ_BUF];
    let mut carry = 0;
    let mut sync_sent: Option<Instant> = None;
    loop {
        let n = match src.read(&mut buf[carry..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            // ERROR_BROKEN_PIPE — псевдоконсоль закрыта.
            Err(_) => break,
        };
        let total = carry + n;
        let ready = utf8_complete(&buf[..total]);
        let (shown, sync) = cx.process(&mut scanner, &buf[..ready]);
        buf.copy_within(ready..total, 0);
        carry = total - ready;
        if shown && shared.mark_dirty() {
            sink.poke();
        }
        // Таймаут DEC 2026 ведёт writer: reader может надолго уснуть в read.
        if let Some(at) = sync
            && sync_sent != Some(at)
        {
            let _ = writer.send(WriterCmd::Sync(at));
        }
        sync_sent = sync;
        cx.flush(sink);
    }
    if carry > 0 {
        cx.process(&mut scanner, &buf[..carry]);
        if shared.mark_dirty() {
            sink.poke();
        }
    }
    cx.flush(sink);
}
