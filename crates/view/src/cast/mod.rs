//! Просмотр записи сеанса `.cast` (F03 §3.6): режим `Cast` вьювера. Разбор asciicast v2
//! ([`parse`]), вывод без VT ([`strip`]), проход по файлу с индексом ключевых смещений и
//! кадров ([`scan`]), повтор в отдельный `alacritty Term` ([`replay`]), поток плеера.
//! Файл в память не читается: события — потоком, текст без VT — во временный файл.
//! Состояние документа — [`CastState`]; команды — `viewer.cast.*`.

mod key;
pub mod parse;
pub(crate) mod player;
pub mod replay;
pub mod scan;
pub mod strip;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crossbeam_channel::Sender;
use urfin_term::vt::grid::{Defaults, Snapshot};

pub use parse::{Ev, Event, Header, Lines, event, header};
pub use replay::Replay;
pub use scan::{CastIndex, KEY_EVERY_BYTES, KEY_EVERY_MS, Key, scan};
pub use strip::Strip;

use crate::source::ByteSource;

/// Шаг ← → (F03 §3.6).
pub const SEEK_STEP_S: i64 = 5;

/// Запись в документе вьювера.
#[derive(Debug)]
pub struct CastState {
    pub header: Header,
    /// Сам файл: Hex и плеер.
    pub(crate) file: Arc<dyn ByteSource>,
    /// Вывод без VT: «Текст» и поиск; появляется после прохода.
    pub(crate) text: Option<Arc<dyn ByteSource>>,
    pub index: Option<Arc<CastIndex>>,
    /// Пройдено байт файла (статус «Индексация…»).
    pub scanned: Arc<AtomicU64>,
    pub(crate) tx: Option<Sender<player::Cmd>>,
    /// Положение плеера, мс.
    pub ms: u64,
    pub playing: bool,
    /// 1, 2 или 4.
    pub speed: u8,
    /// Экран плеера.
    pub frame: Option<Arc<Snapshot>>,
    /// Цвета терминала от темы.
    pub(crate) colors: Option<Defaults>,
    /// Номер последней запрошенной перемотки и выполненной.
    pub(crate) seq: u64,
    pub(crate) seen: u64,
    /// Совпадения поиска на шкале (доли), по порядку `SearchState::marks`.
    pub ticks: Arc<[f32]>,
}

impl CastState {
    pub(crate) fn new(header: Header, file: Arc<dyn ByteSource>) -> CastState {
        CastState {
            header,
            file,
            text: None,
            index: None,
            scanned: Arc::default(),
            tx: None,
            ms: 0,
            playing: false,
            speed: 1,
            frame: None,
            colors: None,
            seq: 0,
            seen: 0,
            ticks: Arc::from(Vec::new()),
        }
    }

    /// Плеер показал последнюю запрошенную перемотку в цветах темы.
    pub fn settled(&self) -> bool {
        self.index.is_some()
            && self.frame.is_some()
            && self.colors.is_some()
            && self.seq == self.seen
    }

    /// Цвета терминала, в которых идут кадры.
    pub fn colors(&self) -> Option<Defaults> {
        self.colors
    }

    /// Размер файла записи.
    pub fn file_len(&self) -> u64 {
        self.file.len()
    }

    pub fn duration_ms(&self) -> u64 {
        self.index.as_ref().map_or(0, |i| i.duration_ms)
    }

    /// Доля шкалы положения плеера.
    pub fn frac(&self) -> f32 {
        self.index.as_ref().map_or(0.0, |i| i.frac(self.ms))
    }

    /// Цвета терминала для кадров; `false` — те же.
    pub(crate) fn colors_changed(&mut self, d: Defaults) -> bool {
        if self.colors == Some(d) {
            return false;
        }
        self.colors = Some(d);
        true
    }

    pub(crate) fn send(&self, c: player::Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(c);
        }
    }
}
