//! Handler `update` без сети (решение 2026-11-11 «self-update»): `Updater::with_parts` и
//! подменная сеть, которая считает запросы. Выключенная настройка, политика и пустой ключ
//! — ни одного запроса; новая версия — баннер; итог прошлой установки — тост.

use std::io::Cursor;
use std::sync::atomic::AtomicUsize;

use pets_update::{ARCH, Policy, Response, Transport};
use urfin_core::fake::FakeListing;
use urfin_core::{Boot, Core, Location, Services, UiEvent, Waker};

use super::*;

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// Сеть: на любой запрос — список выпусков с `v0.2.0` (MSI и подпись).
#[derive(Clone, Default)]
struct Net(Arc<AtomicUsize>);

impl Net {
    fn calls(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl Transport for Net {
    fn get(&self, _url: &str) -> Result<Response, UpdateError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let asset = format!("urfin-0.2.0-windows-{ARCH}.msi");
        let body = format!(
            r#"[{{"tag_name":"v0.2.0","html_url":"https://github.com/Dicobraz421/Urfin/releases/tag/v0.2.0",
            "assets":[{{"name":"{asset}","browser_download_url":"https://example.invalid/{asset}","size":10}},
            {{"name":"{asset}.minisig","browser_download_url":"https://example.invalid/{asset}.minisig","size":1}}]}}]"#
        );
        Ok(Response {
            len: None,
            body: Box::new(Cursor::new(body.into_bytes())),
        })
    }
}

/// Кладёт набор политик в `ext`, как handler `ui` (F00-01).
struct Seed(Option<PolicySet>);

impl Handler for Seed {
    fn namespace(&self) -> &'static str {
        "seed"
    }
    fn commands(&self) -> &'static [CommandMeta] {
        &[]
    }
    fn handle(&mut self, _cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        Ok(())
    }
    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some(set) = self.0.take() {
            cx.state.ext.insert(set);
        }
    }
}

const KEY: &[&str] = &["RWQtest"];

struct Setup {
    core: Core,
    net: Net,
    dir: tempfile::TempDir,
}

/// Core с handler'ом `update`; первая проверка по расписанию — сразу.
fn setup(
    keys: &'static [&'static str],
    policy: Policy,
    config: &str,
    seed: impl FnOnce(&Path),
) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let net = Net::default();
    let updater = Updater::with_parts(
        product_with(keys),
        InstallKind::Msi,
        dir.path(),
        policy,
        Box::new(net.clone()),
    );
    let env = Env::with(
        Some(updater),
        Some(dir.path().to_path_buf()),
        !keys.is_empty(),
        Duration::ZERO,
    );
    let start = Location::local(std::env::temp_dir());
    let mut boot = Boot::new(start.clone(), start);
    boot.config.ext = config.parse().unwrap();
    let services = Services {
        listing: Arc::new(FakeListing::new(8)),
        ..Services::null()
    };
    let mut core = Core::new(boot, services, Arc::new(NoWake));
    core.register(Box::new(UpdateHandler::new(env)));
    Setup { core, net, dir }
}

/// Качать pump, пока `done` не станет правдой (или 2 с); собранные тосты.
fn pump_until(core: &mut Core, mut done: impl FnMut(&Core, &[String]) -> bool) -> Vec<String> {
    let mut toasts = Vec::new();
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        core.pump(Duration::from_millis(5));
        toasts.extend(core.take_ui_events().into_iter().filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        }));
        if done(core, &toasts) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    toasts
}

/// Несколько проходов с паузой: фоновой задаче хватило бы времени на запрос.
fn settle(core: &mut Core) -> Vec<String> {
    let mut toasts = Vec::new();
    for _ in 0..20 {
        core.pump(Duration::from_millis(5));
        toasts.extend(core.take_ui_events().into_iter().filter_map(|e| match e {
            UiEvent::Toast { text, .. } => Some(text),
            _ => None,
        }));
        std::thread::sleep(Duration::from_millis(10));
    }
    toasts
}

fn model(core: &Core) -> SelfUpdate {
    urfin_ui::update::model(core.state())
}

#[test]
fn update_setting_off_makes_no_scheduled_requests() {
    let mut s = setup(KEY, Policy::default(), "[update]\ncheck = false", |_| {});
    settle(&mut s.core);
    assert_eq!(s.net.calls(), 0);
    assert_eq!(model(&s.core).offer, None);
}

#[test]
fn update_policy_makes_no_requests() {
    // Политика крейта (реестр `AllowUpdate = 0`): ни фоновой, ни по кнопке.
    let mut s = setup(KEY, Policy::from_values(Some(0), None), "", |_| {});
    settle(&mut s.core);
    s.core.dispatch(Command::new("update.check"));
    let toasts = settle(&mut s.core);
    assert_eq!(s.net.calls(), 0);
    assert!(!model(&s.core).allowed);
    assert_eq!(toasts, [tr("ui-update-disabled", &[])]);

    // `allow_update = false` в `policy.toml` при разрешающем реестре — тоже ни одного.
    let mut s = setup(KEY, Policy::default(), "", |_| {});
    let (set, errors) = PolicySet::from_toml("allow_update = false");
    assert_eq!(errors, []);
    s.core.register(Box::new(Seed(Some(set))));
    settle(&mut s.core);
    s.core.dispatch(Command::new("update.check"));
    settle(&mut s.core);
    assert_eq!(s.net.calls(), 0);
    assert!(!model(&s.core).allowed);
}

#[test]
fn update_without_key_makes_no_requests() {
    let mut s = setup(&[], Policy::default(), "", |_| {});
    settle(&mut s.core);
    s.core.dispatch(Command::new("update.check"));
    let toasts = settle(&mut s.core);
    assert_eq!(s.net.calls(), 0);
    assert!(!model(&s.core).keyed);
    assert_eq!(toasts, [tr("ui-update-no-key", &[])]);
}

#[test]
fn update_available_shows_offer() {
    let mut s = setup(KEY, Policy::default(), "", |_| {});
    pump_until(&mut s.core, |c, _| model(c).offer.is_some());
    assert_eq!(s.net.calls(), 1);
    let m = model(&s.core);
    assert_eq!(m.offer.as_deref(), Some("0.2.0"));
    assert_eq!(m.status, Status::Available("0.2.0".into()));
    // «Позже» — баннер скрыт; по расписанию запрос не повторяется (сутки).
    s.core.dispatch(Command::new("update.later"));
    settle(&mut s.core);
    assert_eq!(model(&s.core).offer, None);
    assert_eq!(s.net.calls(), 1);
    // Кнопка проверяет сразу.
    s.core.dispatch(Command::new("update.check"));
    pump_until(&mut s.core, |c, _| model(c).offer.is_some());
    assert_eq!(s.net.calls(), 2);
}

#[test]
fn update_result_shows_toast() {
    let failed = |d: &Path| {
        std::fs::write(
            d.join("result.json"),
            r#"{"version":"0.2.0","ok":false,"error":"boom"}"#,
        )
        .unwrap();
    };
    let mut s = setup(&[], Policy::default(), "", failed);
    let toasts = pump_until(&mut s.core, |_, t| !t.is_empty());
    let want = tr(
        "ui-update-failed",
        &[("version", "0.2.0".into()), ("error", "boom".into())],
    );
    assert_eq!(toasts, [want]);
    // Итог читается один раз.
    assert!(!s.dir.path().join("result.json").exists());

    let done = |d: &Path| {
        std::fs::write(
            d.join("result.json"),
            r#"{"version":"0.2.0","ok":true,"error":null}"#,
        )
        .unwrap();
    };
    let mut s = setup(&[], Policy::default(), "", done);
    let toasts = pump_until(&mut s.core, |_, t| !t.is_empty());
    assert_eq!(
        toasts,
        [tr("ui-update-done", &[("version", "0.2.0".into())])]
    );
}

#[test]
fn update_dir_is_next_to_state() {
    let p = Paths::in_dir(PathBuf::from(r"C:\Urfin\data"));
    assert_eq!(dir(&p), PathBuf::from(r"C:\Urfin\data\updates"));
}
