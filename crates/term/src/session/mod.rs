//! Pipeline сеанса (S6 §1.1, S6-04): поток reader (PTY → пре-сканер → эмулятор),
//! поток writer (ввод, resize, закрытие, таймаут DEC 2026), события → `Msg`.
//!
//! UI будится `poke` только при переходе `dirty` false→true: пока UI не забрал
//! снапшот, повторных пробуждений нет (poke ≤ кадров). В простое оба потока
//! заблокированы в ядре — 0 % CPU.

pub mod reader;
pub mod scan;
pub mod writer;

use std::io::{self, Read};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, unbounded};
use urfin_core::{CancelToken, Cx, Msg, MsgSink};

pub use scan::{CwdSource, OscEvent};

use crate::sys::conpty::{self, ExitWatch, PtyControl, PtySize, SpawnSpec};
use crate::vt::shared::{TermShared, VtListener};

/// Номер сеанса в пуле (S6-10).
pub type SessionId = u32;

/// Позиция события в сетке: строка курсора и размер истории в этот момент
/// (марки S6 §1.4 сдвигаются на дельту истории).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub line: i32,
    pub history: u32,
    /// Колонка курсора (конец промпта у `B`).
    pub col: u16,
}

impl Mark {
    /// Строка от верха всего буфера (история + экран); стабильна, пока скроллбэк не
    /// насыщен (тогда верхние строки уходят).
    pub fn abs(self) -> i64 {
        i64::from(self.history) + i64::from(self.line)
    }

    /// Строка на экране сейчас (история выросла — строка ушла вверх); `None` — ушла
    /// за верх экрана.
    pub fn screen_row(self, history_now: usize) -> Option<usize> {
        let abs = i64::from(self.history) + i64::from(self.line);
        usize::try_from(abs - i64::try_from(history_now).ok()?).ok()
    }
}

/// Событие сеанса для UI (редкие; вывод — через `dirty` и `poke`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Osc {
        event: OscEvent,
        at: Mark,
    },
    /// Заголовок (`None` — сброс).
    Title(Option<String>),
    /// OSC 52: текст в буфер обмена.
    Clipboard(String),
    Bell,
    /// Процесс вышел и вывод дочитан; `None` — код неизвестен.
    Exited(Option<u32>),
}

/// Команды потоку writer. UI сам в PTY не пишет: pipe может быть полон.
#[derive(Debug)]
pub enum WriterCmd {
    Bytes(Vec<u8>),
    Resize(PtySize),
    /// Начался DEC 2026: проверить таймаут в этот момент.
    Sync(Instant),
    /// Процесс вышел: закрыть псевдоконсоль, если EOF не придёт сам.
    ChildExited,
    Close,
}

/// Куда сеанс отдаёт пробуждения и события. Clone-дешёвый, из любого потока.
pub trait SessionSink: Send + Sync + 'static {
    fn poke(&self);
    /// Может блокировать (бэкпрешер); зовётся без блокировки эмулятора.
    fn event(&self, ev: SessionEvent);
}

/// Конец PTY для writer: запись, размер, закрытие.
pub trait PtyPort: Send + 'static {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn resize(&mut self, size: PtySize);
    /// Закрыть псевдоконсоль: reader дочитает вывод до EOF.
    fn close(&mut self);
}

/// Параметры сеанса.
#[derive(Clone, Debug)]
pub struct SessionOpts {
    pub size: PtySize,
    pub scrollback: u32,
    /// `term.osc52`.
    pub osc52: String,
    /// Секрет интеграции (S6 §1.3); `None` — без неё.
    pub nonce: Option<String>,
}

/// Запущенные потоки сеанса.
#[derive(Debug)]
pub(crate) struct Pipeline {
    pub shared: Arc<TermShared>,
    pub tx: Sender<WriterCmd>,
}

/// Как запустить долгоживущий поток (Runtime в бою, `std::thread` в тестах).
pub(crate) type Spawn<'a> = &'a dyn Fn(String, Box<dyn FnOnce() + Send>);

/// Поднять reader и writer над готовым PTY.
pub(crate) fn start_pipeline(
    spawn: Spawn<'_>,
    name: &str,
    src: impl Read + Send + 'static,
    port: impl PtyPort,
    exit_code: impl FnOnce() -> Option<u32> + Send + 'static,
    opts: &SessionOpts,
    sink: Arc<dyn SessionSink>,
) -> Pipeline {
    let (tx, rx) = unbounded();
    let (ev_tx, ev_rx) = unbounded();
    let shared = Arc::new(TermShared::new(
        opts.size,
        opts.scrollback,
        &opts.osc52,
        VtListener::new(tx.clone(), ev_tx),
    ));
    let scanner = scan::Scanner::new(opts.nonce.clone());
    {
        let (shared, sink, tx) = (shared.clone(), sink.clone(), tx.clone());
        spawn(
            format!("term-r-{name}"),
            Box::new(move || {
                reader::run(src, &shared, scanner, &ev_rx, &tx, &*sink);
                let _ = tx.send(WriterCmd::Close);
                sink.event(SessionEvent::Exited(exit_code()));
            }),
        );
    }
    {
        let shared = shared.clone();
        spawn(
            format!("term-w-{name}"),
            Box::new(move || writer::run(&rx, port, &shared, &*sink)),
        );
    }
    Pipeline { shared, tx }
}

/// Reader в текущем потоке до EOF, без writer (бенчмарк и тесты разбора):
/// ответы терминала отбрасываются.
#[doc(hidden)]
pub fn parse_inline(src: impl Read, opts: &SessionOpts, sink: &dyn SessionSink) -> Arc<TermShared> {
    let (tx, _rx) = unbounded();
    let (ev_tx, ev_rx) = unbounded();
    let shared = Arc::new(TermShared::new(
        opts.size,
        opts.scrollback,
        &opts.osc52,
        VtListener::new(tx.clone(), ev_tx),
    ));
    let scanner = scan::Scanner::new(opts.nonce.clone());
    reader::run(src, &shared, scanner, &ev_rx, &tx, sink);
    shared
}

/// Порт `ConPTY`: conin и управление псевдоконсолью.
#[derive(Debug)]
struct ConPort {
    writer: std::fs::File,
    control: Arc<PtyControl>,
}

impl PtyPort for ConPort {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        io::Write::write_all(&mut self.writer, bytes)
    }

    fn resize(&mut self, size: PtySize) {
        if let Err(e) = self.control.resize(size) {
            tracing::warn!(error = %e, "term: resize");
        }
    }

    fn close(&mut self) {
        self.control.close();
    }
}

/// Сеанс: процесс в `ConPTY`, эмулятор и потоки. Drop не блокирует: закрытие уходит
/// во writer, reader дочитывает вывод и сообщает `Exited`.
#[derive(Debug)]
pub struct Session {
    id: SessionId,
    shared: Arc<TermShared>,
    tx: Sender<WriterCmd>,
    control: Arc<PtyControl>,
    _exit: Option<ExitWatch>,
}

/// Сколько ждать код выхода после EOF (процесс мог не выйти, если закрыли мы).
const EXIT_WAIT: Duration = Duration::from_secs(1);

impl Session {
    /// Запустить процесс и потоки сеанса (`term-r-<id>`, `term-w-<id>`).
    ///
    /// Потоки — обычные `std::thread`, не `Runtime::spawn_dedicated`: reader спит
    /// в `ReadFile`, пока жива псевдоконсоль, и shutdown Core ждал бы его до
    /// таймаута (как hub vfs). При выходе процесса `ConPTY` закрывается вместе с
    /// хэндлами, shell получает закрытие консоли.
    pub fn start(
        id: SessionId,
        spec: &SpawnSpec,
        opts: &SessionOpts,
        sink: Arc<dyn SessionSink>,
    ) -> io::Result<Session> {
        let pty = conpty::spawn(spec, opts.size)?;
        let control = pty.control.clone();
        let wait = control.clone();
        let port = ConPort {
            writer: pty.writer,
            control: control.clone(),
        };
        let spawn = |name: String, f: Box<dyn FnOnce() + Send>| {
            if let Err(e) = std::thread::Builder::new().name(name).spawn(f) {
                tracing::error!(error = %e, "term: thread");
            }
        };
        let p = start_pipeline(
            &spawn,
            &id.to_string(),
            pty.reader,
            port,
            move || wait.wait(Some(EXIT_WAIT)),
            opts,
            sink,
        );
        let tx = p.tx.clone();
        let exit = control
            .on_exit(move |_| {
                let _ = tx.send(WriterCmd::ChildExited);
            })
            .map_err(|e| tracing::warn!(error = %e, "term: exit watch"))
            .ok();
        Ok(Session {
            id,
            shared: p.shared,
            tx: p.tx,
            control,
            _exit: exit,
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn shared(&self) -> &Arc<TermShared> {
        &self.shared
    }

    /// Канал writer: писать из фоновой задачи (тихий cd — после записи файла).
    pub(crate) fn writer(&self) -> Sender<WriterCmd> {
        self.tx.clone()
    }

    /// Байты в PTY (через writer).
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(WriterCmd::Bytes(bytes));
    }

    /// Новый размер: сетка сразу, PTY — через writer.
    pub fn resize(&self, size: PtySize) {
        if self.shared.size() != size {
            self.shared.resize(size);
            let _ = self.tx.send(WriterCmd::Resize(size));
        }
    }

    /// Закрыть: shell получает закрытие консоли, вывод дочитывается.
    pub fn close(&self) {
        let _ = self.tx.send(WriterCmd::Close);
    }

    /// Убить процесс и всё дерево.
    pub fn kill(&self) -> io::Result<()> {
        self.control.kill_tree()
    }

    /// Кроме shell есть процессы (эвристика без интеграции).
    pub fn busy(&self) -> bool {
        self.control.busy()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// Событие сеанса как `Msg`: разбирает менеджер сеансов.
#[derive(Debug)]
pub struct SessionMsg {
    pub id: SessionId,
    pub event: SessionEvent,
}

impl Msg for SessionMsg {
    fn name(&self) -> &'static str {
        "term.session"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        crate::manager::on_session_event(cx, self.id, self.event);
    }
}

/// Боевой sink: poke через `MsgSink`, события — `SessionMsg`.
#[derive(Debug, Clone)]
pub struct CoreSink {
    pub id: SessionId,
    pub sink: MsgSink,
    pub cancel: CancelToken,
}

impl SessionSink for CoreSink {
    fn poke(&self) {
        self.sink.poke();
    }

    fn event(&self, event: SessionEvent) {
        let _ = self
            .sink
            .send(Box::new(SessionMsg { id: self.id, event }), &self.cancel);
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod pipeline_tests;
