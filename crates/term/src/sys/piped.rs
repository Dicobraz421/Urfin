//! Процесс с пайпами вместо консоли (S0 v6 №15, F02 §3.1): stdin — пустой пайп
//! (сразу EOF), stdout и stderr — пайпы, процесс и всё его дерево — в job object.
//!
//! Процесс стартует приостановленным и входит в job до первой инструкции: дети не
//! успевают родиться вне job, `kill_tree` (`TerminateJobObject`) достаёт всех.
//! Наследуются только хэндлы пайпов ребёнка (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::conpty::{SpawnSpec, command_line, env_block, last_error, pipe, wide};

/// Как запустить.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipedFlags {
    /// Без окна консоли (`CREATE_NO_WINDOW`): консоль у ребёнка есть, окна нет.
    pub no_window: bool,
    /// stderr — в пайп stdout: строки обоих потоков идут в порядке записи.
    pub merge_stderr: bool,
    /// Drop [`PipedChild`] убивает дерево (`KILL_ON_JOB_CLOSE`).
    pub kill_on_drop: bool,
}

/// Запущенный процесс: пайпы вывода и управление деревом. Методы — из любого потока.
#[derive(Debug)]
pub struct PipedChild {
    /// Читать в отдельном потоке до EOF: EOF приходит, когда пайп закрыли все
    /// процессы дерева.
    pub stdout: Option<File>,
    /// `None` при [`PipedFlags::merge_stderr`].
    pub stderr: Option<File>,
    process: OwnedHandle,
    job: OwnedHandle,
    pid: u32,
}

/// Хэндл, который наследует ребёнок.
fn inheritable(h: &OwnedHandle) -> io::Result<()> {
    // SAFETY: хэндл жив.
    if unsafe { SetHandleInformation(h.as_raw_handle(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
        == 0
    {
        return Err(last_error());
    }
    Ok(())
}

fn job(kill_on_close: bool) -> io::Result<OwnedHandle> {
    // SAFETY: безымянный job без атрибутов.
    let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if h.is_null() {
        return Err(last_error());
    }
    // SAFETY: хэндл только что создан.
    let job = unsafe { OwnedHandle::from_raw_handle(h) };
    if kill_on_close {
        // SAFETY: POD.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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
    }
    Ok(job)
}

/// Запуск `spec` с пайпами.
pub fn spawn(spec: &SpawnSpec, flags: PipedFlags) -> io::Result<PipedChild> {
    let (in_read, in_write) = pipe()?;
    let (out_read, out_write) = pipe()?;
    let err = if flags.merge_stderr {
        None
    } else {
        Some(pipe()?)
    };
    let err_write = err.as_ref().map_or(&out_write, |(_, w)| w);
    // Список наследуемых — без повторов (иначе CreateProcess отказывает).
    let mut inherit: Vec<HANDLE> = vec![in_read.as_raw_handle(), out_write.as_raw_handle()];
    if let Some((_, w)) = &err {
        inherit.push(w.as_raw_handle());
    }
    inheritable(&in_read)?;
    inheritable(&out_write)?;
    if let Some((_, w)) = &err {
        inheritable(w)?;
    }
    let job = job(flags.kill_on_drop)?;

    let mut attr_size = 0usize;
    // SAFETY: первый вызов узнаёт размер (ошибка ERROR_INSUFFICIENT_BUFFER ожидаема).
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut attr_size) };
    let mut attr_buf = vec![0u64; attr_size.div_ceil(8)];
    let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = attr_buf.as_mut_ptr().cast();
    // SAFETY: буфер нужного размера, выровнен по 8.
    if unsafe { InitializeProcThreadAttributeList(attrs, 1, 0, &raw mut attr_size) } == 0 {
        return Err(last_error());
    }
    // SAFETY: массив хэндлов живёт до DeleteProcThreadAttributeList.
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            inherit.as_ptr().cast::<c_void>(),
            size_of_val(inherit.as_slice()),
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
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = in_read.as_raw_handle();
    si.StartupInfo.hStdOutput = out_write.as_raw_handle();
    si.StartupInfo.hStdError = err_write.as_raw_handle();
    si.lpAttributeList = attrs;
    let mut cmd = command_line(spec);
    let env = env_block(&spec.env);
    let cwd = spec.cwd.as_ref().map(|c| wide(c.as_os_str()));
    let mut creation = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;
    if flags.no_window {
        creation |= CREATE_NO_WINDOW;
    }
    // SAFETY: POD.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: строки и буферы живут до конца вызова; наследуются только хэндлы из списка.
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            creation,
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
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(pi.hProcess),
            OwnedHandle::from_raw_handle(pi.hThread),
        )
    };
    // Концы ребёнка больше не нужны: иначе EOF stdout не придёт; stdin ребёнка —
    // пустой пайп без писателя.
    drop((in_read, in_write, out_write));
    let (err_read, err_write) = err.unzip();
    drop(err_write);
    // SAFETY: оба хэндла живы; процесс ещё не начал работу.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
        let e = last_error();
        // SAFETY: процесс наш и приостановлен: без job его дерево было бы не убить.
        unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        return Err(e);
    }
    // SAFETY: поток жив и приостановлен (CREATE_SUSPENDED).
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        let e = last_error();
        // SAFETY: job жив.
        unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
        return Err(e);
    }
    Ok(PipedChild {
        stdout: Some(File::from(out_read)),
        stderr: err_read.map(File::from),
        process,
        job,
        pid: pi.dwProcessId,
    })
}

impl PipedChild {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Живых процессов дерева (`ActiveProcesses` job).
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

    /// Убить процесс и всё его дерево (`TerminateJobObject`).
    pub fn kill_tree(&self) -> io::Result<()> {
        // SAFETY: job жив.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// Ждать выхода процесса (не дерева); `None` — ещё работает (таймаут).
    pub fn wait_timeout(&self, timeout: Option<Duration>) -> Option<u32> {
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
}

#[cfg(test)]
#[path = "piped_tests.rs"]
mod tests;
