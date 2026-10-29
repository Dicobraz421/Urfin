//! Бенч S5-25: операция через Shell `IFileOperation` без UI — тот же движок, что у
//! Проводника (S5 §7, «Конкуренты»). Ошибки элементов Shell не сообщает
//! (`FOF_NO_UI`): бенч сверяет результат по числу файлов.

#![allow(unsafe_code)]

use std::io;
use std::path::Path;

use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::UI::Shell::{
    FOF_NO_UI, FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName,
};
use windows::core::{HSTRING, PCWSTR};

use super::trash_win::Com;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ShellOp {
    Copy,
    Move,
    /// Насовсем (без `FOFX_RECYCLEONDELETE`), как Shift+Del.
    Delete,
}

fn item(p: &Path) -> io::Result<IShellItem> {
    // SAFETY: строка пути живёт до конца вызова; COM инициализирован вызывающим.
    unsafe { SHCreateItemFromParsingName(&HSTRING::from(p.as_os_str()), None) }
        .map_err(io::Error::other)
}

/// `src` в каталог `dst` (для `Delete` — не нужен). `Ok(false)` — Shell прервал
/// часть операции.
pub(crate) fn run(op: ShellOp, src: &Path, dst: Option<&Path>) -> io::Result<bool> {
    let _com = Com::init()?;
    let src = item(src)?;
    let dst = dst.map(item).transpose()?;
    // SAFETY: COM-вызовы на STA-потоке с инициализированным COM (`_com`).
    unsafe {
        let fo: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(io::Error::other)?;
        fo.SetOperationFlags(FOF_NO_UI).map_err(io::Error::other)?;
        let need = || {
            dst.as_ref()
                .ok_or_else(|| io::Error::other("no destination"))
        };
        match op {
            ShellOp::Copy => fo.CopyItem(&src, need()?, PCWSTR::null(), None),
            ShellOp::Move => fo.MoveItem(&src, need()?, PCWSTR::null(), None),
            ShellOp::Delete => fo.DeleteItem(&src, None),
        }
        .map_err(io::Error::other)?;
        fo.PerformOperations().map_err(io::Error::other)?;
        Ok(!fo
            .GetAnyOperationsAborted()
            .map_err(io::Error::other)?
            .as_bool())
    }
}
