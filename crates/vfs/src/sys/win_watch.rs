//! Наблюдение за каталогом на Windows (S4 §5.2, S4-12).
//!
//! Один поток `watch-iocp` ждёт `GetQueuedCompletionStatusEx` на общем порту
//! завершения; каждый каталог — хэндл `FILE_FLAG_OVERLAPPED` с
//! `ReadDirectoryChangesExW` (`ReadDirectoryNotifyExtendedInformation`: готовый
//! `Entry` без stat). ФС без Ex-класса — `ReadDirectoryChangesW` (события без
//! `Entry`); без обоих — `Unsupported` (hub включит polling).
//!
//! Владение: `Watch` в куче, его адрес — ключ завершения. После первого чтения
//! `Watch` трогает только поток. `WatchGuard::drop` просит поток отменить чтение
//! и закрыть хэндл и ждёт ответа: после drop каталог можно переименовать или
//! удалить. Память `Watch` (буфер, `OVERLAPPED`) освобождается, когда пришло
//! завершение отменённого чтения. Поток живёт, пока есть наблюдения.
//!
//! Поток — обычный `std::thread`, не `Runtime::spawn_dedicated` (S0 v2 №7):
//! `Vfs::watch` рантайма не получает. Он только ждёт порт (0% CPU в простое) и
//! останавливается с последним гардом; долгий воркер на рантайме — hub (S4-14).

use std::path::Path;
use std::ptr;
use std::sync::Mutex;
use std::thread::JoinHandle;

use compact_str::CompactString;
use crossbeam_channel::{Receiver, Sender};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED,
    FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_EXTENDED_INFORMATION, FILE_NOTIFY_INFORMATION,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesExW,
    ReadDirectoryChangesW, ReadDirectoryNotifyExtendedInformation,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatusEx, OVERLAPPED, OVERLAPPED_ENTRY,
    PostQueuedCompletionStatus,
};

use super::windows::{entry, os_err, verbatim};
use crate::api::{Entry, VfsError, WatchEvent, WatchGuard, WatchTx};

/// 64 КБ — предел буфера уведомлений по SMB (S4 §5.2).
const BUF: usize = 64 * 1024;
/// Размер буфера; тесты уменьшают его, чтобы вызвать переполнение.
fn buf_len() -> usize {
    #[cfg(test)]
    {
        let n = tests::BUF_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
        if n > 0 {
            return n;
        }
    }
    BUF
}

/// Ключ завершения «прочитать управляющие сообщения»; адрес `Watch` нулём не бывает.
const WAKE: usize = 0;
const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_ATTRIBUTES;
const INFINITE: u32 = u32::MAX;

/// NTSTATUS в `OVERLAPPED::Internal`.
const STATUS_SUCCESS: u32 = 0;
const STATUS_NOTIFY_ENUM_DIR: u32 = 0x0000_010C;
const STATUS_CANCELLED: u32 = 0xC000_0120;

/// Наблюдение за одним каталогом. Адрес стабилен (Box) — это ключ завершения.
struct Watch {
    ov: OVERLAPPED,
    h: HANDLE,
    buf: Vec<u64>,
    ex: bool,
    tx: WatchTx,
    /// Старое имя переименования ждёт новое в том же буфере.
    renamed_from: Option<CompactString>,
    /// Чтение выдано, его завершение ещё не забрано.
    pending: bool,
    /// Хэндл закрыт по `Remove`; память освобождается на завершении чтения.
    closing: bool,
}

enum Ctrl {
    Add,
    Remove(usize, Sender<()>),
    Stop,
}

/// Порт завершения и его поток; `count` — живые `WatchGuard`.
struct Service {
    port: Port,
    ctrl: Sender<Ctrl>,
    thread: JoinHandle<()>,
    count: usize,
}

#[derive(Copy, Clone)]
struct Port(HANDLE);

// SAFETY: хэндл порта завершения можно использовать из любого потока.
unsafe impl Send for Port {}

impl Port {
    fn wake(self) {
        // SAFETY: порт открыт, пока жив `Service`; пакет без OVERLAPPED.
        unsafe { PostQueuedCompletionStatus(self.0, 0, WAKE, ptr::null()) };
    }
}

static SERVICE: Mutex<Option<Service>> = Mutex::new(None);

/// Снимает наблюдение при drop.
struct Guard {
    key: usize,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut svc = SERVICE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(s) = svc.as_mut() else {
            return;
        };
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        if s.ctrl.send(Ctrl::Remove(self.key, ack_tx)).is_ok() {
            s.port.wake();
            let _ = ack_rx.recv();
        }
        s.count -= 1;
        stop_if_idle(&mut svc);
    }
}

/// Наблюдать `dir` (без подкаталогов). `Unsupported` — ФС не умеет уведомлять.
pub fn watch(dir: &Path, tx: WatchTx) -> Result<WatchGuard, VfsError> {
    let path = verbatim(dir);
    // SAFETY: путь — UTF-16 с NUL; хэндл закрывается в `Drop` гарда или ниже.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: сразу после неудачного вызова.
        return Err(os_err(dir, unsafe { GetLastError() }));
    }
    let mut svc = SERVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let s = match svc.as_mut() {
        Some(s) => s,
        None => match start() {
            Ok(s) => svc.insert(s),
            Err(code) => {
                // SAFETY: хэндл открыт выше и больше нигде не используется.
                unsafe { CloseHandle(h) };
                return Err(os_err(dir, code));
            }
        },
    };
    let w = Box::into_raw(Box::new(Watch {
        // SAFETY: OVERLAPPED — POD, нули допустимы.
        ov: unsafe { std::mem::zeroed() },
        h,
        buf: vec![0u64; buf_len() / 8],
        ex: true,
        tx,
        renamed_from: None,
        pending: false,
        closing: false,
    }));
    let key = w as usize;
    // SAFETY: `h` и порт открыты; ключ — адрес живого `Watch`.
    let bound = unsafe { CreateIoCompletionPort(h, s.port.0, key, 0) };
    let first = if bound.is_null() {
        // SAFETY: сразу после неудачного вызова.
        Err(unsafe { GetLastError() })
    } else {
        // SAFETY: `w` только что создан и ещё не отдан потоку.
        arm_first(unsafe { &mut *w })
    };
    if let Err(code) = first {
        // SAFETY: чтение не выдано — завершений по `w` не будет; освобождаем сами.
        let w = unsafe { Box::from_raw(w) };
        // SAFETY: хэндл открыт выше.
        unsafe { CloseHandle(w.h) };
        drop(w);
        let unsupported = matches!(
            code,
            ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER
        );
        let e = if unsupported {
            VfsError::Unsupported("directory change notifications")
        } else {
            os_err(dir, code)
        };
        stop_if_idle(&mut svc);
        return Err(e);
    }
    s.count += 1;
    let _ = s.ctrl.send(Ctrl::Add);
    s.port.wake();
    Ok(WatchGuard::new(Guard { key }))
}

/// Наблюдений нет — остановить поток (он дождётся завершений отменённых чтений)
/// и закрыть порт.
fn stop_if_idle(svc: &mut Option<Service>) {
    if svc.as_ref().is_some_and(|s| s.count == 0)
        && let Some(s) = svc.take()
    {
        let _ = s.ctrl.send(Ctrl::Stop);
        s.port.wake();
        let _ = s.thread.join();
        // SAFETY: поток завершён, порт больше никто не использует.
        unsafe { CloseHandle(s.port.0) };
    }
}

fn start() -> Result<Service, u32> {
    // SAFETY: новый порт без файла.
    let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 1) };
    if port.is_null() {
        // SAFETY: сразу после неудачного вызова.
        return Err(unsafe { GetLastError() });
    }
    let port = Port(port);
    let (ctrl, rx) = crossbeam_channel::unbounded();
    let thread = std::thread::Builder::new()
        .name("watch-iocp".into())
        .spawn(move || run(port, &rx))
        .map_err(|e| e.raw_os_error().map_or(0, i32::cast_unsigned))?;
    Ok(Service {
        port,
        ctrl,
        thread,
        count: 0,
    })
}

/// Первое чтение: Ex-класс, при отказе ФС — обычный `ReadDirectoryChangesW`.
fn arm_first(w: &mut Watch) -> Result<(), u32> {
    match arm(w) {
        Err(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER) => {
            tracing::debug!("ReadDirectoryChangesExW unsupported, plain ReadDirectoryChangesW");
            w.ex = false;
            arm(w)
        }
        r => r,
    }
}

/// Выдать чтение изменений.
fn arm(w: &mut Watch) -> Result<(), u32> {
    // SAFETY: OVERLAPPED — POD; чтение не выдано, ядро его не трогает.
    w.ov = unsafe { std::mem::zeroed() };
    let len = u32::try_from(w.buf.len() * 8).unwrap_or(u32::MAX);
    let buf = w.buf.as_mut_ptr().cast();
    // SAFETY: буфер и OVERLAPPED живут в `Watch` до завершения чтения (освобождение —
    // только после завершения, см. `run`); хэндл открыт с FILE_FLAG_OVERLAPPED.
    let ok = unsafe {
        if w.ex {
            ReadDirectoryChangesExW(
                w.h,
                buf,
                len,
                0,
                FILTER,
                ptr::null_mut(),
                &raw mut w.ov,
                None,
                ReadDirectoryNotifyExtendedInformation,
            )
        } else {
            ReadDirectoryChangesW(
                w.h,
                buf,
                len,
                0,
                FILTER,
                ptr::null_mut(),
                &raw mut w.ov,
                None,
            )
        }
    };
    if ok == 0 {
        // SAFETY: сразу после неудачного вызова.
        return Err(unsafe { GetLastError() });
    }
    w.pending = true;
    Ok(())
}

/// Цикл потока: управляющие сообщения и завершения чтений.
fn run(port: Port, ctrl: &Receiver<Ctrl>) {
    // SAFETY: OVERLAPPED_ENTRY — POD.
    let mut done: [OVERLAPPED_ENTRY; 64] = unsafe { std::mem::zeroed() };
    let mut live = 0usize;
    let mut stopping = false;
    loop {
        let mut n = 0u32;
        // SAFETY: порт открыт до выхода из потока; массив на стеке.
        let ok = unsafe {
            GetQueuedCompletionStatusEx(port.0, done.as_mut_ptr(), 64, &raw mut n, INFINITE, 0)
        };
        if ok == 0 {
            continue;
        }
        for e in &done[..n as usize] {
            if e.lpCompletionKey == WAKE {
                while let Ok(c) = ctrl.try_recv() {
                    match c {
                        Ctrl::Add => live += 1,
                        Ctrl::Stop => stopping = true,
                        // SAFETY: ключ — адрес `Watch`, живого до его освобождения здесь же.
                        Ctrl::Remove(key, ack) => {
                            if unsafe { remove(key) } {
                                live -= 1;
                            }
                            let _ = ack.send(());
                        }
                    }
                }
                continue;
            }
            // SAFETY: ключ — адрес `Watch`; завершение пришло по его чтению.
            if unsafe { completed(e.lpCompletionKey, e.dwNumberOfBytesTransferred) } {
                live -= 1;
            }
        }
        if stopping && live == 0 {
            return;
        }
    }
}

/// `Remove`: отменить чтение и закрыть хэндл. `true` — `Watch` освобождён сразу
/// (чтение не висит); иначе — на завершении отменённого чтения.
///
/// # Safety
/// `key` — адрес живого `Watch`, которым владеет поток.
unsafe fn remove(key: usize) -> bool {
    let w = key as *mut Watch;
    // SAFETY: по контракту функции.
    let wr = unsafe { &mut *w };
    wr.closing = true;
    // SAFETY: хэндл открыт; отмена чтения этого хэндла.
    unsafe {
        CancelIoEx(wr.h, ptr::null());
        CloseHandle(wr.h);
    }
    if wr.pending {
        return false;
    }
    // SAFETY: чтение не висит — ядро буфер не трогает.
    drop(unsafe { Box::from_raw(w) });
    true
}

/// Завершение чтения. `true` — `Watch` освобождён.
///
/// # Safety
/// `key` — адрес живого `Watch` с выданным чтением.
unsafe fn completed(key: usize, bytes: u32) -> bool {
    let w = key as *mut Watch;
    // SAFETY: по контракту функции.
    let wr = unsafe { &mut *w };
    wr.pending = false;
    if wr.closing {
        // SAFETY: чтение завершено, хэндл закрыт — больше никто не обращается.
        drop(unsafe { Box::from_raw(w) });
        return true;
    }
    let status = u32::try_from(wr.ov.Internal & 0xFFFF_FFFF).unwrap_or(u32::MAX);
    match status {
        STATUS_SUCCESS if bytes > 0 => {
            let len = (bytes as usize).min(wr.buf.len() * 8);
            parse(wr, len);
        }
        STATUS_SUCCESS | STATUS_NOTIFY_ENUM_DIR => {
            flush_rename(wr);
            send(wr, WatchEvent::Overflow);
        }
        STATUS_CANCELLED => return false,
        _ => {
            // Каталог удалён (`STATUS_DELETE_PENDING`), хэндл закрыт ядром и т.п.
            send(wr, WatchEvent::Gone);
            return false;
        }
    }
    #[cfg(test)]
    {
        let ms = tests::ARM_DELAY_MS.load(std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    if arm(wr).is_err() {
        flush_rename(wr);
        send(wr, WatchEvent::Gone);
    }
    false
}

fn send(w: &Watch, ev: WatchEvent) {
    let _ = w.tx.tx.send((w.tx.id, ev));
}

/// Старое имя без нового — переименование «наружу»: удаление.
fn flush_rename(w: &mut Watch) {
    if let Some(from) = w.renamed_from.take() {
        send(w, WatchEvent::Removed(from));
    }
}

#[allow(
    clippy::cast_ptr_alignment,
    reason = "заголовки читаются read_unaligned, имена выровнены по 2"
)]
fn parse(w: &mut Watch, len: usize) {
    let base = w.buf.as_ptr().cast::<u8>();
    let mut off = 0usize;
    let mut events = Vec::new();
    loop {
        // SAFETY: `off` — из `NextEntryOffset` ядра внутри `len` байт; записи
        // выровнены по 4, а в заголовке Ex есть i64 — читаем копией без выравнивания;
        // имя — `FileNameLength` байт после заголовка.
        let (next, action, name, ent) = unsafe {
            if w.ex {
                let info = base
                    .add(off)
                    .cast::<FILE_NOTIFY_EXTENDED_INFORMATION>()
                    .read_unaligned();
                let name = name_at(
                    base,
                    off + std::mem::offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileName),
                    info.FileNameLength,
                    len,
                );
                let tag =
                    (info.FileAttributes & 0x400 != 0).then_some(info.Anonymous.ReparsePointTag);
                let ent = entry(
                    name,
                    info.FileAttributes,
                    tag,
                    info.FileSize.cast_unsigned(),
                    [
                        info.LastModificationTime,
                        info.CreationTime,
                        info.LastAccessTime,
                    ],
                );
                (info.NextEntryOffset, info.Action, name, Some(ent))
            } else {
                let info = base
                    .add(off)
                    .cast::<FILE_NOTIFY_INFORMATION>()
                    .read_unaligned();
                let name = name_at(
                    base,
                    off + std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName),
                    info.FileNameLength,
                    len,
                );
                (info.NextEntryOffset, info.Action, name, None)
            }
        };
        if !name.is_empty() {
            event(w, action, name, ent, &mut events);
        }
        if next == 0 {
            break;
        }
        off += next as usize;
        if off >= len {
            break;
        }
    }
    // Переименование внутри каталога ядро пишет парой в один буфер; старое имя
    // без нового в конце буфера — перенос наружу.
    if let Some(from) = w.renamed_from.take() {
        events.push(WatchEvent::Removed(from));
    }
    for ev in events {
        send(w, ev);
    }
}

/// Имя записи уведомления, обрезанное по границе прочитанного.
#[allow(
    clippy::cast_ptr_alignment,
    reason = "имена в записях уведомлений выровнены по 2"
)]
///
/// # Safety
/// `base..base+len` — буфер ядра; `at` — начало имени внутри него.
unsafe fn name_at<'a>(base: *const u8, at: usize, bytes: u32, len: usize) -> &'a [u16] {
    let n = (bytes as usize / 2).min(len.saturating_sub(at) / 2);
    // SAFETY: по контракту функции; имена выровнены по 2.
    unsafe { std::slice::from_raw_parts(base.add(at).cast::<u16>(), n) }
}

fn event(w: &mut Watch, action: u32, name: &[u16], ent: Option<Entry>, out: &mut Vec<WatchEvent>) {
    let name = ent
        .as_ref()
        .map_or_else(|| super::windows::decode_name(name).0, |e| e.name.clone());
    if action != FILE_ACTION_RENAMED_NEW_NAME
        && let Some(from) = w.renamed_from.take()
    {
        out.push(WatchEvent::Removed(from));
    }
    match action {
        FILE_ACTION_ADDED => out.push(WatchEvent::Added(name, ent)),
        FILE_ACTION_REMOVED => out.push(WatchEvent::Removed(name)),
        FILE_ACTION_MODIFIED => out.push(WatchEvent::Modified(name, ent)),
        FILE_ACTION_RENAMED_OLD_NAME => w.renamed_from = Some(name),
        FILE_ACTION_RENAMED_NEW_NAME => match w.renamed_from.take() {
            Some(from) => out.push(WatchEvent::Renamed {
                from,
                to: name,
                entry: ent,
            }),
            None => out.push(WatchEvent::Added(name, ent)),
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::api::WatchId;

    pub(super) static BUF_OVERRIDE: AtomicUsize = AtomicUsize::new(0);
    pub(super) static ARM_DELAY_MS: AtomicU64 = AtomicU64::new(0);

    /// Буфер 256 байт, чтение перевыдаётся с паузой 100 мс, за паузу — сотни
    /// созданий → ядро сообщает переполнение, наружу — `Overflow`.
    #[test]
    fn small_buffer_overflows() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        BUF_OVERRIDE.store(256, Ordering::Relaxed);
        let (tx, rx) = crossbeam_channel::unbounded();
        let g = watch(d, WatchTx { id: WatchId(1), tx }).unwrap();
        BUF_OVERRIDE.store(0, Ordering::Relaxed);
        ARM_DELAY_MS.store(100, Ordering::Relaxed);
        let mut overflow = false;
        for round in 0..20 {
            for i in 0..500 {
                std::fs::write(d.join(format!("r{round}-{i}")), b"").unwrap();
            }
            while let Ok((_, ev)) = rx.recv_timeout(Duration::from_millis(50)) {
                overflow |= matches!(ev, WatchEvent::Overflow);
            }
            if overflow {
                break;
            }
        }
        ARM_DELAY_MS.store(0, Ordering::Relaxed);
        drop(g);
        assert!(overflow, "no Overflow with a 256-byte buffer");
    }
}
