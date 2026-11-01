//! Процессы вне терминала (S0 v6 №15): пайпы вместо `ConPTY`, дерево — в job object.
//! Для менеджеров пакетов (F02 §3.1) и команд паролей SSH (F03): вывод читается
//! построчно, отмена и таймаут убивают всё дерево.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

pub use crate::sys::piped::{PipedChild, PipedFlags};

/// Запуск `argv` (первый элемент — программа, остальное — аргументы по правилам CRT,
/// без shell) в каталоге `cwd` с добавками к окружению Urfin `env`.
pub fn spawn_piped(
    argv: &[OsString],
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    flags: PipedFlags,
) -> io::Result<PipedChild> {
    let Some((program, rest)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    };
    let spec = crate::sys::conpty::SpawnSpec {
        program: PathBuf::from(program),
        args: rest.to_vec(),
        cwd: cwd.map(Path::to_path_buf),
        env: env.to_vec(),
    };
    crate::sys::piped::spawn(&spec, flags)
}
