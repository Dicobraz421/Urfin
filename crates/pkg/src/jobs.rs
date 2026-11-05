//! Действия как job (F02 §3.2): очередь на менеджер, прогресс из строк вывода, итог по
//! коду выхода. Исполнение — поток на job; одна job менеджера за раз, разные менеджеры
//! — параллельно.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use urfin_core::{CancelToken, JobId};

use crate::api::Mgr;

/// Шаг проверки отмены в очереди.
const POLL: Duration = Duration::from_millis(50);
/// Молчание, после которого ищется окно UAC (F02 §3.3).
pub const UAC_QUIET: Duration = Duration::from_millis(1500);
/// Шаг снимка процессов, пока job молчит (F02 §3.3).
pub const UAC_POLL: Duration = Duration::from_millis(500);

/// Что показать о молчащем действии (F02 §2.3, §3.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hush {
    /// Идёт: виден текущий пакет.
    #[default]
    Working,
    /// Запрос UAC своего запуска: ждёт точно (choco).
    Uac,
    /// «возможно, ждёт подтверждения UAC» (winget).
    MaybeUac,
    /// «нет вывода 10 мин» и «Прервать».
    Silent,
}

impl Hush {
    /// `quiet` — время без вывода, `exact` — свой запрос UAC ещё без ответа, `consent` —
    /// в сеансе есть окно согласия, открытое после начала шага.
    pub fn of(quiet: Duration, warn_after: Duration, exact: bool, consent: bool) -> Hush {
        if exact {
            Hush::Uac
        } else if quiet >= warn_after {
            Hush::Silent
        } else if consent && quiet >= UAC_QUIET {
            Hush::MaybeUac
        } else {
            Hush::Working
        }
    }
}

/// Очередь job одного менеджера: первая в очереди выполняется, остальные ждут.
#[derive(Debug, Default)]
struct Line {
    queue: Mutex<VecDeque<JobId>>,
    cv: Condvar,
}

/// Очереди менеджеров (у msiexec и установщиков менеджеров глобальная блокировка, F02
/// §0: параллельно всё равно не выйдет).
#[derive(Debug, Default)]
pub struct Gates {
    lines: [Line; 3],
}

/// Своя очередь job: уходит из очереди при drop.
#[derive(Debug)]
pub struct Turn<'a> {
    line: &'a Line,
    id: JobId,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut q = self
            .line
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        q.retain(|x| *x != self.id);
        self.line.cv.notify_all();
    }
}

impl Gates {
    fn line(&self, mgr: Mgr) -> &Line {
        &self.lines[mgr as usize]
    }

    /// Встать в очередь `mgr`: `true` — сразу своя очередь.
    pub fn enter(&self, mgr: Mgr, id: JobId) -> (Turn<'_>, bool) {
        let line = self.line(mgr);
        let mut q = line.queue.lock().unwrap_or_else(PoisonError::into_inner);
        q.push_back(id);
        let first = q.len() == 1;
        (Turn { line, id }, first)
    }

    /// Ждать своей очереди; `false` — отменено.
    pub fn wait(&self, turn: &Turn<'_>, cancel: &CancelToken) -> bool {
        let mut q = turn
            .line
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if cancel.is_cancelled() {
                return false;
            }
            if q.front() == Some(&turn.id) {
                return true;
            }
            q = turn
                .line
                .cv
                .wait_timeout(q, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Ход из строки вывода (F02 §3.2), без опоры на слова: `41%` и `0.6 MB / 1.5 MB`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineProgress {
    /// Доля 0…100.
    pub pct: Option<u8>,
    /// Байты: сделано и всего.
    pub bytes: Option<(u64, u64)>,
}

impl LineProgress {
    pub fn is_some(&self) -> bool {
        self.pct.is_some() || self.bytes.is_some()
    }
}

/// Число с точкой или запятой в конце `s` (`"… 41"`, `"1,5"`).
fn tail_number(s: &str) -> Option<f64> {
    let s = s.trim_end();
    let start = s
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit() || *c == '.' || *c == ',')
        .last()
        .map(|(i, _)| i)?;
    s[start..].replace(',', ".").parse().ok()
}

/// `1.5 MB` → байты; единица — KB/MB/GB (и `КБ/МБ/ГБ`), иначе байты.
fn size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(s.len());
    let n: f64 = s[..split].replace(',', ".").parse().ok()?;
    let unit = s[split..].trim().to_uppercase();
    let mul = match unit.chars().next() {
        Some('K' | 'К') => 1024.0,
        Some('M' | 'М') => 1024.0 * 1024.0,
        Some('G' | 'Г') => 1024.0 * 1024.0 * 1024.0,
        Some('B' | 'Б') | None => 1.0,
        _ => return None,
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "размер ≥ 0"
    )]
    Some((n * mul) as u64)
}

/// Ход по строке: последний `NN%` и пара `X MB / Y MB`.
pub fn progress_of(line: &str) -> LineProgress {
    let mut out = LineProgress::default();
    if let Some(i) = line.rfind('%')
        && let Some(n) = tail_number(&line[..i])
        && (0.0..=100.0).contains(&n)
    {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "0…100"
        )]
        {
            out.pct = Some(n as u8);
        }
    }
    if let Some((a, b)) = line.rsplit_once(" / ") {
        // «… 0.6 MB» — два последних слова.
        let words: Vec<&str> = a.split_whitespace().collect();
        let total: Vec<&str> = b.split_whitespace().take(2).collect();
        if words.len() >= 2 && total.len() == 2 {
            let d = size(&format!(
                "{} {}",
                words[words.len() - 2],
                words[words.len() - 1]
            ));
            let t = size(&format!("{} {}", total[0], total[1]));
            if let (Some(d), Some(t)) = (d, t)
                && t > 0
                && d <= t
            {
                out.bytes = Some((d, t));
                if out.pct.is_none() {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        clippy::cast_precision_loss,
                        reason = "0…100"
                    )]
                    {
                        out.pct = Some((d as f64 * 100.0 / t as f64).round() as u8);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Вывод снимает обе подписи; свой запрос UAC важнее, «нет вывода» — важнее эвристики.
    #[test]
    fn hush_states() {
        let ten = Duration::from_secs(600);
        let quiet = Duration::from_secs(2);
        let hush = |q, e, c| Hush::of(q, ten, e, c);
        assert_eq!(hush(Duration::ZERO, false, true), Hush::Working);
        assert_eq!(hush(quiet, false, false), Hush::Working);
        assert_eq!(hush(quiet, false, true), Hush::MaybeUac);
        assert_eq!(hush(ten, false, true), Hush::Silent);
        assert_eq!(hush(ten, false, false), Hush::Silent);
        assert_eq!(hush(ten, true, false), Hush::Uac);
    }

    #[test]
    fn progress_lines() {
        assert_eq!(progress_of("  ██████████  41%").pct, Some(41));
        let p = progress_of("  ███▒▒▒  1.50 MB / 3.00 MB");
        assert_eq!(p.bytes, Some((1_572_864, 3_145_728)));
        assert_eq!(p.pct, Some(50));
        assert_eq!(progress_of("  ███  0,6 МБ / 1,5 МБ").pct, Some(40));
        assert!(!progress_of("Found 7-Zip [7zip.7zip] Version 24.08").is_some());
        assert!(!progress_of("Successfully installed").is_some());
        assert_eq!(progress_of("Progress: 100%").pct, Some(100));
        assert!(!progress_of("a / b").is_some());
        assert!(!progress_of("150%").is_some());
    }

    /// Две job одного менеджера — по очереди, разных — сразу; отмена в очереди.
    #[test]
    fn queue_per_manager() {
        let gates = Arc::new(Gates::default());
        let (one, first) = gates.enter(Mgr::Winget, JobId(1));
        assert!(first);
        let (two, first) = gates.enter(Mgr::Winget, JobId(2));
        assert!(!first);
        let (_c, first) = gates.enter(Mgr::Scoop, JobId(3));
        assert!(first);
        let cancel = CancelToken::new();
        let g2 = Arc::clone(&gates);
        let t = std::thread::spawn(move || {
            let (d, _) = g2.enter(Mgr::Winget, JobId(4));
            let c = CancelToken::new();
            c.cancel();
            g2.wait(&d, &c)
        });
        assert!(!t.join().unwrap());
        drop(one);
        assert!(gates.wait(&two, &cancel));
    }
}
