//! Спайк размера редактора (S7-16, `--bench-editor <файл>`): передача текста в
//! `EditorState` на UI-потоке, память, кадр прокрутки и нажатие. Нажатие — правка через
//! API `EditorState` (как ввод символа), не синтетический ввод в окно. Итог — строка
//! `key=value` в stdout, затем выход.

use std::time::{Duration, Instant};

/// Кадров прогрева после передачи текста, прокрутки, нажатий и прыжков.
const WARMUP: usize = 20;
const SCROLL: usize = 120;
const TYPE: usize = 300;
const JUMP: usize = 40;

/// Что сделать в этом кадре.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Act {
    None,
    /// Курсор на столько строк вниз (прокрутка страницей).
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
    pub lines: usize,
    /// Загрузка в фоне (Rope в core), мс.
    load_ms: f64,
    /// Rope → строка в фоне, мс.
    pub conv_ms: f64,
    /// `set_value` на UI-потоке, мс.
    pub set_ms: f64,
    /// Пик private после передачи, МБ.
    mem1: f64,
    /// Первый кадр после передачи (UI CPU), мс.
    first_ms: f64,
    last_cycles: u64,
    last_at: Option<Instant>,
    scroll: Vec<f32>,
    typing: Vec<f32>,
    typing_wall: Vec<f32>,
    jump: Vec<f32>,
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
            conv_ms: 0.0,
            set_ms: 0.0,
            mem1: 0.0,
            first_ms: 0.0,
            last_cycles: 0,
            last_at: None,
            scroll: Vec::new(),
            typing: Vec::new(),
            typing_wall: Vec::new(),
            jump: Vec::new(),
        }
    }

    /// Документ загружен в core (текст ещё не передан).
    pub(super) fn loaded(&mut self) {
        if self.load_ms == 0.0 {
            self.load_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        }
    }

    /// Текст передан в `EditorState`.
    pub(super) fn adopted(&mut self, set: Duration) {
        self.set_ms = set.as_secs_f64() * 1000.0;
        self.mem1 = peak_private_mb();
        self.ph = Ph::Warmup;
        self.n = 0;
        self.last_cycles = crate::sys::cpu::thread_cycles();
        self.last_at = Some(Instant::now());
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
            "size_mb={mb:.1} lines={} load_ms={:.0} conv_ms={:.0} set_ms={:.0} first_frame_ms={:.1} mem_before_mb={:.0} mem_after_mb={:.0} scroll_p50={:.2} scroll_p99={:.2} type_p50={:.2} type_p99={:.2} type_max={:.2} type_wall_p50={:.2} type_wall_p99={:.2} jump_p50={:.2} jump_p99={:.2}",
            self.lines,
            self.load_ms,
            self.conv_ms,
            self.set_ms,
            self.first_ms,
            self.mem0,
            self.mem1,
            pct(&self.scroll, 0.5),
            pct(&self.scroll, 0.99),
            pct(&self.typing, 0.5),
            pct(&self.typing, 0.99),
            pct(&self.typing, 1.0),
            pct(&self.typing_wall, 0.5),
            pct(&self.typing_wall, 0.99),
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
        b.loaded();
        b.adopted(Duration::from_millis(5));
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
