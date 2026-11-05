//! Проход по записи (F03 §3.6): один раз при открытии, в фоне. Даёт индекс — ключевые
//! смещения и кадры, длительность, соответствие «текст без VT → время» — и сам текст
//! потоком в приёмник. Память — от длительности и размера окна, не от размера файла.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use urfin_core::CancelToken;
use urfin_term::session::{SessionEvent, SessionOpts, SessionSink, parse_inline};
use urfin_term::sys::conpty::PtySize;
use urfin_term::vt::shared::TermShared;

use super::key::{MARK, synth};
use super::parse::{Ev, Header, Lines, event};
use super::strip::Strip;
use crate::source::ByteSource;

/// Ключ не реже — по времени записи (F03 §3.6).
pub const KEY_EVERY_MS: u64 = 5_000;
/// …и по объёму: `cat` большого файла за секунду не должен делать повтор долгим.
pub const KEY_EVERY_BYTES: u64 = 256 << 10;
/// Память ключей: за каждый такой объём интервал растёт на шаг (многочасовые записи).
const KEY_MEM_STEP: usize = 64 << 20;
/// Точность времени совпадения: записи карты текста не чаще.
const TIME_STEP_MS: u64 = 100;
/// Буфер текста перед приёмником.
const TEXT_BUF: usize = 64 << 10;

/// Ключевой кадр.
#[derive(Clone, Debug)]
pub struct Key {
    /// Время последнего события в кадре, мс.
    pub ms: u64,
    /// Смещение следующего события.
    pub off: u64,
    pub cols: u16,
    pub rows: u16,
    /// Экран потоком VT ([`synth`]); пусто — начало записи.
    pub vt: Box<[u8]>,
    /// Кадр на альтернативном экране: номер последнего ключа основного экрана.
    pub base: Option<u32>,
}

/// Индекс записи.
#[derive(Debug, Default)]
pub struct CastIndex {
    pub duration_ms: u64,
    /// Хотя бы один ключ — начало записи; по возрастанию времени.
    pub keys: Vec<Key>,
    /// (конец текста, время): к этому смещению текста вывод дошёл к этому времени.
    times: Vec<(u64, u64)>,
    /// Битых строк пропущено.
    pub bad: u64,
}

impl CastIndex {
    /// Последний ключ не позже `ms`.
    pub fn key_before(&self, ms: u64) -> usize {
        self.keys.partition_point(|k| k.ms <= ms).saturating_sub(1)
    }

    /// Время, к которому байт текста `off` уже выведен.
    pub fn time_of(&self, off: u64) -> u64 {
        let i = self.times.partition_point(|t| t.0 <= off);
        self.times.get(i).or(self.times.last()).map_or(0, |t| t.1)
    }

    /// Сколько текста выведено к моменту `ms`.
    pub fn text_at(&self, ms: u64) -> u64 {
        let i = self.times.partition_point(|t| t.1 <= ms);
        i.checked_sub(1).map_or(0, |i| self.times[i].0)
    }

    /// Доля шкалы для времени `ms`.
    #[allow(clippy::cast_precision_loss, reason = "доля шкалы")]
    pub fn frac(&self, ms: u64) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        (ms as f64 / self.duration_ms as f64).min(1.0) as f32
    }
}

struct Quiet;

impl SessionSink for Quiet {
    fn poke(&self) {}
    fn event(&self, _: SessionEvent) {}
}

/// Эмулятор записи: без PTY и истории, ответы терминала отбрасываются.
pub(crate) fn emulator(cols: u16, rows: u16) -> Arc<TermShared> {
    let opts = SessionOpts {
        size: PtySize {
            cols: cols.max(1),
            rows: rows.max(1),
        },
        scrollback: 0,
        osc52: "off".into(),
        nonce: None,
    };
    parse_inline(&[][..], &opts, &Quiet)
}

/// Прогон записи. `text` получает вывод без VT кусками; `done` — пройдено байт файла.
/// `Ok(None)` — отменён.
pub fn scan(
    src: &Arc<dyn ByteSource>,
    h: &Header,
    text: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    cancel: &CancelToken,
    done: &AtomicU64,
) -> io::Result<Option<CastIndex>> {
    let mut lines = Lines::new(src.clone(), h.body);
    let term = emulator(h.width, h.height);
    let (mut cols, mut rows) = (h.width, h.height);
    let mut ix = CastIndex::default();
    ix.keys.push(Key {
        ms: 0,
        off: h.body,
        cols,
        rows,
        vt: Box::default(),
        base: None,
    });
    let mut strip = Strip::default();
    let mut buf = Vec::with_capacity(TEXT_BUF + 4096);
    let (mut text_len, mut since, mut mem, mut base) = (0u64, 0u64, 0usize, 0u32);
    let (mut now, mut n) = (0u64, 0u32);
    loop {
        n = n.wrapping_add(1);
        if n.is_multiple_of(256) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            done.store(lines.off(), Ordering::Relaxed);
        }
        let Some(line) = lines.next_line()? else {
            break;
        };
        if line.is_empty() {
            continue;
        }
        let Some(ev) = event(line) else {
            ix.bad += 1;
            continue;
        };
        // Время в записи не убывает; порченую строку назад не пускаем.
        now = now.max(ev.ms);
        match ev.ev {
            Ev::Out(s) => {
                term.inject(s.as_bytes());
                since += s.len() as u64;
                let at = buf.len();
                strip.feed(&s, &mut buf);
                // Забой мог укоротить буфер в этом же куске.
                text_len = (text_len + buf.len() as u64).saturating_sub(at as u64);
                match ix.times.last_mut() {
                    Some(t) if now.saturating_sub(t.1) < TIME_STEP_MS => {
                        t.0 = text_len;
                        t.1 = now;
                    }
                    _ if buf.len() != at => ix.times.push((text_len, now)),
                    _ => {}
                }
                if buf.len() >= TEXT_BUF {
                    text(&buf)?;
                    buf.clear();
                }
            }
            Ev::Resize(w, h) => {
                (cols, rows) = (w.max(1), h.max(1));
                term.resize(PtySize { cols, rows });
            }
            Ev::Other => {}
        }
        ix.duration_ms = now;
        let last = ix.keys.last().map_or(0, |k| k.ms);
        let slow = 1 + (mem / KEY_MEM_STEP) as u64;
        if now.saturating_sub(last) >= KEY_EVERY_MS * slow || since >= KEY_EVERY_BYTES * slow {
            let s = term.snapshot(&MARK);
            let vt = synth(&s).into_boxed_slice();
            mem += vt.len();
            let idx = u32::try_from(ix.keys.len()).unwrap_or(u32::MAX);
            if !s.alt {
                base = idx;
            }
            ix.keys.push(Key {
                ms: now,
                off: lines.off(),
                cols,
                rows,
                vt,
                base: s.alt.then_some(base),
            });
            since = 0;
        }
    }
    if !buf.is_empty() {
        text(&buf)?;
    }
    done.store(lines.off(), Ordering::Relaxed);
    Ok(Some(ix))
}
