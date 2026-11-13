//! Старт приложения (S1 §2.9): логирование, обработчик паник, маркер падения.
//! Системный журнал (Windows «Приложение», источник `Urfin`) — общий слой Pets
//! `pets_shell::SystemLog`: ERROR, WARN и INFO, что пропустил фильтр; падение и
//! неудачный старт — [`system_event`] (уровни событий — `pets-shell/LOGGING.md`).

use std::backtrace::Backtrace;
use std::ffi::OsStr;
use std::fs;
use std::io::IsTerminal;
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
pub(crate) use pets_shell::EventId;
use pets_shell::SystemLog;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
#[cfg(feature = "profile")]
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use urfin_core::{LogCfg, Paths};

/// Переменная окружения с фильтром `tracing`; важнее `[log] level` из конфига.
pub const LOG_ENV: &str = "URFIN_LOG";

/// Маркер падения в `state_dir`; внутри — путь к crash-файлу.
const CRASH_MARKER: &str = "crashed";

/// Target события о панике: в системный журнал оно уходит отдельно (`EventId::Crash`).
const CRASH_TARGET: &str = "urfin::crash";

/// Target неудачного старта: в системный журнал — отдельно (`EventId::StartupFailed`).
pub(crate) const STARTUP_TARGET: &str = "urfin::startup";

/// Источник в журнале «Приложение». Регистрацию в реестре
/// (`HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\Urfin`) делает
/// установщик; без неё Просмотр событий показывает текст с припиской «описание не найдено».
const EVENT_SOURCE: &str = "Urfin";

/// Слой системного журнала; падение и неудачный старт пишутся напрямую ([`system_event`]).
fn system_log() -> SystemLog {
    SystemLog::new(EVENT_SOURCE)
        .skip_target(CRASH_TARGET)
        .skip_target(STARTUP_TARGET)
}

/// Записать событие в системный журнал сразу (падение, неудачный старт). Источник
/// открывается здесь же: старт мог не дойти до [`init_tracing`]. Ошибки записи
/// игнорируются — журнал дополнительный канал, файл лога главный.
pub(crate) fn system_event(id: EventId, text: &str) {
    drop(SystemLog::new(EVENT_SOURCE));
    pets_shell::system_event(id, text);
}

/// Логи: системный журнал всегда (не подробнее INFO, тот же фильтр), stderr в
/// debug-сборках; файл `log_dir/urfin.YYYY-MM-DD.log` (ротация раз в сутки, не больше
/// `keep_files` файлов, неблокирующая запись) — только при уровне «Отладка» (`debug`/`trace`
/// в фильтре; решение 2026-11-13, `pets-shell/LOGGING.md`).
/// Фильтр — `URFIN_LOG`, иначе `cfg.level`; неверный фильтр не мешает старту
/// (S1 §1: ошибка конфигурации не блокирует) — предупреждение в лог. Guard (есть, когда
/// пишется файл) держать до выхода: он дописывает буфер.
pub fn init_tracing(paths: &Paths, cfg: &LogCfg) -> anyhow::Result<Option<WorkerGuard>> {
    init_tracing_as(paths, cfg, "urfin")
}

/// Как [`init_tracing`], файлы — `log_dir/<prefix>.YYYY-MM-DD.log` (`--pkg-check` —
/// `pkg-check`, F02 §4).
fn init_tracing_as(
    paths: &Paths,
    cfg: &LogCfg,
    prefix: &str,
) -> anyhow::Result<Option<WorkerGuard>> {
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
    let (file, guard) = match log_file(paths, cfg, prefix, is_debug(&filter))? {
        Some((file, guard)) => (Some(file), Some(guard)),
        None => (None, None),
    };
    let file_layer = file.map(|file| {
        tracing_subscriber::fmt::layer()
            .with_writer(file)
            .with_ansi(false)
            .with_thread_names(true)
    });
    let stderr_layer = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(std::io::stderr().is_terminal())
    });
    #[cfg(not(feature = "profile"))]
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .with(system_log());
    // Профилирование (S9-10): фильтр конфига — только у лога; Tracy и счётчик Msg — со своими.
    #[cfg(feature = "profile")]
    let registry = tracing_subscriber::registry()
        .with(
            Layer::and_then(file_layer, stderr_layer)
                .and_then(system_log())
                .with_filter(filter),
        )
        .with(crate::profile::tracy_layer())
        .with(crate::profile::msg_layer());
    registry.try_init().context("tracing already initialized")?;
    #[cfg(feature = "profile")]
    crate::profile::init();
    if let Some((value, error)) = bad_env {
        tracing::warn!(%value, %error, "{LOG_ENV} ignored, using [log] level");
    }
    Ok(guard)
}

/// Уровень «Отладка»: фильтр пропускает DEBUG хоть где-то (`debug`, `trace`,
/// `info,urfin_vfs=debug`).
fn is_debug(filter: &Targets) -> bool {
    let debug = |l: LevelFilter| l >= LevelFilter::DEBUG;
    filter.default_level().is_some_and(debug) || filter.iter().any(|(_, l)| debug(l))
}

/// Файл журнала `log_dir/<prefix>.YYYY-MM-DD.log` — только при `debug`; иначе `None`,
/// и каталог не трогается.
fn log_file(
    paths: &Paths,
    cfg: &LogCfg,
    prefix: &str,
    debug: bool,
) -> anyhow::Result<Option<(NonBlocking, WorkerGuard)>> {
    if !debug {
        return Ok(None);
    }
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .max_log_files(usize::from(cfg.keep_files.max(1)))
        .build(&paths.log_dir)
        .with_context(|| format!("log dir {}", paths.log_dir.display()))?;
    Ok(Some(tracing_appender::non_blocking(appender)))
}

/// Паника в UI-потоке (тот, что ставит hook): `log_dir/crash-<unix-ms>.txt`
/// (сообщение, место, бэктрейс, версия, ОС), маркер `state_dir/crashed` —
/// следующий старт покажет баннер — и событие 1000 в системном журнале. Паника в
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
        system_event(
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

/// `--du-elevated` (F01-12, ADR-0021): досчёт недоступных папок анализа места, когда
/// Урфин запустил сам себя через UAC — до каталогов, логов, конфига и GPUI, без окна и
/// без записи куда-либо, кроме одного файла результата. Политику `DuElevatedHelper`
/// процесс перечитывает сам (HKLM и `%ProgramData%`), не доверяя запустившему. `None` —
/// обычный запуск. Первым делом, ещё до разбора аргументов, — поиск DLL только в System32
/// (ADR-0021 «Подмена файлов»); не вышло — код [`elevated::exit::LOADER`].
pub fn du_elevated() -> Option<ExitCode> {
    use urfin_vfs::du::elevated;
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(elevated::FLAG)) {
        return None;
    }
    #[cfg(windows)]
    if urfin_ui::sys::runas::dll_search_system32().is_err() {
        return Some(ExitCode::from(elevated::exit::LOADER));
    }
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let allowed =
        urfin_ui::sys::policy::load().bool(urfin_core::PolicyKey::DuElevatedHelper) != Some(false);
    Some(ExitCode::from(elevated::main(&args, allowed)))
}

/// `--pkg-check` (F02 §4): проверка обновлений по расписанию (задача Планировщика
/// `\Urfin\PkgCheck`) — без окна, GPUI и IPC единственного экземпляра; консоли
/// менеджеров — `CREATE_NO_WINDOW`. Каталоги и конфиг — как у обычного запуска, лог —
/// `logs/pkg-check.*.log`. Итог — в `state/pkg/check.json`; открытому Урфину — звонок по
/// IPC (баннер сразу). Второй запуск, пока идёт первый, выходит сразу с кодом 0.
/// `None` — обычный запуск.
pub fn pkg_check() -> Option<ExitCode> {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(urfin_pkg::schedule::CHECK_FLAG)) {
        return None;
    }
    let paths = Paths::detect();
    if let Err(e) = paths.ensure() {
        system_event(EventId::StartupFailed, &format!("Urfin --pkg-check: {e}"));
        return Some(ExitCode::FAILURE);
    }
    let (config, _) = urfin_core::Config::load(&paths);
    // Файлы лога — `pkg-check.*.log` (имя флага без `--`).
    let prefix = urfin_pkg::schedule::CHECK_FLAG.trim_start_matches('-');
    let _log = init_tracing_as(&paths, &config.log, prefix).ok();
    let (cfg, warning) = config.section::<urfin_pkg::PkgConfig>("pkg");
    if let Some(w) = warning {
        tracing::warn!(warning = ?w, "[pkg] config section ignored");
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "pkg-check: start");
    let (code, written) = urfin_pkg::check_main(&paths.state_dir.join("pkg"), &cfg);
    if written {
        crate::ipc::notify_pkg_checked(&paths);
    }
    Some(ExitCode::from(code))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(s: &str) -> Targets {
        s.parse().unwrap()
    }

    #[test]
    fn log_debug_levels() {
        for s in ["debug", "trace", "info,urfin_vfs=debug", "warn,x=trace"] {
            assert!(is_debug(&targets(s)), "{s}");
        }
        for s in ["error", "warn", "info", "info,urfin_vfs=warn"] {
            assert!(!is_debug(&targets(s)), "{s}");
        }
    }

    #[test]
    fn log_file_only_debug() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: tmp.path().to_path_buf(),
            state_dir: tmp.path().join("state"),
            log_dir: tmp.path().join("logs"),
            cache_dir: tmp.path().join("cache"),
            portable: true,
        };
        let cfg = LogCfg::default();
        assert!(log_file(&paths, &cfg, "t", false).unwrap().is_none());
        assert!(!paths.log_dir.exists(), "no log dir without debug");
        let (mut file, guard) = log_file(&paths, &cfg, "t", true).unwrap().unwrap();
        std::io::Write::write_all(&mut file, b"x\n").unwrap();
        drop(guard);
        let n = fs::read_dir(&paths.log_dir).unwrap().count();
        assert_eq!(n, 1);
    }
}
