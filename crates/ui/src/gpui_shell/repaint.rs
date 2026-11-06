//! Кадр для данных мимо `state.rev`: вывод терминала приходит через `MsgSink::poke` — без
//! `Msg`, поэтому `rev` стоит и `cx.notify()` по нему нет. `TermShared.dirty` при этом
//! поднят, а reader будит окно только на переходе false→true: пока кадр не забрал снапшот,
//! новых пробуждений не будет. Кадра просит проход Core: видимая сетка устарела —
//! один кадр; не пришёл за [`LOST_FRAME`] — ещё раз, иначе один потерянный кадр
//! остановил бы вывод до первого стороннего кадра (клавиша, мышь, фокус).
//!
//! Потолок неактивного окна — здесь: `inactive_frame_interval` окна снят ради ритма
//! автоповтора клавиш. Простой: устаревшего нет — ни кадров, ни таймеров.

use std::time::{Duration, Instant};

/// Кадр неактивного окна не чаще (~30 кадров/с, как job-бар).
const INACTIVE_FRAME: Duration = Duration::from_millis(33);
/// За столько кадр точно должен прийти: vsync спит ~1 кадр даже без монитора.
const LOST_FRAME: Duration = Duration::from_millis(250);

/// Что сделать окну на этом проходе.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Ask {
    /// Попросить кадр.
    pub frame: bool,
    /// Разбудить проход Core через столько (вывод мог остаться без кадра).
    pub recheck: Option<Duration>,
}

/// Учёт просьб о кадре.
#[derive(Debug, Default)]
pub(super) struct Repaint {
    /// Последний кадр окна (любой причины).
    drawn: Option<Instant>,
    /// Кадр попрошен и ещё не пришёл.
    asked: Option<Instant>,
    /// Срок таймера проверки: пока он впереди, второй не нужен.
    guard: Option<Instant>,
}

impl Repaint {
    /// Окно нарисовало кадр.
    pub(super) fn drawn(&mut self, now: Instant) {
        self.drawn = Some(now);
        self.asked = None;
    }

    /// Проход Core: на экране устаревшая сетка.
    pub(super) fn step(&mut self, active: bool, now: Instant) -> Ask {
        if self
            .asked
            .is_some_and(|at| now.saturating_duration_since(at) < LOST_FRAME)
        {
            return Ask {
                frame: false,
                recheck: self.guard(now),
            };
        }
        let gap = if active {
            Duration::ZERO
        } else {
            INACTIVE_FRAME
        };
        let left = self
            .drawn
            .map_or(Duration::ZERO, |t| (t + gap).saturating_duration_since(now));
        if !left.is_zero() {
            return Ask {
                frame: false,
                recheck: Some(left),
            };
        }
        self.asked = Some(now);
        Ask {
            frame: true,
            recheck: self.guard(now),
        }
    }

    /// Таймер проверки просьбы: не больше одного на [`LOST_FRAME`].
    fn guard(&mut self, now: Instant) -> Option<Duration> {
        if self.guard.is_some_and(|t| t > now) {
            return None;
        }
        self.guard = Some(now + LOST_FRAME);
        Some(LOST_FRAME)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use urfin_term::session::{SessionEvent, SessionOpts, SessionSink, parse_inline};
    use urfin_term::sys::conpty::PtySize;
    use urfin_term::vt::shared::TermShared;

    use super::super::term_view::Grid;
    use super::*;

    struct Quiet;

    impl SessionSink for Quiet {
        fn poke(&self) {}
        fn event(&self, _ev: SessionEvent) {}
    }

    fn shared() -> Arc<TermShared> {
        let opts = SessionOpts {
            size: PtySize { cols: 80, rows: 24 },
            scrollback: 100,
            osc52: "off".into(),
            nonce: None,
        };
        parse_inline(&b"$ "[..], &opts, &Quiet)
    }

    const VSYNC: u64 = 16;

    /// Модель окна на часах в мс: вывод идёт каждую мс как у reader (будит только переход
    /// `dirty` false→true), проход Core — по пробуждению и таймерам, кадр — по vsync, если
    /// его попросили; снапшот кадра сбрасывает `dirty`. `rev` не меняется: `Msg` нет.
    struct Sim {
        shared: Arc<TermShared>,
        grid: Grid,
        host: Repaint,
        active: bool,
        t0: Instant,
        now: u64,
        wake: bool,
        timers: Vec<u64>,
        want_frame: bool,
        asks: u32,
        frames: u32,
        pumps: u32,
    }

    impl Sim {
        fn new(active: bool) -> Sim {
            let shared = shared();
            let grid = Grid::watching(&shared);
            shared.take_dirty();
            Sim {
                shared,
                grid,
                host: Repaint::default(),
                active,
                t0: Instant::now(),
                now: 0,
                wake: false,
                timers: Vec::new(),
                want_frame: false,
                asks: 0,
                frames: 0,
                pumps: 0,
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        /// `ms` миллисекунд; `lost(n)` — просьбу о кадре `n` (от 1) потерять.
        fn run(&mut self, ms: u64, output: bool, lost: &dyn Fn(u32) -> bool) {
            for _ in 0..ms {
                self.now += 1;
                if output && self.shared.mark_dirty() {
                    self.wake = true;
                }
                let due = self.timers.iter().any(|t| *t <= self.now);
                self.timers.retain(|t| *t > self.now);
                if std::mem::take(&mut self.wake) || due {
                    self.pump(lost);
                }
                if self.now.is_multiple_of(VSYNC) && std::mem::take(&mut self.want_frame) {
                    self.frames += 1;
                    self.host.drawn(self.at(self.now));
                    self.shared.take_dirty();
                }
            }
        }

        fn pump(&mut self, lost: &dyn Fn(u32) -> bool) {
            self.pumps += 1;
            if !self.grid.stale() {
                return;
            }
            let ask = self.host.step(self.active, self.at(self.now));
            if ask.frame {
                self.asks += 1;
                self.want_frame |= !lost(self.asks);
            }
            if let Some(d) = ask.recheck {
                self.timers
                    .push(self.now + u64::try_from(d.as_millis()).unwrap());
            }
        }
    }

    const NONE: &dyn Fn(u32) -> bool = &|_| false;

    /// Приёмка бага: окно без фокуса, вывод без `Msg` — кадры идут, не чаще 30/с.
    #[test]
    fn inactive_window_draws_output_at_limited_rate() {
        let mut s = Sim::new(false);
        s.run(3000, true, NONE);
        assert!(s.frames >= 3 * 15, "кадров за 3 с: {}", s.frames);
        assert!(s.frames <= 3 * 30, "кадров за 3 с: {}", s.frames);
    }

    /// Вывод кончился: последний кадр с ним на экране, дальше ни кадров, ни таймеров.
    #[test]
    fn idle_has_no_frames_and_no_timers() {
        let mut s = Sim::new(false);
        s.run(1000, true, NONE);
        s.run(1000, false, NONE);
        assert!(!s.grid.stale(), "конец вывода на экране");
        assert_eq!(s.timers, Vec::<u64>::new());
        let (frames, pumps) = (s.frames, s.pumps);
        s.run(5000, false, NONE);
        assert_eq!((s.frames, s.pumps), (frames, pumps));
    }

    /// Потерянный кадр не останавливает обновление: ни первый, ни на ходу.
    #[test]
    fn lost_frame_does_not_stop_output() {
        let mut s = Sim::new(false);
        s.run(3000, true, &|n| n == 1 || n == 7);
        assert!(s.frames >= 3 * 10, "кадров за 3 с: {}", s.frames);
    }

    /// Потерян последний кадр: новых пробуждений reader нет, догоняет таймер.
    #[test]
    fn lost_last_frame_is_retried() {
        let mut s = Sim::new(false);
        s.run(1, true, &|n| n == 1);
        s.run(2000, false, NONE);
        assert!(!s.grid.stale());
        assert_eq!(s.asks, 2);
        assert_eq!(s.frames, 1);
    }

    /// Окно в фокусе без потолка: кадр на каждый vsync, таймеров паузы нет.
    #[test]
    fn active_window_draws_every_vsync() {
        let mut s = Sim::new(true);
        s.run(1600, true, NONE);
        assert!(s.frames >= 90, "кадров: {}", s.frames);
    }

    /// Таймеры не множатся: проверок просьбы — не больше одной на срок.
    #[test]
    fn guard_is_one_per_period() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut r = Repaint::default();
        assert_eq!(
            r.step(true, t),
            Ask {
                frame: true,
                recheck: Some(LOST_FRAME)
            }
        );
        assert_eq!(
            r.step(true, t + ms(1)),
            Ask {
                frame: false,
                recheck: None
            }
        );
        r.drawn(t + ms(2));
        assert_eq!(
            r.step(true, t + ms(3)),
            Ask {
                frame: true,
                recheck: None
            }
        );
    }
}
