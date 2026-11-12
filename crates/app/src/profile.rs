//! Профилирование (S9 §4): Tracy под feature `profile` (S9-10), профиль кучи DHAT под
//! `dhat-heap` (S9-09). В release обе запрещены (`ci/release_features.py`); без них модуль
//! пуст и в exe ничего не добавляет.
//!
//! # Tracy (`profile`)
//!
//! **Версия Tracy GUI — 0.14.x**: протокол `tracy-client` 0.19 / `tracy-client-sys` 0.30.1
//! (`tracy/common/TracyVersion.hpp`). GUI другой минорной версии не подключится; подняли
//! `tracy-client` — сверить версию и поправить здесь.
//!
//! 1. Сборка: `cargo build -p urfin-app --profile profiling --features profile` —
//!    оптимизации release, строки для стеков, без strip (`target/profiling/urfin.exe`).
//! 2. Запуск — копия exe с `urfin.portable` рядом (профиль владельца не трогается).
//!    Клиент Tracy слушает TCP 8086 только на `127.0.0.1` (`only-localhost`); сети в
//!    продукте нет — такой сборки нет в выпуске.
//! 3. Tracy GUI → Connect `127.0.0.1`. Видно:
//!    - зоны — спаны `tracing` всех крейтов уровня DEBUG и выше (`startup.*`, `frame`,
//!      `list.*`, `ops.job`, `term.build`, `view.*` …), события — сообщениями;
//!    - кадры — `frame_mark` на каждый `CoreHost::render` (крючок
//!      `urfin_ui::frame_hook`);
//!    - график `msg/frame` — Msg, применённые Core с прошлого кадра;
//!    - потоки — по именам: `ui` (главный), Runtime `io-i-N` (Interactive), `io-b-N`
//!      (Bulk), rayon `cpu-N`, выделенные — по имени задачи. Имена Tracy берёт у Windows
//!      (`GetThreadDescription`; std ставит его по `thread::Builder::name`).
//!
//! Нет пока: график длины очереди Msg — у `urfin_core::Pump` нет длины входящей очереди
//! (полоса core). Скоупы egui из спеки — не применимо (gpui).
//!
//! # DHAT (`dhat-heap`)
//!
//! `cargo build -p urfin-app --profile profiling --features dhat-heap`; при выходе
//! процесса в текущем каталоге — `dhat-heap.json` (открыть в `dh_view.html` из
//! valgrind/DHAT). С `count-alloc` несовместима: обе ставят глобальный аллокатор.

#[cfg(all(feature = "dhat-heap", feature = "count-alloc"))]
compile_error!("features `dhat-heap` and `count-alloc` both set the global allocator");

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static DHAT: dhat::Alloc = dhat::Alloc;

/// Профиль кучи до конца `main`: при `drop` пишет `dhat-heap.json`.
#[cfg(feature = "dhat-heap")]
pub fn dhat() -> dhat::Profiler {
    dhat::Profiler::new_heap()
}

#[cfg(feature = "profile")]
pub use tracy::{init, msg_layer, tracy_layer};

#[cfg(feature = "profile")]
mod tracy {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tracing::{Event, Level, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::registry::LookupSpan;
    use tracy_client::{Client, PlotName};

    /// Msg, применённые Core с прошлого кадра.
    static MSGS: AtomicU64 = AtomicU64::new(0);

    /// Слой `tracing` → Tracy: спаны и события уровня DEBUG и выше. Свой фильтр — не
    /// `[log] level`: файл лога профилирование не раздувает.
    pub fn tracy_layer<S>() -> impl Layer<S>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        tracing_tracy::TracyLayer::default().with_filter(LevelFilter::DEBUG)
    }

    /// Счётчик Msg: Core на каждый применённый Msg пишет TRACE `applied` в
    /// `urfin_core::bus` (других TRACE там нет).
    pub fn msg_layer<S>() -> impl Layer<S>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        MsgCount.with_filter(Targets::new().with_target("urfin_core::bus", Level::TRACE))
    }

    struct MsgCount;

    impl<S: Subscriber> Layer<S> for MsgCount {
        fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
            if *event.metadata().level() == Level::TRACE {
                MSGS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// После подписчика `tracing` (клиент Tracy уже запущен слоем): имя UI-потока и
    /// крючок кадра.
    pub fn init() {
        if let Some(c) = Client::running() {
            c.set_thread_name("ui");
        }
        urfin_ui::frame_hook::set(frame);
    }

    /// Раз на кадр: граница кадра и график Msg за кадр.
    fn frame() {
        static PLOT: OnceLock<PlotName> = OnceLock::new();
        let Some(c) = Client::running() else {
            return;
        };
        c.frame_mark();
        let plot = *PLOT.get_or_init(|| PlotName::new_leak("msg/frame".into()));
        #[allow(clippy::cast_precision_loss, reason = "счётчик за кадр мал")]
        c.plot(plot, MSGS.swap(0, Ordering::Relaxed) as f64);
    }
}
