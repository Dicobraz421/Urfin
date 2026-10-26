//! Платформенный слой приложения (S0 v5 №5: `unsafe` разрешён только здесь).

#[cfg(windows)]
#[allow(unsafe_code, reason = "WinAPI: журнал событий")]
mod windows;

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

/// Программный Direct3D (WARP) для окна gpui: `GPUI_FORCE_WARP=1` читает патч
/// `pets-gpui-windows` при создании устройства. Звать до запуска других потоков.
pub fn force_warp() {
    // SAFETY: ранний старт, других потоков ещё нет — гонки с чтением окружения нет.
    #[allow(unsafe_code, reason = "set_var в edition 2024 — unsafe")]
    unsafe {
        std::env::set_var("GPUI_FORCE_WARP", "1");
    }
}

/// Коды событий Urfin в журнале Windows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EventId {
    /// Паника: crash-файл записан.
    Crash = 1000,
    /// Ошибка уровня Error из логов.
    Error = 1001,
    /// Приложение не смогло стартовать.
    StartupFailed = 1002,
}

/// Записать ошибку в журнал Windows «Приложение». На других ОС — ничего.
/// Ошибки записи игнорируются: журнал — дополнительный канал, файл лога главный.
pub fn event_log(id: EventId, text: &str) {
    #[cfg(windows)]
    windows::report_error(id as u32, text);
    #[cfg(not(windows))]
    let _ = (id, text);
}
