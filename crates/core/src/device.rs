//! `DeviceGate` (S5 §3.3, S5-19): сколько job одновременно на одном физическом
//! устройстве (`[ops.device_jobs]`: HDD 1, SSD 3, сеть 2, USB 1). Job, которой не
//! хватило слота, стоит в `Queued`; из ждущих одного устройства первой идёт более
//! приоритетная, при равном — раньше пришедшая. Запущенные не вытесняются.
//! `ops.run_now` обходит gate.
//!
//! Один на процесс ([`crate::Runtime::devices`]): копирование, удаление и скан анализа
//! места стоят в одной очереди устройства (решение 2026-10-30).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::{CancelToken, Cancelled, Config, JobId};

/// Как часто ожидание перепроверяет отмену, приоритет и `run_now`.
const POLL: Duration = Duration::from_millis(20);

/// Управление job снаружи (UI-поток): приоритет и «запустить сейчас».
#[derive(Debug, Default)]
pub struct JobCtl {
    /// Больше — раньше (Low 0, Normal 1, High 2).
    pub prio: AtomicU8,
    pub run_now: AtomicBool,
}

/// Job на одно устройство по классу носителя (`[ops.device_jobs]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceJobs {
    pub hdd: u8,
    pub ssd: u8,
    pub net: u8,
    pub usb: u8,
}

impl Default for DeviceJobs {
    fn default() -> Self {
        DeviceJobs {
            hdd: 1,
            ssd: 3,
            net: 2,
            usb: 1,
        }
    }
}

impl DeviceJobs {
    /// `[ops.device_jobs]` из конфига; нет или неверно — по умолчанию.
    pub fn from_config(c: &Config) -> DeviceJobs {
        c.ext
            .get("ops")
            .and_then(|o| o.get("device_jobs"))
            .and_then(|t| t.clone().try_into().ok())
            .unwrap_or_default()
    }
}

/// Устройство и сколько job на нём одновременно.
pub type Slot = (u64, u8);

#[derive(Debug)]
struct Waiter {
    job: JobId,
    seq: u64,
    ctl: Arc<JobCtl>,
    devices: SmallVec<[u64; 2]>,
}

#[derive(Debug, Default)]
struct St {
    running: HashMap<u64, u8>,
    waiting: Vec<Waiter>,
    seq: u64,
}

#[derive(Debug, Default)]
pub struct DeviceGate {
    st: Mutex<St>,
    cv: Condvar,
}

/// Занятые слоты устройств; освобождаются при drop.
#[derive(Debug)]
pub struct DeviceLease {
    gate: Arc<DeviceGate>,
    devices: SmallVec<[u64; 2]>,
}

impl DeviceGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Разбудить ждущих: сменился приоритет или `run_now`.
    pub fn poke(&self) {
        self.cv.notify_all();
    }

    /// Ждёт слоты на всех `slots` (источник и назначение — разные устройства —
    /// оба). `on_queue` — один раз, если пришлось ждать (job → `Queued`).
    pub fn acquire(
        self: &Arc<Self>,
        job: JobId,
        slots: &[Slot],
        ctl: &Arc<JobCtl>,
        cancel: &CancelToken,
        stop: &CancelToken,
        on_queue: &mut dyn FnMut(),
    ) -> Result<DeviceLease, Cancelled> {
        let devices: SmallVec<[u64; 2]> = slots.iter().map(|s| s.0).collect();
        let mut st = self.lock();
        st.seq += 1;
        let seq = st.seq;
        st.waiting.push(Waiter {
            job,
            seq,
            ctl: Arc::clone(ctl),
            devices: devices.clone(),
        });
        let mut queued = false;
        loop {
            if cancel.is_cancelled() || stop.is_cancelled() {
                st.waiting.retain(|w| w.job != job);
                self.cv.notify_all();
                return Err(Cancelled);
            }
            let now = ctl.run_now.load(Ordering::Relaxed);
            let free = slots
                .iter()
                .all(|(d, lim)| st.running.get(d).copied().unwrap_or(0) < (*lim).max(1));
            let prio = ctl.prio.load(Ordering::Relaxed);
            let first = !st.waiting.iter().any(|w| {
                w.job != job && w.devices.iter().any(|d| devices.contains(d)) && {
                    let p = w.ctl.prio.load(Ordering::Relaxed);
                    w.ctl.run_now.load(Ordering::Relaxed) || p > prio || (p == prio && w.seq < seq)
                }
            });
            if now || (free && first) {
                for d in &devices {
                    *st.running.entry(*d).or_default() += 1;
                }
                st.waiting.retain(|w| w.job != job);
                self.cv.notify_all();
                return Ok(DeviceLease {
                    gate: Arc::clone(self),
                    devices,
                });
            }
            if !queued {
                queued = true;
                drop(st);
                on_queue();
                st = self.lock();
                continue;
            }
            st = self
                .cv
                .wait_timeout(st, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.lock().waiting.len()
    }

    #[cfg(test)]
    pub(crate) fn running(&self, device: u64) -> u8 {
        self.lock().running.get(&device).copied().unwrap_or(0)
    }
}

impl Drop for DeviceLease {
    fn drop(&mut self) {
        let mut st = self.gate.lock();
        for d in &self.devices {
            if let Some(n) = st.running.get_mut(d) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    st.running.remove(d);
                }
            }
        }
        drop(st);
        self.gate.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// Ждёт условия (поток ожидающего встал в очередь), не дольше 5 с.
    fn until(f: impl Fn() -> bool) {
        let t0 = std::time::Instant::now();
        while !f() {
            assert!(t0.elapsed() < Duration::from_secs(5), "timeout");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn ctl(prio: u8) -> Arc<JobCtl> {
        let c = Arc::new(JobCtl::default());
        c.prio.store(prio, Ordering::Relaxed);
        c
    }

    /// Ждёт в отдельном потоке; в канал — номер job, получившей слот.
    fn wait_in_thread(
        g: &Arc<DeviceGate>,
        job: u64,
        slots: Vec<Slot>,
        c: Arc<JobCtl>,
        tx: mpsc::Sender<u64>,
        release: mpsc::Receiver<()>,
    ) -> std::thread::JoinHandle<()> {
        let g = Arc::clone(g);
        std::thread::spawn(move || {
            let t = CancelToken::new();
            let lease = g
                .acquire(JobId(job), &slots, &c, &t, &t, &mut || {})
                .unwrap();
            tx.send(job).unwrap();
            let _ = release.recv();
            drop(lease);
        })
    }

    #[test]
    fn hdd_one_job_ssd_parallel() {
        let g = Arc::new(DeviceGate::default());
        let t = CancelToken::new();
        let a = g
            .acquire(JobId(1), &[(7, 1)], &ctl(1), &t, &t, &mut || {})
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let (rel_tx, rel_rx) = mpsc::channel();
        let h = wait_in_thread(&g, 2, vec![(7, 1)], ctl(1), tx, rel_rx);
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "HDD: second waits"
        );
        drop(a);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        rel_tx.send(()).unwrap();
        h.join().unwrap();
        // SSD — три сразу.
        let leases: Vec<_> = (0..3)
            .map(|i| {
                g.acquire(JobId(10 + i), &[(9, 3)], &ctl(1), &t, &t, &mut || {})
                    .unwrap()
            })
            .collect();
        assert_eq!(g.running(9), 3);
        drop(leases);
        assert_eq!(g.running(9), 0);
    }

    #[test]
    fn high_priority_goes_first_run_now_bypasses() {
        let g = Arc::new(DeviceGate::default());
        let t = CancelToken::new();
        let busy = g
            .acquire(JobId(1), &[(7, 1)], &ctl(1), &t, &t, &mut || {})
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let (rel_normal, rx_normal) = mpsc::channel();
        let (rel_high, rx_high) = mpsc::channel();
        let normal = wait_in_thread(&g, 2, vec![(7, 1)], ctl(1), tx.clone(), rx_normal);
        until(|| g.waiting() == 1);
        let high = wait_in_thread(&g, 3, vec![(7, 1)], ctl(2), tx.clone(), rx_high);
        until(|| g.waiting() == 2);
        drop(busy);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            3,
            "High first"
        );
        // Normal ждёт; «запустить сейчас» — обходит gate, пока High ещё идёт.
        let nc = ctl(1);
        let (rel_now, rx_now) = mpsc::channel();
        let now = wait_in_thread(&g, 4, vec![(7, 1)], Arc::clone(&nc), tx, rx_now);
        until(|| g.waiting() == 2);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        nc.run_now.store(true, Ordering::Relaxed);
        g.poke();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            4,
            "run_now bypasses"
        );
        for r in [rel_high, rel_now] {
            r.send(()).unwrap();
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        rel_normal.send(()).unwrap();
        for h in [normal, high, now] {
            h.join().unwrap();
        }
    }

    #[test]
    fn cancel_while_queued() {
        let g = Arc::new(DeviceGate::default());
        let t = CancelToken::new();
        let _busy = g
            .acquire(JobId(1), &[(7, 1)], &ctl(1), &t, &t, &mut || {})
            .unwrap();
        let c = CancelToken::new();
        let h = {
            let (g, c) = (Arc::clone(&g), c.clone());
            std::thread::spawn(move || {
                let mut queued = false;
                let r = g.acquire(
                    JobId(2),
                    &[(7, 1)],
                    &ctl(1),
                    &c,
                    &CancelToken::new(),
                    &mut || {
                        queued = true;
                    },
                );
                (r.is_err(), queued)
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        c.cancel();
        assert_eq!(h.join().unwrap(), (true, true));
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn device_jobs_from_ops_section() {
        let mut c = Config::default();
        assert_eq!(DeviceJobs::from_config(&c), DeviceJobs::default());
        c.ext = "[ops.device_jobs]\nhdd = 2".parse().unwrap();
        let j = DeviceJobs::from_config(&c);
        assert_eq!((j.hdd, j.ssd), (2, 3));
    }
}
