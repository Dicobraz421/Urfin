//! Обновление программы (решение 2026-11-11 «self-update»): handler `update` над общим
//! крейтом `pets-update`. Это единственная сеть Урфина в рантайме: проверка GitHub
//! Releases (через минуту после старта и дальше раз в час — крейт сам решает, прошли ли
//! сутки) и скачивание — только по согласию («Установить»). Телеметрии нет.
//!
//! - Первая строка `main` — [`helper`]: запуск копии exe помощником установки.
//! - Старт: итог прошлой установки ([`pets_update::take_result`]) — тост; удачная
//!   установка видна и строкой «Урфин обновлён до X» (`Welcome::Updated`).
//! - Модель для окна — [`urfin_ui::update::SelfUpdate`] в `AppState.ext`: баннер «Вышла
//!   версия X · Установить», строки «Обновлений» в «О программе».
//! - Ход скачивания — задача job-бара (F11) с отменой; после установки — обычный выход с
//!   сохранением сессии, помощник ставит версию и запускает программу снова.
//!
//! Методы крейта блокирующие: проверка — задача `Lane::Bulk`, скачивание — свой поток.
//! Сеть закрыта, пока нет ключа подписи ([`UPDATE_PUBLIC_KEYS`]), политика (`AllowUpdate`
//! в реестре, `allow_update` в `policy.toml`) запрещает или фоновая проверка выключена.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use pets_update::{
    Cancel, Channel, CheckOutcome, CheckReason, InstallKind, InstallPlan, Product, Settings,
    Update, UpdateError, Updater, Version,
};
use urfin_core::{
    CmdError, Command, CommandMeta, Cx, Handler, JobSpec, JobSummary, Lane, Level, Paths, Phase,
    PolicyKey, PolicySet, msg,
};
use urfin_ui::update::{SelfUpdate, Status, UpdateCfg};

/// Открытые ключи minisign выпусков (строка `RW…` из `.pub`; второй — на время смены
/// ключа). Пусто — ни проверок, ни скачиваний. Ключ общий с Postman (id FBEB16F843D809C6,
/// 2026-11-12); тот же — в переменной `UPDATE_PUBLIC_KEY`.
pub(crate) const UPDATE_PUBLIC_KEYS: &[&str] =
    &["RWTGCdhD+Bbr+0a0+iypW5+F7ZLUgkBqE3nPPC2N3zSxTkuUPSZ5E3gb"];

/// Первая проверка по расписанию — через минуту после старта; дальше раз в час (крейт
/// сам ждёт сутки с прошлой удачной проверки и час после неудачной).
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_hours(1);

/// Продукт для крейта: репозиторий выпусков, имена артефактов, вшитые ключи.
pub(crate) fn product() -> Product {
    product_with(UPDATE_PUBLIC_KEYS)
}

fn product_with(keys: &'static [&'static str]) -> Product {
    Product {
        name: "Urfin",
        repo: "Dicobraz421/Urfin",
        asset_prefix: "urfin",
        current: Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is semver"),
        public_keys: keys,
    }
}

/// Первая строка `main`: запуск помощника установки (`--pets-update-apply`) — ставит
/// обновление и выходит, окна нет.
pub(crate) fn helper() -> Option<std::process::ExitCode> {
    pets_update::run_helper(&product())
        .map(|code| std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

/// Папка обновления в профиле: `%LOCALAPPDATA%\Urfin\updates`, portable — `data\updates`.
pub(crate) fn dir(paths: &Paths) -> PathBuf {
    paths
        .state_dir
        .parent()
        .map_or_else(|| paths.state_dir.join("updates"), |p| p.join("updates"))
}

/// Общее для handler'а и его задач.
pub(crate) struct Env {
    /// `None` — без каталогов (`--screens`): ни сети, ни файлов.
    updater: Option<Updater>,
    /// Папка обновления (итог прошлой установки).
    dir: Option<PathBuf>,
    keyed: bool,
    first: Duration,
    every: Duration,
    /// Идёт проверка или установка.
    busy: AtomicBool,
    /// Найденная версия: ставит `update.install`.
    offer: Mutex<Option<Update>>,
}

impl Env {
    /// Настоящая сеть и политика из реестра; `paths` нет — без обновления.
    pub(crate) fn new(paths: Option<&Paths>) -> Env {
        let updater = paths.map(|p| {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(Path::to_path_buf))
                .unwrap_or_default();
            let kind = InstallKind::detect(&exe_dir, urfin_core::PORTABLE_MARKER);
            Updater::new(product(), kind, dir(p))
        });
        Env::with(
            updater,
            paths.map(dir),
            !UPDATE_PUBLIC_KEYS.is_empty(),
            FIRST_CHECK,
        )
    }

    fn with(updater: Option<Updater>, dir: Option<PathBuf>, keyed: bool, first: Duration) -> Env {
        Env {
            updater,
            dir,
            keyed,
            first,
            every: CHECK_EVERY,
            busy: AtomicBool::new(false),
            offer: Mutex::new(None),
        }
    }

    fn offer(&self) -> std::sync::MutexGuard<'_, Option<Update>> {
        self.offer.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Handler `update`.
pub(crate) struct UpdateHandler {
    env: Arc<Env>,
    started: bool,
    /// Следующая проверка по расписанию.
    next: Option<Instant>,
}

impl UpdateHandler {
    pub(crate) fn new(env: Env) -> UpdateHandler {
        UpdateHandler {
            env: Arc::new(env),
            started: false,
            next: None,
        }
    }
}

const COMMANDS: &[CommandMeta] = &[
    // «Проверить сейчас» (раздел «О программе»): и при выключенной фоновой проверке.
    CommandMeta {
        id: "update.check",
        title: "Check for updates",
        category: "update",
        args: &[],
        repeatable: false,
    },
    // «Установить» в баннере «Вышла версия X».
    CommandMeta {
        id: "update.install",
        title: "Install update",
        category: "update",
        args: &[],
        repeatable: false,
    },
    // «Позже»: баннер скрыт до следующей проверки.
    CommandMeta {
        id: "update.later",
        title: "Remind about update later",
        category: "update",
        args: &[],
        repeatable: false,
    },
];

impl Handler for UpdateHandler {
    fn namespace(&self) -> &'static str {
        "update"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        match cmd.id.as_str() {
            "update.check" => check_now(&self.env, cx),
            "update.install" => install(&self.env, cx),
            "update.later" => edit(cx, |m| m.offer = None),
            _ => {}
        }
        Ok(())
    }

    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        let allowed = allowed(&self.env, cx.state.ext.get::<PolicySet>());
        if !self.started {
            self.started = true;
            cx.state.ext.insert(SelfUpdate {
                allowed,
                keyed: self.env.keyed,
                ..SelfUpdate::default()
            });
            cx.state.touch();
            startup(&self.env, cx);
            // Расписание — только с каталогами и ключом: без ключа запросов нет.
            if self.env.updater.is_some() && self.env.keyed {
                self.next = Some(Instant::now() + self.env.first);
                cx.rt.waker().wake_after(self.env.first);
            }
            return;
        }
        // Политика меняется на лету (F00-01): выключатель и кнопка — по ней.
        if cx
            .state
            .ext
            .get::<SelfUpdate>()
            .is_some_and(|m| m.allowed != allowed)
        {
            edit(cx, |m| m.allowed = allowed);
        }
        let Some(at) = self.next else {
            return;
        };
        if Instant::now() < at {
            return;
        }
        self.next = Some(Instant::now() + self.env.every);
        cx.rt.waker().wake_after(self.env.every);
        let cfg = UpdateCfg::from_config(&cx.state.config);
        // Выключено настройкой — запроса нет и в крейт не идём.
        if allowed && cfg.check {
            spawn_check(&self.env, cx, CheckReason::Scheduled);
        }
    }
}

/// Первый pump: итог прошлой установки и расписание.
fn startup(env: &Arc<Env>, cx: &mut Cx<'_>) {
    if env.updater.is_none() {
        return;
    }
    if let Some(dir) = env.dir.clone() {
        cx.rt
            .spawn(Lane::Bulk, "update.result", cx.rt.token(), move |tcx| {
                let Some(r) = pets_update::take_result(&product(), &dir) else {
                    return;
                };
                let m = msg("update.result", move |cx| installed(cx, &r));
                let _ = tcx.sink.send(m, &tcx.cancel);
            });
    }
    if !env.keyed {
        tracing::info!(target: "update", "update key is not set: update checks are off");
    }
}

/// Тост по итогу прошлой установки. Удачную показывает и строка «Урфин обновлён до X ·
/// с Y» (`screen-whatsnew`): тогда тоста нет.
fn installed(cx: &mut Cx<'_>, r: &pets_update::UpdateResult) {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("version", r.version.clone());
    if r.ok {
        tracing::info!(target: "update", version = %r.version, "updated");
        let shown =
            cx.state.ext.get::<urfin_ui::UiState>().is_some_and(|u| {
                matches!(u.welcome, Some(urfin_ui::state::Welcome::Updated { .. }))
            });
        if !shown {
            cx.toast(Level::Info, urfin_i18n::current().fmt("ui-update-done", &a));
        }
        return;
    }
    let error = r.error.clone().unwrap_or_default();
    tracing::warn!(target: "update", version = %r.version, %error, "update failed");
    a.set("error", error);
    cx.toast(
        Level::Error,
        urfin_i18n::current().fmt("ui-update-failed", &a),
    );
}

/// Обновление не запрещено: политикой крейта (HKLM и HKCU `AllowUpdate`) и набором
/// политик Урфина (`policy.toml`, GPO).
fn allowed(env: &Env, set: Option<&PolicySet>) -> bool {
    let krate = env
        .updater
        .as_ref()
        .is_none_or(|u| u.policy().updates_allowed());
    krate && set.and_then(|p| p.bool(PolicyKey::AllowUpdate)) != Some(false)
}

/// Настройки крейта из `[update]`.
fn settings(cx: &Cx<'_>) -> Settings {
    let cfg = UpdateCfg::from_config(&cx.state.config);
    Settings {
        enabled: cfg.check,
        channel: if cfg.beta(urfin_ui::version()) {
            Channel::Beta
        } else {
            Channel::Stable
        },
    }
}

fn edit(cx: &mut Cx<'_>, f: impl FnOnce(&mut SelfUpdate)) {
    if let Some(m) = cx.state.ext.get_mut::<SelfUpdate>() {
        let was = m.clone();
        f(m);
        if *m != was {
            cx.state.touch();
        }
    }
}

fn tr(key: &str, pairs: &[(&str, String)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in pairs {
        a.set((*k).to_owned(), v.clone());
    }
    urfin_i18n::current().fmt(key, &a)
}

/// `update.check`: проверка по кнопке — сразу, и при выключенной фоновой.
fn check_now(env: &Arc<Env>, cx: &mut Cx<'_>) {
    if !allowed(env, cx.state.ext.get::<PolicySet>()) {
        cx.toast(Level::Info, tr("ui-update-disabled", &[]));
        return;
    }
    if !env.keyed || env.updater.is_none() {
        cx.toast(Level::Info, tr("ui-update-no-key", &[]));
        return;
    }
    if env.busy.load(Ordering::Acquire) {
        return;
    }
    edit(cx, |m| m.status = Status::Checking);
    spawn_check(env, cx, CheckReason::Manual);
}

/// Проверка в задаче Bulk. Без ключа запросов нет вовсе.
fn spawn_check(env: &Arc<Env>, cx: &mut Cx<'_>, reason: CheckReason) {
    if !env.keyed || env.updater.is_none() || env.busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let settings = settings(cx);
    let env = Arc::clone(env);
    cx.rt
        .spawn(Lane::Bulk, "update.check", cx.rt.token(), move |tcx| {
            let res = env.updater.as_ref().map(|u| u.check(&settings, reason));
            let m = msg("update.checked", move |cx| {
                env.busy.store(false, Ordering::Release);
                if let Some(res) = res {
                    checked(&env, cx, reason, res);
                }
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Итог проверки: новая версия — баннер; по кнопке — и «последняя версия» или ошибка.
fn checked(
    env: &Env,
    cx: &mut Cx<'_>,
    reason: CheckReason,
    res: Result<CheckOutcome, UpdateError>,
) {
    let manual = reason == CheckReason::Manual;
    match res {
        Ok(CheckOutcome::Available(u)) => {
            let v = u.version.to_string();
            *env.offer() = Some(u);
            edit(cx, |m| {
                m.status = Status::Available(v.clone());
                m.offer = Some(v);
            });
        }
        Ok(CheckOutcome::UpToDate) if manual => {
            edit(cx, |m| m.status = Status::UpToDate);
            cx.toast(Level::Info, tr("ui-update-latest", &[]));
        }
        Ok(_) => {}
        // По расписанию — только журнал (README крейта).
        Err(e) if !manual => tracing::warn!(target: "update", error = %e, "update check failed"),
        Err(e) => {
            let error = e.to_string();
            edit(cx, |m| m.status = Status::Failed(error.clone()));
            cx.toast(
                Level::Warn,
                tr("ui-update-check-failed", &[("error", error)]),
            );
        }
    }
}

/// `update.install`: скачать (job-бар, отмена), проверить подпись, запустить помощника и
/// выйти обычным выходом (сессия сохраняется).
fn install(env: &Arc<Env>, cx: &mut Cx<'_>) {
    if !allowed(env, cx.state.ext.get::<PolicySet>()) {
        cx.toast(Level::Info, tr("ui-update-disabled", &[]));
        return;
    }
    let Some(update) = env.offer().clone() else {
        return;
    };
    if env.updater.is_none() || env.busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let version = update.version.to_string();
    edit(cx, |m| {
        m.status = Status::Installing(version.clone());
        m.offer = None;
    });
    let job = cx.state.jobs.start(
        JobSpec {
            kind: "update",
            title: tr("ui-update-job", &[("version", version.clone())]),
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "update",
        },
        cx.rt.sink(),
    );
    let env = Arc::clone(env);
    let _ = cx
        .rt
        .spawn_dedicated("update.install".into(), cx.rt.token(), move |tcx| {
            let Some(updater) = env.updater.as_ref() else {
                return;
            };
            let p = job.progress();
            p.items_total.store(1, Ordering::Relaxed);
            p.bytes_total.store(update.size, Ordering::Relaxed);
            p.total_final.store(true, Ordering::Relaxed);
            p.set_phase(Phase::Transfer);
            p.set_current(&update.asset_name);
            let cancel = Cancel::new();
            let res = (|| {
                let got = updater.download(&update, &cancel, &mut |pr| {
                    p.bytes_done.store(pr.received, Ordering::Relaxed);
                    if job.cancel_token().is_cancelled() || tcx.cancel.is_cancelled() {
                        cancel.cancel();
                    }
                })?;
                p.set_phase(Phase::Verify);
                let verified = updater.verify(got)?;
                p.set_phase(Phase::Finalize);
                let mut plan = InstallPlan::restart_current()?;
                if update.kind == InstallKind::Portable {
                    // Флаг portable и данные рядом с exe не перезаписываются.
                    plan.keep = vec![urfin_core::PORTABLE_MARKER.into(), "data".into()];
                }
                updater.install(verified, plan)
            })();
            let ok = res.is_ok();
            match res {
                Ok(()) => {
                    p.items_done.store(1, Ordering::Relaxed);
                    job.finish(Ok(JobSummary {
                        items: 1,
                        bytes: update.size,
                        ..JobSummary::default()
                    }));
                }
                Err(e) => {
                    tracing::warn!(target: "update", error = %e, "update install failed");
                    job.finish(Err(e.to_string()));
                    let error = (!matches!(e, UpdateError::Cancelled)).then(|| e.to_string());
                    let m = msg("update.failed", move |cx| {
                        env.busy.store(false, Ordering::Release);
                        // Баннер снова: поставить можно позже.
                        edit(cx, |m| {
                            m.status = Status::Available(version.clone());
                            m.offer = Some(version.clone());
                        });
                        if let Some(error) = error {
                            cx.toast(
                                Level::Error,
                                tr("ui-update-install-failed", &[("error", error)]),
                            );
                        }
                    });
                    let _ = tcx.sink.send(m, &urfin_core::CancelToken::new());
                }
            }
            if ok {
                // Помощник ждёт выхода программы.
                let m = msg("update.quit", |cx| cx.dispatch(Command::new("app.quit")));
                let _ = tcx.sink.send(m, &urfin_core::CancelToken::new());
            }
        });
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
