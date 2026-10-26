//! Время CPU процесса и текущего потока (бенч G-07): Windows — `GetProcessTimes` /
//! `GetThreadTimes`, другие ОС — 0. Время UI-потока на кадр (S2-20) — по циклам
//! `QueryThreadCycleTime`: `GetThreadTimes` идёт тиками планировщика (~15,6 мс).

#![allow(unsafe_code)]

/// Секунды CPU процесса (ядро + пользователь).
pub fn process_secs() -> f64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let mut t = [windows_sys::Win32::Foundation::FILETIME::default(); 4];
        // SAFETY: псевдодескриптор текущего процесса; структуры — на стеке.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &raw mut t[0],
                &raw mut t[1],
                &raw mut t[2],
                &raw mut t[3],
            )
        };
        if ok != 0 {
            return secs(t[2]) + secs(t[3]);
        }
    }
    0.0
}

/// Секунды CPU текущего потока (поток интерфейса, если зван из него).
pub fn thread_secs() -> f64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
        let mut t = [windows_sys::Win32::Foundation::FILETIME::default(); 4];
        // SAFETY: псевдодескриптор текущего потока; структуры — на стеке.
        let ok = unsafe {
            GetThreadTimes(
                GetCurrentThread(),
                &raw mut t[0],
                &raw mut t[1],
                &raw mut t[2],
                &raw mut t[3],
            )
        };
        if ok != 0 {
            return secs(t[2]) + secs(t[3]);
        }
    }
    0.0
}

#[cfg(windows)]
fn secs(f: windows_sys::Win32::Foundation::FILETIME) -> f64 {
    let ticks = (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    #[allow(clippy::cast_precision_loss, reason = "секунды CPU")]
    let s = ticks as f64 / 10_000_000.0;
    s
}

/// Циклы CPU текущего потока (`QueryThreadCycleTime`).
pub fn thread_cycles() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::GetCurrentThread;
        use windows_sys::Win32::System::WindowsProgramming::QueryThreadCycleTime;
        let mut c = 0u64;
        // SAFETY: псевдодескриптор текущего потока; выходной — на стеке.
        if unsafe { QueryThreadCycleTime(GetCurrentThread(), &raw mut c) } != 0 {
            return c;
        }
    }
    0
}

/// Циклов `thread_cycles` в миллисекунде: калибровка занятым циклом 30 мс (один раз).
pub fn cycles_per_ms() -> f64 {
    static RATE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *RATE.get_or_init(|| {
        let (t, c) = (std::time::Instant::now(), thread_cycles());
        while t.elapsed() < std::time::Duration::from_millis(30) {
            std::hint::spin_loop();
        }
        #[allow(clippy::cast_precision_loss, reason = "частота")]
        let cycles = thread_cycles().saturating_sub(c) as f64;
        (cycles / (t.elapsed().as_secs_f64() * 1000.0)).max(1.0)
    })
}
