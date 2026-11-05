//! Поток плеера документа (F03 §3.6): часы, перемотка и экран — здесь, UI получает
//! готовые снимки сообщениями. Кадров не больше 30 в секунду; кадр без изменений экрана
//! несёт только время.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use urfin_core::{CancelToken, Cx, Msg, Runtime};
use urfin_term::vt::grid::{Defaults, Snapshot};

use super::replay::Replay;
use crate::{DocId, ViewState};

/// Шаг часов при проигрывании.
const TICK: Duration = Duration::from_millis(33);
/// Шаг проверки отмены на паузе.
const IDLE: Duration = Duration::from_millis(200);

/// Команда плееру.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Cmd {
    /// Перемотка; `seq` — номер, его вернёт кадр.
    Seek { ms: u64, seq: u64 },
    Play(bool),
    Speed(u8),
    Colors(Defaults),
}

/// Кадр плеера.
#[derive(Debug)]
pub(crate) struct FrameMsg {
    pub doc: DocId,
    pub ms: u64,
    pub playing: bool,
    /// Номер последней выполненной перемотки.
    pub seq: u64,
    /// Экран; `None` — не менялся.
    pub frame: Option<Arc<Snapshot>>,
}

impl Msg for FrameMsg {
    fn name(&self) -> &'static str {
        "view.cast.frame"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(d) = view.viewer_mut(self.doc) {
            d.on_cast_frame(*self);
        }
    }
}

/// Часы плеера: момент записи `base` был в `at`.
struct Clock {
    at: Instant,
    base: u64,
    speed: u32,
}

impl Clock {
    fn now(&self) -> u64 {
        let ms = self.at.elapsed().as_millis() * u128::from(self.speed);
        self.base
            .saturating_add(u64::try_from(ms).unwrap_or(u64::MAX))
    }

    fn set(&mut self, ms: u64) {
        self.at = Instant::now();
        self.base = ms;
    }
}

/// Поток плеера; живёт до закрытия документа.
pub(crate) fn spawn(
    rt: &Runtime,
    doc: DocId,
    mut replay: Replay,
    rx: Receiver<Cmd>,
    colors: Defaults,
    cancel: CancelToken,
) {
    rt.spawn_dedicated(format!("view.cast.{}", doc.0), cancel, move |cx| {
        let mut colors = colors;
        let (mut playing, mut seq, mut redraw) = (false, 0, true);
        let mut clock = Clock {
            at: Instant::now(),
            base: 0,
            speed: 1,
        };
        loop {
            if cx.cancel.is_cancelled() {
                return;
            }
            let first = match rx.recv_timeout(if playing { TICK } else { IDLE }) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let (mut seek, mut ack) = (None, false);
            // Ползунок тянут: выполняется только последняя перемотка.
            for c in first.into_iter().chain(rx.try_iter()) {
                match c {
                    Cmd::Seek { ms, seq: n } => {
                        seek = Some(ms);
                        seq = n;
                        ack = true;
                    }
                    Cmd::Play(on) => {
                        playing = on;
                        clock.set(replay.ms());
                        ack = true;
                    }
                    Cmd::Speed(n) => {
                        clock.set(clock.now().min(replay.duration_ms()));
                        clock.speed = u32::from(n.max(1));
                    }
                    Cmd::Colors(d) => {
                        colors = d;
                        redraw = true;
                    }
                }
            }
            let mut step = || -> std::io::Result<()> {
                if let Some(ms) = seek {
                    redraw |= replay.seek(ms)?;
                    clock.set(replay.ms());
                }
                if playing {
                    let t = clock.now().min(replay.duration_ms());
                    redraw |= replay.advance(t)?;
                    playing = t < replay.duration_ms();
                }
                Ok(())
            };
            if let Err(e) = step() {
                tracing::warn!(error = %e, "cast replay failed");
                playing = false;
                ack = true;
            }
            if !(redraw || playing || ack) {
                continue;
            }
            let frame = redraw.then(|| Arc::new(replay.snapshot(&colors)));
            redraw = false;
            let msg = FrameMsg {
                doc,
                ms: replay.ms(),
                playing,
                seq,
                frame,
            };
            if cx.sink.send(Box::new(msg), &cx.cancel).is_err() {
                return;
            }
        }
    });
}
