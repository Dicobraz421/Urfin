//! Старт приложения (S1 §2.9): логирование, обработчик паник, маркер падения.
//! Падения и ошибки дублируются в журнал событий Windows (`sys::event_log`).

use std::backtrace::Backtrace;
use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::IsTerminal;
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use urfin_core::{LogCfg, Paths};

use crate::sys::{EventId, event_log};

/// Переменная окружения с фильтром `tracing`; важнее `[log] level` из конфига.
pub const LOG_ENV: &str = "URFIN_LOG";

/// Маркер падения в `state_dir`; внутри — путь к crash-файлу.
const CRASH_MARKER: &str = "crashed";

/// Target события о панике: в журнал Windows оно уходит отдельно (`EventId::Crash`).
const CRASH_TARGET: &str = "urfin::crash";

/// Не больше стольких ошибок в журнал Windows за минуту.
const EVENT_LOG_PER_MINUTE: u32 = 20;

/// Логи: `log_dir/urfin.YYYY-MM-DD.log` (ротация раз в сутки, не больше
/// `keep_files` файлов, неблокирующая запись) и stderr в debug-сборках.
/// Фильтр — `URFIN_LOG`, иначе `cfg.level`; неверный фильтр не мешает старту
/// (S1 §1: ошибка конфигурации не блокирует) — предупреждение в лог. Guard
/// держать до выхода: он дописывает буфер.
pub fn init_tracing(paths: &Paths, cfg: &LogCfg) -> anyhow::Result<WorkerGuard> {
    // `Targets` (`info,urfin_keymap=debug`) вместо `EnvFilter`: без regex в бинаре.
    let config_filter = || {
        cfg.level
            .parse::<Targets>()
            .or_else(|_| "info".parse::<Targets>())
    };
    let mut bad_env = None;
    let filter = match std::env::var(LOG_ENV) {
        Ok(v) if !v.trim().is_empty() => v.parse::<Targets>().or_else(|e| {
            bad_env = Some((v, e.to_string()));
            config_filter()
        })?,
        _ => config_filter()?,
    };
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("urfin")
        .filename_suffix("log")
        .max_log_files(usize::from(cfg.keep_files.max(1)))
        .build(&paths.log_dir)
        .with_context(|| format!("log dir {}", paths.log_dir.display()))?;
    let (file, guard) = tracing_appender::non_blocking(appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file)
        .with_ansi(false)
        .with_thread_names(true);
    let stderr_layer = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(std::io::stderr().is_terminal())
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .with(EventLogLayer::default())
        .try_init()
        .context("tracing already initialized")?;
    if let Some((value, error)) = bad_env {
        tracing::warn!(%value, %error, "{LOG_ENV} ignored, using [log] level");
    }
    Ok(guard)
}

/// Паника в UI-потоке (тот, что ставит hook): `log_dir/crash-<unix-ms>.txt`
/// (сообщение, место, бэктрейс, версия, ОС), маркер `state_dir/crashed` —
/// следующий старт покажет баннер — и событие 1000 в журнале Windows. Паника в
/// другом потоке (задачу перехватит Runtime, S1 §1) — только ошибка в логе.
/// Отчёты никуда не отправляются (офлайн). Затем — прежний hook.
pub fn install_panic_hook(paths: &Paths) {
    let paths = paths.clone();
    let ui_thread = std::thread::current().id();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() != ui_thread {
            let thread = std::thread::current();
            tracing::error!(thread = thread.name().unwrap_or("?"), "panic: {info}");
            previous(info);
            return;
        }
        let details = match write_crash(&paths, info) {
            Ok(file) => {
                tracing::error!(target: CRASH_TARGET, crash = %file.display(), "panic: {info}");
                file.display().to_string()
            }
            Err(e) => {
                tracing::error!(target: CRASH_TARGET, error = %e, "panic: {info} (crash file not written)");
                format!("crash file not written: {e}")
            }
        };
        event_log(
            EventId::Crash,
            &format!(
                "Urfin {} crashed: {}\nLocation: {}\nDetails: {details}",
                env!("CARGO_PKG_VERSION"),
                panic_message(info),
                info.location()
                    .map_or_else(|| "<unknown>".to_owned(), ToString::to_string),
            ),
        );
        previous(info);
    }));
}

fn write_crash(paths: &Paths, info: &PanicHookInfo<'_>) -> std::io::Result<PathBuf> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    fs::create_dir_all(&paths.log_dir)?;
    fs::create_dir_all(&paths.state_dir)?;
    let file = paths.log_dir.join(format!("crash-{ms}.txt"));
    let thread = std::thread::current();
    let text = format!(
        "Urfin {version} crashed\n\
         os: {os} {arch}\n\
         thread: {thread}\n\
         message: {message}\n\
         location: {location}\n\
         time (unix ms): {ms}\n\n\
         backtrace:\n{backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        thread = thread.name().unwrap_or("<unnamed>"),
        message = panic_message(info),
        location = info
            .location()
            .map_or_else(|| "<unknown>".to_owned(), ToString::to_string),
        backtrace = Backtrace::force_capture(),
    );
    fs::write(&file, text)?;
    fs::write(
        paths.state_dir.join(CRASH_MARKER),
        file.to_string_lossy().as_bytes(),
    )?;
    Ok(file)
}

fn panic_message<'a>(info: &'a PanicHookInfo<'_>) -> &'a str {
    let p = info.payload();
    p.downcast_ref::<&str>()
        .copied()
        .or_else(|| p.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic>")
}

/// Маркер прошлого падения: путь к crash-файлу; маркер удаляется.
pub fn take_crash_marker(paths: &Paths) -> Option<PathBuf> {
    let marker = paths.state_dir.join(CRASH_MARKER);
    let text = fs::read_to_string(&marker).ok()?;
    if let Err(e) = fs::remove_file(&marker) {
        tracing::warn!(error = %e, "cannot remove crash marker");
    }
    let file = PathBuf::from(text.trim());
    Some(if file.as_os_str().is_empty() {
        paths.log_dir.clone()
    } else {
        file
    })
}

/// Ошибки уровня Error → журнал событий Windows (не чаще `EVENT_LOG_PER_MINUTE`).
#[derive(Default)]
struct EventLogLayer {
    /// Начало текущей минуты и число записей в ней.
    window: Mutex<Option<(Instant, u32)>>,
}

impl EventLogLayer {
    fn allow(&self) -> bool {
        let mut w = self
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        match &mut *w {
            Some((start, n)) if now.duration_since(*start) < Duration::from_secs(60) => {
                *n += 1;
                *n <= EVENT_LOG_PER_MINUTE
            }
            _ => {
                *w = Some((now, 1));
                true
            }
        }
    }
}

impl<S: Subscriber> Layer<S> for EventLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        let meta = event.metadata();
        if *meta.level() != Level::ERROR || meta.target() == CRASH_TARGET || !self.allow() {
            return;
        }
        let mut text = Fields::default();
        event.record(&mut text);
        event_log(
            EventId::Error,
            &format!("{}: {}{}", meta.target(), text.message, text.rest),
        );
    }
}

/// Сообщение события и остальные поля `k=v`.
#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value}", field.name());
        }
    }
}

/// `--askpass` (F03 §2.7): помощник `SSH_ASKPASS` для `ssh.exe` — до каталогов, логов и
/// GPUI, без окна. `ssh.exe` зовёт `urfin.exe <запрос>` с `URFIN_ASKPASS` и
/// `SSH_ASKPASS_REQUIRE` в окружении; `urfin.exe --askpass <запрос>` — то же явно.
/// `None` — обычный запуск.
pub fn askpass() -> Option<ExitCode> {
    use urfin_net::bridge::askpass;
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let by_ssh = || {
        [askpass::ENV, "SSH_ASKPASS_REQUIRE"]
            .iter()
            .all(|v| std::env::var_os(v).is_some_and(|v| !v.is_empty()))
    };
    let prompt = if first.as_deref() == Some(OsStr::new("--askpass")) {
        args.next()
    } else if by_ssh() {
        first
    } else {
        return None;
    };
    let prompt = prompt
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(ExitCode::from(
        u8::try_from(askpass::run(&prompt)).unwrap_or(1),
    ))
}
