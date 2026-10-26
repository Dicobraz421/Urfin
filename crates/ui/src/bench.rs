//! `--bench` (G-07, S0 v5 №10): один сценарий для интерфейса GPUI и сборки `egui-final`,
//! чтобы сравнение было честным (рецепт pets «Бенч»). Окно открыто на каталоге из 100k
//! файлов; фазы: прогрев → листание по 3 строки за кадр → страница вниз каждый кадр →
//! ресайз туда-обратно → терминал (S6-06: панели спрятаны, сетка 200×60 плотного
//! цветного текста меняется каждый кадр — полная пересборка) → простой. Итог — одна
//! строка `key=value` в stdout.
//!
//! Модуль не знает об окне: окно на каждом кадре зовёт [`Bench::frame`] и выполняет
//! шаг (команды Core, размер окна), CPU и память меряет [`crate::sys::cpu`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use urfin_core::Command;

const WARMUP: usize = 30;
const LIST_FRAMES: usize = 400;
const PAGE_FRAMES: usize = 200;
const RESIZE_FRAMES: usize = 120;
const TERM_FRAMES: usize = 200;

/// Снапшот и отрисовка сетки терминала за кадр, нс (копит окно, забирает фаза `term`).
pub(crate) static GRID_NS: AtomicU64 = AtomicU64::new(0);
/// Аллокации процесса: считает глобальный аллокатор сборки `--features count-alloc`
/// (S2-20, только диагностика); в обычной сборке — 0.
pub static ALLOCS: AtomicU64 = AtomicU64::new(0);
const IDLE_SETTLE: Duration = Duration::from_secs(3);
const IDLE: Duration = Duration::from_secs(10);

/// Что окну сделать в этом кадре.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Ничего: следующий кадр.
    Next,
    /// Команды Core.
    Cmds(Vec<Command>),
    /// Логический размер окна.
    Resize(f32, f32),
    /// Простой: перерисовку не просить; разбудить через столько.
    Idle(Duration),
    /// Конец: строка отчёта, выйти.
    Done(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ph {
    Warmup,
    List,
    Page,
    Resize,
    Term,
    Settle,
    Idle,
    Done,
}

#[derive(Default)]
struct Phase {
    frames: Vec<f32>,
    /// Сборка дерева в `render`, мс.
    builds: Vec<f32>,
    /// Снапшот и отрисовка сетки терминала, мс (фаза `term`).
    grid: Vec<f32>,
    /// CPU UI-потока между кадрами, мс (S2-20: «кадр ≤ 2 мс»).
    ui: Vec<f32>,
    /// Аллокаций между кадрами (`count-alloc`).
    allocs: Vec<f32>,
    started: Option<(Instant, f64, f64)>,
    cpu: f32,
    ui_cpu: f32,
}

impl Phase {
    fn start(&mut self, now: Instant) {
        self.started = Some((
            now,
            crate::sys::cpu::process_secs(),
            crate::sys::cpu::thread_secs(),
        ));
    }

    fn stop(&mut self, now: Instant) {
        if let Some((t, p, u)) = self.started.take() {
            let wall = now.duration_since(t).as_secs_f64().max(1e-6);
            #[allow(clippy::cast_possible_truncation, reason = "проценты")]
            {
                self.cpu = ((crate::sys::cpu::process_secs() - p) / wall * 100.0) as f32;
                self.ui_cpu = ((crate::sys::cpu::thread_secs() - u) / wall * 100.0) as f32;
            }
        }
    }

    fn pct(&self, q: f32) -> f32 {
        pct(&self.frames, q)
    }

    fn report(&self, name: &str) -> String {
        format!(
            "{name}_p50={:.2} {name}_p99={:.2} {name}_max={:.2} {name}_cpu={:.1} {name}_ui_cpu={:.1} {name}_build_p50={:.2} {name}_ui_p50={:.2} {name}_ui_p99={:.2} {name}_allocs_p50={:.0} {name}_allocs_max={:.0}",
            self.pct(0.5),
            self.pct(0.99),
            self.pct(1.0),
            self.cpu,
            self.ui_cpu,
            pct(&self.builds, 0.5),
            pct(&self.ui, 0.5),
            pct(&self.ui, 0.99),
            pct(&self.allocs, 0.5),
            pct(&self.allocs, 1.0),
        )
    }
}

fn pct(frames: &[f32], q: f32) -> f32 {
    if frames.is_empty() {
        return 0.0;
    }
    let mut v = frames.to_vec();
    v.sort_by(f32::total_cmp);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "индекс перцентиля"
    )]
    let i = ((v.len() - 1) as f32 * q).round() as usize;
    v[i]
}

/// Сценарий и замеры.
pub struct Bench {
    ph: Ph,
    n: usize,
    last: Option<Instant>,
    /// Циклы UI-потока и аллокации на прошлом кадре.
    last_cycles: u64,
    last_allocs: u64,
    list: Phase,
    page: Phase,
    resize: Phase,
    term: Phase,
    idle: Phase,
    idle_frames: usize,
    idle_until: Option<Instant>,
    /// Размер окна до ресайза.
    size: (f32, f32),
    /// Сэмплер UI-потока на списке и страницах (`URFIN_PROF`).
    #[cfg(windows)]
    prof: Option<crate::sys::prof::Sampler>,
}

impl std::fmt::Debug for Bench {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bench")
            .field("ph", &self.ph)
            .finish_non_exhaustive()
    }
}

impl Bench {
    pub fn new(size: (f32, f32)) -> Bench {
        Bench {
            ph: Ph::Warmup,
            n: 0,
            last: None,
            last_cycles: crate::sys::cpu::thread_cycles(),
            last_allocs: ALLOCS.load(Ordering::Relaxed),
            list: Phase::default(),
            page: Phase::default(),
            resize: Phase::default(),
            term: Phase::default(),
            idle: Phase::default(),
            idle_frames: 0,
            idle_until: None,
            size,
            #[cfg(windows)]
            prof: None,
        }
    }

    /// Кадр нарисован: шаг сценария.
    pub fn frame(&mut self, now: Instant) -> Step {
        let dt = self
            .last
            .replace(now)
            .map(|t| now.duration_since(t).as_secs_f32() * 1000.0);
        self.n += 1;
        let (ui_ms, frame_allocs) = self.since_last();
        let phase = match self.ph {
            Ph::List => Some(&mut self.list),
            Ph::Page => Some(&mut self.page),
            Ph::Resize => Some(&mut self.resize),
            Ph::Term => Some(&mut self.term),
            _ => None,
        };
        let grid = GRID_NS.swap(0, Ordering::Relaxed);
        let term = self.ph == Ph::Term;
        if let Some(p) = phase {
            if self.n == 1 {
                p.start(now);
            } else if let Some(dt) = dt {
                p.frames.push(dt);
                p.ui.push(ui_ms);
                p.allocs.push(frame_allocs);
                if term {
                    #[allow(clippy::cast_precision_loss, reason = "мс")]
                    p.grid.push(grid as f32 / 1e6);
                }
            }
        }
        match self.ph {
            Ph::Warmup => {
                if self.n >= WARMUP {
                    self.next(Ph::List, now);
                }
                Step::Next
            }
            Ph::List => {
                if self.n >= LIST_FRAMES {
                    self.next(Ph::Page, now);
                    return Step::Cmds(vec![Command::new("panel.cursor.home")]);
                }
                Step::Cmds(vec![
                    Command::new("panel.cursor.down"),
                    Command::new("panel.cursor.down"),
                    Command::new("panel.cursor.down"),
                ])
            }
            Ph::Page => {
                if self.n >= PAGE_FRAMES {
                    self.next(Ph::Resize, now);
                    return Step::Next;
                }
                Step::Cmds(vec![Command::new("panel.cursor.page_down")])
            }
            Ph::Resize => {
                if self.n >= RESIZE_FRAMES {
                    self.next(Ph::Term, now);
                    return Step::Resize(self.size.0, self.size.1);
                }
                // 30 кадров уже, 30 обратно, и так дважды: шаг 8 px.
                #[allow(clippy::cast_precision_loss, reason = "номер кадра")]
                let k = (self.n % 60) as f32;
                let d = if k < 30.0 { k } else { 60.0 - k } * 8.0;
                Step::Resize(self.size.0 - d, self.size.1 - d / 2.0)
            }
            Ph::Term => {
                if self.n >= TERM_FRAMES {
                    self.next(Ph::Settle, now);
                    self.idle_until = Some(now + IDLE_SETTLE);
                    return Step::Cmds(vec![Command::new("screens.term").arg("stop", true)]);
                }
                Step::Cmds(vec![Command::new("screens.term").arg("kind", "dense")])
            }
            Ph::Settle => {
                let until = self.idle_until.unwrap_or(now);
                if now >= until {
                    self.ph = Ph::Idle;
                    self.idle.start(now);
                    self.idle_frames = 0;
                    self.idle_until = Some(now + IDLE);
                    return Step::Idle(IDLE);
                }
                Step::Idle(until - now)
            }
            Ph::Idle => {
                self.idle_frames += 1;
                let until = self.idle_until.unwrap_or(now);
                if now < until {
                    return Step::Idle(until - now);
                }
                self.idle.stop(now);
                self.ph = Ph::Done;
                Step::Done(self.report())
            }
            Ph::Done => Step::Done(self.report()),
        }
    }

    /// CPU UI-потока (мс) и аллокации с прошлого кадра.
    fn since_last(&mut self) -> (f32, f32) {
        let cycles = crate::sys::cpu::thread_cycles();
        let allocs = ALLOCS.load(Ordering::Relaxed);
        #[allow(clippy::cast_precision_loss, reason = "мс и штуки")]
        let out = (
            (cycles.saturating_sub(self.last_cycles) as f64 / crate::sys::cpu::cycles_per_ms())
                as f32,
            allocs.saturating_sub(self.last_allocs) as f32,
        );
        (self.last_cycles, self.last_allocs) = (cycles, allocs);
        out
    }

    /// Сборка дерева кадра заняла столько (зовёт окно в конце `render`).
    pub fn built(&mut self, d: Duration) {
        let p = match self.ph {
            Ph::List => &mut self.list,
            Ph::Page => &mut self.page,
            Ph::Resize => &mut self.resize,
            Ph::Term => &mut self.term,
            _ => return,
        };
        p.builds.push(d.as_secs_f32() * 1000.0);
    }

    fn next(&mut self, to: Ph, now: Instant) {
        match self.ph {
            Ph::List => self.list.stop(now),
            Ph::Page => self.page.stop(now),
            Ph::Resize => self.resize.stop(now),
            Ph::Term => self.term.stop(now),
            _ => {}
        }
        #[cfg(windows)]
        match to {
            Ph::List => self.prof = crate::sys::prof::start(),
            Ph::Resize => {
                if let Some(p) = self.prof.take() {
                    p.finish();
                }
            }
            _ => {}
        }
        self.ph = to;
        self.n = 0;
        self.last = None;
    }

    fn report(&self) -> String {
        let (ws, private) = urfin_vfs::diag::peak_memory().unwrap_or((0, None));
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        #[allow(clippy::cast_precision_loss, reason = "МБ")]
        let mb = |b: u64| b as f64 / 1_048_576.0;
        format!(
            "{} {} {} {} term_grid_p50={:.2} term_grid_p99={:.2} idle_frames={} idle_cpu={:.2} peak_ws_mb={:.1} peak_private_mb={:.1} exe_mb={:.2}",
            self.list.report("list"),
            self.page.report("page"),
            self.resize.report("resize"),
            self.term.report("term"),
            pct(&self.term.grid, 0.5),
            pct(&self.term.grid, 0.99),
            self.idle_frames.saturating_sub(1),
            self.idle.cpu,
            mb(ws),
            mb(private.unwrap_or(0)),
            mb(exe)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сценарий проходит все фазы и кончается отчётом со всеми ключами.
    #[test]
    fn scenario_runs_to_a_report() {
        let mut b = Bench::new((1200.0, 760.0));
        let mut t = Instant::now();
        let mut resized = 0;
        for _ in 0..10_000 {
            t += Duration::from_millis(7);
            match b.frame(t) {
                Step::Done(r) => {
                    for k in [
                        "list_p99",
                        "page_p99",
                        "resize_p99",
                        "term_p99",
                        "term_grid_p99",
                        "idle_cpu",
                        "exe_mb",
                    ] {
                        assert!(r.contains(k), "{r}");
                    }
                    assert!(resized > 0);
                    return;
                }
                Step::Resize(..) => resized += 1,
                Step::Idle(d) => t += d,
                Step::Next | Step::Cmds(_) => {}
            }
        }
        panic!("bench did not finish");
    }
}
