//! Замер экрана поиска (S8-07, S8 §2.7 F6, `--bench-search [строк]`): кадр на сторе из фикстуры
//! `screens.search rows=N`, по умолчанию 1M строк. Фазы: страница вниз каждый кадр, строка
//! вниз, прыжки конец — начало. Движение — команды `search.cursor`, синтетического ввода в окно
//! нет. Время кадра — CPU UI-потока между кадрами, как у `--bench-viewer`. Итог — строка
//! `key=value` в stdout, затем выход. `URFIN_PROF=<файл>` — профиль UI-потока фазы страницы
//! (`URFIN_PROF_PHASE=line` — фазы строки).

const WARMUP: usize = 30;
const PAGE: usize = 200;
const LINE: usize = 200;
const JUMP: usize = 60;

/// Что сделать в этом кадре.
#[derive(Debug, PartialEq)]
pub(super) enum Act {
    None,
    /// `search.cursor to=…`.
    Cursor(&'static str),
    /// Отчёт готов: напечатать и выйти.
    Exit(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ph {
    Load,
    Warmup,
    Page,
    Line,
    Jump,
}

pub(super) struct SearchBench {
    ph: Ph,
    n: usize,
    rows: usize,
    last_cycles: u64,
    #[cfg(windows)]
    prof: Option<crate::sys::prof::Sampler>,
    page: Vec<f32>,
    line: Vec<f32>,
    jump: Vec<f32>,
}

fn pct(v: &[f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "индекс выборки"
    )]
    let at = ((s.len() - 1) as f32 * q).round() as usize;
    s[at]
}

impl SearchBench {
    pub(super) fn new() -> SearchBench {
        // Калибровка счётчика циклов — занятый цикл 30 мс: не в замере.
        crate::sys::cpu::cycles_per_ms();
        SearchBench {
            ph: Ph::Load,
            n: 0,
            rows: 0,
            last_cycles: 0,
            #[cfg(windows)]
            prof: None,
            page: Vec::new(),
            line: Vec::new(),
            jump: Vec::new(),
        }
    }

    fn enter(&mut self, ph: Ph) {
        // `URFIN_PROF=<файл>`: профиль фазы страницы, `URFIN_PROF_PHASE=line` — строки.
        #[cfg(windows)]
        {
            if let Some(p) = self.prof.take() {
                p.finish();
            }
            let want = std::env::var("URFIN_PROF_PHASE").unwrap_or_else(|_| "page".into());
            if (ph == Ph::Page && want == "page") || (ph == Ph::Line && want == "line") {
                self.prof = crate::sys::prof::start();
            }
        }
        self.ph = ph;
        self.n = 0;
    }

    fn report(&self) -> String {
        let cols = |name: &str, v: &[f32]| {
            format!(
                "{name}_p50={:.2} {name}_p99={:.2}",
                pct(v, 0.5),
                pct(v, 0.99)
            )
        };
        format!(
            "rows={} {} {} {}",
            self.rows,
            cols("page", &self.page),
            cols("line", &self.line),
            cols("jump", &self.jump)
        )
    }

    /// Кадр (зовёт фича в начале кадра): замер прошлого кадра и действие этого. `rows` — строк
    /// в сторе экрана.
    pub(super) fn frame(&mut self, rows: usize) -> Act {
        if self.ph == Ph::Load {
            if rows == 0 {
                return Act::None;
            }
            self.rows = rows;
            self.last_cycles = crate::sys::cpu::thread_cycles();
            self.enter(Ph::Warmup);
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
                Act::Cursor("page_down")
            }
            Ph::Line => {
                if self.n > 1 {
                    self.line.push(ui);
                }
                if self.n >= LINE {
                    self.enter(Ph::Jump);
                    return Act::None;
                }
                Act::Cursor("down")
            }
            Ph::Jump => {
                if self.n > 1 {
                    self.jump.push(ui);
                }
                if self.n >= JUMP {
                    return Act::Exit(self.report());
                }
                Act::Cursor(if self.n % 2 == 1 { "end" } else { "home" })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_the_phases_and_reports() {
        let mut b = SearchBench::new();
        assert_eq!(b.frame(0), Act::None, "ряд ещё не готов");
        let mut acts = Vec::new();
        for _ in 0..=(WARMUP + PAGE + LINE + JUMP) {
            match b.frame(1000) {
                Act::Exit(r) => {
                    assert!(r.starts_with("rows=1000 page_p50="), "{r}");
                    assert!(r.contains("jump_p99="), "{r}");
                    assert!(acts.contains(&"page_down") && acts.contains(&"end"));
                    return;
                }
                Act::Cursor(to) => acts.push(to),
                Act::None => {}
            }
        }
        panic!("замер не закончился");
    }

    #[test]
    fn percentiles() {
        let v: Vec<f32> = (1..=100u8).map(f32::from).collect();
        assert!((pct(&v, 0.5) - 51.0).abs() < 1.5);
        assert!((pct(&v, 0.99) - 99.0).abs() < 1.5);
        assert!(pct(&[], 0.5).abs() < f32::EPSILON);
    }
}
