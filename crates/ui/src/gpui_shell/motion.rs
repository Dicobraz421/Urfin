//! Движение палитры B (DESIGN «Движение», решение 2026-11-01): наведение на строку
//! панели и блик полосы прогресса. Кадры — только по событию: наведение — один переход
//! 83 мс после движения мыши, блик — пока идёт задача (кадр job-бара ≤ 30/с). Анимации
//! выключены (`ui.animations`, «Эффекты анимации» Windows) — сразу конечное состояние.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Наведение / уход с строки панели.
pub(super) const HOVER: Duration = Duration::from_millis(83);
/// Цикл блика прогресса.
pub(super) const GLINT_CYCLE: Duration = Duration::from_millis(1500);
/// Ширина блика — доля заполненной части.
pub(super) const GLINT_W: f32 = 0.24;

/// Кривая standard `cubic-bezier(0.33, 0, 0.67, 1)` в точке `t` (0..=1).
pub(super) fn standard(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    // x(s) монотонна: s по x делением пополам, затем y(s).
    let bez = |s: f32, p1: f32, p2: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * p1 + 3.0 * u * s * s * p2 + s * s * s
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let mid = f32::midpoint(lo, hi);
        if bez(mid, 0.33, 0.67) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(f32::midpoint(lo, hi), 0.0, 1.0)
}

/// Доля пройденного перехода длительностью `d` с момента `at` (0..=1).
pub(super) fn progress(at: Instant, d: Duration) -> f32 {
    (at.elapsed().as_secs_f32() / d.as_secs_f32()).min(1.0)
}

/// Левый край блика в долях заполненной части: от −ширины до 1 за цикл, по кривой.
pub(super) fn glint_left(now: Instant) -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(|| now);
    let cycle = GLINT_CYCLE.as_millis();
    #[allow(clippy::cast_precision_loss, reason = "миллисекунды в пределах цикла")]
    let t = (now.saturating_duration_since(start).as_millis() % cycle) as f32 / cycle as f32;
    standard(t) * (1.0 + GLINT_W) - GLINT_W
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_curve_ends_and_symmetry() {
        assert!(standard(0.0).abs() < 1e-3);
        assert!((standard(1.0) - 1.0).abs() < 1e-3);
        assert!((standard(0.5) - 0.5).abs() < 1e-3);
        assert!(standard(0.25) < 0.25, "медленный старт");
        let mut last = 0.0;
        for i in 0..=100 {
            #[allow(clippy::cast_precision_loss, reason = "шаги теста")]
            let y = standard(i as f32 / 100.0);
            assert!(y >= last - 1e-4, "монотонна");
            last = y;
        }
    }

    #[test]
    fn glint_runs_across_filled_part() {
        let x = glint_left(Instant::now());
        assert!((-GLINT_W..=1.0).contains(&x), "{x}");
    }
}
