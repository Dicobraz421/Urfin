//! Параметры открытия документа редактора (S7-15, S7 §5.1): кодировка вручную или детект,
//! конец новых строк, новый файл. Открытие кусками — `open` (ADR-0023 §1); прежнее
//! чтение в Rope снято в S7-34.

use std::io;

use encoding_rs::Encoding;
use urfin_core::Cancelled;
use urfin_vfs::VfsError;

use crate::config::EolCfg;
use crate::detect::Eol;

/// Как открывать.
#[derive(Clone, Debug)]
pub struct LoadOpts {
    /// Кодировка, выбранная вручную (F8, история); `None` — детект.
    pub enc: Option<&'static Encoding>,
    /// Подсказка детекту по языку интерфейса.
    pub tld: Option<&'static [u8]>,
    /// `editor.eol`: EOL новых строк.
    pub eol: EolCfg,
    /// Имя файла: язык подсветки.
    pub name: String,
    /// `editor.new`: файла нет — пустой документ с этой кодировкой (иначе — ошибка).
    pub new: Option<NewDoc>,
}

/// Новый файл (`editor.new`): кодировка и BOM из `[editor] new_codepage` / `new_bom`.
#[derive(Clone, Copy, Debug)]
pub struct NewDoc {
    pub enc: &'static Encoding,
    pub bom: bool,
}

/// Почему не открылось.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    Io(String),
    #[error("cancelled")]
    Cancelled,
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        LoadError::Io(e.to_string())
    }
}

impl From<VfsError> for LoadError {
    fn from(e: VfsError) -> Self {
        match e {
            VfsError::Cancelled => LoadError::Cancelled,
            e => LoadError::Io(e.to_string()),
        }
    }
}

impl From<Cancelled> for LoadError {
    fn from(_: Cancelled) -> Self {
        LoadError::Cancelled
    }
}

/// Концы строк в тексте.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EolCount {
    pub lf: u64,
    pub crlf: u64,
    pub cr: u64,
}

/// EOL по счёту: один вид — он, разные — `Mixed`, нет — `CrLf` (как у детекта).
pub fn eol_of(c: EolCount) -> Eol {
    let kinds = [(c.lf, Eol::Lf), (c.crlf, Eol::CrLf), (c.cr, Eol::Cr)];
    let mut present = kinds.iter().filter(|(n, _)| *n > 0);
    match (present.next(), present.next()) {
        (None, _) => Eol::default(),
        (Some((_, e)), None) => *e,
        _ => Eol::Mixed,
    }
}

/// Конец новых строк: из `editor.eol` или доминирующий в файле (поровну — CRLF; новый
/// файл и файл без переводов строк — CRLF).
pub fn eol_new(cfg: EolCfg, c: EolCount) -> Eol {
    match cfg {
        EolCfg::Lf => Eol::Lf,
        EolCfg::Crlf => Eol::CrLf,
        EolCfg::File => {
            if c.lf > c.crlf && c.lf >= c.cr {
                Eol::Lf
            } else if c.cr > c.crlf && c.cr > c.lf {
                Eol::Cr
            } else {
                Eol::CrLf
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_by_counts() {
        let c = |lf, crlf, cr| EolCount { lf, crlf, cr };
        assert_eq!(eol_of(c(0, 0, 0)), Eol::default());
        assert_eq!(eol_of(c(3, 0, 0)), Eol::Lf);
        assert_eq!(eol_of(c(0, 2, 0)), Eol::CrLf);
        assert_eq!(eol_of(c(1, 2, 0)), Eol::Mixed);
        assert_eq!(eol_new(EolCfg::File, c(3, 1, 0)), Eol::Lf);
        assert_eq!(eol_new(EolCfg::File, c(1, 1, 0)), Eol::CrLf);
        assert_eq!(eol_new(EolCfg::File, c(0, 0, 2)), Eol::Cr);
        assert_eq!(eol_new(EolCfg::Lf, c(0, 5, 0)), Eol::Lf);
    }
}
