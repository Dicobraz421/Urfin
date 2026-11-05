//! Замер вьювера (S7-20, `--bench-viewer <файл>`): открытие на рабочем окне до первого
//! экрана, кадр прокрутки страницей и по строке (тёплый кэш), прыжок (холодный экран),
//! перенос и Hex. Режим при открытии — как у файла: `.log` — Текст, `.json` — Разметка.
//! Прокрутка — `InboxMsg::Scroll`, прыжок — `InboxMsg::ScrollTo`, режимы — команды;
//! синтетического ввода в окно нет. `URFIN_BENCH_FOLLOW=<с>` — слежение за концом файла:
//! CPU процесса и кадров в секунду (растущий файл пишет внешняя программа).
//! `URFIN_PROF=<файл>` и `URFIN_PROF_PHASE=page|line|jump|wrap|hex` — профиль одной фазы.
//! Итог — строка `key=value` в stdout, затем выход.

use std::time::{Duration, Instant};

use super::edit_bench::{pct, peak_private_mb};

const WARMUP: usize = 10;
const PAGE: usize = 120;
const LINE: usize = 120;
const JUMP: usize = 40;
const MODE: usize = 80;

/// Что сделать в этом кадре.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Act {
    None,
    /// Экран на столько строк вниз.
    Down(u32),
    /// Верх экрана на долю файла, ‰.
    Jump(u32),
    /// Команда вьювера.
    Cmd(&'static str),
    /// Отчёт напечатан: выйти.
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ph {
    Load,
    Warmup,
    Page,
    Line,
    Jump,
    Wrap,
    Hex,
    Follow,
}

impl Ph {
    fn name(self) -> &'static str {
        match self {
            Ph::Page => "page",
            Ph::Line => "line",
            Ph::Jump => "jump",
            Ph::Wrap => "wrap",
            Ph::Hex => "hex",
            _ => "",
        }
    }
}

/// Замер одного файла.
pub(super) struct ViewBench {
    started: Instant,
    ph: Ph,
    n: usize,
    mem0: f64,
    pub bytes: u64,
    /// Документ готов, видимые страницы на месте, мс от команды открытия.
    load_ms: f64,
    /// Первый экран нарисован, мс от команды открытия.
    screen_ms: f64,
    mem1: f64,
    last_cycles: u64,
    page: Vec<f32>,
    line: Vec<f32>,
    jump: Vec<f32>,
    wrap: Vec<f32>,
    hex: Vec<f32>,
    follow_for: Option<Duration>,
    /// Начало слежения: часы и CPU процесса.
    follow_from: Option<(Instant, f64)>,
    follow_frames: usize,
    follow_cpu: f64,
    follow_fps: f64,
    #[cfg(windows)]
    prof: Option<crate::sys::prof::Sampler>,
}

impl ViewBench {
    pub(super) fn new() -> ViewBench {
        ViewBench {
            started: Instant::now(),
            ph: Ph::Load,
            n: 0,
            mem0: peak_private_mb(),
            bytes: 0,
            load_ms: 0.0,
            screen_ms: 0.0,
            mem1: 0.0,
            last_cycles: 0,
            page: Vec::new(),
            line: Vec::new(),
            jump: Vec::new(),
            wrap: Vec::new(),
            hex: Vec::new(),
            follow_for: std::env::var("URFIN_BENCH_FOLLOW")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs),
            follow_from: None,
            follow_frames: 0,
            follow_cpu: 0.0,
            follow_fps: 0.0,
            #[cfg(windows)]
            prof: None,
        }
    }

    /// Ещё открывается.
    pub(super) fn waiting(&self) -> bool {
        self.ph == Ph::Load
    }

    /// Кадры сами не идут: слежение будит окно само, замеру нужен только конец фазы.
    pub(super) fn wants_frames(&self) -> bool {
        self.ph != Ph::Follow
    }

    /// Пока идёт слежение: проверить конец фазы.
    pub(super) fn wake(&self) -> Option<Duration> {
        (self.ph == Ph::Follow).then_some(Duration::from_millis(500))
    }

    /// Документ открыт, видимые страницы на месте.
    pub(super) fn opened(&mut self) {
        if self.ph != Ph::Load {
            return;
        }
        self.load_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        self.mem1 = peak_private_mb();
        self.ph = Ph::Warmup;
        self.n = 0;
        self.last_cycles = crate::sys::cpu::thread_cycles();
    }

    /// Смена фазы: профиль — только выбранной (`URFIN_PROF_PHASE`).
    fn enter(&mut self, ph: Ph) {
        #[cfg(windows)]
        {
            if let Some(p) = self.prof.take() {
                p.finish();
            }
            if std::env::var("URFIN_PROF_PHASE").is_ok_and(|v| v == ph.name()) {
                self.prof = crate::sys::prof::start();
            }
        }
        self.ph = ph;
        self.n = 0;
    }

    fn done(&self) -> Act {
        println!("bench viewer {}", self.report());
        Act::Exit
    }

    /// Кадр (зовёт фича в начале кадра): замер прошлого кадра и действие этого.
    pub(super) fn frame(&mut self) -> Act {
        if self.ph == Ph::Load {
            return Act::None;
        }
        let cycles = crate::sys::cpu::thread_cycles();
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "мс"
        )]
        let ui = (cycles.saturating_sub(self.last_cycles) as f64 / crate::sys::cpu::cycles_per_ms())
            as f32;
        self.last_cycles = cycles;
        self.n += 1;
        match self.ph {
            Ph::Load => Act::None,
            Ph::Warmup => {
                if self.n == 1 {
                    self.screen_ms = self.started.elapsed().as_secs_f64() * 1000.0;
                }
                if self.n >= WARMUP {
                    self.enter(Ph::Page);
                }
                Act::None
            }
            Ph::Page => {
                if self.n > 1 {
                    self.page.push(ui);
                }
                if self.n >= PAGE {
                    self.enter(Ph::Line);
                    return Act::None;
                }
                Act::Down(40)
            }
            Ph::Line => {
                if self.n > 1 {
                    self.line.push(ui);
                }
                if self.n >= LINE {
                    self.enter(Ph::Jump);
                    return Act::None;
                }
                Act::Down(1)
            }
            Ph::Jump => {
                if self.n > 1 {
                    self.jump.push(ui);
                }
                if self.n >= JUMP {
                    self.enter(Ph::Wrap);
                    return Act::Cmd("viewer.wrap");
                }
                #[allow(clippy::cast_possible_truncation, reason = "кадров немного")]
                Act::Jump((self.n as u32 * 7919) % 1000)
            }
            Ph::Wrap => {
                // Первый кадр — смена режима, не прокрутка.
                if self.n > 2 {
                    self.wrap.push(ui);
                }
                if self.n >= MODE {
                    self.enter(Ph::Hex);
                    return Act::Cmd("viewer.wrap");
                }
                Act::Down(40)
            }
            Ph::Hex => {
                if self.n == 1 {
                    return Act::Cmd("viewer.hex");
                }
                if self.n > 3 {
                    self.hex.push(ui);
                }
                if self.n >= MODE {
                    if self.follow_for.is_none() {
                        return self.done();
                    }
                    self.enter(Ph::Follow);
                    return Act::Cmd("viewer.hex");
                }
                Act::Down(40)
            }
            Ph::Follow => self.follow(),
        }
    }

    fn follow(&mut self) -> Act {
        if self.n == 1 {
            return Act::Cmd("viewer.follow");
        }
        let now = Instant::now();
        let Some((at, cpu)) = self.follow_from else {
            self.follow_from = Some((now, crate::sys::cpu::process_secs()));
            self.follow_frames = 0;
            return Act::None;
        };
        self.follow_frames += 1;
        let wall = now.duration_since(at);
        if wall < self.follow_for.unwrap_or_default() {
            return Act::None;
        }
        let secs = wall.as_secs_f64();
        self.follow_cpu = (crate::sys::cpu::process_secs() - cpu) / secs * 100.0;
        #[allow(clippy::cast_precision_loss, reason = "кадров немного")]
        {
            self.follow_fps = self.follow_frames as f64 / secs;
        }
        self.done()
    }

    pub(super) fn report(&self) -> String {
        #[allow(clippy::cast_precision_loss, reason = "МБ")]
        let mb = self.bytes as f64 / 1_048_576.0;
        let group = |name: &str, v: &[f32]| {
            format!(
                "{name}_p50={:.2} {name}_p99={:.2} {name}_max={:.2}",
                pct(v, 0.5),
                pct(v, 0.99),
                pct(v, 1.0)
            )
        };
        format!(
            "size_mb={mb:.1} open_ms={:.1} screen_ms={:.1} mem_before_mb={:.0} mem_after_mb={:.0} {} {} {} {} {} follow_cpu={:.1} follow_fps={:.1} mem_end_mb={:.0}",
            self.load_ms,
            self.screen_ms,
            self.mem0,
            self.mem1,
            group("page", &self.page),
            group("line", &self.line),
            group("jump", &self.jump),
            group("wrap", &self.wrap),
            group("hex", &self.hex),
            self.follow_cpu,
            self.follow_fps,
            peak_private_mb(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_run_to_exit() {
        let mut b = ViewBench::new();
        assert_eq!(b.frame(), Act::None);
        assert!(b.waiting());
        b.opened();
        let mut cmds = Vec::new();
        for _ in 0..10_000 {
            match b.frame() {
                Act::Exit => {
                    assert_eq!(cmds, ["viewer.wrap", "viewer.wrap", "viewer.hex"]);
                    assert!(b.report().contains("hex_p99"));
                    return;
                }
                Act::Cmd(c) => cmds.push(c),
                _ => {}
            }
        }
        panic!("bench did not finish");
    }
}
