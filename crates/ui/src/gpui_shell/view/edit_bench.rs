//! Замер редактора на движке (S7-16, S7-34, `--bench-editor <файл>`): открытие до
//! первого экрана, память, кадр прокрутки, нажатие и прыжок. Нажатие — правка через API
//! документа (`InboxMsg::Input`, как commit IME), прокрутка — `InboxMsg::Scroll`, прыжок
//! — `InboxMsg::Caret` на долю документа; не синтетический ввод в окно. Подсветка
//! (S7-35): первые цвета, память после них и время до новых цветов после правки. Итог —
//! строка `key=value` в stdout, затем выход.

use std::time::Instant;

/// Кадров прогрева после передачи текста, прокрутки, нажатий и прыжков.
const WARMUP: usize = 20;
const SCROLL: usize = 120;
const TYPE: usize = 300;
const JUMP: usize = 40;

/// Что сделать в этом кадре.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Act {
    None,
    /// Экран на столько строк вниз.
    Down(u32),
    /// Символ в позицию курсора.
    Type,
    /// Курсор на долю документа (холодный экран).
    Jump(u32),
    /// Отчёт напечатан: выйти.
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ph {
    Load,
    Warmup,
    Scroll,
    Type,
    Jump,
    Done,
}

/// Замер одного файла.
pub(super) struct EdBench {
    started: Instant,
    ph: Ph,
    n: usize,
    /// Пик private до открытия, МБ.
    mem0: f64,
    pub bytes: u64,
    pub lines: u64,
    /// Открытие до первого экрана (документ готов, страницы на месте), мс.
    load_ms: f64,
    /// Пик private после открытия, МБ.
    mem1: f64,
    /// Первый кадр после передачи (UI CPU), мс.
    first_ms: f64,
    last_cycles: u64,
    last_at: Option<Instant>,
    scroll: Vec<f32>,
    typing: Vec<f32>,
    typing_wall: Vec<f32>,
    jump: Vec<f32>,
    /// Подсветка (S7-35): первые цвета от старта, мс; пик private после них, МБ; сколько
    /// ждали новые цвета после правок, мс; счётчик учтённых.
    hl_first_ms: f64,
    mem_hl: f64,
    hl: Vec<f32>,
    hl_seen: u64,
}

fn peak_private_mb() -> f64 {
    let (ws, private) = urfin_vfs::diag::peak_memory().unwrap_or((0, None));
    #[allow(clippy::cast_precision_loss, reason = "МБ")]
    let mb = private.unwrap_or(ws) as f64 / 1_048_576.0;
    mb
}

fn pct(v: &[f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(f32::total_cmp);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "индекс перцентиля"
    )]
    let i = ((v.len() - 1) as f32 * q).round() as usize;
    v[i.min(v.len() - 1)]
}

impl EdBench {
    pub(super) fn new() -> EdBench {
        EdBench {
            started: Instant::now(),
            ph: Ph::Load,
            n: 0,
            mem0: peak_private_mb(),
            bytes: 0,
            lines: 0,
            load_ms: 0.0,
            mem1: 0.0,
            first_ms: 0.0,
            last_cycles: 0,
            last_at: None,
            scroll: Vec::new(),
            typing: Vec::new(),
            typing_wall: Vec::new(),
            jump: Vec::new(),
            hl_first_ms: 0.0,
            mem_hl: 0.0,
            hl: Vec::new(),
            hl_seen: 0,
        }
    }

    /// Документ открыт, первый экран на месте.
    pub(super) fn opened(&mut self) {
        if self.ph != Ph::Load {
            return;
        }
        self.load_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        self.mem1 = peak_private_mb();
        self.ph = Ph::Warmup;
        self.n = 0;
        self.last_cycles = crate::sys::cpu::thread_cycles();
        self.last_at = Some(Instant::now());
    }

    /// Подсветка в кадре: прогрев ждёт первых цветов (`true` — кадр пропустить), при
    /// наборе и прыжках — время до новых цветов после каждой догонки.
    pub(super) fn hl(&mut self, busy: bool, fresh_n: u64, last_ms: Option<f32>) -> bool {
        match self.ph {
            Ph::Warmup if busy => {
                // Кадры ожидания не в замере: отсчёт — с последнего из них.
                self.last_cycles = crate::sys::cpu::thread_cycles();
                self.last_at = Some(Instant::now());
                return true;
            }
            Ph::Warmup if self.hl_first_ms == 0.0 => {
                self.hl_first_ms = self.started.elapsed().as_secs_f64() * 1000.0;
                self.mem_hl = peak_private_mb();
                self.hl_seen = fresh_n;
            }
            Ph::Type | Ph::Jump if fresh_n != self.hl_seen => {
                self.hl_seen = fresh_n;
                self.hl.extend(last_ms);
            }
            _ => {}
        }
        false
    }

    /// Ещё загружается (кадры не нужны).
    pub(super) fn waiting(&self) -> bool {
        self.ph == Ph::Load
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
        let now = Instant::now();
        let wall = self
            .last_at
            .replace(now)
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32() * 1000.0);
        self.n += 1;
        match self.ph {
            Ph::Load | Ph::Done => Act::None,
            Ph::Warmup => {
                if self.n == 1 {
                    self.first_ms = f64::from(ui);
                }
                if self.n >= WARMUP {
                    self.ph = Ph::Scroll;
                    self.n = 0;
                }
                Act::None
            }
            Ph::Scroll => {
                if self.n > 1 {
                    self.scroll.push(ui);
                }
                if self.n >= SCROLL {
                    self.ph = Ph::Type;
                    self.n = 0;
                    return Act::None;
                }
                Act::Down(40)
            }
            Ph::Type => {
                if self.n > 1 {
                    self.typing.push(ui);
                    self.typing_wall.push(wall);
                }
                if self.n >= TYPE {
                    self.ph = Ph::Jump;
                    self.n = 0;
                    return Act::None;
                }
                Act::Type
            }
            Ph::Jump => {
                if self.n > 1 {
                    self.jump.push(ui);
                }
                if self.n >= JUMP {
                    self.ph = Ph::Done;
                    println!("bench editor {}", self.report());
                    return Act::Exit;
                }
                #[allow(clippy::cast_possible_truncation, reason = "кадров немного")]
                Act::Jump((self.n as u32 * 7919) % 1000)
            }
        }
    }

    pub(super) fn report(&self) -> String {
        #[allow(clippy::cast_precision_loss, reason = "МБ")]
        let mb = self.bytes as f64 / 1_048_576.0;
        format!(
            "size_mb={mb:.1} lines={} open_ms={:.1} first_frame_ms={:.1} mem_before_mb={:.0} mem_after_mb={:.0} hl_first_ms={:.0} mem_hl_mb={:.0} scroll_p50={:.2} scroll_p99={:.2} type_p50={:.2} type_p99={:.2} type_max={:.2} type_wall_p50={:.2} type_wall_p99={:.2} hl_n={} hl_p50={:.1} hl_p99={:.1} hl_max={:.1} jump_p50={:.2} jump_p99={:.2}",
            self.lines,
            self.load_ms,
            self.first_ms,
            self.mem0,
            self.mem1,
            self.hl_first_ms,
            self.mem_hl,
            pct(&self.scroll, 0.5),
            pct(&self.scroll, 0.99),
            pct(&self.typing, 0.5),
            pct(&self.typing, 0.99),
            pct(&self.typing, 1.0),
            pct(&self.typing_wall, 0.5),
            pct(&self.typing_wall, 0.99),
            self.hl.len(),
            pct(&self.hl, 0.5),
            pct(&self.hl, 0.99),
            pct(&self.hl, 1.0),
            pct(&self.jump, 0.5),
            pct(&self.jump, 0.99),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_run_to_exit() {
        let mut b = EdBench::new();
        assert_eq!(b.frame(), Act::None);
        assert!(b.waiting());
        b.opened();
        assert!(!b.waiting());
        let mut acts = 0;
        for _ in 0..10_000 {
            match b.frame() {
                Act::Exit => {
                    assert!(b.report().contains("type_p99"));
                    assert!(acts > TYPE);
                    return;
                }
                Act::None => {}
                _ => acts += 1,
            }
        }
        panic!("bench did not finish");
    }
}
