//! Снимок процессов ОС (S0 v6 №15, F02 §3.3): имя, время старта и сеанс Windows.
//!
//! `NtQuerySystemInformation`, а не Toolhelp и `OpenProcess`: `consent.exe` работает от
//! SYSTEM, процесс без повышения прав не откроет его хэндл, а время старта Toolhelp не
//! отдаёт. Этот вызов отдаёт всё одним буфером и хэндлов не требует.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::size_of;
use std::time::{SystemTime, UNIX_EPOCH};

/// `SystemProcessInformation`.
const CLASS_PROCESSES: u32 = 5;
/// `STATUS_INFO_LENGTH_MISMATCH`: буфер мал.
const LENGTH_MISMATCH: i32 = 0xC000_0004_u32.cast_signed();
/// Начальный буфер (хватает на сотни процессов) и предел.
const START_WORDS: usize = 64 * 1024;
const MAX_WORDS: usize = 8 * 1024 * 1024;
/// 1601-01-01 → 1970-01-01, единицы по 100 нс.
const UNIX_AS_FILETIME: i64 = 116_444_736_000_000_000;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        len: u32,
        returned: *mut u32,
    ) -> i32;
}

/// Начало `SYSTEM_PROCESS_INFORMATION` (x64): зарезервированные поля описаны в
/// `winternl.h` байтами, смещения стабильны.
#[repr(C)]
struct ProcessInfo {
    next: u32,
    threads: u32,
    _reserved: [u8; 24],
    /// FILETIME: сотни наносекунд от 1601 года.
    create_time: i64,
    _times: [i64; 2],
    name_len: u16,
    name_max: u16,
    name: *const u16,
    _priority: i32,
    pid: usize,
    _parent: usize,
    _handles: u32,
    session: u32,
}

/// Процесс снимка.
struct Proc<'a> {
    name: &'a [u16],
    create_time: i64,
    pid: usize,
    session: u32,
}

/// Буфер снимка; слова — ради выравнивания записей.
fn snapshot() -> Option<Vec<u64>> {
    let mut buf = vec![0u64; START_WORDS];
    loop {
        let bytes = u32::try_from(buf.len() * size_of::<u64>()).ok()?;
        let mut need = 0u32;
        // SAFETY: буфер живёт весь вызов, его длина в байтах — `bytes`.
        let st = unsafe {
            NtQuerySystemInformation(
                CLASS_PROCESSES,
                buf.as_mut_ptr().cast(),
                bytes,
                &raw mut need,
            )
        };
        if st >= 0 {
            return Some(buf);
        }
        if st != LENGTH_MISMATCH || buf.len() >= MAX_WORDS {
            tracing::debug!(status = st, "term: process snapshot");
            return None;
        }
        // Между вызовами процессы рождаются: запас сверх ответа.
        let want = (need as usize / size_of::<u64>()).max(buf.len()) * 2;
        buf.resize(want.min(MAX_WORDS), 0);
    }
}

/// Записи снимка.
fn each(buf: &[u64], mut f: impl FnMut(&Proc<'_>)) {
    let bytes = buf.len() * size_of::<u64>();
    let mut off = 0usize;
    while off + size_of::<ProcessInfo>() <= bytes {
        // SAFETY: запись лежит в буфере целиком (проверено выше) и выровнена: ядро
        // кладёт записи по границе 8 байт, начало буфера — `u64`.
        let e = unsafe { &*buf.as_ptr().cast::<u8>().add(off).cast::<ProcessInfo>() };
        let name: &[u16] = if e.name.is_null() {
            &[]
        } else {
            // SAFETY: строка имени указывает внутрь того же буфера, длина — в байтах.
            unsafe { std::slice::from_raw_parts(e.name, usize::from(e.name_len) / 2) }
        };
        f(&Proc {
            name,
            create_time: e.create_time,
            pid: e.pid,
            session: e.session,
        });
        if e.next == 0 {
            break;
        }
        off += e.next as usize;
    }
}

fn low(c: u16) -> u16 {
    if (0x41..=0x5A).contains(&c) {
        c + 0x20
    } else {
        c
    }
}

fn filetime(t: SystemTime) -> i64 {
    let unix = t
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() / 100);
    UNIX_AS_FILETIME.saturating_add(i64::try_from(unix).unwrap_or(i64::MAX))
}

/// В сеансе Windows текущего процесса есть процесс `name`, запущенный не раньше `since`.
pub fn session_has(name: &str, since: SystemTime) -> bool {
    let Some(buf) = snapshot() else {
        return false;
    };
    let me = std::process::id() as usize;
    let mut session = None;
    each(&buf, |p| {
        if p.pid == me {
            session = Some(p.session);
        }
    });
    let Some(session) = session else {
        return false;
    };
    let since = filetime(since);
    let mut found = false;
    each(&buf, |p| {
        found = found
            || (p.session == session
                && p.create_time >= since
                && p.name
                    .iter()
                    .copied()
                    .map(low)
                    .eq(name.encode_utf16().map(low)));
    });
    found
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn own_name() -> String {
        std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// Свой процесс: виден, регистр не важен; старше `since` — нет.
    #[test]
    fn procs_own_process() {
        let name = own_name();
        assert!(session_has(&name, UNIX_EPOCH));
        assert!(session_has(&name.to_uppercase(), UNIX_EPOCH));
        let later = SystemTime::now() + Duration::from_secs(3600);
        assert!(!session_has(&name, later));
        assert!(!session_has("urfin-no-such-process.exe", UNIX_EPOCH));
    }

    /// Процесс, запущенный после `since`, виден, пока жив.
    #[test]
    fn procs_child_after_since() {
        let since = SystemTime::now() - Duration::from_secs(2);
        let argv = ["ping.exe", "-n", "4", "127.0.0.1"].map(std::ffi::OsString::from);
        let child = crate::proc::spawn_piped(
            &argv,
            None,
            &[],
            crate::proc::PipedFlags {
                no_window: true,
                merge_stderr: true,
                kill_on_drop: true,
            },
        )
        .unwrap();
        assert!(session_has("PING.EXE", since));
        child.kill_tree().unwrap();
    }
}
