//! Платформенный слой приложения (S0 v5 №5: `unsafe` разрешён только здесь).

#[cfg(windows)]
#[allow(unsafe_code, reason = "WinAPI: mutex, named pipe, SID (S1-15)")]
pub(crate) mod ipc_win;

/// Счётчик аллокаций для `--bench` (S2-20): обёртка над системным аллокатором.
#[cfg(feature = "count-alloc")]
#[allow(unsafe_code, reason = "GlobalAlloc — обёртка над System")]
mod count_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::Ordering;

    struct Counting;

    // SAFETY: делегирует System; счётчик — атомарный, без аллокаций.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            urfin_ui::bench::ALLOCS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: тот же контракт, что у вызывающего.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` выделен `System` с тем же `layout`.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
            urfin_ui::bench::ALLOCS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: тот же контракт, что у вызывающего.
            unsafe { System.realloc(ptr, layout, new) }
        }
    }

    #[global_allocator]
    static A: Counting = Counting;
}

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "WinAPI: время создания процесса, счётчики памяти (S9-09)"
)]
mod proc_win;

/// Сколько процесс прожил к моменту `at`: от создания (`GetProcessTimes`) — загрузчик ОС,
/// DLL, CRT до `main` (S9-09, фаза `startup.process`). `None` — ОС не дала время или
/// часы ушли назад.
pub fn process_age(at: std::time::SystemTime) -> Option<std::time::Duration> {
    #[cfg(windows)]
    {
        at.duration_since(proc_win::created()?).ok()
    }
    #[cfg(not(windows))]
    {
        let _ = at;
        None
    }
}

/// Память процесса, байт (S9-09): рабочий набор и private (commit), текущие и пиковые.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    pub ws: u64,
    pub ws_peak: u64,
    pub private: u64,
    pub private_peak: u64,
}

/// Счётчики памяти текущего процесса; `None` — ОС не дала (или не Windows).
pub fn memory() -> Option<Memory> {
    #[cfg(windows)]
    {
        proc_win::memory()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Программный Direct3D (WARP) для окна gpui: `GPUI_FORCE_WARP=1` читает патч
/// `pets-gpui-windows` при создании устройства. Звать до запуска других потоков.
pub fn force_warp() {
    // SAFETY: ранний старт, других потоков ещё нет — гонки с чтением окружения нет.
    #[allow(unsafe_code, reason = "set_var в edition 2024 — unsafe")]
    unsafe {
        std::env::set_var("GPUI_FORCE_WARP", "1");
    }
}
