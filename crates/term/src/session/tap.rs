//! Отвод сырого вывода сеанса (S0 F03 №3, `TermApi::tap`): байты PTY до эмулятора
//! и смены размера — подписчикам (запись сеанса, F03 §3.5). Ввод сюда не попадает:
//! reader видит только вывод процесса, writer в отвод не пишет.
//!
//! Без подписчиков — одна атомарная проверка на чтение PTY.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::sys::conpty::PtySize;

/// Событие отвода.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TapEvent {
    /// Сырой вывод PTY, как прочитан (UTF-8 не разрезан внутри символа).
    Output(Instant, Vec<u8>),
    /// Новый размер сетки.
    Resize(Instant, PtySize),
}

/// Приёмник отвода; отписка — drop.
pub type TapRx = Receiver<TapEvent>;

/// Отвод сеанса: живёт у слота менеджера и у сетки, ставится до старта reader —
/// подписчик видит вывод с первого байта.
#[derive(Debug, Default)]
pub struct Tap {
    on: AtomicBool,
    subs: Mutex<Vec<Sender<TapEvent>>>,
}

impl Tap {
    /// Подписаться: события с этого момента.
    pub fn subscribe(&self) -> TapRx {
        let (tx, rx) = unbounded();
        let mut subs = self
            .subs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.push(tx);
        self.on.store(true, Ordering::Release);
        rx
    }

    /// Есть подписчики.
    pub fn active(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    fn send(&self, ev: impl Fn() -> TapEvent) {
        if !self.active() {
            return;
        }
        let mut subs = self
            .subs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.retain(|tx| tx.send(ev()).is_ok());
        if subs.is_empty() {
            self.on.store(false, Ordering::Release);
        }
    }

    /// Вывод PTY (reader).
    pub(crate) fn output(&self, bytes: &[u8]) {
        if bytes.is_empty() || !self.active() {
            return;
        }
        let at = Instant::now();
        self.send(|| TapEvent::Output(at, bytes.to_vec()));
    }

    /// Новый размер сетки.
    pub(crate) fn resize(&self, size: PtySize) {
        if !self.active() {
            return;
        }
        let at = Instant::now();
        self.send(|| TapEvent::Resize(at, size));
    }
}
