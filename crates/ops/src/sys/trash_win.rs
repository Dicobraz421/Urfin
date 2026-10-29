//! Корзина через Shell (S5 §4.7, S5-13): один `IFileOperation` на пакет
//! (`FOF_NO_UI | FOFX_RECYCLEONDELETE | FOFX_EARLYFAILURE`), все элементы —
//! одним `PerformOperations`, как Проводник. Поток вызова — STA.
//!
//! `IFileOperationProgressSink::PreDeleteItem` без `TSF_DELETE_RECYCLE_IF_POSSIBLE`
//! значит «в корзину нельзя, удалится насовсем» (больше лимита корзины, сеть,
//! съёмный носитель) — элемент не трогаем: `E_ABORT`, он в [`Outcome::no_trash`],
//! пакет перезапускается без него. Пути длиннее `MAX_PATH` Shell не принимает —
//! `no_trash` до старта.

#![allow(unsafe_code)]
// Код, порождённый `#[implement]` (COM-объект sink), — вне нашего контроля.
#![allow(
    clippy::ref_as_ptr,
    clippy::inline_always,
    reason = "код макроса #[implement]"
)]

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use urfin_core::CancelToken;
use windows::Win32::Foundation::{E_ABORT, MAX_PATH};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FOF_NO_UI, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation,
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
    SHCreateItemFromParsingName, SIGDN_FILESYSPATH, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows::core::{HRESULT, HSTRING, PCWSTR, Ref, implement};

/// Итог пакета.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// Перемещены в корзину.
    pub recycled: Vec<PathBuf>,
    /// В корзину нельзя (удалились бы насовсем): не тронуты.
    pub no_trash: Vec<PathBuf>,
    /// Ошибка Shell по элементу.
    pub failed: Vec<(PathBuf, String)>,
}

/// COM на этом потоке (STA) до конца области.
/// COM текущего потока (STA) на время жизни значения.
pub(crate) struct Com;

impl Com {
    pub(crate) fn init() -> io::Result<Com> {
        // SAFETY: инициализация COM текущего потока; парный `CoUninitialize` в Drop.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(io::Error::other)?;
        Ok(Com)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: парный к успешному `CoInitializeEx` на этом же потоке.
        unsafe { CoUninitialize() };
    }
}

#[derive(Default)]
struct SinkState {
    /// Элементы пакета: (путь вызывающего, имя от Shell). Сравнение — по имени
    /// Shell: путь вызывающего бывает в форме 8.3, сетевой и т.п.
    items: Vec<(PathBuf, PathBuf)>,
    no_trash: Vec<PathBuf>,
    /// Shell собирался удалить насовсем что-то не из пакета (ребёнка): прервано.
    unknown_abort: bool,
    failed: Vec<(PathBuf, String)>,
    recycled: Vec<PathBuf>,
    cancelled: bool,
}

#[implement(IFileOperationProgressSink)]
struct Sink {
    st: Rc<RefCell<SinkState>>,
    /// Отмена job: `UpdateProgress` прерывает пакет.
    cancel: CancelToken,
}

fn path_of(item: &Ref<'_, IShellItem>) -> Option<PathBuf> {
    shell_name(item.ok().ok()?)
}

/// Имя элемента от Shell (`SIGDN_FILESYSPATH`).
fn shell_name(item: &IShellItem) -> Option<PathBuf> {
    // SAFETY: `GetDisplayName` отдаёт строку CoTaskMem — освобождаем после копии.
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

fn same(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

impl SinkState {
    /// Путь вызывающего для имени от Shell.
    fn ours(&self, shell: &Path) -> Option<PathBuf> {
        self.items
            .iter()
            .find(|(_, n)| same(n, shell))
            .map(|(p, _)| p.clone())
    }
}

impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _hr: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    /// Без `TSF_DELETE_RECYCLE_IF_POSSIBLE` Shell удалил бы насовсем — всегда
    /// прерываем, даже если элемент не опознан: насовсем в задаче «в корзину» нельзя.
    fn PreDeleteItem(&self, flags: u32, item: Ref<IShellItem>) -> windows::core::Result<()> {
        #[allow(clippy::cast_sign_loss, reason = "битовый флаг Shell")]
        let recycle = flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 != 0;
        if recycle {
            return Ok(());
        }
        let mut st = self.st.borrow_mut();
        match path_of(&item).and_then(|p| st.ours(&p)) {
            Some(p) => st.no_trash.push(p),
            None => st.unknown_abort = true,
        }
        Err(E_ABORT.into())
    }
    fn PostDeleteItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        hr: HRESULT,
        _created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        let Some(p) = path_of(&item) else {
            return Ok(());
        };
        let mut st = self.st.borrow_mut();
        let Some(p) = st.ours(&p) else {
            return Ok(());
        };
        if hr.is_ok() {
            st.recycled.push(p);
        } else if hr != E_ABORT {
            st.failed
                .push((p, windows::core::Error::from_hresult(hr).message()));
        }
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _total: u32, _done: u32) -> windows::core::Result<()> {
        if self.cancel.is_cancelled() {
            self.st.borrow_mut().cancelled = true;
            return Err(E_ABORT.into());
        }
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Один проход: все `items` одним `PerformOperations`.
fn pass(items: &[PathBuf], st: &Rc<RefCell<SinkState>>, cancel: &CancelToken) -> io::Result<()> {
    // SAFETY: COM-вызовы на STA-потоке с инициализированным COM ([`Com`]);
    // `sink` живёт до `Unadvise`.
    unsafe {
        let op: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(io::Error::other)?;
        op.SetOperationFlags(FOF_NO_UI | FOFX_RECYCLEONDELETE | FOFX_EARLYFAILURE)
            .map_err(io::Error::other)?;
        let sink: IFileOperationProgressSink = Sink {
            st: Rc::clone(st),
            cancel: cancel.clone(),
        }
        .into();
        let cookie = op.Advise(&sink).map_err(io::Error::other)?;
        for p in items {
            let item = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from(p.as_os_str()),
                None,
            );
            match item {
                Ok(item) => {
                    let name = shell_name(&item).unwrap_or_else(|| p.clone());
                    st.borrow_mut().items.push((p.clone(), name));
                    op.DeleteItem(&item, None).map_err(io::Error::other)?;
                }
                Err(e) => st.borrow_mut().failed.push((p.clone(), e.message())),
            }
        }
        // Ошибка — отменённые/прерванные элементы; их итог уже в состоянии sink.
        let r = op.PerformOperations();
        let _ = op.Unadvise(cookie);
        if let Err(e) = r
            && e.code() != E_ABORT
            && !st.borrow().cancelled
            && st.borrow().no_trash.is_empty()
        {
            tracing::debug!(error = %e, "trash pass");
        }
    }
    Ok(())
}

/// В корзину: пакет, при «нельзя в корзину» — пакет заново без такого элемента.
/// `Err(Interrupted)` — отменено (`cancel`).
pub(crate) fn recycle(items: &[PathBuf], cancel: &CancelToken) -> io::Result<Outcome> {
    let _com = Com::init()?;
    let mut out = Outcome::default();
    let mut left: Vec<PathBuf> = Vec::new();
    for p in items {
        if p.as_os_str().len() >= MAX_PATH as usize {
            out.no_trash.push(p.clone());
        } else {
            left.push(p.clone());
        }
    }
    while !left.is_empty() {
        let st = Rc::new(RefCell::new(SinkState::default()));
        pass(&left, &st, cancel)?;
        let st = Rc::try_unwrap(st).map_or_else(|rc| rc.take(), RefCell::into_inner);
        if st.cancelled {
            return Err(io::ErrorKind::Interrupted.into());
        }
        out.recycled.extend(st.recycled);
        out.failed.extend(st.failed);
        if st.unknown_abort && st.no_trash.is_empty() {
            // Не опознали, что Shell хотел удалить насовсем: оставшиеся — вопросом.
            left.retain(|p| p.exists());
            out.no_trash.append(&mut left);
            break;
        }
        if st.no_trash.is_empty() {
            break;
        }
        // Остальные — заново: уже в корзине их нет на месте.
        left.retain(|p| !st.no_trash.iter().any(|n| same(n, p)) && p.exists());
        out.no_trash.extend(st.no_trash);
    }
    Ok(out)
}
