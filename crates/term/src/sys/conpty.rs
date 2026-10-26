//! `ConPTY` (S6 §1.1, S6-02): псевдоконсоль, процесс в job object, ввод/вывод
//! пайпами, размер, закрытие, убийство дерева, ожидание выхода.
//!
//! Порядок закрытия: `ClosePseudoConsole` вызывает писатель (не читатель — иначе
//! дедлок на старых системах), читатель дочитывает conout до EOF. Если в системе
//! есть `ReleasePseudoConsole`, он вызывается сразу после старта — тогда EOF
//! приходит сам с выходом ребёнка.
//!
//! Job object — без `KILL_ON_JOB_CLOSE`, с `BREAKAWAY_OK`: GUI-программы,
//! запущенные из терминала, переживают выход Urfin; job нужен для «занят»
//! (`ActiveProcesses`) и `kill_tree` (`TerminateJobObject`).

#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    RegisterWaitForSingleObject, STARTF_USESTDHANDLES, STARTUPINFOEXW, UnregisterWaitEx,
    UpdateProcThreadAttribute, WT_EXECUTEONLYONCE, WaitForSingleObject,
};

/// Размер псевдоконсоли.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    fn coord(self) -> COORD {
        COORD {
            X: i16::try_from(self.cols.max(1)).unwrap_or(i16::MAX),
            Y: i16::try_from(self.rows.max(1)).unwrap_or(i16::MAX),
        }
    }
}

/// Что запустить.
#[derive(Clone, Debug, Default)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Добавить к окружению Urfin (или заменить одноимённые).
    pub env: Vec<(OsString, OsString)>,
}

/// Запущенный процесс в псевдоконсоли: вывод, ввод и управление.
#[derive(Debug)]
pub struct Pty {
    /// conout: читать в отдельном потоке до EOF.
    pub reader: File,
    /// conin.
    pub writer: File,
    pub control: Arc<PtyControl>,
}

/// Управление псевдоконсолью и процессом. Методы — из любого потока.
#[derive(Debug)]
pub struct PtyControl {
    hpc: Mutex<Option<HpcHandle>>,
    process: OwnedHandle,
    job: OwnedHandle,
    pid: u32,
}

/// `HPCON` как значение, которое можно передать между потоками.
#[derive(Debug)]
struct HpcHandle(HPCON);

// SAFETY: HPCON — непрозрачный хэндл ядра; ConPTY допускает вызовы из любого потока,
// доступ сериализован Mutex.
unsafe impl Send for HpcHandle {}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Аргумент по правилам CRT (`CommandLineToArgvW`): кавычки при пробелах, табах
/// и кавычках; обратные слэши перед кавычкой и в конце удваиваются.
pub fn quote_arg(arg: &OsStr, out: &mut Vec<u16>) {
    let a: Vec<u16> = arg.encode_wide().collect();
    let needs = a.is_empty()
        || a.iter()
            .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
    if !needs {
        out.extend_from_slice(&a);
        return;
    }
    out.push(u16::from(b'"'));
    let mut slashes = 0usize;
    for &c in &a {
        if c == u16::from(b'\\') {
            slashes += 1;
            continue;
        }
        if c == u16::from(b'"') {
            out.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(u16::from(b'\\'), slashes));
        }
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2));
    out.push(u16::from(b'"'));
}

fn command_line(spec: &SpawnSpec) -> Vec<u16> {
    let mut cmd = Vec::new();
    quote_arg(spec.program.as_os_str(), &mut cmd);
    for a in &spec.args {
        cmd.push(u16::from(b' '));
        quote_arg(a, &mut cmd);
    }
    cmd.push(0);
    cmd
}

/// Блок окружения: текущее + добавки, отсортированный без учёта регистра,
/// `KEY=VALUE\0…\0\0` (UTF-16).
fn env_block(extra: &[(OsString, OsString)]) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (k, v) in extra {
        let up = k.to_string_lossy().to_uppercase();
        vars.retain(|(kk, _)| kk.to_string_lossy().to_uppercase() != up);
        vars.push((k.clone(), v.clone()));
    }
    vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut out = Vec::new();
    for (k, v) in vars {
        out.extend(k.encode_wide());
        out.push(u16::from(b'='));
        out.extend(v.encode_wide());
        out.push(0);
    }
    out.push(0);
    out
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn hr(r: i32) -> io::Result<()> {
    if r < 0 {
        // HRESULT_FROM_WIN32: младшие 16 бит — код Win32.
        #[allow(clippy::cast_sign_loss, reason = "разбор HRESULT")]
        let code = (r as u32) & 0xFFFF;
        return Err(io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(0),
        ));
    }
    Ok(())
}

/// Анонимный пайп: (чтение, запись).
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: выходные указатели — локальные; атрибуты по умолчанию (без наследования).
    if unsafe { CreatePipe(&raw mut r, &raw mut w, std::ptr::null(), 0) } == 0 {
        return Err(last_error());
    }
    // SAFETY: хэндлы только что созданы и принадлежат нам.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(r),
            OwnedHandle::from_raw_handle(w),
        )
    })
}

/// `ReleasePseudoConsole` (Windows 11 24H2+): EOF conout приходит с выходом
/// ребёнка без `ClosePseudoConsole`.
fn release_pseudo_console(hpc: HPCON) {
    type Release = unsafe extern "system" fn(HPCON) -> i32;
    let name: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    // SAFETY: kernel32 всегда загружен; имя функции — C-строка.
    let f = unsafe {
        let m = GetModuleHandleW(name.as_ptr());
        if m.is_null() {
            return;
        }
        GetProcAddress(m, c"ReleasePseudoConsole".as_ptr().cast())
    };
    if let Some(f) = f {
        // SAFETY: сигнатура `HRESULT ReleasePseudoConsole(HPCON)`.
        let f: Release =
            unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Release>(f) };
        // SAFETY: hpc живой.
        let _ = unsafe { f(hpc) };
    }
}

/// Job для учёта и `kill_tree`: без `KILL_ON_JOB_CLOSE`, с `BREAKAWAY_OK`.
fn job() -> io::Result<OwnedHandle> {
    // SAFETY: безымянный job без атрибутов.
    let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if h.is_null() {
        return Err(last_error());
    }
    // SAFETY: хэндл только что создан.
    let job = unsafe { OwnedHandle::from_raw_handle(h) };
    // SAFETY: POD.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    // SAFETY: структура своего размера.
    let ok = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(job)
}

/// Запуск `spec` в новой псевдоконсоли размера `size`.
pub fn spawn(spec: &SpawnSpec, size: PtySize) -> io::Result<Pty> {
    let (in_read, in_write) = pipe()?;
    let (out_read, out_write) = pipe()?;
    let mut hpc: HPCON = 0;
    // SAFETY: хэндлы пайпов живы; ConPTY дублирует их себе.
    hr(unsafe {
        CreatePseudoConsole(
            size.coord(),
            in_read.as_raw_handle(),
            out_write.as_raw_handle(),
            0,
            &raw mut hpc,
        )
    })?;
    // Концы ConPTY больше не нужны: иначе EOF conout не придёт.
    drop((in_read, out_write));
    let hpc_guard = scopeguard_hpc(hpc);

    let mut attr_size = 0usize;
    // SAFETY: первый вызов узнаёт размер (ошибка ERROR_INSUFFICIENT_BUFFER ожидаема).
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut attr_size) };
    let mut attr_buf = vec![0u64; attr_size.div_ceil(8)];
    let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = attr_buf.as_mut_ptr().cast();
    // SAFETY: буфер нужного размера, выровнен по 8.
    if unsafe { InitializeProcThreadAttributeList(attrs, 1, 0, &raw mut attr_size) } == 0 {
        return Err(last_error());
    }
    // SAFETY: значение атрибута — сам HPCON (по документации передаётся по значению).
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const c_void,
            size_of::<HPCON>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        let e = last_error();
        // SAFETY: список инициализирован выше.
        unsafe { DeleteProcThreadAttributeList(attrs) };
        return Err(e);
    }

    // SAFETY: POD.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap_or(0);
    // Без стандартных хэндлов родителя: ребёнок видит только консоль.
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.lpAttributeList = attrs;
    let mut cmd = command_line(spec);
    let env = env_block(&spec.env);
    let cwd = spec.cwd.as_ref().map(|c| wide(c.as_os_str()));
    // SAFETY: POD.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: строки и буферы живут до конца вызова; хэндлы не наследуются.
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
            &raw const si.StartupInfo,
            &raw mut pi,
        )
    };
    // SAFETY: список больше не нужен.
    unsafe { DeleteProcThreadAttributeList(attrs) };
    if ok == 0 {
        return Err(last_error());
    }
    // SAFETY: хэндлы процесса и потока — наши.
    let (process, _thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(pi.hProcess),
            OwnedHandle::from_raw_handle(pi.hThread),
        )
    };
    let job = job()?;
    // SAFETY: оба хэндла живы. Ошибка (процесс уже в чужом job без вложенности) — не
    // фатальна: без job нет только «занят» и kill дерева.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
        tracing::warn!(error = %last_error(), "conpty: process not assigned to job");
    }
    release_pseudo_console(hpc);
    let hpc = hpc_guard.disarm();
    Ok(Pty {
        reader: File::from(out_read),
        writer: File::from(in_write),
        control: Arc::new(PtyControl {
            hpc: Mutex::new(Some(HpcHandle(hpc))),
            process,
            job,
            pid: pi.dwProcessId,
        }),
    })
}

/// Закрывает HPCON при ошибке до конца `spawn`.
struct HpcGuard(Option<HPCON>);

fn scopeguard_hpc(h: HPCON) -> HpcGuard {
    HpcGuard(Some(h))
}

impl HpcGuard {
    fn disarm(mut self) -> HPCON {
        self.0.take().unwrap_or(0)
    }
}

impl Drop for HpcGuard {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            // SAFETY: HPCON создан в `spawn` и ещё не закрыт.
            unsafe { ClosePseudoConsole(h) };
        }
    }
}

impl PtyControl {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Новый размер псевдоконсоли.
    pub fn resize(&self, size: PtySize) -> io::Result<()> {
        let g = self.hpc.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(h) = g.as_ref() else {
            return Ok(());
        };
        // SAFETY: HPCON жив, пока лежит в Mutex.
        hr(unsafe { ResizePseudoConsole(h.0, size.coord()) })
    }

    /// Закрыть псевдоконсоль (один раз). Звать не из потока, читающего conout:
    /// на старых системах вызов ждёт, пока вывод дочитают.
    pub fn close(&self) {
        let h = self
            .hpc
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(h) = h {
            // SAFETY: HPCON жив и закрывается один раз (вынут из Mutex).
            unsafe { ClosePseudoConsole(h.0) };
        }
    }

    /// Процесс или его потомки ещё работают (`ActiveProcesses` job).
    pub fn active_processes(&self) -> u32 {
        // SAFETY: POD.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: структура своего размера.
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).unwrap_or(0),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { 0 } else { info.ActiveProcesses }
    }

    /// Занят: в job кроме самого shell есть ещё процессы (без интеграции — эвристика).
    pub fn busy(&self) -> bool {
        self.active_processes() > 1
    }

    /// Убить процесс и всё его дерево (`TerminateJobObject`).
    pub fn kill_tree(&self) -> io::Result<()> {
        // SAFETY: job жив.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// Ждать выхода процесса; `None` — ещё работает (таймаут).
    pub fn wait(&self, timeout: Option<Duration>) -> Option<u32> {
        let ms = timeout.map_or(INFINITE, |d| {
            u32::try_from(d.as_millis()).unwrap_or(INFINITE - 1)
        });
        // SAFETY: хэндл процесса жив.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), ms) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: процесс завершён; выходной — локальный.
                unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) };
                Some(code)
            }
            // WAIT_TIMEOUT и ошибки ожидания.
            _ => None,
        }
    }

    /// Колбэк при выходе процесса (поток системного пула, `RegisterWaitForSingleObject`).
    /// Отписка — drop результата.
    pub fn on_exit(
        self: &Arc<Self>,
        f: impl FnOnce(u32) + Send + 'static,
    ) -> io::Result<ExitWatch> {
        let ctx = Arc::new(ExitCtx {
            control: Arc::clone(self),
            f: Mutex::new(Some(Box::new(f))),
        });
        let raw = Arc::into_raw(Arc::clone(&ctx)).cast::<c_void>();
        let mut wait: HANDLE = std::ptr::null_mut();
        // SAFETY: контекст живёт до UnregisterWaitEx в Drop `ExitWatch` (там же — освобождение).
        let ok = unsafe {
            RegisterWaitForSingleObject(
                &raw mut wait,
                self.process.as_raw_handle(),
                Some(exit_callback),
                raw,
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if ok == 0 {
            // SAFETY: регистрации нет — вернуть ссылку.
            drop(unsafe { Arc::from_raw(raw.cast::<ExitCtx>()) });
            return Err(last_error());
        }
        Ok(ExitWatch {
            wait: Some(wait as isize),
            raw: raw as isize,
        })
    }
}

impl Drop for PtyControl {
    fn drop(&mut self) {
        self.close();
    }
}

struct ExitCtx {
    control: Arc<PtyControl>,
    f: Mutex<Option<OnExit>>,
}

/// Колбэк выхода процесса с кодом.
type OnExit = Box<dyn FnOnce(u32) + Send>;

unsafe extern "system" fn exit_callback(ctx: *mut c_void, _timed_out: bool) {
    // SAFETY: `ctx` — `Arc<ExitCtx>` из `on_exit`, жив до `UnregisterWaitEx`.
    let ctx = unsafe { &*ctx.cast::<ExitCtx>() };
    let f = ctx.f.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(f) = f {
        let code = ctx.control.wait(Some(Duration::ZERO)).unwrap_or(0);
        // Паника не должна пересечь границу FFI.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(code)));
    }
}

/// Подписка на выход процесса; drop — отписка (ждёт текущий колбэк).
#[derive(Debug)]
pub struct ExitWatch {
    wait: Option<isize>,
    raw: isize,
}

impl Drop for ExitWatch {
    fn drop(&mut self) {
        if let Some(w) = self.wait.take() {
            // SAFETY: хэндл ожидания из RegisterWaitForSingleObject; INVALID_HANDLE_VALUE —
            // дождаться завершения колбэка, после чего контекст можно освободить.
            unsafe { UnregisterWaitEx(w as HANDLE, INVALID_HANDLE_VALUE) };
        }
        // SAFETY: ссылка из `Arc::into_raw` в `on_exit`; колбэк больше не вызовется.
        drop(unsafe { Arc::from_raw(self.raw as *const ExitCtx) });
    }
}

/// Хэндлов у текущего процесса (тест утечек).
#[cfg(test)]
pub(crate) fn handle_count() -> u32 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut n = 0u32;
    // SAFETY: псевдохэндл текущего процесса; выходной — локальный.
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &raw mut n) };
    n
}

const FILETIME_ZERO: windows_sys::Win32::Foundation::FILETIME =
    windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };

fn ticks(f: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    u64::from(f.dwHighDateTime) << 32 | u64::from(f.dwLowDateTime)
}

/// Процессорное время текущего процесса, user + kernel (тест и замер простоя, S6-22).
pub fn cpu_time() -> Duration {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut times = [FILETIME_ZERO; 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: псевдохэндл текущего процесса; выходные — локальные.
    unsafe { GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user) };
    Duration::from_nanos((ticks(*kernel) + ticks(*user)) * 100)
}

/// Private bytes текущего процесса (замер памяти сеансов, S6-22); 0 — ошибка.
pub fn private_bytes() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c = std::mem::MaybeUninit::<PROCESS_MEMORY_COUNTERS_EX>::zeroed();
    let Ok(cb) = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()) else {
        return 0;
    };
    // SAFETY: псевдохэндл текущего процесса, буфер нужного размера.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), c.as_mut_ptr().cast(), cb) };
    if ok == 0 {
        return 0;
    }
    // SAFETY: при успехе структура заполнена.
    let c = unsafe { c.assume_init() };
    u64::try_from(c.PrivateUsage).unwrap_or(0)
}

/// Время всех ядер системы с загрузки: (простой, всего) в тиках 100 нс (тест
/// циклов: занятость системы за интервал).
#[cfg(test)]
pub(crate) fn system_times() -> (u64, u64) {
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    let mut times = [FILETIME_ZERO; 3];
    let [idle, kernel, user] = &mut times;
    // SAFETY: выходные — локальные.
    unsafe { GetSystemTimes(idle, kernel, user) };
    // Время ядра включает простой.
    (ticks(*idle), ticks(*kernel) + ticks(*user))
}

#[cfg(test)]
#[path = "conpty_tests.rs"]
mod tests;
