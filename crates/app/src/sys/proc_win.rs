//! Время создания процесса (`GetProcessTimes`) и счётчики памяти — для `--bench-startup`
//! (S9-09, решение 2026-11-12 «startup-process-time»).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{FILETIME, HANDLE};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows_sys::core::BOOL;

use super::Memory;

/// 100-нс интервалов от 1601-01-01 до 1970-01-01 (UTC).
const UNIX_OFFSET_100NS: u64 = 116_444_736_000_000_000;

/// `FILETIME` (100 нс от 1601-01-01 UTC) → `SystemTime`.
fn system_time(ft: FILETIME) -> Option<SystemTime> {
    let t = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    let since_unix = t.checked_sub(UNIX_OFFSET_100NS)?;
    UNIX_EPOCH.checked_add(Duration::from_nanos(since_unix.checked_mul(100)?))
}

/// Когда ОС создала текущий процесс.
pub(super) fn created() -> Option<SystemTime> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: псевдохэндл текущего процесса закрывать не нужно; четыре FILETIME на стеке
    // живут до конца вызова.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if ok == 0 {
        return None;
    }
    system_time(created)
}

/// `PROCESS_MEMORY_COUNTERS_EX` (psapi.h). Своё объявление: в `windows-sys` у app нет
/// feature `Win32_System_ProcessStatus` (Cargo.toml — полоса deps).
#[repr(C)]
#[allow(clippy::struct_field_names, reason = "имена полей — как в psapi.h")]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    /// `GetProcessMemoryInfo` из kernel32 (Windows 7+).
    fn K32GetProcessMemoryInfo(
        process: HANDLE,
        counters: *mut ProcessMemoryCountersEx,
        cb: u32,
    ) -> BOOL;
}

/// Рабочий набор и private (commit) процесса, текущие и пиковые.
pub(super) fn memory() -> Option<Memory> {
    let cb = u32::try_from(size_of::<ProcessMemoryCountersEx>()).ok()?;
    let mut c = std::mem::MaybeUninit::<ProcessMemoryCountersEx>::zeroed();
    // SAFETY: псевдохэндл текущего процесса; буфер размера `cb`, выровнен под структуру.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), c.as_mut_ptr(), cb) };
    if ok == 0 {
        return None;
    }
    // SAFETY: при успехе структура заполнена (а до того обнулена — все поля целые).
    let c = unsafe { c.assume_init() };
    let b = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    Some(Memory {
        ws: b(c.working_set_size),
        ws_peak: b(c.peak_working_set_size),
        private: b(c.private_usage),
        private_peak: b(c.peak_pagefile_usage),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_epoch() {
        let ft = |t: u64| FILETIME {
            dwLowDateTime: t as u32,
            dwHighDateTime: (t >> 32) as u32,
        };
        assert_eq!(system_time(ft(UNIX_OFFSET_100NS)), Some(UNIX_EPOCH));
        assert_eq!(
            system_time(ft(UNIX_OFFSET_100NS + 15)),
            Some(UNIX_EPOCH + Duration::from_nanos(1500))
        );
        assert_eq!(system_time(ft(0)), None);
    }
}
