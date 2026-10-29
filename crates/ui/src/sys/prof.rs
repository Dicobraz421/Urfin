//! Сэмплер UI-потока для `--bench` (G-07): `URFIN_PROF=<файл>` — раз в ~1 мс поток
//! останавливается, его стек раскручивается (`RtlVirtualUnwind`), после замера адреса
//! разрешаются в имена (dbghelp: PDB рядом с exe, у системных DLL — экспорты).
//! В отчёте — функции по «своему» времени (верх стека) и по включённому.
//! Пока поток стоит, сэмплер не выделяет память: куча может быть заперта им.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, CONTEXT_CONTROL_AMD64, CONTEXT_INTEGER_AMD64, GetThreadContext,
    RtlLookupFunctionEntry, RtlVirtualUnwind, SYMBOL_INFOW, SymFromAddrW, SymInitializeW,
    UNW_FLAG_NHANDLER,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, ResumeThread, SuspendThread,
};

const DEPTH: usize = 96;

/// Идущий замер.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    join: JoinHandle<Vec<Vec<u64>>>,
    out: String,
}

impl std::fmt::Debug for Sampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sampler")
    }
}

/// Запуск из UI-потока, если задан `URFIN_PROF`: стеки целиком.
pub fn start() -> Option<Sampler> {
    start_with(DEPTH)
}

/// Как [`start`], но только верхушка стека: на старте, пока грузятся DLL, раскрутка
/// (`RtlLookupFunctionEntry`) ждала бы блокировку загрузчика, которую держит остановленный поток.
pub fn start_leaf() -> Option<Sampler> {
    start_with(1)
}

fn start_with(depth: usize) -> Option<Sampler> {
    let out = std::env::var("URFIN_PROF").ok().filter(|s| !s.is_empty())?;
    let mut th: HANDLE = std::ptr::null_mut();
    // SAFETY: псевдодескрипторы текущих процесса и потока; результат — в `th`.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &raw mut th,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return None;
    }
    let th = th as usize;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let join = std::thread::spawn(move || {
        let th = th as HANDLE;
        let mut samples = Vec::with_capacity(20_000);
        let mut buf = [0u64; DEPTH];
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            // SAFETY: поток жив (UI ждёт `finish`); между Suspend и Resume — без аллокаций.
            let n = unsafe { sample(th, &mut buf, depth) };
            if n > 0 {
                samples.push(buf[..n].to_vec());
            }
        }
        // SAFETY: дубликат дескриптора — наш.
        unsafe { CloseHandle(th) };
        samples
    });
    Some(Sampler { stop, join, out })
}

/// `GetThreadContext` требует выравнивания `CONTEXT` по 16 байт.
#[repr(C, align(16))]
struct Ctx(CONTEXT);

/// Один стек остановленного потока; 0 — не вышло.
unsafe fn sample(th: HANDLE, buf: &mut [u64; DEPTH], depth: usize) -> usize {
    // SAFETY: см. вызывающего.
    unsafe {
        if SuspendThread(th) == u32::MAX {
            return 0;
        }
        let mut aligned: Ctx = std::mem::zeroed();
        let ctx = &mut aligned.0;
        ctx.ContextFlags = CONTEXT_CONTROL_AMD64 | CONTEXT_INTEGER_AMD64;
        let mut n = 0;
        if GetThreadContext(th, &raw mut *ctx) != 0 {
            while n < depth.min(DEPTH) && ctx.Rip != 0 {
                buf[n] = ctx.Rip;
                n += 1;
                let mut base = 0u64;
                let f = RtlLookupFunctionEntry(ctx.Rip, &raw mut base, std::ptr::null_mut());
                if f.is_null() {
                    // Лист без unwind-данных: адрес возврата на вершине стека.
                    ctx.Rip = *(ctx.Rsp as *const u64);
                    ctx.Rsp += 8;
                } else {
                    let mut data = std::ptr::null_mut();
                    let mut frame = 0u64;
                    RtlVirtualUnwind(
                        UNW_FLAG_NHANDLER,
                        base,
                        ctx.Rip,
                        f,
                        &raw mut *ctx,
                        &raw mut data,
                        &raw mut frame,
                        std::ptr::null_mut(),
                    );
                }
            }
        }
        ResumeThread(th);
        n
    }
}

impl Sampler {
    /// Остановить, разрешить имена, записать отчёт в файл `URFIN_PROF`.
    pub fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        let Ok(samples) = self.join.join() else {
            return;
        };
        let (report, folded) = report(&samples);
        if let Err(e) = std::fs::write(&self.out, report) {
            tracing::warn!(%e, "prof: report");
        }
        // Свёрнутые стеки (`корень;…;верх число`) — для flamegraph и поиска вызывающих.
        if let Err(e) = std::fs::write(format!("{}.folded", self.out), folded) {
            tracing::warn!(%e, "prof: folded");
        }
    }
}

fn report(samples: &[Vec<u64>]) -> (String, String) {
    // PDB ищется рядом с exe.
    let dir: Vec<u16> = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.as_os_str().to_owned()))
        .map_or_else(
            || vec![0],
            |d| {
                use std::os::windows::ffi::OsStrExt;
                d.encode_wide().chain(Some(0)).collect()
            },
        );
    // SAFETY: dbghelp для своего процесса; `invade` грузит символы модулей.
    unsafe { SymInitializeW(GetCurrentProcess(), dir.as_ptr(), 1) };
    let mut names: HashMap<u64, String> = HashMap::new();
    let mut own: HashMap<String, u32> = HashMap::new();
    let mut incl: HashMap<String, u32> = HashMap::new();
    let mut stacks: HashMap<String, u32> = HashMap::new();
    for s in samples {
        let mut seen = std::collections::HashSet::new();
        let mut path: Vec<String> = Vec::with_capacity(s.len());
        for (k, &a) in s.iter().enumerate() {
            // Адрес возврата указывает за call: −1, чтобы попасть в саму функцию.
            let a = if k == 0 { a } else { a - 1 };
            let name = names.entry(a).or_insert_with(|| name(a)).clone();
            if k == 0 {
                *own.entry(name.clone()).or_default() += 1;
            }
            path.push(short(&name));
            if seen.insert(name.clone()) {
                *incl.entry(name).or_default() += 1;
            }
        }
        path.reverse();
        *stacks.entry(path.join(";")).or_default() += 1;
    }
    let total = samples.len().max(1);
    let top = |m: &HashMap<String, u32>, n: usize| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let mut out = String::new();
        for (k, c) in v.into_iter().take(n) {
            #[allow(clippy::cast_precision_loss, reason = "проценты")]
            let p = f64::from(*c) * 100.0 / total as f64;
            let _ = writeln!(out, "{p:6.2}%  {k}");
        }
        out
    };
    let mut folded = String::new();
    for (k, c) in &stacks {
        let _ = writeln!(folded, "{k} {c}");
    }
    (
        format!(
            "samples={total}\n\n== self ==\n{}\n== inclusive ==\n{}",
            top(&own, 60),
            top(&incl, 150)
        ),
        folded,
    )
}

/// Имя кадра без параметров обобщений и без `;` (разделитель свёрнутых стеков).
fn short(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0u32;
    for ch in name.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ';' => {}
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn name(a: u64) -> String {
    #[repr(C)]
    struct Sym {
        info: SYMBOL_INFOW,
        rest: [u16; 512],
    }
    // SAFETY: буфер SYMBOL_INFOW с местом под имя; dbghelp инициализирован.
    unsafe {
        let mut s: Sym = std::mem::zeroed();
        s.info.SizeOfStruct = u32::try_from(size_of::<SYMBOL_INFOW>()).unwrap_or(0);
        s.info.MaxNameLen = 512;
        let mut disp = 0u64;
        if SymFromAddrW(GetCurrentProcess(), a, &raw mut disp, &raw mut s.info) != 0 {
            let len = s.info.NameLen.min(512) as usize;
            let p = std::ptr::addr_of!(s.info.Name).cast::<u16>();
            let name = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            return format!("{}!{name}", module(a));
        }
        format!("{}!0x{a:x}", module(a))
    }
}

fn module(a: u64) -> String {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    // SAFETY: адрес — только ключ поиска модуля; буфер имени на стеке.
    unsafe {
        let mut h = std::ptr::null_mut();
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            a as *const u16,
            &raw mut h,
        ) == 0
        {
            return "?".into();
        }
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(h, buf.as_mut_ptr(), 260) as usize;
        let path = String::from_utf16_lossy(&buf[..n]);
        path.rsplit('\\').next().unwrap_or("?").to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_a_busy_thread() {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let w = std::thread::spawn(move || {
            let mut th: HANDLE = std::ptr::null_mut();
            // SAFETY: псевдодескрипторы текущих процесса и потока.
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    GetCurrentThread(),
                    GetCurrentProcess(),
                    &raw mut th,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                );
            }
            tx.send(th as usize).unwrap();
            let mut x = 0u64;
            while !s2.load(Ordering::Relaxed) {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        });
        let th = rx.recv().unwrap() as HANDLE;
        let mut buf = [0u64; DEPTH];
        // SAFETY: поток жив до `stop`.
        let n = unsafe { sample(th, &mut buf, DEPTH) };
        let err = std::io::Error::last_os_error();
        stop.store(true, Ordering::Relaxed);
        w.join().unwrap();
        assert!(n > 0, "{err}");
    }
}
