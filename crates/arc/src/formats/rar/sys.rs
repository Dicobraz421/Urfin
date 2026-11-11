//! FFI `UnRAR`: свои объявления по `dll.hpp` `UnRAR` 7.1.0 (`unrar_sys` 0.5.8).
//!
//! - Код `UnRAR` — статическая библиотека `unrar_sys`; её тянет в сборку крейт `unrar`
//!   (`use unrar as _`). Объявления свои: структуры `dll.hpp` упакованы (`#pragma pack(1)`),
//!   а у `unrar_sys` они `repr(C)` — поля после `CmtBuf` у него сдвинуты (время, ссылки).
//!   Соглашение — `extern "system"` (`PASCAL`/`CALLBACK` — `__stdcall`, на x64 — C).
//! - На диск `UnRAR` не пишет: только `RAR_SKIP` и `RAR_TEST`, данные — колбэком
//!   `UCM_PROCESSDATA` в наш приёмник (S8-33: снимает CVE-2025-6218, CVE-2025-8088 и обход
//!   через symlink). Имя из архива в `UnRAR` не передаётся.
//! - Колбэк получает контекст вызова [`Cx`] со стека: перед каждым вызовом `UnRAR` ставится
//!   свой (`RARSetCallback`), после — снимается. Паника в колбэке ловится (`-1`, стоп).
//! - Пароль — UTF-16 через `UCM_NEEDPASSWORDW`; `RARSetPassword` (ANSI) не используется.
//! - Дескрипторы в разных потоках независимы; общий у `UnRAR` только `ErrHandler` (код
//!   ошибки открытия при гонке может быть неточным, данные — нет).
//! - Большой словарь (`UCM_LARGEDICT`, больше 4 гибибайт) — отказ: защита памяти.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr::NonNull;

// Подключает статическую библиотеку UnRAR (`unrar_sys`) к сборке.
use unrar as _;

pub const ERAR_END_ARCHIVE: i32 = 10;
pub const ERAR_NO_MEMORY: i32 = 11;
pub const ERAR_BAD_DATA: i32 = 12;
pub const ERAR_BAD_ARCHIVE: i32 = 13;
pub const ERAR_UNKNOWN_FORMAT: i32 = 14;
pub const ERAR_EOPEN: i32 = 15;
pub const ERAR_EREAD: i32 = 18;
/// `CmtState`: комментарий не влез в буфер (обрезан).
const ERAR_SMALL_BUF: u32 = 20;
pub const ERAR_MISSING_PASSWORD: i32 = 22;
pub const ERAR_BAD_PASSWORD: i32 = 24;
pub const ERAR_LARGE_DICT: i32 = 25;

/// Режим открытия.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Mode {
    /// Листинг, части файла по томам — отдельными заголовками (`RAR_OM_LIST_INCSPLIT`).
    ListSplit = 2,
    /// Чтение данных (`RAR_OM_EXTRACT`; пишем только `RAR_TEST`).
    Extract = 1,
}

const RAR_SKIP: i32 = 0;
const RAR_TEST: i32 = 1;

const RAR_VOL_ASK: isize = 0;

const UCM_CHANGEVOLUME: u32 = 0;
const UCM_PROCESSDATA: u32 = 1;
const UCM_NEEDPASSWORD: u32 = 2;
const UCM_CHANGEVOLUMEW: u32 = 3;
const UCM_NEEDPASSWORDW: u32 = 4;

pub const RHDF_SPLITBEFORE: u32 = 0x01;
pub const RHDF_ENCRYPTED: u32 = 0x04;
pub const RHDF_DIRECTORY: u32 = 0x20;

pub const ROADF_SOLID: u32 = 0x0008;
pub const ROADF_ENCHEADERS: u32 = 0x0080;

/// `HostOS` заголовка: Windows (иначе Unix, `dll.cpp`).
pub const HOST_WIN32: u32 = 2;

/// Буфер имени файла и цели ссылки (`FileNameEx`, `RedirName`): `MAXPATHSIZE` `UnRAR`.
const NAME: usize = 0x10000;

/// Буфер комментария архива, символов UTF-16.
const COMMENT: usize = 64 << 10;

/// Длина имени тома в `UCM_CHANGEVOLUMEW` (`std::vector<wchar>(MAXPATHSIZE)`).
const VOLUME_NAME: usize = 0x10000;

type Callback = extern "system" fn(u32, isize, isize, isize) -> i32;

#[repr(C, packed)]
struct OpenArchiveDataEx {
    arc_name: *const i8,
    arc_name_w: *const u16,
    open_mode: u32,
    open_result: u32,
    cmt_buf: *mut i8,
    cmt_buf_size: u32,
    cmt_size: u32,
    cmt_state: u32,
    flags: u32,
    callback: Option<Callback>,
    user_data: isize,
    op_flags: u32,
    cmt_buf_w: *mut u16,
    reserved: [u32; 25],
}

#[repr(C, packed)]
struct HeaderDataEx {
    arc_name: [i8; 1024],
    arc_name_w: [u16; 1024],
    file_name: [i8; 1024],
    file_name_w: [u16; 1024],
    flags: u32,
    pack_size: u32,
    pack_size_high: u32,
    unp_size: u32,
    unp_size_high: u32,
    host_os: u32,
    file_crc: u32,
    file_time: u32,
    unp_ver: u32,
    method: u32,
    file_attr: u32,
    cmt_buf: *mut i8,
    cmt_buf_size: u32,
    cmt_size: u32,
    cmt_state: u32,
    dict_size: u32,
    hash_type: u32,
    hash: [u8; 32],
    redir_type: u32,
    redir_name: *mut u16,
    redir_name_size: u32,
    dir_target: u32,
    mtime_low: u32,
    mtime_high: u32,
    ctime_low: u32,
    ctime_high: u32,
    atime_low: u32,
    atime_high: u32,
    arc_name_ex: *mut u16,
    arc_name_ex_size: u32,
    file_name_ex: *mut u16,
    file_name_ex_size: u32,
    reserved: [u32; 982],
}

// Размеры по `dll.hpp` (`#pragma pack(1)`, x64): сверка раскладки при сборке.
const _: () = assert!(size_of::<HeaderDataEx>() == 10244);
const _: () = assert!(size_of::<OpenArchiveDataEx>() == 176);

// `advapi32` (`CryptGenRandom` в `crypt.cpp` UnRAR) `unrar_sys` сам не линкует: без неё
// не собираются бинарники тестов крейта.
#[link(name = "advapi32")]
unsafe extern "system" {}

// Без `#[link]`: символы — из статической библиотеки `unrar_sys`, не импорт DLL.
unsafe extern "system" {
    fn RAROpenArchiveEx(data: *mut OpenArchiveDataEx) -> *mut c_void;
    fn RARCloseArchive(h: *mut c_void) -> i32;
    fn RARReadHeaderEx(h: *mut c_void, data: *mut HeaderDataEx) -> i32;
    fn RARProcessFileW(h: *mut c_void, op: i32, path: *const u16, name: *const u16) -> i32;
    fn RARSetCallback(h: *mut c_void, cb: Option<Callback>, user: isize);
}

/// Приёмник данных записи: `false` — стоп (`UnRAR` прерывает распаковку).
pub type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> bool;

/// Контекст одного вызова `UnRAR` для колбэка.
struct Cx<'p, 's> {
    pw: Option<&'p [u16]>,
    sink: Option<Sink<'s>>,
    /// Приёмник попросил стоп.
    stopped: bool,
    /// `UnRAR` спросил пароль.
    asked: bool,
    /// Нет следующего тома: его имя.
    missing: Option<String>,
}

impl<'p, 's> Cx<'p, 's> {
    fn new(pw: Option<&'p [u16]>, sink: Option<Sink<'s>>) -> Cx<'p, 's> {
        Cx {
            pw,
            sink,
            stopped: false,
            asked: false,
            missing: None,
        }
    }
}

/// Строка UTF-16 до нуля, не длиннее `max`.
///
/// # Safety
/// `p` — строка `UnRAR` с нулём в пределах `max` символов.
unsafe fn wide_str(p: *const u16, max: usize) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: строка UnRAR кончается нулём в пределах `max` (буфер `MAXPATHSIZE`).
    while n < max && unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` символов прочитаны выше.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

extern "system" fn callback(msg: u32, user: isize, p1: isize, p2: isize) -> i32 {
    if user == 0 {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `user` — `&mut Cx` вызова, поставленный `with_cx` на время этого вызова
        // UnRAR в этом же потоке; другой ссылки на него в это время нет.
        let cx = unsafe { &mut *(user as *mut Cx<'_, '_>) };
        match msg {
            UCM_PROCESSDATA => {
                let Ok(len) = usize::try_from(p2) else {
                    return -1;
                };
                if len == 0 || p1 == 0 {
                    return 1;
                }
                // SAFETY: UnRAR отдаёт `len` байт по адресу `p1`, живых до возврата.
                let data = unsafe { std::slice::from_raw_parts(p1 as *const u8, len) };
                let keep = cx.sink.as_mut().is_none_or(|f| f(data));
                if keep {
                    1
                } else {
                    cx.stopped = true;
                    -1
                }
            }
            UCM_NEEDPASSWORDW => {
                cx.asked = true;
                let (Some(pw), Ok(cap)) = (cx.pw, usize::try_from(p2)) else {
                    return -1;
                };
                if cap == 0 || p1 == 0 {
                    return -1;
                }
                let n = pw.len().min(cap - 1);
                // SAFETY: буфер UnRAR на `cap` символов UTF-16 (`MAXPASSWORD`), пишем `n + 1`.
                unsafe {
                    let dst = p1 as *mut u16;
                    std::ptr::copy_nonoverlapping(pw.as_ptr(), dst, n);
                    *dst.add(n) = 0;
                }
                1
            }
            // ANSI-пароль не даём: только UTF-16 выше.
            UCM_NEEDPASSWORD => -1,
            UCM_CHANGEVOLUMEW | UCM_CHANGEVOLUME if p2 == RAR_VOL_ASK => {
                if msg == UCM_CHANGEVOLUMEW && cx.missing.is_none() {
                    // SAFETY: имя тома UnRAR — UTF-16 с нулём в буфере `MAXPATHSIZE`.
                    cx.missing = Some(unsafe { wide_str(p1 as *const u16, VOLUME_NAME) });
                }
                -1
            }
            UCM_CHANGEVOLUMEW | UCM_CHANGEVOLUME => 1,
            // `UCM_LARGEDICT` (5) и прочее: 0 — отказ.
            _ => 0,
        }
    }))
    .unwrap_or(-1)
}

/// Ошибка вызова `UnRAR`: код `ERAR_*` и что видел колбэк.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail {
    pub code: i32,
    /// `UnRAR` спрашивал пароль.
    pub asked: bool,
    /// Нет тома: имя.
    pub missing: Option<String>,
}

/// Заголовок записи.
#[derive(Debug, Default)]
pub struct Header {
    /// Имя UTF-16 как в архиве.
    pub name: Vec<u16>,
    pub flags: u32,
    pub pack: u64,
    pub unp: u64,
    pub host_os: u32,
    pub attr: u32,
    pub unp_ver: u32,
    /// `FILETIME` (UTC); 0 — нет.
    pub mtime: u64,
    /// `FSREDIR_*`: 0 — не ссылка.
    pub redir: u32,
    pub redir_name: String,
}

/// Открытый архив `UnRAR`. Владеет дескриптором; пользоваться — из одного потока за раз.
#[derive(Debug)]
pub struct Archive {
    h: NonNull<c_void>,
    /// `ROADF_*`.
    pub flags: u32,
    pub comment: Option<String>,
    name: Vec<u16>,
    redir: Vec<u16>,
}

// SAFETY: дескриптор UnRAR не привязан к потоку; `&mut self` у всех вызовов — один поток
// за раз.
unsafe impl Send for Archive {}

impl Archive {
    /// Открыть архив `path`; `pw` — пароль UTF-16 (зашифрованные заголовки).
    pub fn open(path: &Path, mode: Mode, pw: Option<&[u16]>) -> Result<Archive, Fail> {
        let wpath: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut cmt = vec![0u16; COMMENT];
        let mut cx = Cx::new(pw, None);
        let mut d = OpenArchiveDataEx {
            arc_name: std::ptr::null(),
            arc_name_w: wpath.as_ptr(),
            open_mode: mode as u32,
            open_result: 0,
            cmt_buf: std::ptr::null_mut(),
            cmt_buf_size: COMMENT as u32,
            cmt_size: 0,
            cmt_state: 0,
            flags: 0,
            callback: Some(callback),
            user_data: &raw mut cx as isize,
            op_flags: 0,
            cmt_buf_w: cmt.as_mut_ptr(),
            reserved: [0; 25],
        };
        // SAFETY: `d` заполнена по `dll.hpp`; путь, буфер комментария и `cx` живы до
        // возврата; колбэк снимается ниже, до того как `cx` уйдёт со стека.
        let h = unsafe { RAROpenArchiveEx(&raw mut d) };
        let h = NonNull::new(h);
        if let Some(h) = h {
            // SAFETY: дескриптор только что открыт.
            unsafe { RARSetCallback(h.as_ptr(), None, 0) };
        }
        let code = i32::try_from(d.open_result).unwrap_or(i32::MAX);
        let fail = Fail {
            code,
            asked: cx.asked,
            missing: cx.missing.take(),
        };
        let Some(h) = h else {
            return Err(fail);
        };
        let mut arc = Archive {
            h,
            flags: d.flags,
            comment: None,
            name: vec![0; NAME],
            redir: vec![0; NAME],
        };
        if code != 0 {
            // Дескриптор есть и при ошибке (не архив): закрывает `Drop`.
            return Err(fail);
        }
        let state = d.cmt_state;
        let size = d.cmt_size as usize;
        if state == 1 || state == ERAR_SMALL_BUF {
            let n = size.saturating_sub(1).min(cmt.len());
            let c = String::from_utf16_lossy(&cmt[..n]);
            arc.comment = (!c.trim().is_empty()).then_some(c);
        }
        Ok(arc)
    }

    /// Вызов `UnRAR` с контекстом `cx` для колбэка.
    fn with_cx<T>(&mut self, cx: &mut Cx<'_, '_>, f: impl FnOnce(*mut c_void) -> T) -> T {
        let h = self.h.as_ptr();
        // SAFETY: `cx` жив до конца вызова `f`; после колбэк снимается, указатель на `cx`
        // в UnRAR не остаётся.
        unsafe { RARSetCallback(h, Some(callback), std::ptr::from_mut(cx) as isize) };
        let out = f(h);
        // SAFETY: тот же дескриптор.
        unsafe { RARSetCallback(h, None, 0) };
        out
    }

    /// Следующий заголовок; `None` — конец архива.
    pub fn read_header(&mut self, pw: Option<&[u16]>) -> Result<Option<Header>, Fail> {
        // Буферы — в куче (`HeaderDataEx` около 10 КиБ).
        let mut d: Box<HeaderDataEx> = Box::new(
            // SAFETY: все поля — числа, массивы чисел и сырые указатели: нули допустимы
            // (`dll.hpp` требует обнулённый `Reserved`).
            unsafe { std::mem::zeroed() },
        );
        d.file_name_ex = self.name.as_mut_ptr();
        d.file_name_ex_size = NAME as u32;
        d.redir_name = self.redir.as_mut_ptr();
        d.redir_name_size = NAME as u32;
        self.name[0] = 0;
        self.redir[0] = 0;
        let mut cx = Cx::new(pw, None);
        let dp: *mut HeaderDataEx = &raw mut *d;
        // SAFETY: `d` и буферы имён живы до возврата; размеры буферов переданы.
        let code = self.with_cx(&mut cx, |h| unsafe { RARReadHeaderEx(h, dp) });
        match code {
            0 => {}
            ERAR_END_ARCHIVE => return Ok(None),
            code => {
                return Err(Fail {
                    code,
                    asked: cx.asked,
                    missing: cx.missing,
                });
            }
        }
        let name_len = self.name.iter().position(|&c| c == 0).unwrap_or(NAME);
        let name = if name_len > 0 {
            self.name[..name_len].to_vec()
        } else {
            let w = d.file_name_w;
            let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
            w[..n].to_vec()
        };
        let redir_len = self.redir.iter().position(|&c| c == 0).unwrap_or(NAME);
        Ok(Some(Header {
            name,
            flags: d.flags,
            pack: u64::from(d.pack_size) | (u64::from(d.pack_size_high) << 32),
            unp: u64::from(d.unp_size) | (u64::from(d.unp_size_high) << 32),
            host_os: d.host_os,
            attr: d.file_attr,
            unp_ver: d.unp_ver,
            mtime: u64::from(d.mtime_low) | (u64::from(d.mtime_high) << 32),
            redir: d.redir_type,
            redir_name: String::from_utf16_lossy(&self.redir[..redir_len]),
        }))
    }

    /// Пропустить данные записи (у solid `UnRAR` распакует её в никуда сам).
    pub fn skip(&mut self, pw: Option<&[u16]>) -> Result<(), Fail> {
        self.process(RAR_SKIP, pw, None).map(drop)
    }

    /// Данные записи — в `sink` (`RAR_TEST`: с проверкой CRC, без записи на диск).
    /// `Ok(false)` — `sink` попросил стоп; дескриптор после этого не годится.
    pub fn test(&mut self, pw: Option<&[u16]>, sink: Sink<'_>) -> Result<bool, Fail> {
        self.process(RAR_TEST, pw, Some(sink))
    }

    fn process(
        &mut self,
        op: i32,
        pw: Option<&[u16]>,
        sink: Option<Sink<'_>>,
    ) -> Result<bool, Fail> {
        let mut cx = Cx::new(pw, sink);
        let code = self.with_cx(&mut cx, |h| {
            // SAFETY: путь и имя назначения не передаются (`RAR_TEST`/`RAR_SKIP` не пишут).
            unsafe { RARProcessFileW(h, op, std::ptr::null(), std::ptr::null()) }
        });
        if cx.stopped {
            return Ok(false);
        }
        if code == 0 {
            return Ok(true);
        }
        Err(Fail {
            code,
            asked: cx.asked,
            missing: cx.missing,
        })
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        // SAFETY: дескриптор открыт `RAROpenArchiveEx` и закрывается один раз.
        unsafe { RARCloseArchive(self.h.as_ptr()) };
    }
}
