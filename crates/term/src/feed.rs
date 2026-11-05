//! Ленточный сеанс (S0 v6 №15, F02 §3.1): эмулятор без PTY, только для чтения. Вывод
//! чужих процессов (менеджеры пакетов идут через пайпы) пишется сюда построчно из любого
//! потока; показывается вместо shell, со скроллбэком и поиском терминала.

use std::sync::Arc;

use crossbeam_channel::unbounded;
use urfin_core::MsgSink;

use crate::sys::conpty::PtySize;
use crate::vt::shared::{TermShared, VtListener};

/// Скрыть курсор: вводить здесь нечего.
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Вход ленточного сеанса. Клонируется дёшево, пишет из любого потока; строки разных
/// потоков не смешиваются внутри строки. Закрытый сеанс сетку не теряет: следующий
/// `feed_session` вернёт его вместе с выводом.
#[derive(Clone, Debug)]
pub struct FeedSink {
    shared: Arc<TermShared>,
    wake: MsgSink,
}

/// Управляющие символы — вон: текст чужого процесса не управляет эмулятором.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\t' || !c.is_control())
        .collect()
}

impl FeedSink {
    pub(crate) fn new(size: PtySize, scrollback: u32, osc52: &str, wake: MsgSink) -> FeedSink {
        // Ответов эмулятора и событий ждать некому: приёмники закрыты сразу.
        let (tx, _) = unbounded();
        let (ev_tx, _) = unbounded();
        let shared = Arc::new(TermShared::new(
            size,
            scrollback,
            osc52,
            VtListener::new(tx, ev_tx),
        ));
        let sink = FeedSink { shared, wake };
        sink.write(HIDE_CURSOR);
        sink
    }

    pub fn shared(&self) -> &Arc<TermShared> {
        &self.shared
    }

    /// Байты в эмулятор как есть.
    fn write(&self, bytes: &[u8]) {
        self.shared.tap.output(bytes);
        {
            let mut vt = self.shared.vt.lock();
            let vt = &mut *vt;
            vt.processor.advance(&mut vt.term, bytes);
        }
        // Пока окно не заберёт снапшот, повторных пробуждений нет.
        if self.shared.mark_dirty() {
            self.wake.poke();
        }
    }

    fn styled(&self, style: &str, text: &str) {
        let text = clean(text);
        self.write(format!("{style}{text}{RESET}\r\n").as_bytes());
    }

    /// Строка вывода.
    pub fn line(&self, text: &str) {
        let mut text = clean(text);
        text.push_str("\r\n");
        self.write(text.as_bytes());
    }

    /// Заголовок операции: `▸ winget upgrade Mozilla.Firefox`.
    pub fn head(&self, text: &str) {
        self.styled(BOLD, text);
    }

    /// Служебная строка: `код выхода 0`.
    pub fn note(&self, text: &str) {
        self.styled(DIM, text);
    }
}
